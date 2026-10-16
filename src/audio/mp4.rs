//! ISO/MP4 gapless information. Symphonia's MP4 demuxer parses but does not
//! apply edit lists, so AAC priming/padding would otherwise reach the output
//! and break gapless joins. [`Tap`] observes the bytes the demuxer already
//! reads (no extra request on HTTP sources) and keeps a copy of `moov`;
//! [`gapless`] derives the valid sample range from iTunSMPB or the edit list.
use std::{
    io,
    sync::{Arc, Mutex},
};

/// `moov` boxes larger than this are not copied (the track then plays
/// untrimmed, as before gapless support).
const MAX_MOOV: u64 = 16 * 1024 * 1024;

/// Encoder delay and valid length, in samples of the track's sample rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Gapless {
    pub delay: u64,
    pub valid: Option<u64>,
}

#[derive(Default)]
struct Scan {
    /// Absolute offset of the next top-level box header, while scanning.
    next: Option<u64>,
    header: Vec<u8>,
    /// Start and length of `moov` while it is being copied.
    moov: Option<(u64, u64)>,
    first: bool,
}

/// MediaSource wrapper recording `moov` as the demuxer reads it.
pub(super) struct Tap<S> {
    inner: S,
    position: u64,
    scan: Scan,
    captured: Arc<Mutex<Option<Vec<u8>>>>,
    copy: Vec<u8>,
}
impl<S> Tap<S> {
    pub fn new(inner: S) -> (Self, Arc<Mutex<Option<Vec<u8>>>>) {
        let captured = Arc::new(Mutex::new(None));
        (
            Self {
                inner,
                position: 0,
                scan: Scan {
                    next: Some(0),
                    first: true,
                    ..Default::default()
                },
                captured: captured.clone(),
                copy: Vec::new(),
            },
            captured,
        )
    }
    fn observe(&mut self, mut offset: u64, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if let Some((start, length)) = self.scan.moov {
                let copied = start + self.copy.len() as u64;
                if offset > copied {
                    // The demuxer skipped part of moov: give up on gapless.
                    self.stop();
                    return;
                }
                let skip = (copied - offset).min(bytes.len() as u64) as usize;
                let take = ((start + length - copied) as usize).min(bytes.len() - skip);
                self.copy.extend_from_slice(&bytes[skip..skip + take]);
                if self.copy.len() as u64 == length {
                    *crate::mutex_lock(&self.captured) = Some(std::mem::take(&mut self.copy));
                    self.stop();
                }
                return;
            }
            let Some(next) = self.scan.next else {
                return;
            };
            let have = next + self.scan.header.len() as u64;
            if offset + (bytes.len() as u64) <= have {
                return;
            }
            if offset > have {
                // Header bytes were never read (the demuxer seeked past).
                self.stop();
                return;
            }
            let skip = (have - offset) as usize;
            // Read the 8-byte header first; the 64-bit size only if flagged.
            let want = if self.scan.header.len() < 8 { 8 } else { 16 };
            let take = (want - self.scan.header.len()).min(bytes.len() - skip);
            self.scan
                .header
                .extend_from_slice(&bytes[skip..skip + take]);
            offset += (skip + take) as u64;
            bytes = &bytes[skip + take..];
            let header = &self.scan.header;
            if header.len() < 8 {
                continue;
            }
            let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
            let kind = [header[4], header[5], header[6], header[7]];
            let (size, length) = match size32 {
                1 if header.len() < 16 => continue,
                1 => (
                    u64::from_be_bytes(header[8..16].try_into().unwrap_or_default()),
                    16,
                ),
                // Zero (box extends to EOF) is only valid for a final mdat.
                n => (u64::from(n), 8),
            };
            if (self.scan.first && &kind != b"ftyp") || size < length {
                self.stop();
                return;
            }
            self.scan.first = false;
            if &kind == b"moov" {
                if size > MAX_MOOV {
                    self.stop();
                    return;
                }
                // The accumulated header bytes are the first bytes of moov.
                self.copy = std::mem::take(&mut self.scan.header);
                if self.copy.len() as u64 > size {
                    self.copy.truncate(size as usize);
                }
                self.scan.moov = Some((next, size));
                self.scan.next = None;
                if self.copy.len() as u64 == size {
                    *crate::mutex_lock(&self.captured) = Some(std::mem::take(&mut self.copy));
                    self.stop();
                    return;
                }
                continue;
            }
            self.scan.header.clear();
            self.scan.next = next.checked_add(size);
            if self.scan.next.is_none() {
                return;
            }
        }
    }
    fn stop(&mut self) {
        self.scan.next = None;
        self.scan.moov = None;
        self.copy = Vec::new();
    }
}
impl<S: io::Read> io::Read for Tap<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(out)?;
        let offset = self.position;
        self.position += count as u64;
        if self.scan.next.is_some() || self.scan.moov.is_some() {
            self.observe(offset, &out[..count]);
        }
        Ok(count)
    }
}
impl<S: io::Seek> io::Seek for Tap<S> {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        self.position = self.inner.seek(to)?;
        Ok(self.position)
    }
}
impl<S: symphonia::core::io::MediaSource> symphonia::core::io::MediaSource for Tap<S> {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }
    fn byte_len(&self) -> Option<u64> {
        self.inner.byte_len()
    }
}

/// Child boxes of a container payload as (type, payload).
fn boxes(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let size32 = u32::from_be_bytes(data[..4].try_into().ok()?);
        let kind: [u8; 4] = data[4..8].try_into().ok()?;
        let (size, header) = match size32 {
            0 => (data.len() as u64, 8),
            1 => (u64::from_be_bytes(data.get(8..16)?.try_into().ok()?), 16),
            n => (u64::from(n), 8),
        };
        if size < header || size > data.len() as u64 {
            return None;
        }
        let (item, rest) = data.split_at(size as usize);
        data = rest;
        Some((kind, &item[header as usize..]))
    })
}
fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data)
        .find(|(k, _)| k == kind)
        .map(|(_, payload)| payload)
}
fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}
fn be_u64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}
/// (timescale, duration) of an mvhd or mdhd full box.
fn header_times(data: &[u8]) -> Option<(u32, u64)> {
    match data.first()? {
        0 => Some((be_u32(data, 12)?, u64::from(be_u32(data, 16)?))),
        1 => Some((be_u32(data, 20)?, be_u64(data, 24)?)),
        _ => None,
    }
}
/// First non-empty edit: (segment duration in movie units, media time).
fn first_edit(elst: &[u8]) -> Option<(u64, u64)> {
    let version = *elst.first()?;
    let count = be_u32(elst, 4)? as usize;
    let size = if version == 1 { 20 } else { 12 };
    (0..count.min(16)).find_map(|n| {
        let at = 8 + n * size;
        let (duration, time) = if version == 1 {
            (be_u64(elst, at)?, be_u64(elst, at + 8)? as i64)
        } else {
            (
                u64::from(be_u32(elst, at)?),
                i64::from(be_u32(elst, at + 4)? as i32),
            )
        };
        u64::try_from(time).ok().map(|time| (duration, time))
    })
}
/// iTunes `----:com.apple.iTunes:iTunSMPB`: (delay, padding, valid samples).
fn itunsmpb(moov: &[u8]) -> Option<(u64, u64)> {
    let meta = child(child(moov, b"udta")?, b"meta")?;
    // ISO meta is a full box; QuickTime-style meta is not.
    let meta = if be_u32(meta, 0)? == 0 {
        &meta[4..]
    } else {
        meta
    };
    boxes(child(meta, b"ilst")?)
        .filter(|(kind, _)| kind == b"----")
        .find_map(|(_, tag)| {
            let name = child(tag, b"name")?;
            if name.get(4..)? != b"iTunSMPB" {
                return None;
            }
            let text = std::str::from_utf8(child(tag, b"data")?.get(8..)?).ok()?;
            let fields: Vec<_> = text.split_whitespace().collect();
            let delay = u64::from_str_radix(fields.get(1)?, 16).ok()?;
            let valid = u64::from_str_radix(fields.get(3)?, 16).ok()?;
            Some((delay, valid))
        })
}

/// Gapless trim of the first sound track, in samples at `rate`.
pub(super) fn gapless(moov: &[u8], rate: u32) -> Option<Gapless> {
    let moov = moov.get(8..).filter(|_| moov.get(4..8) == Some(b"moov"))?;
    if let Some((delay, valid)) = itunsmpb(moov).filter(|(_, valid)| *valid > 0) {
        return Some(Gapless {
            delay,
            valid: Some(valid),
        });
    }
    let (movie_scale, _) = header_times(child(moov, b"mvhd")?)?;
    let trak = boxes(moov)
        .filter(|(k, _)| k == b"trak")
        .find(|(_, trak)| {
            child(trak, b"mdia")
                .and_then(|mdia| child(mdia, b"hdlr"))
                .and_then(|hdlr| hdlr.get(8..12))
                == Some(b"soun")
        })?;
    let trak = trak.1;
    let (media_scale, media_duration) = header_times(child(child(trak, b"mdia")?, b"mdhd")?)?;
    let (segment, media_time) = first_edit(child(child(trak, b"edts")?, b"elst")?)?;
    if movie_scale == 0 || media_scale == 0 || rate == 0 {
        return None;
    }
    let to_rate = |value: u64, scale: u32| {
        (u128::from(value) * u128::from(rate) / u128::from(scale)).min(u128::from(u64::MAX)) as u64
    };
    let delay = to_rate(media_time, media_scale);
    // The edit duration uses the (often millisecond) movie timescale. The
    // media duration minus the delay is exact when it agrees within that
    // rounding; otherwise trust the edit.
    let edit = (u128::from(segment) * u128::from(media_scale) / u128::from(movie_scale)) as u64;
    let tolerance = u64::from(media_scale / movie_scale.max(1)) + 2;
    let remaining = media_duration.saturating_sub(media_time);
    let valid = if segment == 0 || remaining.abs_diff(edit) <= tolerance {
        remaining
    } else {
        edit.min(remaining)
    };
    Some(Gapless {
        delay,
        valid: Some(to_rate(valid, media_scale)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }
    fn full(version: u8, rest: &[u8]) -> Vec<u8> {
        let mut out = vec![version, 0, 0, 0];
        out.extend_from_slice(rest);
        out
    }
    fn times(scale: u32, duration: u32) -> Vec<u8> {
        let mut out = vec![0; 8];
        out.extend_from_slice(&scale.to_be_bytes());
        out.extend_from_slice(&duration.to_be_bytes());
        out
    }
    fn moov(edit: Option<(u32, i32)>, smpb: Option<&str>) -> Vec<u8> {
        let mut hdlr = vec![0; 4];
        hdlr.extend_from_slice(b"soun");
        let mut mdia = mp4_box(b"mdhd", &full(0, &times(48000, 49024)));
        mdia.extend(mp4_box(b"hdlr", &full(0, &hdlr)));
        let mut trak = mp4_box(b"tkhd", &full(0, &[0; 80]));
        if let Some((duration, time)) = edit {
            let mut elst = 1u32.to_be_bytes().to_vec();
            elst.extend_from_slice(&duration.to_be_bytes());
            elst.extend_from_slice(&time.to_be_bytes());
            elst.extend_from_slice(&[0, 1, 0, 0]);
            trak.extend(mp4_box(b"edts", &mp4_box(b"elst", &full(0, &elst))));
        }
        trak.extend(mp4_box(b"mdia", &mdia));
        let mut body = mp4_box(b"mvhd", &full(0, &times(1000, 1000)));
        body.extend(mp4_box(b"trak", &trak));
        if let Some(text) = smpb {
            let mut tag = mp4_box(b"mean", &full(0, b"com.apple.iTunes"));
            tag.extend(mp4_box(b"name", &full(0, b"iTunSMPB")));
            let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
            data.extend_from_slice(text.as_bytes());
            tag.extend(mp4_box(b"data", &data));
            let ilst = mp4_box(b"ilst", &mp4_box(b"----", &tag));
            body.extend(mp4_box(b"udta", &mp4_box(b"meta", &full(0, &ilst))));
        }
        mp4_box(b"moov", &body)
    }

    #[test]
    fn edit_list_and_itunsmpb_define_the_valid_sample_range() {
        assert_eq!(
            gapless(&moov(Some((1000, 1024)), None), 48000),
            Some(Gapless {
                delay: 1024,
                valid: Some(48000)
            })
        );
        // iTunSMPB is exact and takes precedence over the edit list.
        let smpb = " 00000000 00000840 000001C0 000000000000BB80 00000000";
        assert_eq!(
            gapless(&moov(Some((1000, 1024)), Some(smpb)), 48000),
            Some(Gapless {
                delay: 0x840,
                valid: Some(48000)
            })
        );
        assert_eq!(gapless(&moov(None, None), 48000), None);
    }

    #[test]
    fn tap_copies_moov_after_a_skipped_mdat_without_extra_reads() {
        let mut file = mp4_box(b"ftyp", b"M4A \0\0\0\0");
        file.extend(mp4_box(b"free", b""));
        file.extend(mp4_box(b"mdat", &vec![7; 100_000]));
        let movie = moov(Some((1000, 1024)), None);
        let moov_at = file.len() as u64;
        file.extend(&movie);
        let (mut tap, captured) = Tap::new(std::io::Cursor::new(file));
        // Demuxer-like access: headers, then a seek over the media data.
        let mut header = [0; 8];
        tap.read_exact(&mut header).unwrap();
        tap.seek(SeekFrom::Start(16)).unwrap();
        tap.read_exact(&mut header).unwrap();
        tap.read_exact(&mut header).unwrap();
        tap.seek(SeekFrom::Start(moov_at)).unwrap();
        let mut rest = Vec::new();
        for _ in 0..movie.len() / 7 + 1 {
            let mut chunk = [0; 7];
            let n = tap.read(&mut chunk).unwrap();
            rest.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(captured.lock().unwrap().as_deref(), Some(&movie[..]));
        // Not an MP4: nothing is captured.
        let (mut tap, captured) = Tap::new(std::io::Cursor::new(b"ID3\x04\0\0\0\0\0\0".to_vec()));
        tap.read_to_end(&mut Vec::new()).unwrap();
        assert!(captured.lock().unwrap().is_none());
    }
}
