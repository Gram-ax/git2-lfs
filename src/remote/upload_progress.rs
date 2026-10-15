use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use futures::channel::mpsc::UnboundedSender;

use super::Progress;
use super::ProgressEvent;

/// Where [`LfsRemote::upload`](super::LfsRemote::upload) reports how many bytes of the object it has sent;
/// each attempt gets its own and counts from 0.
///
/// Reports reach `on_progress` only when the upload future yields, so an upload that sends without
/// yielding (a blocking transport) reports with [`UploadProgress::sent_and_yield`].
#[derive(Clone, Debug)]
pub struct UploadProgress {
	object: usize,
	attempt: usize,
	tx: UnboundedSender<UploadMessage>,
}

impl UploadProgress {
	pub(super) fn new(object: usize, attempt: usize, tx: UnboundedSender<UploadMessage>) -> Self {
		Self { object, attempt, tx }
	}

	/// `bytes` sent so far, not since the last report.
	pub fn sent(&self, bytes: usize) {
		// nobody receives once the push is over
		_ = self.tx.unbounded_send(UploadMessage::Sent {
			object: self.object,
			attempt: self.attempt,
			bytes,
		});
	}

	pub async fn sent_and_yield(&self, bytes: usize) {
		self.sent(bytes);
		YieldNow(false).await;
	}
}

/// Returns `Pending` once and wakes itself, which any executor handles.
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

/// `object` is an index among the objects the server asked to upload.
#[derive(Debug)]
pub(super) enum UploadMessage {
	Sent { object: usize, attempt: usize, bytes: usize },
	Uploaded { object: usize },
}

#[derive(Debug)]
struct TrackedObject {
	size: usize,
	/// By the current attempt.
	sent: usize,
	attempt: usize,
	uploaded: bool,
}

/// `bytes_handled` is the sum over objects of what their current attempt has sent.
#[derive(Debug)]
pub(super) struct UploadTracker {
	objects: Vec<TrackedObject>,
	total_bytes: usize,
	bytes_handled: usize,
	objects_handled: usize,
}

impl UploadTracker {
	pub(super) fn new(sizes: impl IntoIterator<Item = usize>) -> Self {
		let objects = sizes
			.into_iter()
			.map(|size| TrackedObject {
				size,
				sent: 0,
				attempt: 0,
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

	/// `None` when `message` changes nothing.
	pub(super) fn handle(&mut self, message: UploadMessage) -> Option<Progress> {
		let o;
		match message {
			UploadMessage::Sent { object, attempt, bytes } => {
				o = &mut self.objects[object];
				// a late report of an earlier attempt, as from a request body hyper still polls
				if o.uploaded || attempt < o.attempt {
					return None;
				}

				if attempt > o.attempt {
					self.bytes_handled -= o.sent;
					o.sent = 0;
					o.attempt = attempt;
				}

				// a transport that counts more than the object must not push the total past `total_bytes`
				let bytes = bytes.min(o.size);
				if bytes <= o.sent {
					return None;
				}

				self.bytes_handled += bytes - o.sent;
				o.sent = bytes;
			}
			UploadMessage::Uploaded { object } => {
				o = &mut self.objects[object];
				self.bytes_handled += o.size - o.sent;
				o.sent = o.size;
				o.uploaded = true;
				self.objects_handled += 1;
			}
		}

		let next_object_size = o.size;
		Some(Progress::Upload(ProgressEvent {
			total_objects: self.objects.len(),
			total_bytes: self.total_bytes,
			bytes_handled: self.bytes_handled,
			objects_handled: self.objects_handled,
			next_object_size,
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sent(attempt: usize, bytes: usize) -> UploadMessage {
		UploadMessage::Sent { object: 0, attempt, bytes }
	}

	fn bytes_handled(event: Option<Progress>) -> usize {
		event.expect("an event").bytes_handled()
	}

	#[test]
	fn reports_that_change_nothing_are_dropped() {
		let mut tracker = UploadTracker::new([100]);

		tracker.handle(sent(0, 50));
		assert!(tracker.handle(sent(0, 50)).is_none(), "the same count again");
		assert!(tracker.handle(sent(0, 20)).is_none(), "a count going back");
		assert_eq!(bytes_handled(tracker.handle(sent(0, 500))), 100, "a count past the size is cut to it");
		assert_eq!(bytes_handled(tracker.handle(sent(1, 10))), 10, "a new attempt counts from 0");
		assert!(tracker.handle(sent(0, 90)).is_none(), "a late report of an earlier attempt");
		tracker.handle(UploadMessage::Uploaded { object: 0 });
		assert!(tracker.handle(sent(1, 100)).is_none(), "after the object is uploaded");
	}
}
