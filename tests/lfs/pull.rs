use std::sync::OnceLock;

use assertables::assert_some;
use git2_lfs::ext::RemoteLfsExt;
use git2_lfs::ext::RepoLfsExt;
use git2_lfs::remote::LfsRemote;
use git2_lfs::remote::reqwest::ReqwestLfsClient;
use rstest::rstest;
use tempfile::TempDir;

use crate::sandbox;

const TEST_REPO_URL: &str = "https://github.com/pashokitsme/test-lfs";

fn init_test_repo(to: &TempDir) -> git2::Repository {
  static TEST_REPO: OnceLock<TempDir> = OnceLock::new();

  let from = TEST_REPO.get_or_init(|| {
    let tempdir = TempDir::new().unwrap();
    git2::Repository::clone(TEST_REPO_URL, tempdir.path().join("repo")).unwrap();
    tempdir
  });

  copy_dir::copy_dir(from.path().join("repo"), to.path().join("repo")).unwrap();
  git2::Repository::open(to.path().join("repo")).unwrap()
}

#[rstest]
#[tokio::test]
async fn lfs_resolve_missing_objects(sandbox: TempDir) -> Result<(), anyhow::Error> {
  let repo = init_test_repo(&sandbox);

  let tree = repo.head()?.peel_to_tree()?;
  let missing = repo.find_tree_missing_lfs_objects(&tree)?;

  let mut missing = missing.iter();

  assert_eq!(missing.len(), 2, "expected 2 missing objects, got {:?}", missing);

  assert_eq!(
    assert_some!(missing.next()).hex(),
    "f2ca1bb6c7e907d06dafe4687e579fce76b37e4e93b7605022da52e6ccc26fd2"
  );

  assert_eq!(
    assert_some!(missing.next()).hex(),
    "2979f96ff274fb60b4e4fe1544c785a0b46ce8781031906619b26579c20f27e3"
  );

  Ok(())
}

#[rstest]
#[tokio::test]
async fn lfs_pull_missing(sandbox: TempDir) -> Result<(), anyhow::Error> {
  let repo = init_test_repo(&sandbox);

  let lfs_url = repo.find_remote("origin")?.lfs_url();
  let lfs_url = assert_some!(lfs_url);

  let tree = repo.head()?.peel_to_tree()?;
  let missing = repo.find_tree_missing_lfs_objects(&tree)?;

  let client = ReqwestLfsClient::new(lfs_url, None);
  let lfs_remote = LfsRemote::new(&repo, client);

  lfs_remote.pull(&missing).await?;

  let missing = repo.find_tree_missing_lfs_objects(&tree)?;

  assert_eq!(missing.len(), 0, "expected 0 missing objects, got {:?}", missing);

  Ok(())
}
