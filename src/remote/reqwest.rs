use crate::Pointer;
use crate::remote::LfsRemote;
use crate::remote::UploadProgress;
use crate::remote::Write;
use crate::remote::dto::BatchResponse;

use bytes::Bytes;
use reqwest::header::CONTENT_LENGTH;
use reqwest::header::HeaderMap;
use reqwest::header::LOCATION;
use url::Url;

use sha2::Digest;
use sha2::Sha256;

use async_trait::async_trait;

use crate::remote::RemoteError;

use crate::remote::MEDIA_TYPE;
use crate::remote::dto::*;

const USER_AGENT: &str = "gx-lfs/0.0.0";

const UPLOAD_CHUNK: usize = 64 * 1024;

trait ReqwestExt {
	async fn or_err<T: FnOnce(String) -> RemoteError>(self, or_else: T) -> Result<reqwest::Response, RemoteError>;
}

pub struct ReqwestLfsClient {
	client: reqwest::Client,
	url: Url,
	access_token: Option<String>,
	headers: Option<HeaderMap>,
}

impl ReqwestLfsClient {
	pub fn new(url: Url, access_token: Option<String>) -> Self {
		Self {
			client: reqwest::Client::new(),
			url,
			access_token,
			headers: None,
		}
	}

	pub fn headers(self, headers: HeaderMap) -> Self {
		Self {
			headers: Some(headers),
			..self
		}
	}
}

impl ReqwestExt for Result<reqwest::Response, reqwest::Error> {
	async fn or_err<T: FnOnce(String) -> RemoteError>(self, or_else: T) -> Result<reqwest::Response, RemoteError> {
		let res = self.map_err(|e| RemoteError::Custom(Box::new(e)))?;

		if !res.status().is_success() {
			use reqwest::StatusCode as S;

			return match res.status() {
				S::FORBIDDEN | S::UNAUTHORIZED => Err(RemoteError::AccessDenied),
				S::NOT_FOUND => Err(RemoteError::NotFound),
				_ => {
					let status = res.status();
					let body = res.text().await.unwrap_or_default();
					Err(or_else(format!("{} - {}", status, body)))
				}
			};
		}

		Ok(res)
	}
}

/// `blob` in chunks, each reported to `progress` as hyper takes it to send.
fn upload_body(blob: &[u8], progress: UploadProgress) -> reqwest::Body {
	let blob = Bytes::copy_from_slice(blob);
	let chunks = (0..blob.len()).step_by(UPLOAD_CHUNK).map(move |start| {
		let end = (start + UPLOAD_CHUNK).min(blob.len());
		progress.sent(end);
		Ok::<_, std::io::Error>(blob.slice(start..end))
	});

	reqwest::Body::wrap_stream(futures::stream::iter(chunks))
}

/// reqwest can't replay a streamed body, so it leaves a 307/308 of an upload to us.
fn redirect_to(href: &Url, res: &reqwest::Response) -> Option<Url> {
	use reqwest::StatusCode as S;

	if !matches!(res.status(), S::TEMPORARY_REDIRECT | S::PERMANENT_REDIRECT) {
		return None;
	}

	href.join(res.headers().get(LOCATION)?.to_str().ok()?).ok()
}

#[async_trait]
impl LfsRemote for ReqwestLfsClient {
	async fn batch(&self, req: BatchRequest) -> Result<BatchResponse, RemoteError> {
		let mut batch_url = self.url.clone();
		batch_url
			.path_segments_mut()
			.map_err(|_| RemoteError::UrlParse(url::ParseError::RelativeUrlWithoutBase))?
			.pop_if_empty()
			.push("objects")
			.push("batch");

		let mut request = self
			.client
			.post(batch_url)
			.header("User-Agent", USER_AGENT)
			.header("Accept", MEDIA_TYPE)
			.header("Content-Type", MEDIA_TYPE)
			.json(&req);

		if let Some(token) = &self.access_token {
			request = request.basic_auth("oauth2", Some(token));
		}

		if let Some(headers) = &self.headers {
			request = request.headers(headers.clone());
		}

		let res = request.send().await.or_err(RemoteError::Batch).await?;
		let res = res.json::<BatchResponse>().await.map_err(|e| RemoteError::Custom(Box::new(e)))?;

		if res.objects.is_empty() {
			return Err(RemoteError::EmptyResponse);
		}

		Ok(res)
	}

	async fn download(&self, action: &ObjectAction, to: &mut Write) -> Result<Pointer, RemoteError> {
		use futures::StreamExt;

		let mut req = self.client.get(&action.href);

		for (key, value) in action.header.iter() {
			req = req.header(key, value);
		}

		req = req.header("User-Agent", USER_AGENT);

		let res = req.send().await.or_err(RemoteError::Download).await?;

		let mut bytes = res.bytes_stream();
		let mut total = 0;

		let mut checksum = Sha256::new();

		while let Some(chunk) = bytes.next().await {
			let chunk = chunk.map_err(|e| RemoteError::Download(crate::report_error(&e)))?;
			to.write_all(&chunk)?;
			total += chunk.len();
			checksum.update(&chunk);
		}

		let hash = checksum.finalize();

		Ok(Pointer::from_parts(hash.as_slice(), total))
	}

	async fn upload(&self, action: &ObjectAction, blob: &[u8], progress: UploadProgress) -> Result<(), RemoteError> {
		let put = |url: Url, with_auth: bool, progress| {
			let mut req = self.client.put(url);

			for (key, value) in action
				.header
				.iter()
				.filter(|(key, _)| with_auth || !key.eq_ignore_ascii_case("authorization"))
			{
				req = req.header(key, value);
			}

			req = req.header("User-Agent", USER_AGENT);
			// a streamed body has no length of its own, and hyper sends a body without one chunked, which not every
			// lfs server accepts; a `Transfer-Encoding` the server asked for in the action's headers still wins
			req = req.header(CONTENT_LENGTH, blob.len());
			req.body(upload_body(blob, progress)).send()
		};

		let href = Url::parse(&action.href)?;
		let mut res = put(href.clone(), true, progress.clone()).await;

		if let Some(to) = res.as_ref().ok().and_then(|res| redirect_to(&href, res)) {
			// like reqwest's own redirects, credentials don't go to another host
			let same_host = to.host_str() == href.host_str() && to.port_or_known_default() == href.port_or_known_default();
			res = put(to, same_host, progress.resend()).await;
		}

		res.or_err(RemoteError::Upload).await?;

		Ok(())
	}

	async fn verify(&self, action: &ObjectAction, pointer: &Pointer) -> Result<(), RemoteError> {
		let mut req = self.client.post(&action.href);

		for (key, value) in action.header.iter() {
			req = req.header(key, value);
		}

		req = req.header("User-Agent", USER_AGENT);

		req
			.json(&BatchObject {
				oid: pointer.hex(),
				size: pointer.size() as u64,
			})
			.send()
			.await
			.or_err(RemoteError::Verify)
			.await?;

		Ok(())
	}
}
