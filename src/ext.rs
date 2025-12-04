use std::borrow::Cow;
use std::path::Path;
use std::str::FromStr;

use git2::*;
use tracing::*;
use url::Url;

use crate::Error;
use crate::Pointer;
use crate::pointer::POINTER_ROUGH_LEN;

pub trait RepoLfsExt {
  fn get_lfs_blob_content<'r>(&self, blob: &'r git2::Blob<'_>) -> Result<Cow<'r, [u8]>, Error>;
  fn find_tree_missing_lfs_objects(&self, tree: &git2::Tree<'_>) -> Result<Vec<Pointer>, Error>;
  fn find_objects_to_push(
    &self,
    reference: &git2::Reference,
    upstream: &git2::Reference,
  ) -> Result<Vec<Pointer>, Error>;
}

pub trait RemoteLfsExt {
  fn lfs_url(&self) -> Option<Url>;
}

pub trait BlobLfsExt {
  fn is_lfs_pointer(&self) -> bool;
}

impl RemoteLfsExt for Remote<'_> {
  fn lfs_url(&self) -> Option<Url> {
    let url = self.url()?;
    let url = url.trim_end_matches("/");
    let url =
      if url.ends_with(".git") { format!("{}/info/lfs", url) } else { format!("{}.git/info/lfs", url) };

    Url::parse(&url).ok()
  }
}

impl RepoLfsExt for git2::Repository {
  fn get_lfs_blob_content<'r>(&self, blob: &'r git2::Blob<'_>) -> Result<Cow<'r, [u8]>, Error> {
    let Some(pointer) = Pointer::from_str_short(blob.content()) else {
      return Ok(Cow::Borrowed(blob.content()));
    };

    let path = self.path().join("lfs/objects").join(pointer.path());

    if !path.exists() {
      let err = git2::Error::new(
        ErrorCode::NotFound,
        ErrorClass::Odb,
        format!(
          "object '{}' contains lfs pointer but the target object '{}' wasn't found (tried {})",
          blob.id(),
          pointer.hex(),
          Path::new("lfs/objects").join(pointer.path()).display()
        ),
      );

      return Err(err.into());
    }

    let content = std::fs::read(path)?;
    Ok(Cow::Owned(content))
  }

  fn find_tree_missing_lfs_objects(&self, tree: &git2::Tree<'_>) -> Result<Vec<Pointer>, Error> {
    let mut missing = Vec::new();

    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
      let Some(ObjectType::Blob) = entry.kind() else {
        return TreeWalkResult::Ok;
      };

      let oid = entry.id();
      let Ok(blob) = self.find_blob(oid) else {
        warn!(
          "blob '{}' ({}{}) not found during traversing tree {}",
          oid,
          dir,
          entry.name().unwrap_or_default(),
          tree.id()
        );

        return TreeWalkResult::Ok;
      };

      match Pointer::from_str_short(blob.content()) {
        Some(pointer) if !self.path().join("lfs/objects").join(pointer.path()).exists() => {
          debug!(
            "blob '{}' ({}{}) is lfs pointer but object is missing",
            oid,
            dir,
            entry.name().unwrap_or_default()
          );
          missing.push(pointer)
        }
        _ => (),
      }

      TreeWalkResult::Ok
    })?;

    Ok(missing)
  }

  fn find_objects_to_push(
    &self,
    reference: &git2::Reference,
    upstream: &git2::Reference,
  ) -> Result<Vec<Pointer>, Error> {
    let head_tree = reference.resolve()?.peel_to_tree()?;
    let upstream_tree = upstream.resolve()?.peel_to_tree()?;

    let diff = self.diff_tree_to_tree(Some(&upstream_tree), Some(&head_tree), None)?;

    let mut objects_to_push = Vec::new();

    for delta in diff.deltas().filter(|d| d.new_file().exists()) {
      let maybe_lfs_oid = delta.new_file().id();
      let blob = self.find_blob(delta.new_file().id())?;

      if !POINTER_ROUGH_LEN.contains(&blob.size()) {
        continue;
      }

      let Ok(pointer) = Pointer::from_str(String::from_utf8_lossy(blob.content()).as_ref()) else {
        debug!(oid = %maybe_lfs_oid, "skipping non-lfs pointer file");
        continue;
      };

      objects_to_push.push(pointer);
    }

    Ok(objects_to_push)
  }
}

impl BlobLfsExt for git2::Blob<'_> {
  fn is_lfs_pointer(&self) -> bool {
    Pointer::is_pointer(self.content())
  }
}
