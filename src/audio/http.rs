//! Authenticated, redirect-confined HTTP byte transport. No credentials enter Gst URIs.
use gstreamer as gst;
use gstreamer_app as app;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use tokio::sync::watch;

#[derive(Clone)]
pub(super) struct SessionCloser(Arc<CloseWorker>);
struct CloseWorker {
    jobs: Option<mpsc::SyncSender<(url::Url, String)>>,
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl SessionCloser {
    pub(super) fn new() -> crate::Result<Self> {
        let (jobs, receiver) = mpsc::sync_channel::<(url::Url, String)>(64);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let worker = thread::Builder::new()
            .name("plexfreq-transcode-close".into())
            .spawn(move || {
                let Ok(client) = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(2))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                else {
                    return;
                };
                let mut shutdown_jobs = 0;
                while let Ok((uri, token)) = receiver.recv() {
                    if stop.load(Ordering::SeqCst) {
                        if shutdown_jobs >= 2 {
                            break;
                        }
                        shutdown_jobs += 1;
                    }
                    let _ = client.get(uri).header("X-Plex-Token", token).send();
                }
            })?;
        Ok(Self(Arc::new(CloseWorker {
            jobs: Some(jobs),
            stopping,
            worker: Some(worker),
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
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Representation {
    validator: Option<String>,
    total: Option<u64>,
    started: bool,
}
fn retryable(representation: &Representation, offset: u64, progressive: bool) -> bool {
    // A temporary outage is not decoder failure: keep queued bytes and wait
    // for the same representation until seek/stop cancels this reader.
    !progressive && (offset == 0 || representation.validator.is_some())
}
fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs(1 << failures.saturating_sub(1).min(3))
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
                .and_then(crate::cache::parse_range)?;
            if start != offset || length.is_some_and(|n| n != end - start + 1) {
                return None;
            }
            (Some(total), Some(end - start + 1))
        } else {
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
            self.total = total;
            self.started = true;
        }
        Some((if partial { 0 } else { offset }, total, expected))
    }
}

pub struct Reader {
    stop: watch::Sender<Option<u64>>,
    worker: Option<thread::JoinHandle<()>>,
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
        let (stop, mut changed) = watch::channel(Some(0u64));
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
        let received_bytes = Arc::new(AtomicU64::new(0));
        let progress = received_bytes.clone();
        let worker=thread::Builder::new().name("plexfreq-stream".into()).spawn(move|| {
            let stop_uri=url::Url::parse(&uri).ok().and_then(|url|crate::quality::stop_url(&url));
            let stop_token=token.clone();
            let progressive=stop_uri.is_some();
            let runtime=match tokio::runtime::Builder::new_current_thread().enable_all().build(){Ok(runtime)=>runtime,Err(_)=>{gst::element_error!(output,gst::ResourceError::Failed,["Audio streaming failed"]);return;}};
            runtime.block_on(async move {
                // Redirects are disabled deliberately: the Plex token travels as
                // a header, and an open redirect could leak it to an arbitrary
                // origin. Plex serves media directly; a 3xx surfaces as a
                // streaming failure instead of silently following.
                let client=match reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).read_timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build(){Ok(client)=>client,Err(_)=>{gst::element_error!(output,gst::ResourceError::Failed,["Audio streaming failed"]);return;}};
                let mut representation = Representation::default();
                let mut retry_offset = None;
                let mut failures: u32 = 0;
                'requests: loop {
                    if retry_offset.is_some() {
                        tokio::select! {
                            _ = changed.changed() => { retry_offset = None; failures = 0; continue; },
                            _ = tokio::time::sleep(retry_delay(failures)) => {},
                        }
                    } else {
                        failures = 0;
                    }
                    let Some(offset)=retry_offset.take().or(*changed.borrow_and_update()) else{return;};
                    crate::diagnostics::event(format_args!("HTTP audio request offset={offset} progressive={progressive} retry={failures}"));
                    let mut request_builder=client.get(&uri).header("Accept-Encoding", "identity");
                    if !progressive {request_builder=request_builder.header("Range",format!("bytes={offset}-"));}
                    if let Some(validator) = representation.validator.as_deref() {
                        request_builder = request_builder.header("If-Range", validator);
                    }
                    if !token.is_empty() {
                        request_builder=request_builder.header("X-Plex-Token",&token);
                    }
                    let request=request_builder;
                    let response=tokio::select!{_=changed.changed()=>continue,response=request.send()=>response};
                    let mut response=match response {
                        Ok(r) if r.status().is_success()=>r,
                        response if (response.is_err() || response.as_ref().is_ok_and(|r| r.status().is_server_error())) && retryable(&representation, offset, progressive) => {
                            failures = failures.saturating_add(1);
                            retry_offset = Some(offset);
                            crate::diagnostics::event(format_args!("audio stream interrupted; retry={failures} delay_ms={}", retry_delay(failures).as_millis()));
                            continue;
                        },
                        _=>{gst::element_error!(output,gst::ResourceError::Read,["Audio streaming failed"]);return;}
                    };
                    let Some((mut skip, total, expected)) = representation.layout(response.status(), response.headers(), offset) else {gst::element_error!(output,gst::ResourceError::Read,["Invalid audio representation"]);return;};
                    crate::diagnostics::event(format_args!("HTTP audio response status={} total={total:?} expected={expected:?} validator={}", response.status().as_u16(), representation.validator.is_some()));
                    if let Some(total)=total{output.set_size(total.min(i64::MAX as u64) as i64);}
                    let mut interrupted=false;let mut received=0u64;let mut delivered=0u64;
                    loop {
                        let chunk=tokio::select!{_=changed.changed()=>{interrupted=true;break;},chunk=response.chunk()=>chunk};
                        match chunk {
                            Ok(Some(bytes))=>{
                                received=received.saturating_add(bytes.len() as u64);
                                progress.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                                if expected.is_some_and(|n|received>n){gst::element_error!(output,gst::ResourceError::Read,["Invalid audio representation"]);return;}
                                let ignore=skip.min(bytes.len() as u64) as usize;skip-=ignore as u64;if ignore==bytes.len(){continue;}
                                 let chunk_size = if output.max_bytes() == 0 {64 * 1024} else {output.max_bytes().min(64 * 1024) as usize};
                                 for chunk in bytes[ignore..].chunks(chunk_size){
                                     while output.max_bytes() > 0 && output.current_level_bytes().saturating_add(chunk.len() as u64) > output.max_bytes() {
                                         tokio::select! {
                                             _ = changed.changed() => { continue 'requests; },
                                             _ = tokio::time::sleep(Duration::from_millis(5)) => {},
                                         }
                                     }
                                     if changed.has_changed().unwrap_or(true) { continue 'requests; }
                                     if output.push_buffer(gst::Buffer::from_mut_slice(chunk.to_vec())).is_err(){return;}
                                     delivered+=chunk.len() as u64;
                                     failures=0;
                                 }
                            },
                             Ok(None) if skip == 0 && expected.is_none_or(|n|received==n)=>{crate::diagnostics::event(format_args!("HTTP audio EOF received={received} delivered={delivered}"));let _=output.end_of_stream();break;},
                            _=>{
                                let resume = offset + delivered;
                                if retryable(&representation, resume, progressive) {
                                    failures = failures.saturating_add(1);
                                    retry_offset = Some(resume);
                                    crate::diagnostics::event(format_args!("audio stream interrupted; retry={failures} delay_ms={}", retry_delay(failures).as_millis()));
                                    continue 'requests;
                                }
                                gst::element_error!(output,gst::ResourceError::Read,["Audio streaming failed"]);return;
                            },
                        }
                    }
                    if !interrupted && changed.changed().await.is_err(){return;}
                }
            });
            // The reader owns the server session, including predecoded successors.
            // Normal finish/cancel/error and decoder teardown all take this path.
            if let Some(stop_uri)=stop_uri {
                closer.close(stop_uri,stop_token);
            }
        })?;
        Ok(Self {
            stop,
            worker: Some(worker),
            input: source.clone(),
            received: received_bytes,
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
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gst::prelude::*;
    use reqwest::{header::HeaderMap, StatusCode};

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
        assert!(retryable(&representation, 0, false));
        assert!(!retryable(&representation, 1, false));
        representation.validator = Some("\"fixture\"".into());
        assert!(retryable(&representation, 1, false));
        assert!(!retryable(&representation, 1, true));
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
