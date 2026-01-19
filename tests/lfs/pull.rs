use std::cell::RefCell;
use std::sync::OnceLock;

use assertables::assert_some;
use git2_lfs::ext::RemoteLfsExt;
use git2_lfs::ext::RepoLfsExt;
use git2_lfs::remote::LfsClient;
use git2_lfs::remote::Progress;
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
	let mut missing = repo.find_tree_missing_lfs_objects(&tree)?;
	missing.sort_by_key(|p| p.hex());

	let mut missing = missing.iter();

	assert_eq!(missing.len(), 2, "expected 2 missing objects, got {:?}", missing);

	assert_eq!(
		assert_some!(missing.next()).hex(),
		"2979f96ff274fb60b4e4fe1544c785a0b46ce8781031906619b26579c20f27e3"
	);

	assert_eq!(
		assert_some!(missing.next()).hex(),
		"f2ca1bb6c7e907d06dafe4687e579fce76b37e4e93b7605022da52e6ccc26fd2"
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

	assert_eq!(missing.len(), 2);

	let client = ReqwestLfsClient::new(lfs_url, None);

	let download_hits = RefCell::new(0);
	let verify_hits = RefCell::new(0);
	let upload_hits = RefCell::new(0);

	let lfs_remote = LfsClient::new(&repo, client).on_progress(Some(Box::new(|progress| match progress {
		Progress::Download(_) => *download_hits.borrow_mut() += 1,
		Progress::Verify(_) => *verify_hits.borrow_mut() += 1,
		Progress::Upload(_) => *upload_hits.borrow_mut() += 1,
	})));

	lfs_remote.pull(&missing).await?;

	let missing = repo.find_tree_missing_lfs_objects(&tree)?;

	assert_eq!(missing.len(), 0, "expected 0 missing objects, got {:?}", missing);

	assert_eq!(*download_hits.borrow(), 2);
	assert_eq!(*verify_hits.borrow(), 0);
	assert_eq!(*upload_hits.borrow(), 0);

	Ok(())
}
