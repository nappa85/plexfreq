//! Progressive music transcoding; credentials remain HTTP headers at the reader.
use crate::{model::Item, plex::server_path, Error, Result};
use serde::{Deserialize, Serialize};
use url::Url;

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
            mobile_kbps: 0,
            download_kbps: 0,
            codec_fallback: true,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if [self.wifi_kbps, self.mobile_kbps, self.download_kbps]
            .iter()
            .any(|n| ![0, 64, 96, 128, 160, 192, 256, 320].contains(n))
        {
            return Err(Error::Input("Choose original audio or 64–320 kbps"));
        }
        Ok(())
    }
    pub fn streaming(&self, wifi: bool) -> u32 {
        if wifi {
            self.wifi_kbps
        } else {
            self.mobile_kbps
        }
    }
}
pub fn transcode_url(base: &Url, key: &str, kbps: u32, session: &str) -> Result<Url> {
    crate::numeric(key)?;
    if !(64..=320).contains(&kbps) || uuid::Uuid::parse_str(session).is_err() {
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
pub(crate) fn with_offset(uri: &str, milliseconds: u64) -> Result<(String, bool)> {
    let mut url = Url::parse(uri)?;
    let transcode = is_transcode(&url);
    if transcode {
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
    }
    Ok((url.into(), transcode))
}
pub(crate) fn codec_supported(item: &Item) -> bool {
    // Unknown codec is tried directly; decode failure has one bounded fallback.
    item.media.first().is_none_or(|m| {
        m.audio_codec.is_empty()
            || [
                "mp3",
                "flac",
                "aac",
                "alac",
                "pcm",
                "pcm_s16le",
                "pcm_s24le",
                "pcm_s32le",
                "vorbis",
                "opus",
                "wma",
                "wmav2",
            ]
            .contains(&m.audio_codec.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progressive_seek_uses_time_and_cleanup_never_contains_credentials() {
        let session = uuid::Uuid::new_v4().to_string();
        let mut uri = transcode_url(
            &Url::parse("https://fixture.invalid/").unwrap(),
            "1",
            160,
            &session,
        )
        .unwrap();
        uri.query_pairs_mut().append_pair("X-Plex-Token", "fixture");
        let (seek, transcode) = with_offset(uri.as_str(), 12345).unwrap();
        assert!(transcode);
        let seek = Url::parse(&seek).unwrap();
        assert!(seek
            .query_pairs()
            .any(|(k, v)| k == "offset" && v == "12.345"));
        let stop = stop_url(&seek).unwrap();
        let (next_seek, _) = with_offset(uri.as_str(), 20000).unwrap();
        let next_stop = stop_url(&Url::parse(&next_seek).unwrap()).unwrap();
        assert_ne!(
            stop.query(),
            next_stop.query(),
            "a late stop for the old decoder must not kill the new seek session"
        );
        assert_eq!(stop.origin(), uri.origin());
        assert_eq!(stop.query_pairs().count(), 1);
        assert!(!stop.as_str().contains("fixture&"));
    }
}
