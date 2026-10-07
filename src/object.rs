//! Writing lfs objects so that an object's path only ever holds the complete object.
//!
//! An object is first written to a temporary file in `<git dir>/lfs/tmp`, checked against its pointer
//! (size and sha256 of the bytes that actually reached the file) and only then renamed to
//! `<git dir>/lfs/objects/<aa>/<bb>/<oid>`. A write that fails, is cancelled (the future is dropped) or
//! does not match the pointer removes its temporary file and leaves the object's path untouched.
//!
//! The temporary files live outside `lfs/objects` because everything under `lfs/objects` is taken for
//! an object by whoever lists that directory, and next to it in `.git/lfs` so that the rename stays on
//! one file system.

use std::fs::File;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use sha2::Digest;
use sha2::Sha256;
use tracing::*;

use crate::Pointer;

/// A temporary file nobody has written to for this long is a leftover of a process that was killed
/// in the middle of a download.
const STALE_TMP_AGE: Duration = Duration::from_secs(60 * 60);

const TMP_SUFFIX: &str = ".part";

#[derive(thiserror::Error, Debug)]
pub enum ObjectWriteError {
	#[error("lfs object {pointer}: expected {expected} bytes, got {actual}")]
	SizeMismatch { pointer: Pointer, expected: u64, actual: u64 },

	#[error("lfs object {pointer}: got sha256:{actual}")]
	ChecksumMismatch { pointer: Pointer, actual: String },

	#[error("io: {0}")]
	Io(#[from] std::io::Error),
}

/// `<git dir>/lfs/tmp` for `objects_dir` = `<git dir>/lfs/objects`.
fn tmp_dir(objects_dir: &Path) -> PathBuf {
	objects_dir.parent().unwrap_or(objects_dir).join("tmp")
}

/// Whether `path` holds the complete object of `pointer`: a file of the pointer's size. A shorter file
/// is what an interrupted download left behind before downloads went through a temporary file.
pub(crate) fn is_complete(path: &Path, pointer: &Pointer) -> bool {
	match pointer.size() {
		// a pointer without a size line parses with size 0: nothing to check the size against
		0 => path.is_file(),
		size => path.metadata().is_ok_and(|meta| meta.is_file() && meta.len() == size as u64),
	}
}

fn tmp_file_name(pointer: &Pointer, id: u64) -> String {
	format!("{}-{}-{}{}", pointer.hex(), std::process::id(), id, TMP_SUFFIX)
}

/// The id of the process that created the temporary file `name`, see [`tmp_file_name`].
fn tmp_file_pid(name: &str) -> Option<u32> {
	let mut parts = name.strip_suffix(TMP_SUFFIX)?.rsplit('-');
	parts.next()?;
	parts.next()?.parse().ok()
}

/// Removes the temporary files that a killed process left in `<git dir>/lfs/tmp`. Best effort: a file
/// that cannot be removed now is tried again next time.
pub(crate) fn remove_stale_tmp_files(objects_dir: &Path) {
	let Ok(entries) = std::fs::read_dir(tmp_dir(objects_dir)) else {
		return;
	};

	let now = SystemTime::now();
	let pid = std::process::id();

	for entry in entries.filter_map(|entry| entry.ok()) {
		let name = entry.file_name();
		let name = name.to_string_lossy();

		// this process's own files belong to downloads still running in it, however slow
		if !name.ends_with(TMP_SUFFIX) || tmp_file_pid(&name) == Some(pid) {
			continue;
		}

		// not DirEntry::metadata: on Windows it comes from the directory listing, which may hold the
		// time a file was opened rather than its last write while it is still being written
		let Ok(modified) = std::fs::metadata(entry.path()).and_then(|meta| meta.modified()) else {
			continue;
		};

		if now.duration_since(modified).is_ok_and(|age| age >= STALE_TMP_AGE) {
			match std::fs::remove_file(entry.path()) {
				Ok(()) => debug!(path = %entry.path().display(), "removed stale lfs temporary file"),
				Err(err) => warn!(path = %entry.path().display(), error = %err, "failed to remove stale lfs temporary file"),
			}
		}
	}
}

/// Writes the object of `pointer`: bytes go to a temporary file through [`Write`], and
/// [`ObjectWriter::commit`] moves the file to the object's path once it matches the pointer.
/// Dropping the writer without a successful commit removes the temporary file.
pub(crate) struct ObjectWriter {
	pointer: Pointer,
	object_path: PathBuf,
	tmp_path: PathBuf,
	file: Option<BufWriter<File>>,
	hasher: Sha256,
	written: u64,
	/// The temporary file has become the object, there is nothing to clean up.
	committed: bool,
}

impl ObjectWriter {
	pub fn create(objects_dir: &Path, pointer: &Pointer) -> std::io::Result<Self> {
		static NEXT_ID: AtomicU64 = AtomicU64::new(0);

		let tmp_dir = tmp_dir(objects_dir);
		std::fs::create_dir_all(&tmp_dir)?;

		let (tmp_path, file) = loop {
			let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
			let tmp_path = tmp_dir.join(tmp_file_name(pointer, id));

			match File::options().create_new(true).write(true).open(&tmp_path) {
				Ok(file) => break (tmp_path, file),
				Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
				Err(err) => return Err(err),
			}
		};

		Ok(Self {
			pointer: *pointer,
			object_path: objects_dir.join(pointer.path()),
			tmp_path,
			file: Some(BufWriter::new(file)),
			hasher: Sha256::new(),
			written: 0,
			committed: false,
		})
	}

	/// Checks the written bytes against the pointer and moves them to the object's path, replacing
	/// whatever file was there.
	pub fn commit(mut self) -> Result<(), ObjectWriteError> {
		let file = self.file.take().expect("ObjectWriter::file is only taken by commit");
		let file = file.into_inner().map_err(|err| err.into_error())?;

		let expected = self.pointer.size() as u64;

		// a pointer without a size line parses with size 0: nothing to check the size against
		if expected != 0 && self.written != expected {
			return Err(ObjectWriteError::SizeMismatch {
				pointer: self.pointer,
				expected,
				actual: self.written,
			});
		}

		let hash = std::mem::take(&mut self.hasher).finalize();

		if hash.as_slice() != self.pointer.hash() {
			return Err(ObjectWriteError::ChecksumMismatch {
				pointer: self.pointer,
				actual: hex::encode(hash),
			});
		}

		// best effort: the rename is what keeps a partial object away from the object's path, the sync
		// only makes it survive a power loss, and not every file system the crate runs on supports it
		if let Err(err) = file.sync_all() {
			debug!(path = %self.tmp_path.display(), error = %err, "failed to sync lfs temporary file");
		}
		drop(file);

		std::fs::create_dir_all(self.object_path.parent().expect("object path has a parent"))?;

		if let Err(err) = std::fs::rename(&self.tmp_path, &self.object_path) {
			// Windows refuses to replace a file that is open; a complete object already there will do
			if is_complete(&self.object_path, &self.pointer) {
				warn!(path = %self.object_path.display(), error = %err, "could not replace lfs object, keeping the complete one already there");
				return Ok(());
			}

			return Err(err.into());
		}

		self.committed = true;

		debug!(path = %self.object_path.display(), size = self.written, "lfs object written");
		Ok(())
	}
}

impl Write for ObjectWriter {
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		let file = self.file.as_mut().expect("ObjectWriter::file is only taken by commit");
		let written = file.write(buf)?;

		// only the bytes the file took count, so a writer that drops some is caught by the checks
		self.hasher.update(&buf[..written]);
		self.written += written as u64;

		Ok(written)
	}

	fn flush(&mut self) -> std::io::Result<()> {
		self.file.as_mut().expect("ObjectWriter::file is only taken by commit").flush()
	}
}

impl Drop for ObjectWriter {
	fn drop(&mut self) {
		if self.committed {
			return;
		}

		// close the file first: Windows does not delete an open file
		drop(self.file.take());

		match std::fs::remove_file(&self.tmp_path) {
			Ok(()) => debug!(path = %self.tmp_path.display(), "removed lfs temporary file of an unfinished write"),
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => (),
			Err(err) => warn!(path = %self.tmp_path.display(), error = %err, "failed to remove lfs temporary file"),
		}
	}
}

/// Writes `bytes` as the object of `pointer`, see [`ObjectWriter`].
pub(crate) fn write_object(objects_dir: &Path, pointer: &Pointer, bytes: &[u8]) -> Result<(), ObjectWriteError> {
	let mut writer = ObjectWriter::create(objects_dir, pointer)?;
	writer.write_all(bytes)?;
	writer.commit()
}

#[cfg(test)]
mod tests {
	use std::io::Write;

	use assert_matches::assert_matches;

	use super::*;

	const OBJECT: &[u8] = b"the complete lfs object";

	fn write(objects_dir: &Path, bytes: &[u8]) -> Result<(), ObjectWriteError> {
		let pointer = Pointer::from_blob_bytes(OBJECT).unwrap();
		let mut writer = ObjectWriter::create(objects_dir, &pointer)?;
		writer.write_all(bytes)?;
		writer.commit()
	}

	fn assert_nothing_written(objects_dir: &Path) {
		let pointer = Pointer::from_blob_bytes(OBJECT).unwrap();
		assert!(!objects_dir.join(pointer.path()).exists());
		assert_eq!(std::fs::read_dir(tmp_dir(objects_dir)).unwrap().count(), 0);
	}

	#[test]
	fn commit_rejects_short_object() {
		let dir = tempfile::tempdir().unwrap();
		let objects_dir = dir.path().join("lfs/objects");

		let result = write(&objects_dir, &OBJECT[..OBJECT.len() - 1]);

		assert_matches!(result, Err(ObjectWriteError::SizeMismatch { expected, actual, .. }) if expected == OBJECT.len() as u64 && actual == OBJECT.len() as u64 - 1);
		assert_nothing_written(&objects_dir);
	}

	#[test]
	fn commit_rejects_other_bytes_of_the_same_size() {
		let dir = tempfile::tempdir().unwrap();
		let objects_dir = dir.path().join("lfs/objects");

		let mut other = OBJECT.to_vec();
		other[0] ^= 0xff;

		assert_matches!(write(&objects_dir, &other), Err(ObjectWriteError::ChecksumMismatch { .. }));
		assert_nothing_written(&objects_dir);
	}

	#[test]
	fn commit_replaces_partial_object() {
		let dir = tempfile::tempdir().unwrap();
		let objects_dir = dir.path().join("lfs/objects");
		let pointer = Pointer::from_blob_bytes(OBJECT).unwrap();
		let path = objects_dir.join(pointer.path());

		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, &OBJECT[..5]).unwrap();
		assert!(!is_complete(&path, &pointer));

		write(&objects_dir, OBJECT).unwrap();

		assert_eq!(std::fs::read(&path).unwrap(), OBJECT);
		assert!(is_complete(&path, &pointer));
		assert_eq!(std::fs::read_dir(tmp_dir(&objects_dir)).unwrap().count(), 0);
	}

	#[test]
	fn tmp_file_pid_reads_the_creator() {
		let pointer = Pointer::from_blob_bytes(OBJECT).unwrap();

		assert_eq!(tmp_file_pid(&tmp_file_name(&pointer, 7)), Some(std::process::id()));
		assert_eq!(tmp_file_pid("not-a-pid-0.part"), None);
		assert_eq!(tmp_file_pid("foreign"), None);
	}
}
