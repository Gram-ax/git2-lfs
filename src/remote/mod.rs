use std::fs::File;
use std::io::BufWriter;

use crate::Pointer;

use async_trait::async_trait;

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

  #[error("not found")]
  NotFound,

  #[error("download failed: {0}")]
  Download(String),

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

#[async_trait]
pub trait Download: Send + Sync {
  async fn batch(&self, req: BatchRequest) -> Result<BatchResponse, RemoteError>;
  async fn download(&self, action: &ObjectAction, to: &mut Write) -> Result<Pointer, RemoteError>;
}

pub struct LfsRemote<'a, C: Send + Sync> {
  repo: &'a git2::Repository,
  client: C,
}

impl<'a, C: Download + Send + Sync> LfsRemote<'a, C> {
  pub fn new(repo: &'a git2::Repository, client: C) -> Self {
    Self { repo, client }
  }

  pub async fn pull(&self, pointers: &[Pointer]) -> Result<(), RemoteError> {
    if pointers.is_empty() {
      return Ok(());
    }

    let request = BatchRequest {
      operation: "download".to_string(),
      transfers: vec!["basic".to_string()],
      objects: pointers.iter().map(|p| BatchObject { oid: p.hex(), size: p.size() as u64 }).collect(),
      hash_algo: Some("sha256".to_string()),
    };

    let response = self.client.batch(request).await?;

    self.download_objects(response, pointers).await
  }

  async fn download_objects(&self, response: BatchResponse, pointers: &[Pointer]) -> Result<(), RemoteError> {
    let object_dir = self.repo.path().join("lfs/objects");

    debug!(response = ?response);

    for object in response.objects {
      let actions = object.actions.ok_or(RemoteError::EmptyResponse)?;
      let download_action = actions.download.ok_or(RemoteError::EmptyResponse)?;

      let pointer = pointers.iter().find(|p| p.hex() == object.oid).ok_or(RemoteError::NotFound)?;

      let path = object_dir.join(pointer.path());
      std::fs::create_dir_all(path.parent().unwrap())?;

      let mut buf = BufWriter::new(File::options().create_new(true).write(true).open(&path)?);

      let local_path = path.strip_prefix(&object_dir).unwrap_or(&path);
      info!(path = %local_path.display(), download = %download_action.href, "downloading lfs object");

      let downloaded_pointer = self.client.download(&download_action, &mut buf).await?;

      if downloaded_pointer.hash() != pointer.hash() {
        error!(path = %local_path.display(), expected = %pointer, got = %downloaded_pointer, "checksum mismatch; removing downloaded object");
        std::fs::remove_file(path)?;
        return Err(RemoteError::ChecksumMismatch);
      }
    }
    Ok(())
  }
}
