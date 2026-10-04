# Rust audio engine and runtime

The user authorized replacing the old worker/player and minimizing C++. The
implementation now uses Rust-owned request, audio, HTTP, network and MPRIS workers.
The earlier evaluation below remains as rationale; Qt Multimedia is no longer the
application's playback engine.

## Implemented topology

```
QML → small QObject/list-model/translation bridge → opaque Runtime C ABI
  Rust Core actor: Plex, queue/radio, prepared successors, persistence/history
  Rust audio actor: PCM scheduling, transport, position, EQ/gain/crossfade
  Rust HTTP readers: header authentication, byte seeks, cancellation, no redirects
  GStreamer: installed codecs/resampling + appsink/appsrc + PulseAudio music sink
  Rust zbus workers: MPRIS and read-only network observation
```

No QThread or QMediaPlayer is created by the C++ bridge. Qt polls bounded runtime
events/snapshots; it does not perform audio transport, measure heard time, advance
the queue, run maintenance timers or own D-Bus policy. PCM never crosses Qt/JSON.

Decoders convert to 48 kHz stereo F32 PCM. One persistent output stream timestamps
successive buffers by frame count. Prepared successors provide gapless joins;
equal-power crossfades overlap an exact configured frame count (off by default,
0–12 seconds, suppressed within the same identified album). A Rust ten-band
peaking EQ, metadata gain option and sample clamp run on PCM before the sink.
This does not claim bit-perfect sample-rate matching or Plexamp's proprietary fades.

Source HTTP is Rust-owned: the decoder sees an appsrc byte stream rather than a
credential-bearing URI. Requests use X-Plex-Token headers, disable redirects and
validate partial-range start/representation encoding. Local files use native file
decoding. Output uses pulsesink with media.role=music; tests use synchronized fake
or captured PCM sinks without changing system audio services.

The Core HTTP actor may block on metadata without blocking audio controls/output.
It prepares the next queue/radio snapshot in advance and commits it when that
successor becomes audible. Only one successor is decoded ahead, with bounded
buffering; a not-yet-prepared successor after slow/outage conditions can still
require buffering. Codec trims, long sessions, power and routing need wider tests.

## Current boundary

| Layer | Responsibilities |
| --- | --- |
| Rust | Plex authentication/discovery/metadata/mutations, credentials and private storage, normalized models and URLs, queue/shuffle/radio/mix/autoplay policy, download/cache/offline policy, lyrics parsing and listening-history outbox/sync |
| C++ | Thread-confined Core/C-ABI marshalling, QObject and incremental list-model adapters, QMediaPlayer/output and native transport/seek/volume, elapsed-listening measurement, MPRIS, read-only network facts and lifetime-coordinated Rust cancellation hints |
| QML | Platform presentation, navigation/history, input/dialogs and translating/presenting returned data |

Rust produces a playback plan (authenticated remote URL or completed local file).
Qt fetches/decodes uncached streams and presents audio; QML Image may also fetch
uncached artwork URLs. Thus credential policy is Rust-owned, but credential-bearing
framework URLs do cross into Qt. They must not be logged as though redacted.

QMediaPlayer is a battle-tested high-level player. The limiting factor is access
to decoded samples, pre-roll and transition scheduling, not an assumption that
Qt's decoders or platform audio stack are unreliable. On this Sailfish runtime
the Qt adapter already uses GStreamer; the host Qt6 build uses FFmpeg.

## Preferred first evaluation: Rust controls GStreamer directly

- Retain proven native codec/output infrastructure while replacing QMediaPlayer's
  high-level orchestration with a dedicated Rust engine and gstreamer-rs bindings.
- Pre-roll the next decoder, control one continuous output timeline, and expose
  decoded PCM using appsink/appsrc when Rust mixing/DSP needs sample access.
- Rust owns transition timing, queue hand-off, fades, gains, EQ/limiting and
  authoritative playback/sample counters. Native plugins still perform decoding
  and the platform sink still handles output.
- Audit exact phone/SDK GStreamer/GLib versions and plugins before pinning Rust
  bindings. Qt5.6 alone does not tell us which GStreamer API is available.
- Preserve music-role/output properties, pause/seek, call/headset behavior and
  sandbox permissions. Use existing user-session audio infrastructure; no root,
  driver/service changes or privileged helper.

This is a Rust-owned engine, not necessarily a pure-Rust decoder implementation.

## Alternative: pure Rust decode/DSP with a thin native output sink

Symphonia + an explicit resampler (e.g. Rubato) + preallocated PCM mixing/ring
buffers can provide more control. PulseAudio output is a natural Sailfish/Linux
candidate; a Qt PCM sink is another adapter to evaluate. CPAL is an option to
audit, not a guarantee that the selected Linux backend preserves Sailfish routing.

Symphonia 0.6.1 lists MP3/FLAC/PCM/Vorbis/ALAC/AAC-LC, but not Opus; its support
table marks AAC-LC and ISO/MP4 gapless support as absent. A pure-Rust route must
address these coverage/trimming gaps instead of assuming every library format is
supported. Decoding alone does not implement an audio engine.

## Threading and verification requirements

The engine must have a dedicated Rust scheduling/audio domain. The current
blocking Plex Core worker must never run network/disk work in an audio callback.
Use preallocated bounded buffers; keep allocation, blocking locks and metadata
resolution off the real-time path. PCM is not transported as JSON through Qt.

Before switching the default:
1. Prove timestamp/seek/cancellation behavior and exact PCM joins with generated
   fixtures, including different sample rates and encoder delay/padding.
2. Measure gaps and crossfade/gain/EQ output; retaining a pipeline is not itself
   evidence of gapless playback.
3. Validate codec coverage, long lock-screen/background sessions, call/Bluetooth
   routing, latency, power and cache/offline transitions on the device.
4. Keep the existing adapter as a fallback during staged rollout.

Sample-rate matching is also an output-stack constraint: requesting a rate does
not establish bit-perfect delivery through PulseAudio or Bluetooth. DSP likewise
does not reproduce Plexamp's discovery algorithms, mix weighting or Sweet Fades
behavior automatically.

## Consulted sources

- GStreamer appsink/appsrc, buffers/timestamps and application interaction:
  https://gstreamer.freedesktop.org/documentation/tutorials/basic/short-cutting-the-pipeline.html
- Symphonia 0.6.1 format/codec/gapless tables:
  https://docs.rs/symphonia/0.6.1/symphonia/
- Existing project source: `src/lib.rs`, `src/features.rs`, `src/cache.rs`,
  `src/ffi.rs`, `app/src/backend.*` and `app/src/mpris.*`.
