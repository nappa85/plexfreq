//! Authenticated, redirect-confined HTTP byte transport. No credentials enter Gst URIs.
use gstreamer as gst;
use gstreamer_app as app;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
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
impl Representation {
    fn layout(
        &mut self,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
        offset: u64,
    ) -> Option<(u64, Option<u64>, Option<u64>)> {
        if !matches!(status.as_u16(), 200 | 206)
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
                .seek_data(move |_, offset| seek.send(Some(offset)).is_ok())
                .build(),
        );
        let output = source.clone();
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
                loop {
                    let Some(offset)=*changed.borrow_and_update() else{return;};
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
                    let mut response=match response {Ok(r) if r.status().is_success()=>r,_=>{gst::element_error!(output,gst::ResourceError::Read,["Audio streaming failed"]);return;}};
                    let Some((mut skip, total, expected)) = representation.layout(response.status(), response.headers(), offset) else {gst::element_error!(output,gst::ResourceError::Read,["Invalid audio representation"]);return;};
                    if let Some(total)=total{output.set_size(total.min(i64::MAX as u64) as i64);}
                    let mut interrupted=false;let mut received=0u64;
                    loop {
                        let chunk=tokio::select!{_=changed.changed()=>{interrupted=true;break;},chunk=response.chunk()=>chunk};
                        match chunk {
                            Ok(Some(bytes))=>{
                                received=received.saturating_add(bytes.len() as u64);
                                if expected.is_some_and(|n|received>n){gst::element_error!(output,gst::ResourceError::Read,["Invalid audio representation"]);return;}
                                let ignore=skip.min(bytes.len() as u64) as usize;skip-=ignore as u64;if ignore==bytes.len(){continue;}
                                for chunk in bytes[ignore..].chunks(64*1024){if output.push_buffer(gst::Buffer::from_mut_slice(chunk.to_vec())).is_err(){return;}}
                            },
                            Ok(None)=>{if skip>0 || expected.is_some_and(|n|received!=n){gst::element_error!(output,gst::ResourceError::Read,["Invalid audio representation"]);return;}let _=output.end_of_stream();break;},
                            Err(_)=>{gst::element_error!(output,gst::ResourceError::Read,["Audio streaming failed"]);return;},
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
        })
    }
    pub fn stop(&self) {
        let _ = self.stop.send(None);
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
    use reqwest::{header::HeaderMap, StatusCode};

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
