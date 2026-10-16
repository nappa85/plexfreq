//! The single source of truth for which Plex originals PlexFreq decodes
//! itself. Everything else is transcoded by the server (streaming) or
//! downloaded as a server transcode (offline cache).
use crate::model::Item;
use symphonia::core::codecs::audio::{well_known as codec, AudioCodecId};

/// One decodable Plex (container, audioCodec) family and the Symphonia codec
/// that implements it. Plex reports lowercase identifiers; comparison is
/// case-insensitive. Containers map to compiled Symphonia demuxers:
/// mp3 → MpaReader, flac → FlacReader, mp4/m4a/m4b → IsoMp4Reader,
/// aac → AdtsReader, ogg → OggReader, wav → WavReader.
pub(crate) struct Format {
    pub containers: &'static [&'static str],
    pub codecs: &'static [&'static str],
    /// Checked against the compiled registry by the tests below.
    #[cfg_attr(not(test), allow(dead_code))]
    pub decoder: AudioCodecId,
}

pub(crate) const FORMATS: &[Format] = &[
    Format {
        containers: &["mp3"],
        codecs: &["mp3"],
        decoder: codec::CODEC_ID_MP3,
    },
    Format {
        containers: &["flac"],
        codecs: &["flac"],
        decoder: codec::CODEC_ID_FLAC,
    },
    Format {
        containers: &["mp4", "m4a", "m4b", "aac"],
        codecs: &["aac"],
        decoder: codec::CODEC_ID_AAC,
    },
    Format {
        containers: &["mp4", "m4a", "m4b"],
        codecs: &["alac"],
        decoder: codec::CODEC_ID_ALAC,
    },
    Format {
        containers: &["ogg"],
        codecs: &["vorbis"],
        decoder: codec::CODEC_ID_VORBIS,
    },
    Format {
        containers: &["wav"],
        codecs: &["pcm", "pcm_s16le"],
        decoder: codec::CODEC_ID_PCM_S16LE,
    },
    Format {
        containers: &["wav"],
        codecs: &["pcm_s24le"],
        decoder: codec::CODEC_ID_PCM_S24LE,
    },
    Format {
        containers: &["wav"],
        codecs: &["pcm_s32le"],
        decoder: codec::CODEC_ID_PCM_S32LE,
    },
    Format {
        containers: &["wav"],
        codecs: &["pcm_f32le"],
        decoder: codec::CODEC_ID_PCM_F32LE,
    },
    Format {
        containers: &["wav"],
        codecs: &["pcm_u8"],
        decoder: codec::CODEC_ID_PCM_U8,
    },
];

/// Whether the engine can decode this (container, codec) pair. Unknown or
/// empty values are not guessed: they are transcoded.
pub(crate) fn supported(container: &str, audio_codec: &str) -> bool {
    let (container, audio_codec) = (container.trim(), audio_codec.trim());
    !container.is_empty()
        && !audio_codec.is_empty()
        && FORMATS.iter().any(|format| {
            format
                .containers
                .iter()
                .any(|c| c.eq_ignore_ascii_case(container))
                && format
                    .codecs
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(audio_codec))
        })
}

/// Whether the track's first media (the one PlexFreq plays) is decodable.
/// A track without media metadata is transcoded rather than tried directly.
pub(crate) fn item_supported(item: &Item) -> bool {
    item.media
        .first()
        .is_some_and(|m| supported(&m.container, &m.audio_codec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_entry_has_a_compiled_decoder_and_others_do_not() {
        let registry = symphonia::default::get_codecs();
        for format in FORMATS {
            assert!(
                registry.get_audio_decoder(format.decoder).is_some(),
                "{:?} has no compiled decoder",
                format.codecs
            );
        }
        // Formats Plex libraries commonly hold that this build cannot decode
        // must route to a server transcode.
        for id in [codec::CODEC_ID_OPUS, codec::CODEC_ID_WMA] {
            assert!(registry.get_audio_decoder(id).is_none());
        }
        for (container, audio_codec) in [
            ("ogg", "opus"),
            ("asf", "wmav2"),
            ("wma", "wma"),
            ("mka", "flac"),
            ("aiff", "pcm"),
            ("ape", "ape"),
            ("dsf", "dsd_lsbf_planar"),
            ("", "mp3"),
            ("mp3", ""),
            ("", ""),
        ] {
            assert!(
                !supported(container, audio_codec),
                "{container}/{audio_codec}"
            );
        }
        for (container, audio_codec) in [
            ("mp3", "mp3"),
            ("FLAC", "flac"),
            ("mp4", "aac"),
            ("m4a", "alac"),
            ("aac", "aac"),
            ("ogg", "vorbis"),
            ("wav", "pcm"),
        ] {
            assert!(
                supported(container, audio_codec),
                "{container}/{audio_codec}"
            );
        }
    }

    #[test]
    fn items_without_media_or_container_are_transcoded() {
        let mut item: Item = serde_json::from_value(serde_json::json!({
            "type": "track", "ratingKey": "1",
            "Media": [{"audioCodec": "flac", "container": "flac"}]
        }))
        .unwrap();
        assert!(item_supported(&item));
        item.media[0].container.clear();
        assert!(!item_supported(&item));
        item.media.clear();
        assert!(!item_supported(&item));
    }
}
