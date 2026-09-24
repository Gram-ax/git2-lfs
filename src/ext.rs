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
	fn find_lfs_objects_to_push(
		&self,
		local_branch: &git2::Reference,
		upstream_branch: Option<&git2::Reference>,
		limit: usize,
	) -> Result<Vec<Pointer>, Error>;
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
		match self.workdir() {
			Some(workdir) => {
				let abs_path = workdir.join(rel_path);
				let size = abs_path.metadata()?.len() as usize;

				if !crate::pointer::POINTER_ROUGH_LEN.contains(&size) {
					return Ok(None);
				}

				let content = std::fs::read(abs_path)?;
				Ok(Pointer::from_str_short(&content))
			}
			None => {
				let head = self.head()?.peel_to_tree()?;
				let entry = head.get_path(rel_path)?;
				let pointer = self
					.find_blob(entry.id())
					.ok()
					.filter(|b| crate::pointer::POINTER_ROUGH_LEN.contains(&b.size()))
					.and_then(|b| Pointer::from_str_short(b.content()));
				Ok(pointer)
			}
		}
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

	fn find_lfs_objects_to_push(
		&self,
		local_branch: &git2::Reference,
		upstream_branch: Option<&git2::Reference>,
		limit: usize,
	) -> Result<Vec<Pointer>, Error> {
		let mut scan = PushScan::default();

		let odb = self.odb()?;
		let mut revwalk = self.revwalk()?;

		revwalk.push(local_branch.peel_to_commit()?.id())?;

		if let Some(upstream_branch) = upstream_branch {
			revwalk.hide(upstream_branch.peel_to_commit()?.id())?;
		}

		let mut commits = 0usize;

		for commit in revwalk.take(limit) {
			let commit = self.find_commit(commit?)?;
			let tree = commit.tree()?;
			commits += 1;

			// only the changed paths matter: a blob that survived untouched from an earlier
			// commit has already been inspected there (or lives in the hidden upstream part).
			let Ok(parent) = commit.parent(0) else {
				// root commit: there is nothing to diff against, walk the whole tree
				tree.walk(git2::TreeWalkMode::PostOrder, |_, entry| {
					let Some(ObjectType::Blob) = entry.kind() else {
						return TreeWalkResult::Ok;
					};

					inspect_blob(self, &odb, entry.id(), commit.id(), &mut scan);
					TreeWalkResult::Ok
				})?;

				continue;
			};

			let parent_tree = parent.tree()?;
			let diff = self.diff_tree_to_tree(Some(&parent_tree), Some(&tree), None)?;

			for delta in diff.deltas() {
				let oid = delta.new_file().id();

				if oid.is_zero() {
					continue;
				}

				inspect_blob(self, &odb, oid, commit.id(), &mut scan);
			}
		}

		info!(
			commits = commits,
			blobs_inspected = scan.blobs_inspected,
			headers_read = scan.headers_read,
			pointers = scan.objects_to_push.len(),
			"lfs: scanned commits for objects to push"
		);

		Ok(scan.objects_to_push.into_iter().collect())
	}
}

#[derive(Default)]
struct PushScan {
	seen: HashSet<Oid>,
	objects_to_push: HashSet<Pointer>,
	blobs_inspected: usize,
	headers_read: usize,
}

/// Checks whether `oid` is an lfs pointer and, if so, records it in `scan`.
///
/// The object header (type and size) is cheap to read compared to the object itself, so the
/// full blob is only loaded for oids that are of the right type and of a plausible size.
fn inspect_blob(repo: &git2::Repository, odb: &Odb<'_>, oid: Oid, commit: Oid, scan: &mut PushScan) {
	scan.blobs_inspected += 1;

	if !scan.seen.insert(oid) {
		return;
	}

	scan.headers_read += 1;

	let Ok((size, kind)) = odb.read_header(oid) else {
		return;
	};

	if kind != ObjectType::Blob || !POINTER_ROUGH_LEN.contains(&size) {
		return;
	}

	let Ok(blob) = repo.find_blob(oid) else {
		return;
	};

	let Ok(pointer) = Pointer::from_str(String::from_utf8_lossy(blob.content()).as_ref()) else {
		debug!(oid = %oid, "skipping non-lfs pointer file");
		return;
	};

	debug!(blob = %oid, commit = %commit, "found lfs-pointer!");
	scan.objects_to_push.insert(pointer);
}
