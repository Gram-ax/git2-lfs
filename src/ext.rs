use std::borrow::Cow;
use std::collections::HashSet;
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
	fn try_get_dangling_pointer(&self, rel_path: &Path) -> Result<Option<Pointer>, Error>;
	fn find_lfs_objects_to_push(&self, local_branch: &git2::Reference, upstream_branch: Option<&git2::Reference>) -> Result<Vec<Pointer>, Error>;
}

pub trait RemoteLfsExt {
	fn lfs_url(&self) -> Option<Url>;
}

impl RemoteLfsExt for Remote<'_> {
	fn lfs_url(&self) -> Option<Url> {
		let url = self.url()?;
		let url = url.trim_end_matches("/");
		let url = if url.ends_with(".git") {
			format!("{}/info/lfs", url)
		} else {
			format!("{}.git/info/lfs", url)
		};

		Url::parse(&url).ok()
	}
}

impl RepoLfsExt for git2::Repository {
	fn try_get_dangling_pointer(&self, rel_path: &Path) -> Result<Option<Pointer>, Error> {
		let Some(workdir) = self.workdir() else {
			return Ok(None);
		};
		let abs_path = workdir.join(rel_path);

		let size = abs_path.metadata()?.len() as usize;

		if !crate::pointer::POINTER_ROUGH_LEN.contains(&size) {
			return Ok(None);
		}

		let content = std::fs::read(abs_path)?;
		Ok(Pointer::from_str_short(&content))
	}

	fn get_lfs_blob_content<'r>(&self, blob: &'r git2::Blob<'_>) -> Result<Cow<'r, [u8]>, Error> {
		let Some(pointer) = Pointer::from_str_short(blob.content()) else {
			return Ok(Cow::Borrowed(blob.content()));
		};

		let path = self.path().join("lfs/objects").join(pointer.path());

		if !path.exists() {
			warn!(pointer = %pointer, "lfs object not found; returning the original content");
			return Ok(Cow::Borrowed(blob.content()));
		}

		let content = std::fs::read(path)?;
		Ok(Cow::Owned(content))
	}

	fn find_tree_missing_lfs_objects(&self, tree: &git2::Tree<'_>) -> Result<Vec<Pointer>, Error> {
		let mut missing = HashSet::<Pointer>::new();

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

					missing.insert(pointer);
				}
				_ => (),
			}

			TreeWalkResult::Ok
		})?;

		Ok(missing.into_iter().collect())
	}

	fn find_lfs_objects_to_push(&self, local_branch: &git2::Reference, upstream_branch: Option<&git2::Reference>) -> Result<Vec<Pointer>, Error> {
		let mut objects_to_push = HashSet::new();

		let mut revwalk = self.revwalk()?;

		revwalk.push(local_branch.peel_to_commit()?.id())?;

		if let Some(upstream_branch) = upstream_branch {
			revwalk.hide(upstream_branch.peel_to_commit()?.id())?;
		}

		for commit in revwalk {
			let commit = self.find_commit(commit?)?;
			let tree = commit.tree()?;

			tree.walk(git2::TreeWalkMode::PostOrder, |_, entry| {
				let Some(ObjectType::Blob) = entry.kind() else {
					return TreeWalkResult::Ok;
				};

				let oid = entry.id();

				let Ok(blob) = self.find_blob(oid) else {
					return TreeWalkResult::Ok;
				};

				if !POINTER_ROUGH_LEN.contains(&blob.size()) {
					return TreeWalkResult::Ok;
				}

				let Ok(pointer) = Pointer::from_str(String::from_utf8_lossy(blob.content()).as_ref()) else {
					debug!(oid = %oid, "skipping non-lfs pointer file");
					return TreeWalkResult::Ok;
				};

				debug!(blob = %oid, commit = %commit.id(), "found lfs-pointer!");
				objects_to_push.insert(pointer);
				TreeWalkResult::Ok
			})?;
		}

		Ok(objects_to_push.into_iter().collect())
	}
}
