//! Authenticated, redirect-confined HTTP byte transport. The decoder thread
//! reads through [`Source`], a blocking `Read + Seek` view of a bounded byte
//! window filled by an async task on the shared runtime. Credentials stay in
//! request headers; the decoder never sees a URL.
use std::{
    collections::VecDeque,
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    time::Duration,
};
use tokio::sync::{watch, Notify};

/// Consecutive failed attempts allowed before any byte reached the decoder, or
/// for a response that cannot make progress (1s/2s/4s backoff). Exhausting
/// them reports a decoder error so the runtime can choose a fallback source.
const UNPROVEN_RETRIES: u32 = 3;
/// How long dropping the last closer waits for queued stop requests before
/// leaving them to the detached worker; shutdown must not wait on the network.
const CLOSE_GRACE: Duration = Duration::from_millis(250);
/// Undecoded bytes buffered ahead of the decoder (many seconds of audio), so
/// scheduling jitter on the shared I/O thread cannot starve decoding.
const CAPACITY: usize = 4 * 1024 * 1024;
/// A forward seek this close to the buffered end waits for the bytes already
/// in flight instead of opening a new ranged request.
const FORWARD_WAIT: u64 = 64 * 1024;
/// Upper bound of one blocking wait before cancellation is re-checked.
const WAIT_SLICE: Duration = Duration::from_millis(100);

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
    /// The representation was delivered completely and EOF recorded.
    Complete,
    /// The transfer failed before completion; retry from `offset`.
    /// `stalled` marks a response that cannot make progress by retrying.
    Interrupted {
        offset: u64,
        delivered: u64,
        stalled: bool,
    },
    /// A non-retryable failure reported to the decoder.
    Failed(&'static str),
    /// The decoder no longer reads (teardown).
    Closed,
}

/// The byte window shared by the network task (producer) and the decoder
/// thread (consumer). `generation` changes on every reposition, so bytes of a
/// superseded request can never be appended after a seek.
struct Window {
    generation: u64,
    /// Absolute offset of `data[0]`.
    start: u64,
    data: VecDeque<u8>,
    eof: bool,
    error: Option<&'static str>,
    closed: bool,
    total: Option<u64>,
}
struct Buffer {
    window: Mutex<Window>,
    /// Signalled to the decoder thread on data, EOF, error or close.
    readable: Condvar,
    /// Signalled to the network task when the decoder consumed bytes.
    room: Notify,
}
impl Buffer {
    fn lock(&self) -> std::sync::MutexGuard<'_, Window> {
        crate::mutex_lock(&self.window)
    }
}

/// Requested (offset, generation); `None` is terminal cancellation.
type Request = Option<(u64, u64)>;

/// The reader worker: the only producer of the window's bytes.
struct Stream {
    client: reqwest::Client,
    uri: String,
    token: String,
    progressive: bool,
    buffer: Arc<Buffer>,
    changed: watch::Receiver<Request>,
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
            let Some((requested, generation)) = *self.changed.borrow_and_update() else {
                return;
            };
            let offset = match retry.take() {
                Some(offset) => offset,
                None => requested,
            };
            crate::diagnostics::event(format_args!(
                "HTTP audio request offset={offset} progressive={} retry={failures}",
                self.progressive
            ));
            match self.request(offset, generation).await {
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
        let mut window = self.buffer.lock();
        window.error.get_or_insert(message);
        drop(window);
        self.buffer.readable.notify_all();
    }
    async fn request(&mut self, offset: u64, generation: u64) -> Attempt {
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
        if let Some(total) = total.filter(|_| !self.progressive) {
            let mut window = self.buffer.lock();
            window.total.get_or_insert(total);
        }
        self.body(response, offset, generation, skip, expected)
            .await
    }
    async fn body(
        &mut self,
        mut response: reqwest::Response,
        offset: u64,
        generation: u64,
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
                    let mut window = self.buffer.lock();
                    if window.generation != generation {
                        return Attempt::Superseded;
                    }
                    window.eof = true;
                    drop(window);
                    self.buffer.readable.notify_all();
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
            if let Err(attempt) = self
                .deliver(&bytes[ignore..], generation, &mut delivered)
                .await
            {
                return attempt;
            }
        }
    }
    /// Append bytes to the window, waiting (cancellably) for decoder room.
    /// The generation check and the append happen under one lock, so bytes
    /// of a request superseded by a seek are never appended after it.
    async fn deliver(
        &mut self,
        bytes: &[u8],
        generation: u64,
        delivered: &mut u64,
    ) -> Result<(), Attempt> {
        for chunk in bytes.chunks(64 * 1024) {
            loop {
                {
                    let mut window = self.buffer.lock();
                    if window.closed {
                        return Err(Attempt::Closed);
                    }
                    if window.generation != generation {
                        return Err(Attempt::Superseded);
                    }
                    if window.data.is_empty() || window.data.len() + chunk.len() <= CAPACITY {
                        window.data.extend(chunk);
                        break;
                    }
                }
                // Notify keeps one permit, so a consumption between the check
                // above and this await is not lost; the timer is a fallback.
                tokio::select! {
                    _ = self.changed.changed() => return Err(Attempt::Superseded),
                    _ = self.buffer.room.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
            self.buffer.readable.notify_all();
            *delivered += chunk.len() as u64;
            self.proven = true;
        }
        Ok(())
    }
}

/// Owner of one HTTP stream: its network task and server session.
pub struct Reader {
    stop: watch::Sender<Request>,
    worker: Option<crate::rt::Task>,
    /// Keeps the closer alive past the task: if this is the last clone, its
    /// drop waits (on the caller's thread, never the runtime thread) for the
    /// stop request the task queued.
    _closer: SessionCloser,
    buffer: Arc<Buffer>,
    received: Arc<AtomicU64>,
}
impl Reader {
    /// Start fetching from offset zero and return the decoder's byte view.
    /// `progressive` (server transcodes) cannot seek by byte offset.
    pub fn new(uri: String, token: String, closer: SessionCloser) -> crate::Result<(Self, Source)> {
        let progressive = url::Url::parse(&uri)
            .ok()
            .is_some_and(|url| crate::quality::is_transcode(&url));
        let (stop, changed) = watch::channel(Some((0u64, 0u64)));
        let buffer = Arc::new(Buffer {
            window: Mutex::new(Window {
                generation: 0,
                start: 0,
                data: VecDeque::new(),
                eof: false,
                error: None,
                closed: false,
                total: None,
            }),
            readable: Condvar::new(),
            room: Notify::new(),
        });
        let received = Arc::new(AtomicU64::new(0));
        let progress = received.clone();
        let retained = closer.clone();
        let shared = buffer.clone();
        // A task on the shared I/O runtime (no thread or runtime per track).
        // The window holds up to 4 MiB of undecoded input, many seconds of
        // audio, so millisecond scheduling jitter shared with other I/O tasks
        // cannot starve the decoder; the task only awaits sockets/timers and
        // takes the window lock for short copies, never a blocking wait.
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
            let stream = client.map(|client| Stream {
                client,
                uri,
                token,
                progressive,
                buffer: shared.clone(),
                changed,
                progress,
                representation: Representation::default(),
                proven: false,
            });
            match stream {
                Ok(stream) => stream.run().await,
                Err(_) => {
                    let mut window = shared.lock();
                    window.error.get_or_insert("Audio streaming failed");
                    drop(window);
                    shared.readable.notify_all();
                }
            }
            // The reader owns the server session, including predecoded
            // successors. Finish/cancel/error and decoder teardown all
            // take this path.
            if let Some(stop_uri) = stop_uri {
                closer.close(stop_uri, stop_token);
            }
        });
        let source = Source {
            buffer: buffer.clone(),
            request: stop.clone(),
            position: 0,
            progressive,
        };
        Ok((
            Self {
                stop,
                worker: Some(worker),
                _closer: retained,
                buffer,
                received,
            },
            source,
        ))
    }
    /// (bytes received from the network, bytes queued for the decoder).
    pub(super) fn diagnostics(&self) -> (u64, u64) {
        (
            self.received.load(Ordering::Relaxed),
            self.buffer.lock().data.len() as u64,
        )
    }
    /// Cancel the transfer and release a decoder blocked in `read`.
    pub fn stop(&self) {
        self.stop.send_replace(None);
        self.buffer.lock().closed = true;
        self.buffer.readable.notify_all();
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            // Every await in the reader selects on the stop watch, so this
            // returns promptly; afterwards no byte can reach the window.
            worker.join(None);
        }
    }
}

/// The decoder thread's blocking byte view of a [`Reader`]. Seeks only move
/// the logical position; a ranged request is issued when a read needs bytes
/// outside the buffered window, so probe-time End/Start round trips are free.
pub struct Source {
    buffer: Arc<Buffer>,
    request: watch::Sender<Request>,
    position: u64,
    progressive: bool,
}
impl Source {
    /// Restart the window at `position` and ask the network task for it.
    fn reposition(&self, window: &mut Window) -> io::Result<()> {
        if self.progressive {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "progressive stream cannot seek",
            ));
        }
        window.generation += 1;
        window.start = self.position;
        window.data.clear();
        window.eof = false;
        let request = (self.position, window.generation);
        // None is terminal cancellation; a late seek during teardown must not
        // resurrect a stopped network reader.
        self.request.send_if_modified(|current| {
            if current.is_none() {
                return false;
            }
            *current = Some(request);
            true
        });
        self.buffer.room.notify_one();
        Ok(())
    }
}
impl io::Read for Source {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let buffer = self.buffer.clone();
        let mut window = buffer.lock();
        loop {
            if window.closed {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "audio reader stopped",
                ));
            }
            if let Some(message) = window.error {
                return Err(io::Error::other(message));
            }
            if window.total.is_some_and(|total| self.position >= total) {
                return Ok(0);
            }
            let end = window.start + window.data.len() as u64;
            if self.position < window.start
                || self.position > end && (window.eof || self.position - end > FORWARD_WAIT)
            {
                self.reposition(&mut window)?;
                continue;
            }
            if self.position > window.start {
                // Discard bytes the decoder skipped (or will skip) over.
                let skip = (self.position - window.start).min(window.data.len() as u64);
                window.data.drain(..skip as usize);
                window.start += skip;
                buffer.room.notify_one();
            }
            if self.position == window.start && !window.data.is_empty() {
                let count = out.len().min(window.data.len());
                let (front, back) = window.data.as_slices();
                let first = count.min(front.len());
                out[..first].copy_from_slice(&front[..first]);
                out[first..count].copy_from_slice(&back[..count - first]);
                window.data.drain(..count);
                window.start += count as u64;
                self.position += count as u64;
                drop(window);
                buffer.room.notify_one();
                return Ok(count);
            }
            if window.eof && self.position >= end {
                return Ok(0);
            }
            window = buffer
                .readable
                .wait_timeout(window, WAIT_SLICE)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|poison| poison.into_inner().0);
        }
    }
}
impl io::Seek for Source {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        let total = self.buffer.lock().total;
        let target = match to {
            io::SeekFrom::Start(offset) => Some(offset),
            io::SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            io::SeekFrom::End(delta) => total.and_then(|total| total.checked_add_signed(delta)),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid audio seek"))?;
        if self.progressive && target != self.position {
            let window = self.buffer.lock();
            let end = window.start + window.data.len() as u64;
            // Only movement inside the buffered window (or a short forward
            // skip of bytes in flight) is possible without byte ranges.
            if target < window.start || target > end + FORWARD_WAIT {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "progressive stream cannot seek",
                ));
            }
        }
        self.position = target;
        Ok(target)
    }
}
impl symphonia::core::io::MediaSource for Source {
    fn is_seekable(&self) -> bool {
        !self.progressive
    }
    fn byte_len(&self) -> Option<u64> {
        self.buffer.lock().total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{header::HeaderMap, StatusCode};
    use std::{
        io::{Read, Seek, SeekFrom, Write},
        thread,
    };

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
            request.push(byte[0]);
        }
        String::from_utf8(request).unwrap().to_lowercase()
    }

    fn accept(listener: &std::net::TcpListener, timeout: Duration) -> std::net::TcpStream {
        let until = std::time::Instant::now() + timeout;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    return stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < until, "No request");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        }
    }

    fn reader(address: std::net::SocketAddr) -> (Reader, Source) {
        Reader::new(
            format!("http://{address}/audio"),
            String::new(),
            SessionCloser::new().unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn full_input_window_does_not_block_reader_shutdown() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let size = CAPACITY * 2;
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let _ = stream.write_all(&vec![1; size]);
        });
        let (reader, _source) = reader(address);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        // Nothing reads: the task fills the window and then waits for room.
        while reader.diagnostics().1 < CAPACITY as u64 - 64 * 1024
            || reader.received.load(Ordering::Relaxed) <= CAPACITY as u64
        {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(reader.diagnostics().1 <= CAPACITY as u64);
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, response) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            sent.send(()).unwrap();
        });
        let (reader, _source) = reader(address);
        response.recv_timeout(Duration::from_secs(3)).unwrap();
        thread::sleep(Duration::from_millis(100));
        let start = std::time::Instant::now();
        drop(reader);
        assert!(start.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }

    #[test]
    fn stop_releases_a_decoder_blocked_in_read() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release, done) = mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            // Headers but no body: the decoder blocks waiting for bytes.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n")
                .unwrap();
            let _ = done.recv_timeout(Duration::from_secs(5));
        });
        let (reader, mut source) = reader(address);
        let blocked = thread::spawn(move || source.read(&mut [0; 4]));
        thread::sleep(Duration::from_millis(200));
        let start = std::time::Instant::now();
        reader.stop();
        let result = blocked.join().unwrap();
        assert!(result.is_err(), "{result:?}");
        assert!(start.elapsed() < Duration::from_millis(500));
        drop(reader);
        release.send(()).unwrap();
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for attempt in 0..unavailable_responses + 2 {
                let offset = if attempt == 0 { 0 } else { 65536 };
                let mut stream = accept(&listener, Duration::from_secs(12));
                let request = read_request(&mut stream);
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
        let (reader, mut source) = reader(address);
        if unavailable_responses > 0 {
            // Leave the initial bytes queued across the outage, as on the
            // phone. A reported error would discard them before recovery.
            let until = std::time::Instant::now() + Duration::from_secs(30);
            while reader.received.load(Ordering::Relaxed) < 131072 {
                assert!(
                    reader.buffer.lock().error.is_none(),
                    "Buffered stream must not fail during a temporary outage"
                );
                assert!(
                    std::time::Instant::now() < until,
                    "No recovery after outage"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes).unwrap();
        drop(reader);
        server.join().unwrap();
        assert_eq!(bytes.len(), 131072);
        assert!(bytes[..65536].iter().all(|&v| v == 1));
        assert!(bytes[65536..].iter().all(|&v| v == 2));
    }

    /// Serves `body` with byte ranges and records each requested range start.
    fn ranged_server(
        body: Vec<u8>,
        requests: usize,
    ) -> (std::net::SocketAddr, thread::JoinHandle<Vec<u64>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut offsets = Vec::new();
            for _ in 0..requests {
                let mut stream = accept(&listener, Duration::from_secs(5));
                let request = read_request(&mut stream);
                let offset: u64 = request
                    .split("range: bytes=")
                    .nth(1)
                    .and_then(|r| r.split('-').next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                offsets.push(offset);
                let total = body.len() as u64;
                let _ = write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/flac\r\nContent-Length: {}\r\nContent-Range: bytes {offset}-{}/{total}\r\nETag: \"seek\"\r\nConnection: close\r\n\r\n", total - offset, total - 1);
                let _ = stream.write_all(&body[offset as usize..]);
            }
            offsets
        });
        (address, server)
    }

    #[test]
    fn media_source_seeks_by_byte_range_and_reuses_buffered_bytes() {
        use symphonia::core::io::MediaSource;
        // Larger than the window, so the far target can never be buffered yet.
        let size = CAPACITY as u64 + 2_000_000;
        let far = size - 100_000;
        let body: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
        let (address, server) = ranged_server(body.clone(), 2);
        let (reader, mut source) = reader(address);
        let mut head = [0; 16];
        source.read_exact(&mut head).unwrap();
        assert_eq!(&head, &body[..16]);
        assert_eq!(source.byte_len(), Some(size));
        assert!(source.is_seekable());
        // Probe-style End/Start round trip does not issue a request.
        assert_eq!(source.seek(SeekFrom::End(0)).unwrap(), size);
        assert_eq!(source.read(&mut head).unwrap(), 0);
        source.seek(SeekFrom::Start(16)).unwrap();
        source.read_exact(&mut head).unwrap();
        assert_eq!(&head, &body[16..32]);
        // A far seek opens one new ranged request at the target.
        source.seek(SeekFrom::Start(far)).unwrap();
        let mut tail = Vec::new();
        source.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, body[far as usize..]);
        drop(reader);
        let offsets = server.join().unwrap();
        assert_eq!(offsets, vec![0, far]);
    }

    #[test]
    fn progressive_transcode_source_is_not_byte_seekable() {
        use symphonia::core::io::MediaSource;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(!request.contains("\r\nrange:"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n0123456789")
                .unwrap();
        });
        let session = uuid::Uuid::new_v4().to_string();
        let uri = crate::quality::transcode_url(
            &url::Url::parse(&format!("http://{address}/")).unwrap(),
            "1",
            128,
            &session,
        )
        .unwrap();
        let (reader, mut source) =
            Reader::new(uri.into(), String::new(), SessionCloser::new().unwrap()).unwrap();
        assert!(!source.is_seekable());
        let mut head = [0; 4];
        source.read_exact(&mut head).unwrap();
        assert_eq!(&head, b"0123");
        assert!(source.seek(SeekFrom::Start(0)).is_err());
        assert!(source.seek(SeekFrom::Start(1_000_000)).is_err());
        let mut rest = Vec::new();
        source.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"456789");
        drop(reader);
        server.join().unwrap();
    }

    /// Waits for the reader's reported error, as the decoder would see it.
    fn read_error(source: &mut Source, timeout: Duration) -> bool {
        let (sent, received) = mpsc::channel();
        let mut probe = Source {
            buffer: source.buffer.clone(),
            request: source.request.clone(),
            position: 0,
            progressive: false,
        };
        thread::spawn(move || {
            let _ = sent.send(probe.read(&mut [0; 1]).is_err());
        });
        received.recv_timeout(timeout).unwrap_or(false)
    }

    #[test]
    fn unreachable_source_reports_error_after_bounded_retries() {
        // Nothing listens on this port: every attempt is refused.
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let (reader, mut source) = reader(address);
        let started = std::time::Instant::now();
        // Backoff before the error is 1s + 2s + 4s.
        assert!(
            read_error(&mut source, Duration::from_secs(12)),
            "unproven source retried forever"
        );
        assert!(started.elapsed() >= Duration::from_secs(6));
        drop(reader);
    }

    #[test]
    fn failing_first_response_reports_error_after_bounded_retries() {
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
                read_request(&mut stream);
                requests += 1;
                let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
            requests
        });
        let (reader, mut source) = reader(address);
        assert!(
            read_error(&mut source, Duration::from_secs(12)),
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
