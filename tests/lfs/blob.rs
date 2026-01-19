use std::borrow::Cow;
use std::path::Path;

use assert_matches::assert_matches;
use git2_lfs::Pointer;
use git2_lfs::ext::RepoLfsExt;
use rstest::rstest;
use tempfile::TempDir;

use crate::repo;
use crate::sandbox;

#[rstest]
fn repo_get_lfs_blob_content_returns_owned_bytes_for_pointer(
	_sandbox: TempDir,
	#[with(&_sandbox)] repo: git2::Repository,
) -> Result<(), anyhow::Error> {
	let path = Path::new("blob.bin");
	let blob_bytes = b"blob content";

	let blob = write_blob(&repo, path, blob_bytes)?;
	assert!(Pointer::is_pointer(blob.content()));

	let resolved = repo.get_lfs_blob_content(&blob)?;

	assert_matches!(resolved, Cow::Owned(_));
	assert_eq!(resolved.as_ref(), blob_bytes);

	Ok(())
}

#[rstest]
fn repo_get_lfs_blob_content_returns_borrowed_bytes_for_regular_blob(
	_sandbox: TempDir,
	#[with(&_sandbox)] repo: git2::Repository,
) -> Result<(), anyhow::Error> {
	let path = Path::new("text.txt");
	let blob_bytes = b"hello world";

	let blob = write_blob(&repo, path, blob_bytes)?;
	assert!(!Pointer::is_pointer(blob.content()));

	let resolved = repo.get_lfs_blob_content(&blob)?;

	assert_matches!(resolved, Cow::Borrowed(_));
	assert_eq!(resolved.as_ref(), blob_bytes);

	Ok(())
}

#[rstest]
fn repo_get_lfs_pointer_if_object_missing(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let path = Path::new("blob.bin");
	let blob_bytes = b"blob content";

	let blob = write_blob(&repo, path, blob_bytes)?;
	let pointer = Pointer::from_blob_bytes(blob_bytes)?;
	let object_path = repo.path().join("lfs/objects").join(pointer.path());

	assert!(object_path.exists());
	std::fs::remove_file(&object_path)?;
	assert!(!object_path.exists());

	let resolved = repo.get_lfs_blob_content(&blob)?;

	assert_matches!(resolved, Cow::Borrowed(_));
	assert_eq!(resolved.as_ref(), pointer.as_bytes()?);

	Ok(())
}

#[rstest]
fn blob_is_lfs_pointer_reports_pointer_state(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let pointer_path = Path::new("blob.bin");
	let pointer_bytes = b"blob";
	let text_path = Path::new("text.txt");
	let text_bytes = b"hello";

	let pointer_blob = write_blob(&repo, pointer_path, pointer_bytes)?;
	let text_blob = write_blob(&repo, text_path, text_bytes)?;

	assert!(Pointer::is_pointer(pointer_blob.content()));
	assert!(!Pointer::is_pointer(text_blob.content()));

	Ok(())
}

fn write_blob<'repo>(repo: &'repo git2::Repository, path: &Path, data: &[u8]) -> Result<git2::Blob<'repo>, anyhow::Error> {
	let workdir = repo.workdir().expect("expected non-bare repository");
	let file_path = workdir.join(path);
	if let Some(parent) = file_path.parent() {
		std::fs::create_dir_all(parent)?;
	}

	std::fs::write(&file_path, data)?;

	let mut index = repo.index()?;
	index.add_all(["*"], git2::IndexAddOption::default(), None)?;
	index.write()?;

	let tree_id = index.write_tree()?;
	let tree = repo.find_tree(tree_id)?;
	let oid = tree.get_path(path)?.id();
	let blob = repo.find_blob(oid)?;

	Ok(blob)
}
