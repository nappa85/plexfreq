//! Authenticated, redirect-confined HTTP byte transport. No credentials enter Gst URIs.
use gstreamer as gst;
use gstreamer_app as app;
use std::{thread, time::Duration};
use tokio::sync::watch;

pub struct Reader {
    stop: watch::Sender<Option<u64>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Reader {
    pub fn new(source: &app::AppSrc, uri: String, token: String) -> crate::Result<Self> {
        let (stop, mut changed) = watch::channel(Some(0u64));
        let seek = stop.clone();
        source.set_callbacks(
            app::AppSrcCallbacks::builder()
                .seek_data(move |_, offset| seek.send(Some(offset)).is_ok())
                .build(),
        );
        let output = source.clone();
        let worker=thread::Builder::new().name("plexfreq-stream".into()).spawn(move|| {
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async move {
                let client=match reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).read_timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build(){Ok(client)=>client,Err(_)=>return};
                loop {
                    let Some(offset)=*changed.borrow_and_update() else{return;};
                    let request=client.get(&uri).header("X-Plex-Token",&token).header("Range",format!("bytes={offset}-"));
                    let response=tokio::select!{_=changed.changed()=>continue,response=request.send()=>response};
                    let mut response=match response {Ok(r) if r.status().is_success()=>r,_=>{gst::element_error!(output,gst::ResourceError::Read,["Audio streaming failed"]);return;}};
                    let partial=response.status()==reqwest::StatusCode::PARTIAL_CONTENT;
                    if response.headers().get("content-encoding").and_then(|v|v.to_str().ok()).is_some_and(|v|v!="identity"){gst::element_error!(output,gst::ResourceError::Read,["Invalid audio representation"]);return;}
                    if partial {
                        let start=response.headers().get("content-range").and_then(|v|v.to_str().ok()).and_then(|v|v.strip_prefix("bytes ")).and_then(|v|v.split_once('-')).and_then(|(start,_)|start.parse::<u64>().ok());
                        if start!=Some(offset){gst::element_error!(output,gst::ResourceError::Read,["Invalid audio representation"]);return;}
                    }
                    let total=if partial {response.headers().get("content-range").and_then(|v|v.to_str().ok()).and_then(|v|v.rsplit_once('/')).and_then(|(_,total)|total.parse::<u64>().ok())}else{response.content_length()};
                    if let Some(total)=total{output.set_size(total.min(i64::MAX as u64) as i64);}
                    let mut skip=if partial{0}else{offset};let mut interrupted=false;
                    loop {
                        let chunk=tokio::select!{_=changed.changed()=>{interrupted=true;break;},chunk=response.chunk()=>chunk};
                        match chunk {
                            Ok(Some(bytes))=>{
                                let ignore=skip.min(bytes.len() as u64) as usize;skip-=ignore as u64;if ignore==bytes.len(){continue;}
                                for chunk in bytes[ignore..].chunks(64*1024){if output.push_buffer(gst::Buffer::from_mut_slice(chunk.to_vec())).is_err(){return;}}
                            },
                            Ok(None)=>{let _=output.end_of_stream();break;},
                            Err(_)=>{gst::element_error!(output,gst::ResourceError::Read,["Audio streaming failed"]);return;},
                        }
                    }
                    if !interrupted && changed.changed().await.is_err(){return;}
                }
            });
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
