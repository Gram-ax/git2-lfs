use crate::Pointer;
use crate::remote::Read;
use crate::remote::Remote;
use crate::remote::Write;
use crate::remote::dto::BatchResponse;
use crate::report_error;

use reqwest::Body;
use reqwest::header::HeaderMap;
use url::Url;

use sha2::Digest;
use sha2::Sha256;

use async_trait::async_trait;

use crate::remote::RemoteError;

use crate::remote::MEDIA_TYPE;
use crate::remote::dto::*;

trait ReqwestExt {
  async fn or_err<T: FnOnce(String) -> RemoteError>(
    self,
    or_else: T,
  ) -> Result<reqwest::Response, RemoteError>;
}

pub struct ReqwestLfsClient {
  client: reqwest::Client,
  url: Url,
  access_token: Option<String>,
  headers: Option<HeaderMap>,
}

impl ReqwestLfsClient {
  pub fn new(url: Url, access_token: Option<String>) -> Self {
    Self { client: reqwest::Client::new(), url, access_token, headers: None }
  }

  pub fn headers(self, headers: HeaderMap) -> Self {
    Self { headers: Some(headers), ..self }
  }
}

impl ReqwestExt for Result<reqwest::Response, reqwest::Error> {
  async fn or_err<T: FnOnce(String) -> RemoteError>(
    self,
    or_else: T,
  ) -> Result<reqwest::Response, RemoteError> {
    let res = self.map_err(|e| RemoteError::Custom(Box::new(e)))?;

    if !res.status().is_success() {
      use reqwest::StatusCode as S;

      return match res.status() {
        S::FORBIDDEN => Err(RemoteError::AccessDenied),
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

#[async_trait]
impl Remote for ReqwestLfsClient {
  async fn batch(&self, req: BatchRequest) -> Result<BatchResponse, RemoteError> {
    let mut batch_url = self.url.clone();
    batch_url
      .path_segments_mut()
      .map_err(|_| RemoteError::UrlParse(url::ParseError::RelativeUrlWithoutBase))?
      .pop_if_empty()
      .push("objects")
      .push("batch");

    let mut request =
      self.client.post(batch_url).header("Accept", MEDIA_TYPE).header("Content-Type", MEDIA_TYPE).json(&req);

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

    let res = req.send().await.or_err(RemoteError::Download).await?;

    let mut bytes = res.bytes_stream();
    let mut total = 0;

    let mut checksum = Sha256::new();

    while let Some(chunk) = bytes.next().await {
      let chunk = chunk.map_err(|e| RemoteError::Download(crate::report_error(&e)))?;
      total += to.write(&chunk)?;
      checksum.update(&chunk);
    }

    let hash = checksum.finalize();

    Ok(Pointer::from_parts(hash.as_slice(), total))
  }

  async fn upload(&self, action: &ObjectAction, from: Vec<u8>) -> Result<(), RemoteError> {
    let mut req = self.client.put(&action.href);

    for (key, value) in action.header.iter() {
      req = req.header(key, value);
    }

    req.body(from).send().await.or_err(RemoteError::Upload).await?;

    Ok(())
  }

  async fn verify(&self, action: &ObjectAction, pointer: &Pointer) -> Result<(), RemoteError> {
    let mut req = self.client.put(&action.href);

    for (key, value) in action.header.iter() {
      req = req.header(key, value);
    }

    req
      .json(&BatchObject { oid: pointer.hex(), size: pointer.size() as u64 })
      .send()
      .await
      .or_err(RemoteError::Verify)
      .await?;

    Ok(())
  }
}
