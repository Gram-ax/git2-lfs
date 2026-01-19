use std::fs::File;
use std::io::BufWriter;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::Pointer;

use async_trait::async_trait;

use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use tracing::*;

pub use dto::*;

mod dto;

#[cfg(all(feature = "reqwest-backend", not(target_family = "wasm")))]
pub mod reqwest;

pub const MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

#[derive(thiserror::Error, Debug)]
pub enum RemoteError {
	#[error("access denied")]
	AccessDenied,

	#[error("object error: {0}")]
	ObjectError(String),

	#[error("not found")]
	NotFound,

	#[error("batch failed: {0}")]
	Batch(String),

	#[error("download failed: {0}")]
	Download(String),

	#[error("upload failed: {0}")]
	Upload(String),

	#[error("verify failed: {0}")]
	Verify(String),

	#[error("checksum mismatch")]
	ChecksumMismatch,

	#[error("empty response")]
	EmptyResponse,

	#[error("url parse error: {0}")]
	UrlParse(#[from] url::ParseError),

	#[error("io: {0}")]
	Io(#[from] std::io::Error),

	#[error(transparent)]
	Custom(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub type Write = dyn std::io::Write + Send;
pub type Read = dyn std::io::Read + Send;

pub type OnProgress<'a> = dyn Fn(Progress) -> () + 'a;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Progress {
	Download(ProgressEvent),
	Verify(ProgressEvent),
	Upload(ProgressEvent),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
	pub total_objects: usize,
	pub total_bytes: usize,

	pub bytes_handled: usize,
	pub objects_handled: usize,

	pub next_object_size: usize,
}

impl Progress {
	pub fn total_objects(&self) -> usize {
		match self {
			Progress::Download(event) | Progress::Upload(event) | Progress::Verify(event) => event.total_objects,
		}
	}

	pub fn total_bytes(&self) -> usize {
		match self {
			Progress::Download(event) | Progress::Upload(event) | Progress::Verify(event) => event.total_bytes,
		}
	}

	pub fn bytes_handled(&self) -> usize {
		match self {
			Progress::Download(event) | Progress::Upload(event) | Progress::Verify(event) => event.bytes_handled,
		}
	}

	pub fn objects_handled(&self) -> usize {
		match self {
			Progress::Download(event) | Progress::Upload(event) | Progress::Verify(event) => event.objects_handled,
		}
	}

	pub fn next_object_size(&self) -> usize {
		match self {
			Progress::Download(event) | Progress::Upload(event) | Progress::Verify(event) => event.next_object_size,
		}
	}
}

#[async_trait]
pub trait LfsRemote: Send + Sync {
	async fn batch(&self, req: BatchRequest) -> Result<BatchResponse, RemoteError>;
	async fn download(&self, action: &ObjectAction, to: &mut Write) -> Result<Pointer, RemoteError>;
	async fn upload(&self, action: &ObjectAction, blob: &[u8]) -> Result<(), RemoteError>;
	async fn verify(&self, action: &ObjectAction, pointer: &Pointer) -> Result<(), RemoteError>;
}

pub struct LfsClient<'a, C: Send + Sync> {
	repo: &'a git2::Repository,
	client: C,
	on_progress: Option<Box<OnProgress<'a>>>,
	concurrency_limit: usize,
}

impl<'a, C: LfsRemote + Send + Sync> LfsClient<'a, C> {
	pub fn new(repo: &'a git2::Repository, client: C) -> Self {
		Self {
			repo,
			client,
			on_progress: None,
			concurrency_limit: 1,
		}
	}

	pub fn concurrency_limit(self, concurrency_limit: usize) -> Self {
		Self { concurrency_limit, ..self }
	}

	pub fn on_progress(self, on_progress: Option<Box<OnProgress<'a>>>) -> Self {
		Self { on_progress, ..self }
	}

	pub async fn pull(&self, pointers: &[Pointer]) -> Result<(), RemoteError> {
		if pointers.is_empty() {
			return Ok(());
		}

		let request = BatchRequest {
			operation: "download".to_string(),
			transfers: vec!["basic".to_string()],
			objects: pointers
				.iter()
				.map(|p| BatchObject {
					oid: p.hex(),
					size: p.size() as u64,
				})
				.collect(),
			hash_algo: Some("sha256".to_string()),
		};

		let response = self.client.batch(request).await?;

		self.download_objects(response, pointers).await
	}

	pub async fn push(&self, pointers: &[Pointer]) -> Result<(), RemoteError> {
		if pointers.is_empty() {
			return Ok(());
		}

		let request = BatchRequest {
			operation: "upload".to_string(),
			transfers: vec!["basic".to_string()],
			objects: pointers
				.iter()
				.map(|p| BatchObject {
					oid: p.hex(),
					size: p.size() as u64,
				})
				.collect(),
			hash_algo: Some("sha256".to_string()),
		};

		let response = self.client.batch(request).await?;

		self.upload_objects(response, pointers).await
	}

	async fn download_objects(&self, response: BatchResponse, pointers: &[Pointer]) -> Result<(), RemoteError> {
		let object_dir = self.repo.path().join("lfs/objects");

		debug!(response = ?response, "download: got batch response");
		let total_objects = response.objects.len();
		let total_bytes = response.objects.iter().map(|o| o.size).sum::<u64>() as usize;

		let handled_bytes = AtomicUsize::new(0);
		let handled_objects = AtomicUsize::new(0);

		let futures = response.objects.into_iter().map(async |object| {
			let n = handled_objects.fetch_add(1, Ordering::Relaxed) + 1;
			if let Some(error) = object.error {
				return Err(RemoteError::ObjectError(format!("{} - {}", error.code, error.message)));
			}

			let Some(actions) = object.actions else {
				debug!(
					"download ({}/{}): server didn't want us to do anything with '{}' (actions is None); skip",
					n, total_objects, object.oid
				);
				return Ok(());
			};

			let download_action = actions.download.ok_or(RemoteError::EmptyResponse)?;

			if let Some(on_progress) = &self.on_progress {
				let event = ProgressEvent {
					total_objects,
					total_bytes,
					bytes_handled: handled_bytes.fetch_add(object.size as usize, Ordering::Relaxed),
					objects_handled: n - 1,
					next_object_size: object.size as usize,
				};

				on_progress(Progress::Download(event));
			}

			let pointer = pointers.iter().find(|p| p.hex() == object.oid).ok_or(RemoteError::NotFound)?;

			let path = object_dir.join(pointer.path());
			std::fs::create_dir_all(path.parent().unwrap())?;

			let mut attempt = 0;
			let retry_delay = Duration::from_millis(500);

			while attempt < 3 {
				if path.exists() {
					std::fs::remove_file(&path)?;
				}

				let mut buf = BufWriter::new(File::options().create_new(true).write(true).open(&path)?);

				let local_path = path.strip_prefix(&object_dir).unwrap_or(&path);
				info!(url = %download_action.href, size = %pointer.size(), "download ({}/{})", n, total_objects);
				let download_result = self.client.download(&download_action, &mut buf).await;
				drop(buf);

				let download_checksum_result = download_result.and_then(|p| {
					if p.hash() != pointer.hash() {
						error!(path = %local_path.display(), expected = %pointer, got = %p, "download ({}/{}): checksum mismatch", n, total_objects);
						std::fs::remove_file(&path)?;
						Err(RemoteError::ChecksumMismatch)
					} else {
						Ok(p)
					}
				});

				if let Err(e) = download_checksum_result {
					error!(error = %e, "download ({}/{}): failed, retrying", n, total_objects);
					attempt += 1;
					std::fs::remove_file(&path)?;
					std::thread::sleep(retry_delay);
					continue;
				}

				break;
			}

			Ok(())
		});

		let r = futures::stream::iter(futures)
			.buffer_unordered(self.concurrency_limit)
			.collect::<Vec<_>>()
			.await;
		for r in r.iter().filter_map(|r| r.as_ref().err()) {
			error!(error = %r, "download failed");
		}

		if let Some(res) = r.into_iter().find_map(|r| r.err()) {
			return Err(res);
		}

		Ok(())
	}

	async fn upload_objects(&self, response: BatchResponse, pointers: &[Pointer]) -> Result<(), RemoteError> {
		let object_dir = self.repo.path().join("lfs/objects");

		debug!(response = ?response, "upload: got batch response");

		let retry_delay = Duration::from_millis(500);

		let total_objects = response.objects.len();
		let total_bytes = response.objects.iter().map(|o| o.size).sum::<u64>() as usize;
		let handled_bytes = AtomicUsize::new(0);
		let handled_objects = AtomicUsize::new(0);

		let futures = response.objects.into_iter().map(async |object| {
			let n = handled_objects.fetch_add(1, Ordering::Relaxed) + 1;
			let handled_bytes = handled_bytes.fetch_add(object.size as usize, Ordering::Relaxed);

			if let Some(error) = object.error.as_ref() {
				return Err(RemoteError::ObjectError(format!("{} - {}", error.code, error.message)));
			}

			let Some(actions) = object.actions.as_ref() else {
				debug!(
					"upload ({}/{}): server didn't want us to do anything with '{}' (actions is None); skip",
					n, total_objects, object.oid
				);
				return Ok(());
			};

			if let Some(on_progress) = &self.on_progress {
				let event = ProgressEvent {
					total_objects,
					total_bytes,
					bytes_handled: handled_bytes,
					objects_handled: n - 1,
					next_object_size: object.size as usize,
				};

				on_progress(Progress::Upload(event));
			}

			let pointer = pointers.iter().find(|p| p.hex() == object.oid).ok_or(RemoteError::NotFound)?;
			let rel_object_path = pointer.path();

			if let Some(upload_action) = actions.upload.as_ref() {
				let object_path = object_dir.join(&rel_object_path);
				let content = std::fs::read(object_path)?;

				let mut attempt = 0;

				while attempt < 3 {
					debug!(url = %upload_action.href, size = %content.len(), attempt = %attempt, "upload ({}/{})", n, total_objects);
					match self.client.upload(&upload_action, &content).await {
						Ok(()) => break,
						Err(e) => {
							error!( error = %e, "upload ({}/{}): failed, retrying", n, total_objects);
							attempt += 1;
						}
					}
					std::thread::sleep(retry_delay);
				}
			}

			if let Some(verify_action) = actions.verify.as_ref() {
				if let Some(on_progress) = &self.on_progress {
					let event = ProgressEvent {
						total_objects,
						total_bytes,
						bytes_handled: handled_bytes,
						objects_handled: n - 1,
						next_object_size: object.size as usize,
					};

					on_progress(Progress::Verify(event));
				}

				info!(path = %rel_object_path.display(), verify = %verify_action.href, "upload ({}/{}): verifying lfs object", n, total_objects);
				self.client.verify(&verify_action, pointer).await?;
			}

			Ok(())
		});

		let r = futures::stream::iter(futures)
			.buffer_unordered(self.concurrency_limit)
			.collect::<Vec<_>>()
			.await;

		for r in r.iter().filter_map(|r| r.as_ref().err()) {
			error!(error = %r, "upload failed");
		}

		if let Some(res) = r.into_iter().find_map(|r| r.err()) {
			return Err(res);
		}

		Ok(())
	}
}
