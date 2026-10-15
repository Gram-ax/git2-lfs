//! How far an upload has got, inside each object as well as object by object.
//!
//! An [`LfsRemote::upload`](super::LfsRemote::upload) reports the bytes it has sent through its
//! [`UploadProgress`]. The reports go through one channel, together with the start and the end of every
//! attempt, to the task that runs the push, where an [`UploadTracker`] keeps the counters and turns each
//! change into a [`Progress`] event.

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use futures::channel::mpsc::UnboundedSender;

use super::Progress;
use super::ProgressEvent;

/// Where [`LfsRemote::upload`](super::LfsRemote::upload) reports how much of the object it has sent.
///
/// A value belongs to one attempt to upload one object; a retry gets a new one and counts from 0 again.
/// Clones report to the same place, so a clone can go into a request body that another task sends.
///
/// The reports become [`Progress::Upload`] events in the task that runs the push, whenever the upload
/// future yields. An upload that sends without yielding, through a blocking transport, reports with
/// [`UploadProgress::sent_and_yield`], or its events come only once it has finished.
#[derive(Clone, Debug)]
pub struct UploadProgress {
	object: usize,
	attempt: usize,
	tx: Option<UnboundedSender<UploadMessage>>,
}

impl UploadProgress {
	/// Reports nowhere; for calling [`LfsRemote::upload`](super::LfsRemote::upload) outside of a push.
	pub fn none() -> Self {
		Self {
			object: 0,
			attempt: 0,
			tx: None,
		}
	}

	pub(super) fn new(object: usize, attempt: usize, tx: UnboundedSender<UploadMessage>) -> Self {
		Self {
			object,
			attempt,
			tx: Some(tx),
		}
	}

	/// `bytes` of the object are sent so far in this attempt.
	pub fn sent(&self, bytes: usize) {
		if let Some(tx) = &self.tx {
			// the push is over once nobody receives: a late report has nowhere to go
			_ = tx.unbounded_send(UploadMessage::Sent {
				object: self.object,
				attempt: self.attempt,
				bytes,
			});
		}
	}

	/// [`sent`](Self::sent), then yields once, so the push passes the report on before the upload goes on.
	pub async fn sent_and_yield(&self, bytes: usize) {
		self.sent(bytes);
		YieldNow(false).await;
	}
}

/// Returns `Pending` once and wakes itself, which any executor handles: no runtime is needed.
struct YieldNow(bool);

impl Future for YieldNow {
	type Output = ();

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
		if self.0 {
			return Poll::Ready(());
		}

		self.0 = true;
		cx.waker().wake_by_ref();
		Poll::Pending
	}
}

/// `object` is the index of an object among those the server asked to upload.
#[derive(Debug)]
pub(super) enum UploadMessage {
	Started {
		object: usize,
		attempt: usize,
	},
	Sent {
		object: usize,
		attempt: usize,
		bytes: usize,
	},
	Uploaded {
		object: usize,
	},
	/// An object of `size` bytes is being verified; it need not be one that was uploaded.
	Verify {
		size: usize,
	},
}

#[derive(Debug)]
struct TrackedObject {
	size: usize,
	/// Of the current attempt.
	sent: usize,
	attempt: Option<usize>,
	uploaded: bool,
}

/// Counts what has been uploaded: `bytes_handled` is the sum of what each object has sent in its current
/// attempt, so a retry takes the bytes of the failed attempt back.
#[derive(Debug)]
pub(super) struct UploadTracker {
	objects: Vec<TrackedObject>,
	total_bytes: usize,
	bytes_handled: usize,
	objects_handled: usize,
}

impl UploadTracker {
	/// `sizes` of the objects the server asked to upload.
	pub(super) fn new(sizes: impl IntoIterator<Item = usize>) -> Self {
		let objects = sizes
			.into_iter()
			.map(|size| TrackedObject {
				size,
				sent: 0,
				attempt: None,
				uploaded: false,
			})
			.collect::<Vec<_>>();

		Self {
			total_bytes: objects.iter().map(|o| o.size).sum(),
			objects,
			bytes_handled: 0,
			objects_handled: 0,
		}
	}

	/// The event for `message`, or `None` when it changes nothing: a report that doesn't move its attempt
	/// forward, a late one of an earlier attempt or of an object already uploaded.
	pub(super) fn handle(&mut self, message: UploadMessage) -> Option<Progress> {
		match message {
			UploadMessage::Started { object, attempt } => {
				let o = &mut self.objects[object];
				self.bytes_handled -= o.sent;
				o.sent = 0;
				o.attempt = Some(attempt);
				let size = o.size;
				Some(Progress::Upload(self.event(size)))
			}
			UploadMessage::Sent { object, attempt, bytes } => {
				let o = &mut self.objects[object];
				// a transport that counts more than the object must not push the total past `total_bytes`
				let bytes = bytes.min(o.size);
				if o.uploaded || o.attempt != Some(attempt) || bytes <= o.sent {
					return None;
				}

				self.bytes_handled += bytes - o.sent;
				o.sent = bytes;
				let size = o.size;
				Some(Progress::Upload(self.event(size)))
			}
			UploadMessage::Uploaded { object } => {
				let o = &mut self.objects[object];
				if o.uploaded {
					return None;
				}

				self.bytes_handled += o.size - o.sent;
				o.sent = o.size;
				o.uploaded = true;
				self.objects_handled += 1;
				let size = o.size;
				Some(Progress::Upload(self.event(size)))
			}
			UploadMessage::Verify { size } => Some(Progress::Verify(self.event(size))),
		}
	}

	/// The last event, once every object is uploaded; `None` while one isn't, or when there was nothing to upload.
	pub(super) fn finish(&self) -> Option<Progress> {
		let done = !self.objects.is_empty() && self.objects_handled == self.objects.len();
		done.then(|| Progress::Upload(self.event(0)))
	}

	fn event(&self, next_object_size: usize) -> ProgressEvent {
		ProgressEvent {
			total_objects: self.objects.len(),
			total_bytes: self.total_bytes,
			bytes_handled: self.bytes_handled,
			objects_handled: self.objects_handled,
			next_object_size,
		}
	}
}

#[cfg(test)]
mod tests {
	use assert_matches::assert_matches;

	use super::*;

	fn upload(event: Option<Progress>) -> (usize, usize) {
		match event {
			Some(Progress::Upload(e)) => (e.bytes_handled, e.objects_handled),
			other => panic!("expected an upload event, got {other:?}"),
		}
	}

	fn started(object: usize, attempt: usize) -> UploadMessage {
		UploadMessage::Started { object, attempt }
	}

	fn sent(object: usize, attempt: usize, bytes: usize) -> UploadMessage {
		UploadMessage::Sent { object, attempt, bytes }
	}

	#[test]
	fn bytes_add_up_over_objects_in_flight() {
		let mut tracker = UploadTracker::new([100, 50]);

		assert_eq!(upload(tracker.handle(started(0, 0))), (0, 0));
		assert_eq!(upload(tracker.handle(started(1, 0))), (0, 0));
		assert_eq!(upload(tracker.handle(sent(0, 0, 40))), (40, 0));
		assert_eq!(upload(tracker.handle(sent(1, 0, 10))), (50, 0));
		assert_eq!(upload(tracker.handle(sent(0, 0, 60))), (70, 0));
		assert_eq!(upload(tracker.handle(UploadMessage::Uploaded { object: 1 })), (110, 1));
		assert!(tracker.finish().is_none(), "object 0 is not uploaded yet");
		assert_eq!(upload(tracker.handle(UploadMessage::Uploaded { object: 0 })), (150, 2));

		let last = tracker.finish();
		assert_eq!(upload(last.clone()), (150, 2));
		assert_eq!(last.unwrap().total_bytes(), 150);
	}

	#[test]
	fn retry_replaces_bytes_of_failed_attempt() {
		let mut tracker = UploadTracker::new([100]);

		tracker.handle(started(0, 0));
		assert_eq!(upload(tracker.handle(sent(0, 0, 70))), (70, 0));
		assert_eq!(upload(tracker.handle(started(0, 1))), (0, 0));
		assert!(tracker.handle(sent(0, 0, 80)).is_none(), "a late report of the failed attempt");
		assert_eq!(upload(tracker.handle(sent(0, 1, 30))), (30, 0));
	}

	#[test]
	fn reports_that_change_nothing_are_dropped() {
		let mut tracker = UploadTracker::new([100]);

		assert!(tracker.handle(sent(0, 0, 10)).is_none(), "before the attempt has started");
		tracker.handle(started(0, 0));
		tracker.handle(sent(0, 0, 50));
		assert!(tracker.handle(sent(0, 0, 50)).is_none(), "the same count again");
		assert!(tracker.handle(sent(0, 0, 20)).is_none(), "a count going back");
		assert_eq!(upload(tracker.handle(sent(0, 0, 500))), (100, 0), "a count past the size is cut to it");
		assert_eq!(upload(tracker.handle(UploadMessage::Uploaded { object: 0 })), (100, 1));
		assert!(tracker.handle(sent(0, 0, 100)).is_none(), "after the object is uploaded");
		assert!(tracker.handle(UploadMessage::Uploaded { object: 0 }).is_none());
	}

	#[test]
	fn verify_carries_the_counters() {
		let mut tracker = UploadTracker::new([100]);

		tracker.handle(started(0, 0));
		tracker.handle(UploadMessage::Uploaded { object: 0 });

		assert_matches!(
			tracker.handle(UploadMessage::Verify { size: 100 }),
			Some(Progress::Verify(ProgressEvent {
				total_objects: 1,
				total_bytes: 100,
				bytes_handled: 100,
				objects_handled: 1,
				next_object_size: 100,
			}))
		);
	}

	#[test]
	fn nothing_to_upload_has_no_last_event() {
		assert!(UploadTracker::new([]).finish().is_none());
	}

	#[test]
	fn upload_progress_can_go_into_another_task() {
		fn assert_shareable<T: Clone + Send + Sync + 'static>() {}
		assert_shareable::<UploadProgress>();
	}
}
