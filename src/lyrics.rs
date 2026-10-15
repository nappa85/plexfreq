use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, PartialEq)]
pub struct Line {
    pub time: Option<u64>,
    pub text: String,
}

/// PMS JSON is a lyric document, not a string to render verbatim.
pub fn decode(text: &str) -> Result<Vec<Line>> {
    let text = text.trim_start_matches('\u{feff}').trim();
    if !matches!(text.chars().next(), Some('{' | '[' | '"')) {
        return Ok(parse(text));
    }
    let value = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => value,
        Err(_) => return Ok(parse(text)),
    };
    if let Some(text) = value
        .as_str()
        .or_else(|| value.get("lyrics").and_then(|v| v.as_str()))
    {
        return Ok(parse(text));
    }
    let documents = value
        .get("MediaContainer")
        .and_then(|v| v.get("Lyrics"))
        .and_then(|v| v.as_array())
        .ok_or(Error::ProtocolAt("lyrics JSON"))?;
    let Some(document) = documents.first() else {
        return Ok(vec![]);
    };
    let raw = document
        .get("Line")
        .and_then(|v| v.as_array())
        .ok_or(Error::ProtocolAt("lyric lines"))?;
    let timed = document
        .get("timed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut lines = Vec::new();
    for line in raw.iter().take(10000) {
        let spans = match line.get("Span") {
            None | Some(serde_json::Value::Null) => &[][..],
            Some(serde_json::Value::Array(spans)) => spans.as_slice(),
            _ => return Err(Error::ProtocolAt("lyric spans")),
        };
        let mut text = String::new();
        for span in spans {
            text.push_str(
                span.get("text")
                    .and_then(|v| v.as_str())
                    .ok_or(Error::ProtocolAt("lyric span text"))?,
            );
        }
        let time = if timed {
            line.get("start").and_then(timestamp).or_else(|| {
                spans
                    .first()
                    .and_then(|s| s.get("start"))
                    .and_then(timestamp)
            })
        } else {
            None
        };
        lines.push(Line { time, text });
    }
    Ok(lines)
}

fn timestamp(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let milliseconds = value.as_f64()?;
        // Round fractional milliseconds like LRC timestamps. The exclusive upper
        // bound avoids saturating a float at 2^64 to a valid-looking timestamp.
        let rounded = milliseconds.round();
        (milliseconds >= 0.0 && rounded.is_finite() && rounded < u64::MAX as f64)
            .then_some(rounded as u64)
    })
}

pub fn normalize_cached(lines: Vec<Line>) -> Result<Vec<Line>> {
    if lines
        .first()
        .is_some_and(|l| l.text.trim_start().starts_with('{'))
    {
        decode(
            &lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        Ok(lines)
    }
}

pub fn parse(text: &str) -> Vec<Line> {
    fn is_metadata_tag(rest: &str) -> bool {
        // LRC metadata like [ar:Artist], [ti:Title], [al:Album], [by:X],
        // [length:03:12] carries a colon inside brackets. Plain bracketed
        // content like [hello] or [Chorus] has no colon and must be kept.
        // Empty brackets are never content.
        if !(rest.starts_with('[') && rest.ends_with(']')) {
            return false;
        }
        let inner = &rest[1..rest.len().saturating_sub(1)];
        if inner.trim().is_empty() {
            return true;
        }
        // Only real LRC ID tags ([ar:..], [length:..], [#:comment]) are
        // metadata: a short lowercase identifier without spaces before the
        // colon. Section annotations like [Verse 1: Artist] are content.
        inner.split_once(':').is_some_and(|(id, _)| {
            (1..=8).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'#')
        })
    }
    // (sort anchor, line): an untimed line inherits the anchor of the closest
    // preceding timed source line so a stable sort keeps it in place.
    let mut lines = Vec::new();
    let mut anchor = 0u64;
    let mut offset = 0i64;
    for raw in text.trim_start_matches('\u{feff}').lines().take(10000) {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        if let Some(value) = rest
            .strip_prefix("[offset:")
            .and_then(|s| s.strip_suffix(']'))
        {
            offset = value.parse().unwrap_or(0);
            continue;
        }
        while let Some(tag) = rest.strip_prefix('[').and_then(|s| s.split_once(']')) {
            let Some((minutes, seconds)) = tag.0.split_once(':') else {
                break;
            };
            let (Ok(minutes), Ok(seconds)) = (minutes.parse::<u64>(), seconds.parse::<f64>())
            else {
                break;
            };
            if !seconds.is_finite() || !(0.0..60.0).contains(&seconds) || minutes > 10000 {
                break;
            }
            times.push(minutes * 60000 + (seconds * 1000.0).round() as u64);
            rest = tag.1.trim_start();
        }
        if let Some(first) = times.first() {
            anchor = *first;
            for time in times {
                lines.push((
                    time,
                    Line {
                        time: Some(time),
                        text: rest.into(),
                    },
                ));
            }
        } else if !rest.is_empty() && !is_metadata_tag(rest) {
            lines.push((
                anchor,
                Line {
                    time: None,
                    text: rest.into(),
                },
            ));
        }
    }
    // Stable: equal anchors keep source order. The uniform offset (clamped
    // at 0) is monotonic, so sorting by the raw time stays correct after it.
    lines.sort_by_key(|(anchor, _)| *anchor);
    lines
        .into_iter()
        .map(|(_, mut line)| {
            // LRC: a positive offset makes lyrics appear sooner.
            line.time = line
                .time
                .map(|time| (time as i64).saturating_sub(offset).max(0) as u64);
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timed_plain_and_multiple_timestamps() {
        let lines =
            parse("[ar:Fixture]\n[offset:-100]\n[00:02.50][00:04.000]Repeated\n[00:01]First");
        assert_eq!(
            lines.iter().map(|l| l.time.unwrap()).collect::<Vec<_>>(),
            vec![1100, 2600, 4100]
        );
        assert_eq!(parse("plain\nlyrics")[1].text, "lyrics");
    }
    #[test]
    fn json_payload_is_not_displayed_as_literal_lyrics() {
        let lines = decode(r#"{"lyrics":"[00:01.00]Fixture line"}"#).unwrap();
        assert_eq!(lines[0].text, "Fixture line");
        assert!(lines.iter().all(|line| !line.text.contains("\"lyrics\"")));
    }
    #[test]
    fn plex_json_spans_preserve_lines_and_repair_old_offline_cache() {
        let payload = r#"{"MediaContainer":{"size":1,"Lyrics":[{"provider":"fixture","timed":false,"Line":[{"Span":[{"text":"First "},{"text":"line"}]},{"Span":[{"text":"Second line"}]},{},{"Span":null}]}]}}"#;
        let lines = decode(payload).unwrap();
        assert_eq!(lines[0].text, "First line");
        assert_eq!(lines[1].text, "Second line");
        assert_eq!(lines[0].time, None);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2].text, "");
        assert_eq!(lines[3].text, "");
        assert_eq!(normalize_cached(parse(payload)).unwrap(), lines);
        assert!(decode(r#"{"error":"unrelated JSON"}"#).is_err());
        assert_eq!(decode("{malformed json").unwrap(), parse("{malformed json"));
        assert!(decode(r#"{"MediaContainer":{"Lyrics":[]}}"#)
            .unwrap()
            .is_empty());
    }
}
