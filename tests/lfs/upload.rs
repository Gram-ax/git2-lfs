//! Uploads report how far they have got inside each object, not only object by object.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use git2_lfs::Pointer;
use git2_lfs::remote::reqwest::ReqwestLfsClient;
use git2_lfs::remote::*;
use rstest::rstest;
use tempfile::TempDir;
use url::Url;

use crate::repo;
use crate::sandbox;

const CHUNK: usize = 1024;

/// A push that doesn't end by then is taken for hung.
const PUSH_TIMEOUT: Duration = Duration::from_secs(20);

/// What the server asks for an object in its batch response.
#[derive(Clone, Copy)]
enum Ask {
	Upload,
	UploadAndVerify,
	/// No actions: the server has the object.
	Nothing,
	Verify,
}

/// How the remote takes one upload attempt.
#[derive(Clone)]
enum Take {
	/// Reports every chunk and yields after it, as a transport that blocks while it sends has to.
	Object,
	/// Reports chunks up to this many bytes, then the connection breaks.
	BreakAfter(usize),
	/// Reports every chunk from another thread, as a body sent by another task does, and keeps its
	/// `UploadProgress` after the upload is over.
	FromThreadAndKeep,
}

struct MockObject {
	pointer: Pointer,
	ask: Ask,
}

struct MockRemote {
	objects: Vec<MockObject>,
	/// One entry per attempt; the last one repeats.
	plan: Mutex<VecDeque<Take>>,
	/// What `on_progress` got so far.
	events: Arc<Mutex<Vec<Progress>>>,
	/// `bytes_handled` of the last event right after each report of a chunk.
	seen_after_report: Mutex<Vec<usize>>,
	kept: Mutex<Vec<UploadProgress>>,
	uploads: AtomicUsize,
	verifies: AtomicUsize,
}

impl MockRemote {
	fn new(objects: Vec<MockObject>, plan: &[Take]) -> Self {
		Self {
			objects,
			plan: Mutex::new(plan.iter().cloned().collect()),
			events: Arc::new(Mutex::new(vec![])),
			seen_after_report: Mutex::new(vec![]),
			kept: Mutex::new(vec![]),
			uploads: AtomicUsize::new(0),
			verifies: AtomicUsize::new(0),
		}
	}

	fn next(&self) -> Take {
		let mut plan = self.plan.lock().unwrap();
		if plan.len() > 1 {
			plan.pop_front().unwrap()
		} else {
			plan.front().unwrap().clone()
		}
	}

	fn pointers(&self) -> Vec<Pointer> {
		self.objects.iter().map(|o| o.pointer).collect()
	}

	fn events(&self) -> Vec<Progress> {
		self.events.lock().unwrap().clone()
	}

	fn last_bytes_handled(&self) -> usize {
		self.events.lock().unwrap().last().map(|e| e.bytes_handled()).unwrap_or_default()
	}
}

fn action(href: String) -> ObjectAction {
	ObjectAction {
		href,
		header: HashMap::new(),
		expires_in: None,
		expires_at: None,
	}
}

fn chunk_ends(size: usize, chunk: usize) -> Vec<usize> {
	(1..=size.div_ceil(chunk)).map(|i| (i * chunk).min(size)).collect()
}

#[async_trait]
impl LfsRemote for MockRemote {
	async fn batch(&self, _req: BatchRequest) -> Result<BatchResponse, RemoteError> {
		let objects = self
			.objects
			.iter()
			.map(|o| {
				let href = format!("https://lfs.invalid/{}", o.pointer.hex());
				let upload = matches!(o.ask, Ask::Upload | Ask::UploadAndVerify).then(|| action(href.clone()));
				let verify = matches!(o.ask, Ask::Verify | Ask::UploadAndVerify).then(|| action(href.clone()));

				BatchResponseObject {
					oid: o.pointer.hex(),
					size: o.pointer.size() as u64,
					authenticated: None,
					error: None,
					actions: (!matches!(o.ask, Ask::Nothing)).then_some(ObjectActions {
						download: None,
						upload,
						verify,
					}),
				}
			})
			.collect();

		Ok(BatchResponse {
			transfer: None,
			hash_algo: None,
			objects,
		})
	}

	async fn download(&self, _action: &ObjectAction, _to: &mut Write) -> Result<Pointer, RemoteError> {
		unreachable!("the tests only upload")
	}

	async fn upload(&self, _action: &ObjectAction, blob: &[u8], progress: UploadProgress) -> Result<(), RemoteError> {
		self.uploads.fetch_add(1, Ordering::SeqCst);

		match self.next() {
			Take::Object => {
				for end in chunk_ends(blob.len(), CHUNK) {
					progress.sent_and_yield(end).await;
					self.seen_after_report.lock().unwrap().push(self.last_bytes_handled());
				}
				Ok(())
			}
			Take::BreakAfter(n) => {
				for end in chunk_ends(n, CHUNK) {
					progress.sent_and_yield(end).await;
				}
				Err(RemoteError::Upload("connection reset".to_string()))
			}
			Take::FromThreadAndKeep => {
				self.kept.lock().unwrap().push(progress.clone());

				let (done, sent) = futures::channel::oneshot::channel();
				let size = blob.len();
				std::thread::spawn(move || {
					for end in chunk_ends(size, CHUNK) {
						progress.sent(end);
						std::thread::sleep(Duration::from_millis(1));
					}
					_ = done.send(());
				});

				sent.await.unwrap();
				Ok(())
			}
		}
	}

	async fn verify(&self, _action: &ObjectAction, _pointer: &Pointer) -> Result<(), RemoteError> {
		self.verifies.fetch_add(1, Ordering::SeqCst);
		Ok(())
	}
}

#[async_trait]
impl LfsRemote for &MockRemote {
	async fn batch(&self, req: BatchRequest) -> Result<BatchResponse, RemoteError> {
		(*self).batch(req).await
	}

	async fn download(&self, action: &ObjectAction, to: &mut Write) -> Result<Pointer, RemoteError> {
		(*self).download(action, to).await
	}

	async fn upload(&self, action: &ObjectAction, blob: &[u8], progress: UploadProgress) -> Result<(), RemoteError> {
		(*self).upload(action, blob, progress).await
	}

	async fn verify(&self, action: &ObjectAction, pointer: &Pointer) -> Result<(), RemoteError> {
		(*self).verify(action, pointer).await
	}
}

/// Puts `content` into the repository's lfs objects, as `git add` of an lfs file does.
fn store(repo: &git2::Repository, content: &[u8]) -> Pointer {
	let pointer = Pointer::from_blob_bytes(content).unwrap();
	let path = repo.path().join("lfs/objects").join(pointer.path());
	std::fs::create_dir_all(path.parent().unwrap()).unwrap();
	std::fs::write(path, content).unwrap();
	pointer
}

fn content(size: usize, seed: usize) -> Vec<u8> {
	(0..size).map(|i| ((i + seed) * 31 % 251) as u8).collect()
}

async fn push<C: LfsRemote>(
	repo: &git2::Repository,
	client: C,
	pointers: &[Pointer],
	events: Arc<Mutex<Vec<Progress>>>,
	concurrency: usize,
) -> Result<(), RemoteError> {
	let client = LfsClient::new(repo, client)
		.concurrency_limit(concurrency)
		.on_progress(Some(Box::new(move |event| events.lock().unwrap().push(event))));

	tokio::time::timeout(PUSH_TIMEOUT, client.push(pointers)).await.expect("the push hung")
}

fn assert_monotonic(events: &[Progress]) {
	for pair in events.windows(2) {
		assert!(
			pair[1].bytes_handled() >= pair[0].bytes_handled() && pair[1].objects_handled() >= pair[0].objects_handled(),
			"went back: {:?} -> {:?}",
			pair[0],
			pair[1]
		);
	}
}

fn assert_last_is_final(events: &[Progress], total_objects: usize, total_bytes: usize) {
	let last = events.last().expect("no events");
	assert_eq!((last.objects_handled(), last.total_objects()), (total_objects, total_objects));
	assert_eq!((last.bytes_handled(), last.total_bytes()), (total_bytes, total_bytes));
}

/// `bytes_handled` of the events before the first object is uploaded.
fn in_flight(events: &[Progress]) -> Vec<usize> {
	events.iter().filter(|e| e.objects_handled() == 0).map(|e| e.bytes_handled()).collect()
}

#[rstest]
#[tokio::test]
async fn upload_reports_every_chunk_while_object_uploads(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let size = CHUNK * 10 + 17;
	let pointer = store(&repo, &content(size, 0));
	let remote = MockRemote::new(vec![MockObject { pointer, ask: Ask::Upload }], &[Take::Object]);
	let events = remote.events.clone();

	push(&repo, &remote, &remote.pointers(), events, 1).await?;

	assert_eq!(
		*remote.seen_after_report.lock().unwrap(),
		chunk_ends(size, CHUNK),
		"each report reaches on_progress before the upload goes on"
	);

	let events = remote.events();
	assert!(events.iter().all(|e| e.total_objects() == 1 && e.total_bytes() == size));
	assert_monotonic(&events);
	assert_eq!(in_flight(&events), chunk_ends(size, CHUNK));
	assert_last_is_final(&events, 1, size);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn upload_counts_only_objects_server_asks_to_upload(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let uploaded = [CHUNK * 3, CHUNK * 2 + 3];
	let objects = vec![
		MockObject {
			pointer: store(&repo, &content(uploaded[0], 1)),
			ask: Ask::Upload,
		},
		MockObject {
			pointer: store(&repo, &content(5000, 2)),
			ask: Ask::Nothing,
		},
		MockObject {
			pointer: store(&repo, &content(700, 3)),
			ask: Ask::Verify,
		},
		MockObject {
			pointer: store(&repo, &content(uploaded[1], 4)),
			ask: Ask::UploadAndVerify,
		},
	];
	let remote = MockRemote::new(objects, &[Take::Object]);
	let events = remote.events.clone();
	let total_bytes = uploaded.iter().sum::<usize>();

	push(&repo, &remote, &remote.pointers(), events, 1).await?;

	assert_eq!(remote.uploads.load(Ordering::SeqCst), 2);
	assert_eq!(remote.verifies.load(Ordering::SeqCst), 2);

	let events = remote.events();
	for event in &events {
		assert!(matches!(event, Progress::Upload(_)), "{event:?}");
		assert_eq!((event.total_objects(), event.total_bytes()), (2, total_bytes), "{event:?}");
	}

	assert_monotonic(&events);
	assert_last_is_final(&events, 2, total_bytes);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn upload_retry_reports_from_zero(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let size = CHUNK * 8;
	let pointer = store(&repo, &content(size, 5));
	let remote = MockRemote::new(
		vec![MockObject { pointer, ask: Ask::Upload }],
		&[Take::BreakAfter(CHUNK * 5), Take::Object],
	);
	let events = remote.events.clone();

	push(&repo, &remote, &remote.pointers(), events, 1).await?;

	assert_eq!(remote.uploads.load(Ordering::SeqCst), 2);

	let events = remote.events();
	let retry = events
		.windows(2)
		.position(|pair| pair[1].bytes_handled() < pair[0].bytes_handled())
		.expect("the retry starts over")
		+ 1;

	let (first, second) = events.split_at(retry);
	assert_monotonic(first);
	assert_monotonic(second);
	assert_eq!(first.last().unwrap().bytes_handled(), CHUNK * 5);
	assert_eq!(second[0].bytes_handled(), CHUNK, "the failed attempt's bytes are replaced, not added to");

	assert_last_is_final(&events, 1, size);

	Ok(())
}

#[rstest]
#[tokio::test]
async fn upload_bytes_of_objects_in_flight_add_up(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	let sizes = [CHUNK * 6, CHUNK * 3 + 5];
	let objects = sizes
		.iter()
		.enumerate()
		.map(|(i, size)| MockObject {
			pointer: store(&repo, &content(*size, i)),
			ask: Ask::Upload,
		})
		.collect();
	let remote = MockRemote::new(objects, &[Take::Object]);
	let events = remote.events.clone();
	let total_bytes = sizes.iter().sum::<usize>();

	push(&repo, &remote, &remote.pointers(), events, 2).await?;

	let events = remote.events();
	assert_monotonic(&events);
	// before either object is uploaded, more is sent than the larger one holds: both count
	assert!(
		in_flight(&events).into_iter().any(|bytes| bytes > sizes[0]),
		"the bytes of both objects add up: {events:?}"
	);

	assert_last_is_final(&events, 2, total_bytes);

	Ok(())
}

/// An `UploadProgress` kept after its upload holds the channel open; the push must end anyway.
#[rstest]
#[tokio::test]
async fn upload_reported_from_another_thread_ends_with_progress_kept(
	_sandbox: TempDir,
	#[with(&_sandbox)] repo: git2::Repository,
) -> anyhow::Result<()> {
	let sizes = [CHUNK * 4 + 1, CHUNK * 2];
	let objects = sizes
		.iter()
		.enumerate()
		.map(|(i, size)| MockObject {
			pointer: store(&repo, &content(*size, i + 10)),
			ask: Ask::Upload,
		})
		.collect();
	let remote = MockRemote::new(objects, &[Take::FromThreadAndKeep]);
	let events = remote.events.clone();
	let total_bytes = sizes.iter().sum::<usize>();

	push(&repo, &remote, &remote.pointers(), events, 1).await?;

	let events = remote.events();
	assert_monotonic(&events);
	assert_eq!(in_flight(&events), chunk_ends(sizes[0], CHUNK));
	assert_last_is_final(&events, 2, total_bytes);

	let kept = remote.kept.lock().unwrap();
	assert_eq!(kept.len(), 2);
	kept[0].sent(1);
	assert_eq!(remote.events().len(), events.len(), "a report after the push goes nowhere");

	Ok(())
}

struct CapturedRequest {
	head: Vec<String>,
	body: Vec<u8>,
}

impl CapturedRequest {
	fn header(&self, name: &str) -> Option<&str> {
		self.head.iter().skip(1).find_map(|line| {
			let (key, value) = line.split_once(':')?;
			key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
		})
	}
}

fn response(status: &str, headers: &str, body: &str) -> String {
	format!(
		"HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
		body.len()
	)
}

/// Serves `responses` one request per connection, in order, and hands every request over.
fn serve(listener: TcpListener, responses: Vec<String>) -> std::sync::mpsc::Receiver<CapturedRequest> {
	let (tx, rx) = std::sync::mpsc::channel();

	std::thread::spawn(move || {
		for response in responses {
			let (stream, _) = listener.accept().unwrap();
			let mut reader = BufReader::new(stream.try_clone().unwrap());

			let mut head = vec![];
			loop {
				let mut line = String::new();
				reader.read_line(&mut line).unwrap();
				let line = line.trim_end().to_string();
				if line.is_empty() {
					break;
				}
				head.push(line);
			}

			let mut request = CapturedRequest { head, body: vec![] };
			let len = request.header("content-length").map(|l| l.parse::<usize>().unwrap()).unwrap_or_default();
			request.body = vec![0; len];
			reader.read_exact(&mut request.body).unwrap();

			std::io::Write::write_all(&mut &stream, response.as_bytes()).unwrap();
			tx.send(request).unwrap();
		}
	});

	rx
}

#[rstest]
#[tokio::test]
async fn reqwest_upload_streams_body_with_content_length(_sandbox: TempDir, #[with(&_sandbox)] repo: git2::Repository) -> anyhow::Result<()> {
	const BODY_CHUNK: usize = 64 * 1024;

	let size = BODY_CHUNK * 3 + 5;
	let object = content(size, 7);
	let pointer = store(&repo, &object);

	let listener = TcpListener::bind("127.0.0.1:0")?;
	let base = format!("http://{}", listener.local_addr()?);
	let batch = serde_json::json!({
		"objects": [{
			"oid": pointer.hex(),
			"size": size,
			"actions": {
				"upload": {
					"href": format!("{base}/upload/{}", pointer.hex()),
					"header": { "X-Test": "yes" }
				}
			}
		}]
	});

	let responses = vec![
		response("200 OK", &format!("Content-Type: {MEDIA_TYPE}\r\n"), &batch.to_string()),
		response("200 OK", "", ""),
	];
	let requests = serve(listener, responses);

	let client = ReqwestLfsClient::new(Url::parse(&format!("{base}/repo.git/info/lfs"))?, None);
	let events = Arc::new(Mutex::new(vec![]));
	push(&repo, client, &[pointer], events.clone(), 1).await?;

	let batch = requests.recv_timeout(PUSH_TIMEOUT)?;
	assert_eq!(batch.head[0], "POST /repo.git/info/lfs/objects/batch HTTP/1.1");

	let upload = requests.recv_timeout(PUSH_TIMEOUT)?;
	assert_eq!(upload.head[0], format!("PUT /upload/{} HTTP/1.1", pointer.hex()));
	assert_eq!(upload.header("content-length"), Some(size.to_string().as_str()));
	assert_eq!(upload.header("transfer-encoding"), None, "the body goes out with a length, not chunked");
	assert_eq!(upload.header("user-agent"), Some("gx-lfs/0.0.0"));
	assert_eq!(upload.header("x-test"), Some("yes"), "the action's headers go along");
	assert!(upload.body == object, "the body is the object, byte for byte");

	let events = events.lock().unwrap();
	assert_eq!(in_flight(&events), chunk_ends(size, BODY_CHUNK), "every chunk of the body is reported");
	assert_last_is_final(&events, 1, size);

	Ok(())
}
