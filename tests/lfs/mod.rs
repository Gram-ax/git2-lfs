use std::path::Path;

use assert_matches::assert_matches;
use assertables::assert_ok;
use git2::build::CheckoutBuilder;
use git2::*;
use git2_lfs::Pointer;
use rstest::rstest;
use tempfile::TempDir;

use crate::repo;
use crate::sandbox;

mod blob;
mod pull;
mod push;

#[rstest]
fn lfs_ignore_nonlfs_files(sandbox: TempDir, #[with(&sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let object_dir = repo.path().join("lfs/objects");

	assert!(!object_dir.exists());

	std::fs::write(sandbox.path().join("hello.txt"), "hello").unwrap();

	let mut index = repo.index().unwrap();
	index.add_all(["*"], IndexAddOption::default(), None).unwrap();
	index.write().unwrap();

	let tree_id = index.write_tree().unwrap();
	let tree = repo.find_tree(tree_id).unwrap();

	let blob_oid = tree.get_path(Path::new("hello.txt")).unwrap().id();
	let blob = repo.find_blob(blob_oid).unwrap();
	assert_eq!(String::from_utf8(blob.content().to_vec()).unwrap(), "hello");

	assert!(!object_dir.exists());

	Ok(())
}

#[rstest]
fn lfs_clean_add_naive(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let text = "hello";
	let text_path = Path::new("text.txt");

	let bin = b"blob";
	let bin_path = Path::new("blob.bin");

	let workdir = repo.workdir().unwrap();

	let bin_expected_pointer = Pointer::from_blob_bytes(bin)?;

	assert_ok!(std::fs::write(workdir.join(text_path), text));
	assert_ok!(std::fs::write(workdir.join(bin_path), bin));

	assert_matches!(assert_ok!(repo.status_file(text_path)), Status::WT_NEW);
	assert_matches!(assert_ok!(repo.status_file(bin_path)), Status::WT_NEW);

	let mut index = repo.index().unwrap();

	index.add_all(["*"], IndexAddOption::default(), None).unwrap();

	index.write().unwrap();
	let tree = index.write_tree().unwrap();
	let tree = repo.find_tree(tree).unwrap();

	let text_oid = tree.get_path(text_path).unwrap().id();
	let bin_oid = tree.get_path(bin_path).unwrap().id();

	let text_content = repo.find_blob(text_oid).unwrap();
	let text_content = text_content.content();
	let bin_content = repo.find_blob(bin_oid).unwrap();
	let bin_content = bin_content.content();

	assert_eq!(assert_ok!(std::str::from_utf8(text_content)), text);

	let pointer_bytes = bin_expected_pointer.as_bytes()?;

	assert_eq!(
		assert_ok!(std::str::from_utf8(bin_content)),
		assert_ok!(std::str::from_utf8(&pointer_bytes))
	);

	let hex = bin_expected_pointer.hex();
	let object_path = repo.path().join("lfs/objects/").join(&hex[..2]).join(&hex[2..4]).join(&hex);

	assert!(object_path.exists());

	let object_content = std::fs::read(object_path)?;
	assert_eq!(object_content, bin);

	Ok(())
}

#[rstest]
fn lfs_smudge_checkout(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let text = "hello";
	let text_path = Path::new("text.txt");

	let bin = b"blob";
	let bin_path = Path::new("blob.bin");

	let workdir = repo.workdir().unwrap();

	assert_ok!(std::fs::write(workdir.join(text_path), text));
	assert_ok!(std::fs::write(workdir.join(bin_path), bin));

	let mut index = repo.index().unwrap();

	index.add_all(["*"], IndexAddOption::default(), None).unwrap();
	index.write().unwrap();

	let tree_id = index.write_tree().unwrap();
	let tree = repo.find_tree(tree_id).unwrap();

	let bin_oid = tree.get_path(bin_path).unwrap().id();
	let bin_blob = repo.find_blob(bin_oid).unwrap();
	assert!(Pointer::is_pointer(bin_blob.content()));

	let signature = git2::Signature::now("Tester", "tester@example.com").unwrap();
	let commit_id = repo.commit(Some("HEAD"), &signature, &signature, "initial commit", &tree, &[]).unwrap();
	repo.find_commit(commit_id).unwrap();

	let bin_fs_path = workdir.join(bin_path);
	std::fs::remove_file(&bin_fs_path).unwrap();
	assert!(!bin_fs_path.exists());

	let mut checkout = CheckoutBuilder::new();
	checkout.force();
	repo.checkout_head(Some(&mut checkout)).unwrap();

	let restored = std::fs::read(&bin_fs_path)?;
	assert_eq!(restored, bin);

	let text_fs_path = workdir.join(text_path);
	let text_content = std::fs::read_to_string(text_fs_path)?;
	assert_eq!(text_content, text);

	Ok(())
}

#[rstest]
fn lfs_checkout_same_file_twice(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let bin_path = Path::new("data.bin");
	let workdir = repo.workdir().unwrap();

	let bin_v1 = b"version 1 content";
	assert_ok!(std::fs::write(workdir.join(bin_path), bin_v1));

	let mut index = repo.index().unwrap();
	index.add_all(["*"], IndexAddOption::default(), None).unwrap();
	index.write().unwrap();

	let tree_id = index.write_tree().unwrap();
	let tree = repo.find_tree(tree_id).unwrap();

	let signature = git2::Signature::now("Tester", "tester@example.com").unwrap();
	let commit_id = repo.commit(Some("HEAD"), &signature, &signature, "add data.bin v1", &tree, &[]).unwrap();

	let branch_name = "feature-branch";
	repo.branch(branch_name, &repo.find_commit(commit_id).unwrap(), false).unwrap();

	repo.set_head(&format!("refs/heads/{}", branch_name))?;
	repo.checkout_head(Some(CheckoutBuilder::new().force()))?;

	let bin_v2 = b"version 2 content";
	assert_ok!(std::fs::write(workdir.join(bin_path), bin_v2));

	index.add_all(["*"], IndexAddOption::default(), None).unwrap();
	index.write().unwrap();

	let tree_id_v2 = index.write_tree().unwrap();
	let tree_v2 = repo.find_tree(tree_id_v2).unwrap();

	repo
		.commit(
			Some("HEAD"),
			&signature,
			&signature,
			"update data.bin v2",
			&tree_v2,
			&[&repo.find_commit(commit_id).unwrap()],
		)
		.unwrap();

	let content_v2 = std::fs::read(workdir.join(bin_path))?;
	assert_eq!(content_v2, bin_v2);

	repo.set_head("refs/heads/master")?;
	repo.checkout_head(Some(CheckoutBuilder::new().force()))?;

	let content_v1 = std::fs::read(workdir.join(bin_path))?;
	assert_eq!(content_v1, bin_v1);

	repo.set_head(&format!("refs/heads/{}", branch_name))?;
	repo.checkout_head(Some(CheckoutBuilder::new().force()))?;

	let content_v2_again = std::fs::read(workdir.join(bin_path))?;
	assert_eq!(content_v2_again, bin_v2);

	Ok(())
}

#[rstest]
fn lfs_smudge_missing_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let bin = b"missing blob content";
	let bin_path = Path::new("missing.bin");

	let workdir = repo.workdir().unwrap();

	// Create a pointer for the blob
	let pointer = Pointer::from_blob_bytes(bin)?;
	let pointer_bytes = pointer.as_bytes()?;

	// Ensure the LFS object does NOT exist
	let hex = pointer.hex();
	let object_path = repo.path().join("lfs/objects/").join(&hex[..2]).join(&hex[2..4]).join(&hex);
	assert!(!object_path.exists());

	assert_ok!(std::fs::write(workdir.join(bin_path), bin), "failed to write pointer to file");

	let mut index = repo.index().unwrap();
	index.add_all(["*"], IndexAddOption::default(), None).unwrap();
	index.write().unwrap();

	let tree_id = index.write_tree().unwrap();
	let tree = repo.find_tree(tree_id).unwrap();

	let signature = git2::Signature::now("Tester", "tester@example.com").unwrap();
	repo.commit(Some("HEAD"), &signature, &signature, "add missing.bin", &tree, &[]).unwrap();

	std::fs::remove_dir_all(repo.path().join("lfs/objects"))?;

	assert!(!object_path.exists(), "lfs object shouldn't exist at this point");

	std::fs::remove_file(workdir.join(bin_path)).unwrap();
	assert!(!workdir.join(bin_path).exists());

	let mut checkout = CheckoutBuilder::new();
	checkout.force();
	repo.checkout_head(Some(&mut checkout)).unwrap();

	let content = std::fs::read(workdir.join(bin_path))?;
	assert_eq!(String::from_utf8(content)?, String::from_utf8(pointer_bytes)?);

	Ok(())
}

#[rstest]
fn repo_read_attrs(sandbox: TempDir) -> Result<(), anyhow::Error> {
	std::fs::write(sandbox.path().join(".gitattributes"), "*.bin filter=lfs diff=lfs").unwrap();

	std::fs::write(sandbox.path().join("hello.bin"), "hello").unwrap();

	let repo = git2::Repository::init(sandbox.path()).unwrap();
	let attr = repo.get_attr(Path::new("hello.bin"), "filter", AttrCheckFlags::default()).unwrap();

	assert_matches!(attr, Some("lfs"));

	Ok(())
}

#[rstest]
fn lfs_checkout_checkout_pointer_and_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> Result<(), anyhow::Error> {
	let bin = b"blob content for lfs test";
	let bin_path = Path::new("bin.bin");
	let workdir = repo.workdir().unwrap();

	let txt = b"text content for lfs test";
	let txt_path = Path::new("text.txt");

	let pointer = Pointer::from_blob_bytes(bin)?;
	let pointer_bytes = pointer.as_bytes()?;

	let hex = pointer.hex();
	let object_path = repo.path().join("lfs/objects/").join(&hex[..2]).join(&hex[2..4]).join(&hex);

	assert!(!object_path.exists());

	assert_ok!(std::fs::write(workdir.join(bin_path), &bin), "failed to write pointer to file");

	std::fs::write(workdir.join(txt_path), &txt)?;

	let mut index = repo.index().unwrap();
	index.add_all(["*"], IndexAddOption::default(), None).unwrap();
	index.write().unwrap();
	let tree_id = index.write_tree().unwrap();

	let signature = git2::Signature::now("Tester", "tester@example.com").unwrap();
	repo
		.commit(
			Some("HEAD"),
			&signature,
			&signature,
			"add bin.bin with pointer",
			&repo.find_tree(tree_id)?,
			&[],
		)
		.unwrap();

	assert!(object_path.exists());
	assert_eq!(assert_ok!(std::fs::read(workdir.join(bin_path))), bin);

	std::fs::remove_file(&object_path).unwrap();
	std::fs::remove_file(workdir.join(bin_path)).unwrap();

	let mut checkout = CheckoutBuilder::new();
	checkout.force();
	repo.checkout_tree(&repo.head()?.peel(ObjectType::Tree)?, Some(&mut checkout))?;

	assert_eq!(assert_ok!(std::fs::read(workdir.join(bin_path))), pointer_bytes);

	assert_eq!(assert_ok!(std::fs::read(workdir.join(txt_path))), txt);
	std::fs::write(workdir.join(txt_path), "hello")?;

	std::fs::write(&object_path, &bin)?;

	let status = repo.status_file(&bin_path)?;

	assert_matches!(status, Status::CURRENT);

	std::fs::write(workdir.join(bin_path), bin)?;
	assert_eq!(assert_ok!(std::fs::read(workdir.join(bin_path))), bin);

	assert_matches!(status, Status::CURRENT);
	assert_eq!(assert_ok!(std::fs::read_to_string(workdir.join(txt_path))), "hello");

	Ok(())
}
