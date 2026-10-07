//! Downloads never leave a partial object at the object's path: the object goes through a temporary
//! file in `.git/lfs/tmp` and is moved into `lfs/objects` only once its size and sha256 match the pointer.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use async_trait::async_trait;
use futures::FutureExt;
use git2_lfs::Pointer;
use git2_lfs::ext::RepoLfsExt;
use git2_lfs::remote::*;
use git2_lfs::sha2::Digest;
use git2_lfs::sha2::Sha256;
use rstest::rstest;
use tempfile::TempDir;

use crate::repo;
use crate::sandbox;

const CHUNK: usize = 1024;

/// What the server does on one download attempt.
#[derive(Clone)]
enum Serve {
	/// Sends the object.
	Object,
	/// Sends this many bytes of the object, then the connection breaks.
	BreakAfter(usize),
	/// Sends this many bytes of the object and ends the response as if it were complete.
	Truncated(usize),
	/// Sends these bytes instead of the object.
	Body(Vec<u8>),
	/// Sends this many bytes of the object, then stalls forever.
	StallAfter(usize),
}

struct MockRemote {
	object: Vec<u8>,
	pointer: Pointer,
	/// One entry per attempt; the last one repeats.
	plan: Mutex<VecDeque<Serve>>,
	downloads: AtomicUsize,
}

impl MockRemote {
	fn new(object: &[u8], plan: &[Serve]) -> Self {
		Self {
			object: object.to_vec(),
			pointer: Pointer::from_blob_bytes(object).unwrap(),
			plan: Mutex::new(plan.iter().cloned().collect()),
			downloads: AtomicUsize::new(0),
		}
	}

	fn next(&self) -> Serve {
		let mut plan = self.plan.lock().unwrap();
		if plan.len() > 1 {
			plan.pop_front().unwrap()
		} else {
			plan.front().unwrap().clone()
		}
	}
}

fn send(bytes: &[u8], to: &mut Write) -> Result<Pointer, RemoteError> {
	for chunk in bytes.chunks(CHUNK) {
		to.write_all(chunk)?;
	}

	let hash = Sha256::digest(bytes);
	Ok(Pointer::from_parts(hash.as_slice(), bytes.len()))
}

#[async_trait]
impl LfsRemote for MockRemote {
	async fn batch(&self, _req: BatchRequest) -> Result<BatchResponse, RemoteError> {
		Ok(BatchResponse {
			transfer: None,
			hash_algo: None,
			objects: vec![BatchResponseObject {
				oid: self.pointer.hex(),
				size: self.pointer.size() as u64,
				authenticated: None,
				error: None,
				actions: Some(ObjectActions {
					download: Some(ObjectAction {
						href: "https://lfs.invalid/object".to_string(),
						header: HashMap::new(),
						expires_in: None,
						expires_at: None,
					}),
					upload: None,
					verify: None,
				}),
			}],
		})
	}

	async fn download(&self, _action: &ObjectAction, to: &mut Write) -> Result<Pointer, RemoteError> {
		self.downloads.fetch_add(1, Ordering::SeqCst);

		match self.next() {
			Serve::Object => send(&self.object, to),
			Serve::Truncated(n) => send(&self.object[..n], to),
			Serve::Body(body) => send(&body, to),
			Serve::BreakAfter(n) => {
				send(&self.object[..n], to)?;
				Err(RemoteError::Download("connection reset".to_string()))
			}
			Serve::StallAfter(n) => {
				send(&self.object[..n], to)?;
				futures::future::pending().await
			}
		}
	}

	async fn upload(&self, _action: &ObjectAction, _blob: &[u8]) -> Result<(), RemoteError> {
		unreachable!("the tests only download")
	}

	async fn verify(&self, _action: &ObjectAction, _pointer: &Pointer) -> Result<(), RemoteError> {
		unreachable!("the tests only download")
	}
}

fn object_bytes() -> Vec<u8> {
	(0..CHUNK * 64 + 17).map(|i| (i * 31 % 251) as u8).collect()
}

fn object_path(repo: &git2::Repository, pointer: &Pointer) -> PathBuf {
	repo.path().join("lfs/objects").join(pointer.path())
}

fn tmp_files(repo: &git2::Repository) -> Vec<PathBuf> {
	match std::fs::read_dir(repo.path().join("lfs/tmp")) {
		Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
		Err(_) => vec![],
	}
}

fn assert_no_object(repo: &git2::Repository, pointer: &Pointer) {
	let path = object_path(repo, pointer);
	assert!(!path.exists(), "{} must not exist", path.display());
	assert!(!pointer.is_object_present(&repo.path().join("lfs/objects")));
	assert_eq!(tmp_files(repo), Vec::<PathBuf>::new(), "temporary files must be removed");
}

async fn pull(repo: &git2::Repository, remote: &MockRemote) -> Result<(), RemoteError> {
	LfsClient::new(repo, remote).pull(&[remote.pointer]).await
}

#[async_trait]
impl LfsRemote for &MockRemote {
	async fn batch(&self, req: BatchRequest) -> Result<BatchResponse, RemoteError> {
		(*self).batch(req).await
	}

	async fn download(&self, action: &ObjectAction, to: &mut Write) -> Result<Pointer, RemoteError> {
		(*self).download(action, to).await
	}

	async fn upload(&self, action: &ObjectAction, blob: &[u8]) -> Result<(), RemoteError> {
		(*self).upload(action, blob).await
	}

	async fn verify(&self, action: &ObjectAction, pointer: &Pointer) -> Result<(), RemoteError> {
		(*self).verify(action, pointer).await
	}
}

#[rstest]
#[tokio::test]
async fn download_writes_complete_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let remote = MockRemote::new(&object, &[Serve::Object]);

	pull(&repo, &remote).await?;

	assert_eq!(std::fs::read(object_path(&repo, &remote.pointer))?, object);
	assert!(remote.pointer.is_object_present(&repo.path().join("lfs/objects")));
	assert_eq!(tmp_files(&repo), Vec::<PathBuf>::new());
	assert_eq!(remote.downloads.load(Ordering::SeqCst), 1);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn download_broken_midway_leaves_no_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let remote = MockRemote::new(&object, &[Serve::BreakAfter(object.len() / 3)]);

	// an object that keeps failing is left missing rather than failing the whole pull
	pull(&repo, &remote).await?;

	assert_eq!(remote.downloads.load(Ordering::SeqCst), 3, "a failed download is retried");
	assert_no_object(&repo, &remote.pointer);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn download_with_wrong_sha256_leaves_no_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let mut corrupted = object.clone();
	corrupted[object.len() / 2] ^= 0xff;

	let remote = MockRemote::new(&object, &[Serve::Body(corrupted)]);

	pull(&repo, &remote).await?;

	assert_eq!(remote.downloads.load(Ordering::SeqCst), 3, "every attempt is rejected");
	assert_no_object(&repo, &remote.pointer);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn download_cut_short_by_server_leaves_no_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let remote = MockRemote::new(&object, &[Serve::Truncated(object.len() - 1)]);

	pull(&repo, &remote).await?;

	assert_eq!(remote.downloads.load(Ordering::SeqCst), 3, "every attempt is rejected");
	assert_no_object(&repo, &remote.pointer);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn cancelled_download_leaves_no_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let remote = MockRemote::new(&object, &[Serve::StallAfter(object.len() / 2)]);

	// the download stalls after writing half of the object; dropping the future is how a pull is cancelled
	let pending = pull(&repo, &remote).now_or_never();

	assert!(pending.is_none(), "the download must still be in progress");
	assert_eq!(remote.downloads.load(Ordering::SeqCst), 1);
	assert_no_object(&repo, &remote.pointer);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn download_retried_after_break_writes_complete_object(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let remote = MockRemote::new(&object, &[Serve::BreakAfter(CHUNK * 3), Serve::Object]);

	pull(&repo, &remote).await?;

	assert_eq!(remote.downloads.load(Ordering::SeqCst), 2);
	assert_eq!(std::fs::read(object_path(&repo, &remote.pointer))?, object);
	assert_eq!(tmp_files(&repo), Vec::<PathBuf>::new());

	Ok(())
}

/// A partial object left at the object's path by a download before this fix is not taken for the object:
/// it is reported missing and the next pull replaces it.
#[rstest]
#[tokio::test]
async fn partial_object_from_before_is_missing_and_replaced(sandbox: TempDir, #[with(&sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let remote = MockRemote::new(&object, &[Serve::Object]);
	let path = object_path(&repo, &remote.pointer);

	// commit a pointer to the object
	std::fs::write(sandbox.path().join("big.bin"), &object)?;
	let mut index = repo.index()?;
	index.add_path(Path::new("big.bin"))?;
	index.write()?;
	let tree = repo.find_tree(index.write_tree()?)?;
	assert!(Pointer::is_pointer(repo.find_blob(tree.get_path(Path::new("big.bin"))?.id())?.content()));

	std::fs::write(&path, &object[..object.len() / 4])?;

	assert!(!remote.pointer.is_object_present(&repo.path().join("lfs/objects")));
	assert_eq!(repo.find_tree_missing_lfs_objects(&tree)?, vec![remote.pointer]);

	let blob = repo.find_blob(tree.get_path(Path::new("big.bin"))?.id())?;
	assert_eq!(
		repo.get_lfs_blob_content(&blob)?.as_ref(),
		blob.content(),
		"a partial object is not handed out as the content"
	);

	pull(&repo, &remote).await?;

	assert_eq!(std::fs::read(&path)?, object);
	assert_eq!(repo.find_tree_missing_lfs_objects(&tree)?, vec![]);

	Ok(())
}

/// `git add` of a file whose object is present only partially writes the complete object.
#[rstest]
fn clean_replaces_partial_object(sandbox: TempDir, #[with(&sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let object = object_bytes();
	let pointer = Pointer::from_blob_bytes(&object)?;
	let path = object_path(&repo, &pointer);

	std::fs::create_dir_all(path.parent().unwrap())?;
	std::fs::write(&path, &object[..10])?;

	std::fs::write(sandbox.path().join("big.bin"), &object)?;
	let mut index = repo.index()?;
	index.add_path(Path::new("big.bin"))?;

	assert_eq!(std::fs::read(&path)?, object);
	assert_eq!(tmp_files(&repo), Vec::<PathBuf>::new());

	Ok(())
}

#[rstest]
#[tokio::test]
async fn stale_temporary_files_are_removed(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let tmp_dir = repo.path().join("lfs/tmp");
	std::fs::create_dir_all(&tmp_dir)?;

	let stale = tmp_dir.join("stale-1-0.part");
	let fresh = tmp_dir.join("fresh-1-0.part");
	let foreign = tmp_dir.join("foreign");
	let own = tmp_dir.join(format!("own-{}-0.part", std::process::id()));

	for path in [&stale, &fresh, &foreign, &own] {
		std::fs::write(path, b"partial")?;
	}

	let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
	std::fs::File::options().write(true).open(&stale)?.set_modified(two_hours_ago)?;
	std::fs::File::options().write(true).open(&foreign)?.set_modified(two_hours_ago)?;
	std::fs::File::options().write(true).open(&own)?.set_modified(two_hours_ago)?;

	let object = object_bytes();
	let remote = MockRemote::new(&object, &[Serve::Object]);
	pull(&repo, &remote).await?;

	assert!(!stale.exists(), "a temporary file untouched for an hour is a leftover");
	assert!(fresh.exists(), "a fresh temporary file may belong to a download in progress");
	assert!(foreign.exists(), "only temporary files of this crate are removed");
	assert!(own.exists(), "a temporary file of this process belongs to a download still running in it");

	Ok(())
}
