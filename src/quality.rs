//! Progressive music transcoding; credentials remain HTTP headers at the reader.
use crate::{plex::server_path, Error, Result};
use serde::{Deserialize, Serialize};
use url::Url;

pub(crate) const BITRATES: &[u32] = &[64, 96, 128, 160, 192, 256, 320];

#[derive(Clone, Deserialize, Serialize, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub wifi_kbps: u32,
    pub mobile_kbps: u32,
    pub download_kbps: u32,
    pub codec_fallback: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            wifi_kbps: 0,
            // A conservative default for metered, often unstable links.
            mobile_kbps: 320,
            download_kbps: 0,
            codec_fallback: true,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if [self.wifi_kbps, self.mobile_kbps, self.download_kbps]
            .iter()
            .any(|n| *n != 0 && !BITRATES.contains(n))
        {
            return Err(Error::Input("Choose original audio or 64–320 kbps"));
        }
        Ok(())
    }
    /// The live streaming cap: the mobile choice only on a known cellular
    /// link; Wi-Fi, Ethernet and an unknown network use the Wi-Fi choice.
    pub fn streaming(&self, cellular: bool) -> u32 {
        if cellular {
            self.mobile_kbps
        } else {
            self.wifi_kbps
        }
    }
}
pub fn transcode_url(base: &Url, key: &str, kbps: u32, session: &str) -> Result<Url> {
    crate::plex::valid_id(key)?;
    if !BITRATES.contains(&kbps) || uuid::Uuid::parse_str(session).is_err() {
        return Err(Error::Input("Invalid audio source"));
    }
    // Plex's Kodi audioobject uses start.m3u8 + protocol=http for a progressive
    // MP3 body. This is not an HLS URL handed to a native network plugin.
    let mut url = server_path(base, "/music/:/transcode/universal/start.m3u8")?;
    url.query_pairs_mut().extend_pairs([
        ("path", format!("/library/metadata/{key}")),
        ("protocol", "http".into()),
        ("mediaIndex", "0".into()),
        ("partIndex", "0".into()),
        ("directPlay", "0".into()),
        ("directStream", "0".into()),
        ("audioCodec", "mp3".into()),
        ("musicBitrate", kbps.to_string()),
        ("maxAudioBitrate", kbps.to_string()),
        ("minAudioBitrate", kbps.to_string()),
        ("directStreamAudio", "0".into()),
        ("hasMDE", "1".into()),
        ("X-Plex-Client-Profile-Name", "Chrome".into()),
        ("X-Plex-Client-Profile-Extra",format!("add-transcode-target(type=musicProfile&context=streaming&protocol=http&container=mp3&audioCodec=mp3)+add-limitation(scope=musicCodec&scopeName=mp3&type=upperBound&name=audio.bitrate&value={kbps}&replace=true)")),
        ("session", session.into()),
        ("offset", "0".into()),
        ("X-Plex-Platform", "Linux".into()),
        ("X-Plex-Product", "PlexFreq".into()),
    ]);
    Ok(url)
}
pub(crate) fn is_transcode(url: &Url) -> bool {
    url.path() == "/music/:/transcode/universal/start.m3u8"
}
pub(crate) fn stop_url(url: &Url) -> Option<Url> {
    if !is_transcode(url) {
        return None;
    }
    let session = url
        .query_pairs()
        .find(|(k, _)| k == "session")?
        .1
        .into_owned();
    uuid::Uuid::parse_str(&session).ok()?;
    let mut stop = url.clone();
    // OpenPHT's StopTranscodeSession uses the universal video stop route;
    // the supplied session identifies our music transcoder instance.
    stop.set_path("/video/:/transcode/universal/stop");
    stop.set_query(None);
    stop.query_pairs_mut().append_pair("session", &session);
    Some(stop)
}
/// A Plex transcode `url` restarted at `milliseconds`: the session and
/// offset parameters are replaced, the others keep their order.
pub(crate) fn seek(url: &Url, milliseconds: u64) -> Url {
    let mut url = url.clone();
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(k, _)| k != "offset" && k != "session")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        // Teardown is asynchronous. A seek must not reuse the session
        // whose stop request may still be in the cleanup worker's queue.
        .append_pair("session", &uuid::Uuid::new_v4().to_string())
        .append_pair("offset", &format!("{:.3}", milliseconds as f64 / 1000.));
    url
}
#[cfg(test)]
mod tests {
    use super::seek as seek_to;
    use super::*;
    #[test]
    fn progressive_seek_uses_time_and_cleanup_never_contains_credentials() {
        let session = uuid::Uuid::new_v4().to_string();
        let uri = transcode_url(
            &Url::parse("https://fixture.invalid/").unwrap(),
            "1",
            160,
            &session,
        )
        .unwrap();
        assert!(is_transcode(&uri));
        let seek = seek_to(&uri, 12345);
        assert!(seek
            .query_pairs()
            .any(|(k, v)| k == "offset" && v == "12.345"));
        let stop = stop_url(&seek).unwrap();
        let next_stop = stop_url(&seek_to(&uri, 20000)).unwrap();
        assert_ne!(
            stop.query(),
            next_stop.query(),
            "a late stop for the old decoder must not kill the new seek session"
        );
        assert_eq!(stop.origin(), uri.origin());
        assert_eq!(stop.query_pairs().count(), 1);
        assert!(!stop.as_str().contains("Token"));
    }
}
