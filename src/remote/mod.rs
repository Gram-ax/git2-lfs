use std::fs::File;
use std::io::BufWriter;
use std::time::Duration;

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
}

impl<'a, C: LfsRemote + Send + Sync> LfsClient<'a, C> {
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

  pub async fn push(&self, pointers: &[Pointer]) -> Result<(), RemoteError> {
    if pointers.is_empty() {
      return Ok(());
    }

    let request = BatchRequest {
      operation: "upload".to_string(),
      transfers: vec!["basic".to_string()],
      objects: pointers.iter().map(|p| BatchObject { oid: p.hex(), size: p.size() as u64 }).collect(),
      hash_algo: Some("sha256".to_string()),
    };

    let response = self.client.batch(request).await?;

    self.upload_objects(response, pointers).await
  }

  async fn download_objects(&self, response: BatchResponse, pointers: &[Pointer]) -> Result<(), RemoteError> {
    let object_dir = self.repo.path().join("lfs/objects");

    debug!(response = ?response);

    for (index, object) in response.objects.into_iter().enumerate() {
      if let Some(error) = object.error {
        return Err(RemoteError::ObjectError(format!("{} - {}", error.code, error.message)));
      }

      let Some(actions) = object.actions else {
        warn!(index = %index, "pull: server didn't want us to do anything with '{}' (actions is None); skip", object.oid);
        continue;
      };

      let download_action = actions.download.ok_or(RemoteError::EmptyResponse)?;

      let pointer = pointers.iter().find(|p| p.hex() == object.oid).ok_or(RemoteError::NotFound)?;

      let path = object_dir.join(pointer.path());
      std::fs::create_dir_all(path.parent().unwrap())?;

      let mut attempt = 0;
      let delay = Duration::from_millis(500);

      while attempt < 3 {
        if path.exists() {
          std::fs::remove_file(&path)?;
        }
        let mut buf = BufWriter::new(File::options().create_new(true).write(true).open(&path)?);

        let local_path = path.strip_prefix(&object_dir).unwrap_or(&path);
        info!(index = %index, path = %local_path.display(), download = %download_action.href, size = %pointer.size(), "downloading lfs object");
        let download_result = self.client.download(&download_action, &mut buf).await;
        drop(buf);

        match download_result {
          Ok(downloaded_pointer) => {
            if downloaded_pointer.hash() != pointer.hash() {
              error!(path = %local_path.display(), expected = %pointer, got = %downloaded_pointer, "checksum mismatch; removing downloaded object");
              std::fs::remove_file(path)?;
              return Err(RemoteError::ChecksumMismatch);
            }

            break;
          }
          Err(e) => {
            error!(index = %index, attempt = %attempt, error = %e, "download failed; retrying");
            attempt += 1;
            std::fs::remove_file(&path)?;
            std::thread::sleep(delay);
          }
        }
      }
    }
    Ok(())
  }

  async fn upload_objects(&self, response: BatchResponse, pointers: &[Pointer]) -> Result<(), RemoteError> {
    let object_dir = self.repo.path().join("lfs/objects");

    debug!(response = ?response);

    for (index, object) in response.objects.into_iter().enumerate() {
      if let Some(error) = object.error {
        return Err(RemoteError::ObjectError(format!("{} - {}", error.code, error.message)));
      }

      let Some(actions) = object.actions else {
        warn!(index = %index, "push: server didn't want us to do anything with '{}' (actions is None); skip", object.oid);
        continue;
      };

      let pointer = pointers.iter().find(|p| p.hex() == object.oid).ok_or(RemoteError::NotFound)?;
      let rel_object_path = pointer.path();

      if let Some(upload_action) = actions.upload {
        let object_path = object_dir.join(&rel_object_path);
        let content = std::fs::read(object_path)?;

        let mut attempt = 0;
        let delay = Duration::from_millis(500);

        while attempt < 3 {
          info!(index = %index, path = %rel_object_path.display(), upload = %upload_action.href, size = %content.len(), attempt = %attempt, "uploading lfs object");
          match self.client.upload(&upload_action, &content).await {
            Ok(()) => break,
            Err(e) => {
              error!(index = %index, error = %e, "upload failed; retrying");
              attempt += 1;
            }
          }
          std::thread::sleep(delay);
        }
      }

      if let Some(verify_action) = actions.verify {
        info!(index = %index, path = %rel_object_path.display(), verify = %verify_action.href, "verifying lfs object");
        self.client.verify(&verify_action, pointer).await?;
      }
    }

    Ok(())
  }
}
