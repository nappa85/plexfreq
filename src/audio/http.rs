//! Authenticated, redirect-confined HTTP byte transport. No credentials enter Gst URIs.
use gstreamer as gst;
use gstreamer_app as app;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
use tokio::sync::watch;

/// Consecutive failed attempts allowed before any byte reached the decoder, or
/// for a response that cannot make progress (1s/2s/4s backoff). Exhausting
/// them reports a decoder error so the runtime can choose a fallback source.
const UNPROVEN_RETRIES: u32 = 3;
/// How long dropping the last closer waits for queued stop requests before
/// leaving them to the detached worker; shutdown must not wait on the network.
const CLOSE_GRACE: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(super) struct SessionCloser(Arc<CloseWorker>);
struct CloseWorker {
    jobs: Option<tokio::sync::mpsc::Sender<(url::Url, String)>>,
    stopping: Arc<AtomicBool>,
    finished: std::sync::Mutex<mpsc::Receiver<()>>,
}
impl SessionCloser {
    pub(super) fn new() -> crate::Result<Self> {
        let (jobs, mut receiver) = tokio::sync::mpsc::channel::<(url::Url, String)>(64);
        let (finished_tx, finished) = mpsc::channel::<()>();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        // Detached task on the shared runtime: it is not joined, so a slow
        // server cannot delay the thread dropping the last closer. Dropping
        // `finished_tx` signals exit. Without a client the closer is inert.
        if let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            crate::rt::handle().spawn(async move {
                let _finished = finished_tx;
                let mut shutdown_jobs = 0;
                while let Some((uri, token)) = receiver.recv().await {
                    if stop.load(Ordering::SeqCst) {
                        if shutdown_jobs >= 2 {
                            break;
                        }
                        shutdown_jobs += 1;
                    }
                    let _ = client.get(uri).header("X-Plex-Token", token).send().await;
                }
            });
        }
        Ok(Self(Arc::new(CloseWorker {
            jobs: Some(jobs),
            stopping,
            finished: std::sync::Mutex::new(finished),
        })))
    }
    fn close(&self, uri: url::Url, token: String) {
        // Cleanup cannot block transport; the server also expires idle sessions.
        if let Some(jobs) = &self.0.jobs {
            let _ = jobs.try_send((uri, token));
        }
    }
}
impl Drop for CloseWorker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.jobs.take();
        // Usually lets the final stop requests finish; never waits on timeouts.
        // The last clone may die with a reader task on the runtime thread,
        // which must not block on work only that same thread can run.
        if !crate::rt::on_runtime_thread() {
            let _ = crate::mutex_lock(&self.finished).recv_timeout(CLOSE_GRACE);
        }
    }
}

#[derive(Default)]
struct Representation {
    validator: Option<String>,
    total: Option<u64>,
    started: bool,
}
/// A temporary outage is not decoder failure: keep queued bytes and wait for
/// the same representation until seek/stop cancels this reader. Until bytes
/// have reached the decoder (`proven`), only `UNPROVEN_RETRIES` are allowed.
fn retryable(
    representation: &Representation,
    offset: u64,
    progressive: bool,
    proven: bool,
    failures: u32,
) -> bool {
    !progressive
        && (offset == 0 || representation.validator.is_some())
        && (proven || failures < UNPROVEN_RETRIES)
}
fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs(1 << failures.saturating_sub(1).min(3))
}
/// `bytes first-last/complete-length`; the complete length may be unknown
/// (`*`, RFC 9110 section 14.4), as for growing or generated media.
fn content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let (span, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let (start, end) = (start.parse::<u64>().ok()?, end.parse::<u64>().ok()?);
    let total = match total {
        "*" => None,
        total => Some(total.parse::<u64>().ok()?),
    };
    (end >= start && total.is_none_or(|total| end < total)).then_some((start, end, total))
}
impl Representation {
    fn layout(
        &mut self,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
        offset: u64,
    ) -> Option<(u64, Option<u64>, Option<u64>)> {
        if !matches!(status.as_u16(), 200 | 206)
            || !crate::plex::audio_mime(
                headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(""),
            )
            || headers
                .get("content-encoding")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
        {
            return None;
        }
        let validator = crate::cache::response_validator(headers);
        let length = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
        let (total, expected) = if partial {
            let (start, end, total) = headers
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(content_range)?;
            if start != offset || length.is_some_and(|n| n != end - start + 1) {
                return None;
            }
            (total, Some(end - start + 1))
        } else {
            // A full body shorter than the requested offset can never reach it.
            if length.is_some_and(|n| n < offset) {
                return None;
            }
            (length, length)
        };
        if self.started
            && (self.total.zip(total).is_some_and(|(old, new)| old != new)
                || offset > 0 && self.validator.is_some() && validator != self.validator
                || self
                    .validator
                    .as_ref()
                    .zip(validator.as_ref())
                    .is_some_and(|(old, new)| old != new))
        {
            return None;
        }
        // A server may ignore Range, or return 200 because If-Range failed.
        // Discarding an arbitrary prefix is safe only for the same validated
        // representation; otherwise the decoder's offsets refer to old bytes.
        if !partial
            && offset > 0
            && (!self.started || self.validator.is_none() || validator != self.validator)
        {
            return None;
        }
        if !self.started {
            self.validator = validator;
            self.started = true;
        }
        self.total = self.total.or(total);
        Some((if partial { 0 } else { offset }, total, expected))
    }
}

/// Result of one HTTP request for the current decoder offset.
enum Attempt {
    /// A decoder seek or stop superseded the request.
    Superseded,
    /// The representation was delivered completely and EOS queued.
    Complete,
    /// The transfer failed before completion; retry from `offset`.
    /// `stalled` marks a response that cannot make progress by retrying.
    Interrupted {
        offset: u64,
        delivered: u64,
        stalled: bool,
    },
    /// A non-retryable failure reported on the decoder bus.
    Failed(&'static str),
    /// The appsrc no longer accepts data (decoder teardown).
    Closed,
}

/// The reader worker: the only producer of `output`'s bytes.
struct Stream {
    client: reqwest::Client,
    uri: String,
    token: String,
    progressive: bool,
    output: app::AppSrc,
    changed: watch::Receiver<Option<u64>>,
    progress: Arc<AtomicU64>,
    representation: Representation,
    proven: bool,
}
impl Stream {
    async fn run(mut self) {
        let mut retry = None;
        let mut failures: u32 = 0;
        loop {
            if retry.is_some() {
                tokio::select! {
                    _ = self.changed.changed() => {
                        retry = None;
                        failures = 0;
                        continue;
                    }
                    _ = tokio::time::sleep(retry_delay(failures)) => {}
                }
            } else {
                failures = 0;
            }
            let Some(offset) = retry.take().or(*self.changed.borrow_and_update()) else {
                return;
            };
            crate::diagnostics::event(format_args!(
                "HTTP audio request offset={offset} progressive={} retry={failures}",
                self.progressive
            ));
            match self.request(offset).await {
                Attempt::Superseded => {}
                Attempt::Complete => {
                    // Stay alive for a decoder seek back into the stream.
                    if self.changed.changed().await.is_err() {
                        return;
                    }
                }
                Attempt::Interrupted {
                    offset,
                    delivered,
                    stalled,
                } => {
                    if delivered > 0 {
                        failures = 0;
                    }
                    let proven = self.proven && !stalled;
                    if !retryable(
                        &self.representation,
                        offset,
                        self.progressive,
                        proven,
                        failures,
                    ) {
                        self.fail("Audio streaming failed");
                        return;
                    }
                    failures = failures.saturating_add(1);
                    retry = Some(offset);
                    crate::diagnostics::event(format_args!(
                        "audio stream interrupted; retry={failures} delay_ms={}",
                        retry_delay(failures).as_millis()
                    ));
                }
                Attempt::Failed(message) => {
                    self.fail(message);
                    return;
                }
                Attempt::Closed => return,
            }
        }
    }
    fn fail(&self, message: &'static str) {
        gst::element_error!(self.output, gst::ResourceError::Read, ["{}", message]);
    }
    async fn request(&mut self, offset: u64) -> Attempt {
        let mut request = self
            .client
            .get(&self.uri)
            .header("Accept-Encoding", "identity");
        if !self.progressive {
            request = request.header("Range", format!("bytes={offset}-"));
        }
        if let Some(validator) = self.representation.validator.as_deref() {
            request = request.header("If-Range", validator);
        }
        if !self.token.is_empty() {
            request = request.header("X-Plex-Token", &self.token);
        }
        let response = tokio::select! {
            _ = self.changed.changed() => return Attempt::Superseded,
            response = request.send() => response,
        };
        let response = match response {
            Ok(response) if response.status().is_success() => response,
            Ok(response) if !response.status().is_server_error() => {
                return Attempt::Failed("Audio streaming failed")
            }
            _ => {
                return Attempt::Interrupted {
                    offset,
                    delivered: 0,
                    stalled: false,
                }
            }
        };
        let Some((skip, total, expected)) =
            self.representation
                .layout(response.status(), response.headers(), offset)
        else {
            return Attempt::Failed("Invalid audio representation");
        };
        crate::diagnostics::event(format_args!(
            "HTTP audio response status={} total={total:?} expected={expected:?} validator={}",
            response.status().as_u16(),
            self.representation.validator.is_some()
        ));
        if let Some(total) = total {
            self.output.set_size(total.min(i64::MAX as u64) as i64);
        }
        self.body(response, offset, skip, expected).await
    }
    async fn body(
        &mut self,
        mut response: reqwest::Response,
        offset: u64,
        mut skip: u64,
        expected: Option<u64>,
    ) -> Attempt {
        let mut received = 0u64;
        let mut delivered = 0u64;
        loop {
            let chunk = tokio::select! {
                _ = self.changed.changed() => return Attempt::Superseded,
                chunk = response.chunk() => chunk,
            };
            let bytes = match chunk {
                Ok(Some(bytes)) => bytes,
                Ok(None) if skip == 0 && expected.is_none_or(|n| received == n) => {
                    crate::diagnostics::event(format_args!(
                        "HTTP audio EOF received={received} delivered={delivered}"
                    ));
                    let _ = self.output.end_of_stream();
                    return Attempt::Complete;
                }
                end => {
                    return Attempt::Interrupted {
                        offset: offset + delivered,
                        delivered,
                        // A full response ending before the requested offset.
                        stalled: matches!(end, Ok(None)) && skip > 0,
                    };
                }
            };
            received = received.saturating_add(bytes.len() as u64);
            self.progress
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            if expected.is_some_and(|n| received > n) {
                return Attempt::Failed("Invalid audio representation");
            }
            let ignore = skip.min(bytes.len() as u64) as usize;
            skip -= ignore as u64;
            if let Err(attempt) = self.deliver(&bytes[ignore..], &mut delivered).await {
                return attempt;
            }
        }
    }
    /// Push bytes in appsrc-sized chunks, waiting (cancellably) for queue room.
    async fn deliver(&mut self, bytes: &[u8], delivered: &mut u64) -> Result<(), Attempt> {
        let chunk_size = match self.output.max_bytes() {
            0 => 64 * 1024,
            max => max.min(64 * 1024) as usize,
        };
        for chunk in bytes.chunks(chunk_size) {
            while self.output.max_bytes() > 0
                && self
                    .output
                    .current_level_bytes()
                    .saturating_add(chunk.len() as u64)
                    > self.output.max_bytes()
            {
                tokio::select! {
                    _ = self.changed.changed() => return Err(Attempt::Superseded),
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
            }
            // appsrc calls seek_data, which publishes the new offset under the
            // watch write lock, before flushing its queue. Holding the read lock
            // across check-and-push orders every push either before that flush
            // (and discards it) or after the new offset is visible here, so
            // bytes from the old offset never follow a seek.
            let pushed = {
                let _offset = self.changed.borrow();
                if self.changed.has_changed().unwrap_or(true) {
                    return Err(Attempt::Superseded);
                }
                self.output
                    .push_buffer(gst::Buffer::from_mut_slice(chunk.to_vec()))
            };
            if pushed.is_err() {
                return Err(Attempt::Closed);
            }
            *delivered += chunk.len() as u64;
            self.proven = true;
        }
        Ok(())
    }
}

pub struct Reader {
    stop: watch::Sender<Option<u64>>,
    worker: Option<crate::rt::Task>,
    /// Keeps the closer alive past the task: if this is the last clone, its
    /// drop waits (on the caller's thread, never the runtime thread) for the
    /// stop request the task queued.
    _closer: SessionCloser,
    input: app::AppSrc,
    received: Arc<AtomicU64>,
}
impl Reader {
    pub fn new(
        source: &app::AppSrc,
        uri: String,
        token: String,
        closer: SessionCloser,
    ) -> crate::Result<Self> {
        let (stop, changed) = watch::channel(Some(0u64));
        let seek = stop.clone();
        source.set_callbacks(
            app::AppSrcCallbacks::builder()
                .seek_data(move |_, offset| {
                    // None is terminal cancellation. A late decoder seek during
                    // teardown must not resurrect a stopped network reader.
                    if seek.is_closed() {
                        return false;
                    }
                    seek.send_if_modified(|requested| {
                        if requested.is_none() {
                            return false;
                        }
                        *requested = Some(offset);
                        true
                    })
                })
                .build(),
        );
        let output = source.clone();
        // The HTTP worker is the sole producer. Use a cancellable watermark
        // instead of blocking inside Gst push_buffer, which cannot poll seek.
        output.set_block(false);
        let received = Arc::new(AtomicU64::new(0));
        let progress = received.clone();
        let retained = closer.clone();
        // A task on the shared I/O runtime (no thread or runtime per track).
        // appsrc holds up to 4 MiB of undecoded input, many seconds of audio,
        // so millisecond scheduling jitter shared with other I/O tasks cannot
        // starve the decoder; the task only awaits sockets/timers and does
        // non-blocking pushes.
        let worker = crate::rt::spawn(async move {
            let stop_uri = url::Url::parse(&uri)
                .ok()
                .and_then(|url| crate::quality::stop_url(&url));
            let stop_token = token.clone();
            // Redirects are disabled deliberately: the Plex token travels as
            // a header, and an open redirect could leak it to an arbitrary
            // origin. Plex serves media directly; a 3xx surfaces as a
            // streaming failure instead of silently following.
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build();
            match client {
                Ok(client) => {
                    Stream {
                        client,
                        uri,
                        token,
                        progressive: stop_uri.is_some(),
                        output,
                        changed,
                        progress,
                        representation: Representation::default(),
                        proven: false,
                    }
                    .run()
                    .await
                }
                Err(_) => gst::element_error!(
                    output,
                    gst::ResourceError::Failed,
                    ["Audio streaming failed"]
                ),
            }
            // The reader owns the server session, including predecoded
            // successors. Finish/cancel/error and decoder teardown all
            // take this path.
            if let Some(stop_uri) = stop_uri {
                closer.close(stop_uri, stop_token);
            }
        });
        Ok(Self {
            stop,
            worker: Some(worker),
            _closer: retained,
            input: source.clone(),
            received,
        })
    }
    pub(super) fn diagnostics(&self) -> (u64, u64) {
        (
            self.received.load(Ordering::Relaxed),
            self.input.current_level_bytes(),
        )
    }
    pub fn stop(&self) {
        self.stop.send_replace(None);
        // Release appsrc consumers waiting for preroll while HTTP is cancelled.
        let _ = self.input.end_of_stream();
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            // Every await in the reader selects on the stop watch, so this
            // returns promptly; afterwards no push can reach the appsrc.
            worker.join(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gst::prelude::*;
    use reqwest::{header::HeaderMap, StatusCode};
    use std::thread;

    #[test]
    fn full_input_queue_does_not_block_reader_shutdown() {
        use std::io::{Read, Write};
        gst::init().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 131072\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let _ = stream.write_all(&vec![1; 131072]);
        });
        let source = app::AppSrc::builder().max_bytes(65536).block(true).build();
        let reader = Reader::new(
            &source,
            format!("http://{address}/audio"),
            String::new(),
            SessionCloser::new().unwrap(),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while source.current_level_bytes() == 0 || reader.received.load(Ordering::Relaxed) <= 65536
        {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(source.current_level_bytes() <= 65536);
        let start = std::time::Instant::now();
        drop(reader);
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    #[test]
    fn recovery_backoff_is_capped_and_requires_a_validated_original() {
        let mut representation = Representation::default();
        assert!(retryable(&representation, 0, false, true, 0));
        assert!(!retryable(&representation, 1, false, true, 0));
        representation.validator = Some("\"fixture\"".into());
        assert!(retryable(&representation, 1, false, true, 0));
        assert!(!retryable(&representation, 1, true, true, 0));
        // Buffered bytes justify waiting out an outage; an unproven source
        // gets a bounded number of attempts so the runtime can fall back.
        assert!(retryable(&representation, 1, false, true, 1000));
        assert!(retryable(
            &representation,
            0,
            false,
            false,
            UNPROVEN_RETRIES - 1
        ));
        assert!(!retryable(
            &representation,
            0,
            false,
            false,
            UNPROVEN_RETRIES
        ));
        for (failures, seconds) in [(1, 1), (2, 2), (3, 4), (4, 8), (32, 8), (u32::MAX, 8)] {
            assert_eq!(retry_delay(failures), Duration::from_secs(seconds));
        }
    }

    #[test]
    fn retry_backoff_is_cancelled_promptly_by_stop() {
        use std::io::{Read, Write};
        gst::init().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, response) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            sent.send(()).unwrap();
        });
        let source = app::AppSrc::builder().build();
        let reader = Reader::new(
            &source,
            format!("http://{address}/audio"),
            String::new(),
            SessionCloser::new().unwrap(),
        )
        .unwrap();
        response.recv_timeout(Duration::from_secs(3)).unwrap();
        thread::sleep(Duration::from_millis(100));
        let start = std::time::Instant::now();
        drop(reader);
        assert!(start.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }

    #[test]
    fn interrupted_original_stream_resumes_without_duplicate_bytes() {
        resumed_original_stream(0);
    }

    #[test]
    fn original_stream_survives_retry_exhaustion_without_losing_buffered_bytes() {
        resumed_original_stream(4);
    }

    fn resumed_original_stream(unavailable_responses: usize) {
        use std::io::{Read, Write};
        gst::init().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for attempt in 0..unavailable_responses + 2 {
                let offset = if attempt == 0 { 0 } else { 65536 };
                let until = std::time::Instant::now() + Duration::from_secs(12);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < until, "No resumed request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap().to_lowercase();
                assert!(request.contains(&format!("range: bytes={offset}-")));
                if offset > 0 {
                    assert!(request.contains("if-range: \"fixture\""));
                }
                if attempt > 0 && attempt <= unavailable_responses {
                    stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    continue;
                }
                write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-131071/131072\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n", 131072-offset, offset).unwrap();
                // First response ends early; the second completes the same bytes.
                stream
                    .write_all(&vec![if offset == 0 { 1 } else { 2 }; 65536])
                    .unwrap();
            }
        });
        let pipeline = gst::Pipeline::new();
        let source = app::AppSrc::builder().format(gst::Format::Bytes).build();
        let sink = app::AppSink::builder().sync(false).build();
        pipeline
            .add_many([source.upcast_ref::<gst::Element>(), sink.upcast_ref()])
            .unwrap();
        source.link(&sink).unwrap();
        let reader = Reader::new(
            &source,
            format!("http://{address}/audio"),
            String::new(),
            SessionCloser::new().unwrap(),
        )
        .unwrap();
        if unavailable_responses > 0 {
            // Leave the initial bytes queued across the outage, as on the phone.
            // A bus error would discard them before the connection recovers.
            let until = std::time::Instant::now() + Duration::from_secs(30);
            while reader.received.load(Ordering::Relaxed) < 131072 {
                assert!(
                    pipeline
                        .bus()
                        .unwrap()
                        .pop_filtered(&[gst::MessageType::Error])
                        .is_none(),
                    "Buffered stream must not fail during a temporary outage"
                );
                assert!(
                    std::time::Instant::now() < until,
                    "No recovery after outage"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        pipeline.set_state(gst::State::Playing).unwrap();
        let mut bytes = Vec::new();
        let until = std::time::Instant::now() + Duration::from_secs(30);
        while bytes.len() < 131072 && std::time::Instant::now() < until {
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
                bytes
                    .extend_from_slice(sample.buffer().unwrap().map_readable().unwrap().as_slice());
            }
        }
        reader.stop();
        pipeline.set_state(gst::State::Null).unwrap();
        drop(reader);
        server.join().unwrap();
        assert_eq!(bytes.len(), 131072);
        assert!(bytes[..65536].iter().all(|&v| v == 1));
        assert!(bytes[65536..].iter().all(|&v| v == 2));
        assert!(pipeline
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error])
            .is_none());
    }

    /// Waits for an error posted by the reader on `pipeline`'s bus.
    fn bus_error(pipeline: &gst::Pipeline, timeout: Duration) -> bool {
        pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_mseconds(timeout.as_millis() as u64),
                &[gst::MessageType::Error],
            )
            .is_some()
    }

    fn reader_pipeline(address: std::net::SocketAddr) -> (gst::Pipeline, Reader) {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        let source = app::AppSrc::builder().format(gst::Format::Bytes).build();
        let sink = app::AppSink::builder().sync(false).build();
        pipeline
            .add_many([source.upcast_ref::<gst::Element>(), sink.upcast_ref()])
            .unwrap();
        source.link(&sink).unwrap();
        let reader = Reader::new(
            &source,
            format!("http://{address}/audio"),
            String::new(),
            SessionCloser::new().unwrap(),
        )
        .unwrap();
        (pipeline, reader)
    }

    #[test]
    fn unreachable_source_reports_error_after_bounded_retries() {
        // Nothing listens on this port: every attempt is refused.
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let (pipeline, reader) = reader_pipeline(address);
        let started = std::time::Instant::now();
        // Backoff before the error is 1s + 2s + 4s.
        assert!(
            bus_error(&pipeline, Duration::from_secs(12)),
            "unproven source retried forever"
        );
        assert!(started.elapsed() >= Duration::from_secs(6));
        drop(reader);
    }

    #[test]
    fn failing_first_response_reports_error_after_bounded_retries() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let server = thread::spawn(move || {
            let mut requests = 0;
            while !stopped.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
                    request.push(byte[0]);
                }
                requests += 1;
                let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
            requests
        });
        let (pipeline, reader) = reader_pipeline(address);
        assert!(
            bus_error(&pipeline, Duration::from_secs(12)),
            "unproven source retried forever"
        );
        drop(reader);
        stop.store(true, Ordering::SeqCst);
        assert_eq!(server.join().unwrap(), 1 + UNPROVEN_RETRIES as usize);
    }

    #[test]
    fn dropping_the_closer_does_not_wait_for_a_silent_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let closer = SessionCloser::new().unwrap();
        let uri = url::Url::parse(&format!("http://{address}/stop")).unwrap();
        for _ in 0..3 {
            closer.close(uri.clone(), String::new());
        }
        // Let the worker block on the accepted-but-unanswered first request.
        thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        drop(closer);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(listener);
    }

    #[test]
    fn content_range_accepts_unknown_total_and_rejects_malformed() {
        assert_eq!(content_range("bytes 4-9/10"), Some((4, 9, Some(10))));
        assert_eq!(content_range("bytes 4-9/*"), Some((4, 9, None)));
        for invalid in ["bytes 4-10/10", "bytes 9-4/*", "bytes */10", "items 4-9/10"] {
            assert_eq!(content_range(invalid), None, "{invalid}");
        }
        let mut state = Representation::default();
        state
            .layout(StatusCode::OK, &headers(Some("\"old\""), None, "10"), 0)
            .unwrap();
        assert_eq!(
            state.layout(
                StatusCode::PARTIAL_CONTENT,
                &headers(Some("\"old\""), Some("bytes 4-9/*"), "6"),
                4
            ),
            Some((0, None, Some(6)))
        );
    }

    #[test]
    fn full_response_shorter_than_requested_offset_is_rejected() {
        let mut state = Representation::default();
        state
            .layout(StatusCode::OK, &headers(Some("\"old\""), None, "10"), 0)
            .unwrap();
        assert!(state
            .layout(StatusCode::OK, &headers(Some("\"old\""), None, "10"), 11)
            .is_none());
        assert_eq!(
            state.layout(StatusCode::OK, &headers(Some("\"old\""), None, "10"), 10),
            Some((10, Some(10), Some(10)))
        );
    }

    fn headers(etag: Option<&str>, range: Option<&str>, length: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", length.parse().unwrap());
        if let Some(etag) = etag {
            headers.insert("etag", etag.parse().unwrap());
        }
        if let Some(range) = range {
            headers.insert("content-range", range.parse().unwrap());
        }
        headers
    }

    #[test]
    fn full_response_seek_requires_unchanged_validator() {
        for etag in [None, Some("W/\"old\""), Some("\"old\"")] {
            let mut state = Representation::default();
            assert!(state
                .layout(StatusCode::OK, &headers(etag, None, "10"), 0)
                .is_some());
            let result = state.layout(StatusCode::OK, &headers(etag, None, "10"), 4);
            assert_eq!(result.is_some(), etag == Some("\"old\""));
        }
        let mut state = Representation::default();
        state
            .layout(StatusCode::OK, &headers(Some("\"old\""), None, "10"), 0)
            .unwrap();
        assert!(state
            .layout(StatusCode::OK, &headers(Some("\"new\""), None, "10"), 4)
            .is_none());
        assert!(state
            .layout(StatusCode::OK, &headers(None, None, "10"), 4)
            .is_none());
    }

    #[test]
    fn partial_seek_checks_range_length_and_representation() {
        let mut state = Representation::default();
        state
            .layout(StatusCode::OK, &headers(Some("\"old\""), None, "10"), 0)
            .unwrap();
        for (etag, range, length) in [
            ("\"old\"", "bytes 5-9/10", "5"),
            ("\"old\"", "bytes 4-9/10", "5"),
            ("\"new\"", "bytes 4-9/10", "6"),
            ("\"old\"", "bytes 4-10/11", "7"),
        ] {
            assert!(state
                .layout(
                    StatusCode::PARTIAL_CONTENT,
                    &headers(Some(etag), Some(range), length),
                    4
                )
                .is_none());
        }
        assert_eq!(
            state.layout(
                StatusCode::PARTIAL_CONTENT,
                &headers(Some("\"old\""), Some("bytes 4-9/10"), "6"),
                4
            ),
            Some((0, Some(10), Some(6)))
        );
    }
}
