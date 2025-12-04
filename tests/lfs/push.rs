use std::path::Path;

use git2_lfs::ext::RepoLfsExt;
use rstest::rstest;
use tempfile::TempDir;

use crate::repo;
use crate::sandbox;

#[rstest]
fn lfs_find_objects_to_push(
  _sandbox: TempDir,
  #[with(&_sandbox)] repo: git2::Repository,
) -> Result<(), anyhow::Error> {
  let workdir = repo.workdir().unwrap();
  let sig = repo.signature()?;

  let readme = workdir.join("README.md");
  std::fs::write(&readme, "Hello")?;

  let mut index = repo.index()?;
  index.add_path(Path::new("README.md"))?;
  let oid = index.write_tree()?;
  let tree = repo.find_tree(oid)?;
  let parent_id = repo.commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[])?;
  let parent_commit = repo.find_commit(parent_id)?;

  let upstream = repo.reference("refs/remotes/origin/master", parent_id, true, "upstream")?;

  let bin = workdir.join("file.bin");
  let content = vec![0u8; 100];
  std::fs::write(&bin, &content)?;

  index.add_path(Path::new("file.bin"))?;
  let oid = index.write_tree()?;
  let tree = repo.find_tree(oid)?;
  repo.commit(Some("HEAD"), &sig, &sig, "LFS", &tree, &[&parent_commit])?;

  let head = repo.head()?;
  let objects = repo.find_objects_to_push(&head, &upstream)?;

  assert_eq!(objects.len(), 1, "expected 1 object");
  assert_eq!(objects[0].size(), 100, "expected object size 100");

  Ok(())
}

#[rstest]
fn lfs_find_objects_to_push_multiple_commits(
  _sandbox: TempDir,
  #[with(&_sandbox)] repo: git2::Repository,
) -> Result<(), anyhow::Error> {
  let workdir = repo.workdir().unwrap();
  let sig = repo.signature()?;

  let readme = workdir.join("README.md");
  std::fs::write(&readme, "root")?;

  let mut index = repo.index()?;
  index.add_path(Path::new("README.md"))?;
  let oid = index.write_tree()?;
  let tree = repo.find_tree(oid)?;
  let parent_id = repo.commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[])?;
  let parent_commit = repo.find_commit(parent_id)?;

  let upstream = repo.reference("refs/remotes/origin/master", parent_id, true, "upstream")?;

  // Commit 1
  let bin1 = workdir.join("file1.bin");
  std::fs::write(&bin1, vec![1u8; 100])?;
  index.add_path(Path::new("file1.bin"))?;
  let oid = index.write_tree()?;
  let tree = repo.find_tree(oid)?;
  let c1_id = repo.commit(Some("HEAD"), &sig, &sig, "Add file1", &tree, &[&parent_commit])?;
  let c1_commit = repo.find_commit(c1_id)?;

  // Commit 2
  let bin2 = workdir.join("file2.bin");
  std::fs::write(&bin2, vec![2u8; 200])?;
  index.add_path(Path::new("file2.bin"))?;
  let oid = index.write_tree()?;
  let tree = repo.find_tree(oid)?;
  repo.commit(Some("HEAD"), &sig, &sig, "Add file2", &tree, &[&c1_commit])?;

  let head = repo.head()?;
  let objects = repo.find_objects_to_push(&head, &upstream)?;

  assert_eq!(objects.len(), 2, "expected 2 objects");

  let sizes: Vec<usize> = objects.iter().map(|p| p.size()).collect();
  assert!(sizes.contains(&100));
  assert!(sizes.contains(&200));

  Ok(())
}
