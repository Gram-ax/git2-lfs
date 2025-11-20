use std::path::Path;

use assert_matches::assert_matches;
use assertables::assert_ok;
use git2::build::CheckoutBuilder;
use git2::*;
use lfs::Pointer;
use rstest::rstest;
use tempfile::TempDir;

use crate::repo;
use crate::sandbox;

#[rstest]
fn lfs_clean_add_naive(
  _sandbox: TempDir,
  #[with(&_sandbox)] repo: git2::Repository,
) -> Result<(), anyhow::Error> {
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

  let mut pointer_bytes = Vec::new();
  bin_expected_pointer.write_pointer(&mut pointer_bytes)?;

  assert_eq!(assert_ok!(std::str::from_utf8(bin_content)), assert_ok!(std::str::from_utf8(&pointer_bytes)));

  let hex = bin_expected_pointer.hex();
  let object_path = repo.path().join("lfs/objects/").join(&hex[..=2]).join(&hex[2..=4]).join(&hex[5..]);

  assert!(object_path.exists());

  let object_content = std::fs::read(object_path)?;
  assert_eq!(object_content, bin);

  Ok(())
}

#[rstest]
fn lfs_smudge_checkout(
  _sandbox: TempDir,
  #[with(&_sandbox)] repo: git2::Repository,
) -> Result<(), anyhow::Error> {
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
