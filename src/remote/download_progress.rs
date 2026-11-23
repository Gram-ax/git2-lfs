use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::Pointer;

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSnapshot {
	pub oid: String,
	pub size: usize,
	pub received: usize,
}

pub type OnDownloadProgress<'a> = dyn Fn(DownloadSnapshot) + Sync + 'a;

struct Download {
	repo: PathBuf,
	oid: String,
	size: usize,
	received: AtomicUsize,
}

/// Active object downloads shared by callers that use the same tracker.
#[derive(Default)]
pub struct DownloadTracker {
	active: Mutex<Vec<Arc<Download>>>,
}

impl DownloadTracker {
	pub fn current(&self, repo: &Path) -> Vec<DownloadSnapshot> {
		self
			.active
			.lock()
			.unwrap()
			.iter()
			.filter(|download| download.repo == repo)
			.map(|download| DownloadSnapshot {
				oid: download.oid.clone(),
				size: download.size,
				received: download.received.load(Ordering::Relaxed),
			})
			.collect()
	}
}

/// Reports cumulative bytes for the current attempt. The entry lives until the object is
/// committed, abandoned, or cancelled.
pub struct DownloadProgress<'a> {
	tracker: Option<&'a DownloadTracker>,
	on_progress: Option<&'a OnDownloadProgress<'a>>,
	active: Option<Arc<Download>>,
}

impl<'a> DownloadProgress<'a> {
	pub(super) fn new(tracker: Option<&'a DownloadTracker>, on_progress: Option<&'a OnDownloadProgress<'a>>, repo: &Path, pointer: &Pointer) -> Self {
		let active = (tracker.is_some() || on_progress.is_some()).then(|| {
			Arc::new(Download {
				repo: repo.to_path_buf(),
				oid: pointer.hex(),
				size: pointer.size(),
				received: AtomicUsize::new(0),
			})
		});
		if let Some((tracker, download)) = tracker.zip(active.as_ref()) {
			tracker.active.lock().unwrap().push(download.clone());
		}
		Self {
			tracker,
			on_progress,
			active,
		}
	}

	pub fn received(&self, bytes: usize) {
		if let Some(download) = &self.active {
			let received = bytes.min(download.size);
			let previous = download.received.swap(received, Ordering::Relaxed);
			if let Some(report) = self.on_progress.filter(|_| received != previous) {
				report(DownloadSnapshot {
					oid: download.oid.clone(),
					size: download.size,
					received,
				});
			}
		}
	}
}

impl Drop for DownloadProgress<'_> {
	fn drop(&mut self) {
		if let Some((tracker, download)) = self.tracker.zip(self.active.as_ref()) {
			tracker.active.lock().unwrap().retain(|item| !Arc::ptr_eq(item, download));
		}
	}
}
