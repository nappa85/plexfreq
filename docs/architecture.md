# Architecture

PlexFreq is a Plex music player for Sailfish OS (Qt 5.6 + Silica) and desktop
Linux (Qt 6 + Controls 2). This document describes how it works today, and lists
the external facts it depends on with their sources. The code is authoritative
wherever this document disagrees with it.

## 1. Overview and responsibilities

```
Silica (Qt5.6) / Controls2 (Qt6) QML            presentation, navigation (Session.qml)
  │  backend.command(op,args) · backend.state/cache/playback · image://plexfreq
C++ adapter (app/src)                            Backend QObject, EntryModel, ArtworkProvider,
  │  C ABI: pf_runtime_submit / pf_runtime_poll  translations, window activation
Rust runtime (src/runtime.rs)                    admission, Core worker, events
  ├─ Core (src/lib.rs + domain modules)          Plex HTTP, queue/radio, cache, offline, store
  ├─ audio engine (src/audio)                    Symphonia decode → rubato → DSP → libpulse
  └─ platform (src/runtime/platform.rs, pulse/)  MPRIS, BlueZ, ConnMan/NM, recording hold
```

**Rust** owns everything with policy or I/O. That covers Plex and media HTTP,
credential persistence and use, models, queue and radio policy, worker
lifetime, audio decoding, DSP and transport, the cache, offline snapshots,
MPRIS, Bluetooth and network facts, persistence and diagnostics.

**C++** is a thin adapter. It turns Rust JSON into Qt properties, keeps two list
models, translates messages, serves images through a provider (the bytes come
from Rust) and raises the window. It has no Plex endpoint, queue order or media
API (no QMediaPlayer, no QThread). The only credential it handles is the token
the user types for a manual connection, which it forwards to Rust (§11).

**QML** handles presentation and navigation only. `app/qml/shared` holds
QtQuick 2.6-compatible code used by both shells. `app/qml/pages` is Silica and
`app/qml/desktop` is Controls 2. Shared QML never imports either toolkit.

### Threads

| Thread | Role |
|---|---|
| GUI | Qt event loop. Polls the runtime every 100 ms; transport controls run synchronously at admission. |
| `plexfreq-core` | Runtime worker. Owns `Core`, handles requests, engine events, health and maintenance ticks. |
| `plexfreq-audio` | Audio actor: controls, pump, DSP. Dispatches the libpulse playback mainloop without blocking. |
| `plexfreq-decode` | One per current or prepared track: Symphonia demux/decode, trim, resample. |
| `plexfreq-rt` | The process-wide tokio current-thread runtime (`src/rt.rs`). |
| `plexfreq-rt-blk` | Tokio blocking pool, at most 8 threads: DNS, cache/artwork filesystem writes, libpulse diagnostic samples. |
| `plexfreq-recording` | libpulse recording subscription, which uses a blocking mainloop. |
| `plexfreq-plan-refresh` | Serialized refresh of download plans against a read-only `Core::inspect`. |
| `plexfreq-log` | Captures stdout/stderr into the dated log (Sailfish only). |

The runtime on `plexfreq-rt` runs the D-Bus observers, MPRIS, the diagnostic
heartbeat, cache and artwork downloads, audio HTTP readers, health probes and
the download-yield task. Nothing blocking may run on it; blocking work goes through `rt::unblock`, and
waiting on a task from the runtime thread itself is refused. Core stays
synchronous: it drives Plex requests with `rt::block_on` on its own thread, so
JSON decoding never runs on the I/O thread. A current-thread runtime needs its
own thread inside `Runtime::block_on` for that to work (tokio
`Handle::block_on` docs). Dropping a `Task` aborts it. zbus uses its `tokio`
feature, so its internal tasks run on the shared runtime.

### Source map

| Path | Content |
|---|---|
| `src/runtime.rs`, `src/runtime/` | Admission, worker loop, held-failure resolution, download yield; MPRIS, BlueZ and network observers, heartbeat, health probes, libpulse introspection and recording subscription. |
| `src/lib.rs`, `src/command.rs` | `Core`: typed commands and routing, playback planning, gone navigation, cache scheduling. |
| `src/plex.rs`, `src/quality.rs` | HTTP client, sign-in, bounded bodies, URL confinement, probes; bitrate policy and transcode URLs. |
| `src/queue.rs`, `src/radio.rs`, `src/health.rs`, `src/history.rs` | Queue and occurrences, radio windows, gone marks, listen qualification. |
| `src/cache.rs`, `src/offline.rs`, `src/artwork.rs` | Audio cache and downloads; metadata snapshots, lyrics and artwork store; `image://plexfreq` ids. |
| `src/daily.rs`, `features.rs`, `discovery.rs`, `filters.rs`, `catalogue.rs`, `refresh.rs` | Discovery, download plans, playlists, mixes, filters, insights. |
| `src/store.rs`, `src/diagnostics.rs` | Session files; log capture and redaction. |
| `src/audio/*` | Audio engine. |
| `src/ffi.rs`, `app/src/` | C ABI and Qt adapter. |
| `app/qml/shared/Session.qml` | Routes, history, paging, sign-in polling, shared presentation helpers. |

## 2. Runtime and command flow

### C ABI

`pf_runtime_new(state_dir)` starts the runtime, `pf_runtime_submit` takes a JSON
request and returns an admission reply, `pf_runtime_poll` returns events,
playback and busy state, and `pf_runtime_free` stops and joins the worker.
Returned strings are freed by Rust, and every entry point runs under
`catch_unwind`, so no panic unwinds into C++. Artwork handles outlive the
runtime; once the worker has exited, their fetches fail.

### Admission

The runtime parses wire JSON into a typed `Command` (`src/command.rs`) once.
Malformed, unknown and `_`-prefixed ops fail synchronously (`accepted:false`);
the C++ adapter also refuses `_` ops before submitting. Every command has a
routing class, assigned by an exhaustive match, so a new op cannot go
unclassified:

| Class | Effect |
|---|---|
| `engine` | Play/pause/stop/toggle/seek/volume run on the calling thread against the audio handle, without entering the worker queue. Toggle follows logical intent: a buffering, held-failure or recording-held track toggles to pause. |
| `listing` | A start of 0 replaces the list and starts a new view generation; a nonzero start appends and counts as loading-more. |
| `view_bound` | Listings and similar-artists. Their Plex requests are cancelled when the view moves on, and the reply is discarded. |
| `passive` | Status, lyrics, timeline, checkpoints and similar. These never raise `busy`. |
| `transport` | Play/next/previous/radio and similar. Counted as pending user actions until the worker dequeues them. |
| `reprepare` | Queue edits, shuffle, repeat, quality and audio config. The gapless successor is prepared again. |
| `resets_session` | Connect, select server and logout. These stop the engine and clear the presented state. |

Requests go to the worker over a bounded channel (256). If it is full, the call
fails with "Backend command queue is unavailable" and nothing is counted as
busy. Position checkpoints from the GUI and MPRIS never block: they are kept
per occurrence, newest first, and applied in order, so an older checkpoint can
never move a saved position backwards.

### Worker

The worker handles one request at a time, interleaved with engine events,
finished health probes and the held-failure state machine. Replies whose view
went stale are discarded, both when they are produced and when Qt polls them.
The artwork source is updated before each reply is sent, so the reply's image
ids resolve.

Maintenance runs every 5 s (position checkpoint and, while a track is loaded
and playing, a `playing` timeline; §4.7), every 2 s (download refresh and a
cache-status update) and every 30 s (history sync while online).

At a gapless transition, the plan of the new occurrence is published exactly
once, by whichever of the poll and the worker sees it first. Before any track
has been decoded after start, the saved resume position and duration are shown
for display only; that projection never reaches checkpoints.

A Stop resets the saved position. The engine applies Stop asynchronously, so
until it has, engine snapshots of the stopped occurrence are ignored and a
stale position cannot overwrite the reset.

### Panic containment

Core execution runs under `catch_unwind`, and so does each worker iteration. A
panic answers the request with "Backend panic", rebuilds Core from persisted
state, re-applies the audio configuration, download control and last network
facts, and publishes the rebuilt status. The audible track keeps playing. Every
dequeued request is guarded, so a panic can never leave the UI busy. After 3
panics within 60 s, or a panic during recovery, the worker stops. On exit it
answers every queued request with an error, applies queued checkpoints, records
a final position, stops the engine, clears the artwork credentials and releases
the download hold.

## 3. Plex integration

### HTTP

One pooled `reqwest` client (rustls) is shared by Core, plan refresh and
artwork: 15 s total timeout, 5 s connect timeout, redirects disabled. The Plex
token always travels in an `X-Plex-Token` request header; an open redirect could
send that header to another origin, so a 3xx surfaces as a failure instead.
Default headers are `Accept: application/json` (new clients should prefer JSON;
https://developer.plex.tv/pms/), `X-Plex-Product`, `X-Plex-Version`,
`X-Plex-Platform` and a persistent `X-Plex-Client-Identifier` (a UUID in the
session).

JSON bodies are bounded to 8 MiB before deserialization, lyrics to 512 KiB and
artwork to 4 MiB. 401/403 map to Unauthorized, other non-2xx to an HTTP error,
transport errors to Network. Reqwest's per-read timeout resets after every
successful read, unlike the whole-request timeout (docs.rs reqwest 0.12
`ClientBuilder::read_timeout`).

URL confinement (`plex.rs`): a server URL must be a bare HTTP(S) origin.
Request paths must be query-free and server-relative; `//`, `\`, dot segments,
control characters, and literal or encoded separators/escapes
(`%2e %2f %5c %25 %3f %23 %00`) are rejected, and the joined origin is checked.
Advertised keys (station and artwork keys) may carry queries, but never an
`X-Plex-*` parameter.

### Authentication and servers

- **PIN sign-in.** `POST https://plex.tv/api/v2/pins`, then poll
  `api/v2/pins/{id}` while the user authorizes in a browser.
- **Servers.** `api/v2/resources?includeHttps=1&includeRelay=1`
  (python-plexapi `myplex.py`, a community implementation rather than a
  stability promise). The resource list mixes servers with players. Players may
  carry a null or missing `accessToken`, so only resources whose `provides`
  contains exactly `server` are decoded. Malformed server entries are skipped
  (https://www.plexopedia.com/plex-media-server/api-plextv/resources/).
- **Connecting.** Connections are tried in this order: non-relay, then HTTPS,
  then local, using the resource's own access token. Certificate validation is
  never disabled.
- **Manual connect.** The user enters a server origin and a token.
- **Persistence.** Account and server tokens persist only in the private session
  (§10). Server summaries sent to the UI never include tokens.

### PMS endpoints used

Music types are artist=8, album=9 and track=10. Pages use
`X-Plex-Container-Start` and `X-Plex-Container-Size`, normally 100 items.
Sources for this table: https://developer.plex.tv/pms/, python-plexapi
(`library.py`, `audio.py`, `playqueue.py`, `playlist.py`, `media.py`,
`mixins/rating.py`, `mixins/played_unplayed.py`) and https://plexapi.dev
reference pages.

| Purpose | Endpoint and behaviour |
|---|---|
| Browse | `/library/sections/N/all?type=…&sort=titleSort:asc[&title=…]`. Sort is `field:asc/desc`. |
| Alphabet | `/library/sections/N/firstCharacter` (artist type, titleSort). Returns Directory groups with key/size/title; summed sizes give page offsets. |
| Detail / children | `/library/metadata/N`, `/children`. Durations are in ms. Year and index values may be null and are parsed as signed. |
| Similar artists | `/library/metadata/N/similar`. 400/404 mean unavailable. Capped at 12 numeric library artists. |
| Sonic | `/library/metadata/{ids}/nearest`, section `computePath?startID&endID` (Sonic Adventure). Sonic analysis needs a server-admin Plex Pass on PMS ≥ 1.24 (https://support.plex.tv/articles/sonic-analysis-music/). A missing `musicAnalysisVersion` is not a signal; the endpoint answer is. |
| Stations | Metadata with `includeStations=1`. `Stations` may be an array or a `{size, Metadata}` container. The station key becomes a play queue `server://{machine}/com.plexapp.plugins.library{key}` via `POST /playQueues`; windows come from `GET /playQueues/{id}?center=&window=&includeBefore=&includeAfter=`. |
| Hubs / search | Section hubs; `/hubs/search?query&limit` returns `Hub.Metadata`. Recent hubs are identified by `music.recent.added.<section>` and `music.recent.played.<section>`. |
| Lyrics | The `Part.Stream` with `streamType` 4 and its advertised key. The response may be plain/LRC or JSON `MediaContainer.Lyrics[].Line[].Span[].text`. A blank line is `{}` with no Span. Start times may be fractional ms. |
| Rating | `PUT /:/rate?key&identifier=com.plexapp.plugins.library&rating=0..10`. |
| Playlists | Regular items are identified by `playlistItemID` (uppercase on the server). Smart playlists have no occurrence ids and are read-only here. `leafCount`; duration may be absent for empty playlists. |
| Timeline | `POST /:/timeline` with key/ratingKey, state, time, duration, `continuing`, plus `playQueueID`/`playQueueItemID` for stations. |
| Scrobble | `GET /:/scrobble?key&identifier=com.plexapp.plugins.library`. This is the established python-plexapi form; the community OpenAPI describes POST with ratingKey. |
| Transcode | See §4.2. |

The parsers tolerate `gain`/`albumGain` as numeric strings (unusable values
are omitted without invalidating the item) and radio flags given as `"1"`.

### Missing items

After a music section is re-created, old ratingKeys and Part URLs answer 404.
The track comes back under a new ratingKey with the same `guid=plex://track/…`.
PlexFreq does not look items up again by guid: a Gone item is skipped, and
the user plays the new copy from the library.
A transcode `start.m3u8?path=/library/metadata/<stale>` answers 400, not 404.

PMS's own 404 carries `X-Plex-Protocol: 1.0` with an HTML body. A 404 without
that header may come from a proxy and proves nothing. Only a 404 with the header
counts as Gone; 401/403 is Auth; everything else is Unknown.

An emptied station answers 200 with zero items for any `center`. A healthy queue
answers zero items for a center it no longer holds. Zero items therefore does
not prove a station is dead.

## 4. Playback and queue

### 4.1 Queue and occurrences

The queue (`src/queue.rs`, at most 10 000 items) holds the items and cursor, the
shuffle permutation, the repeat mode (off, one, all) and history behind the
cursor.

Shuffle keeps the current track and visits every item once in permutation
order; Previous follows that permutation. Repeat-one applies to automatic
advance only, so a user Next skips it. The queue ends with no selection unless
repeat-all wraps. The queue view shows playback order as an editable linear
list.

Each start of a track creates a playback occurrence, which rejects stale
checkpoints. The engine works in occurrences, not rating keys: station queues
repeat songs under distinct `playQueueItemID`s, and playlists use
`playlistItemID`. List row identity likewise includes these ids.

### 4.2 Plans, sources and quality

Core resolves the selected item into a plan: the queue, the visible track, the
stream URL, the playback source (`cache`, `stream` or `transcode`) and the
bitrate. A completed cache file is preferred before any metadata or network
work. The stream URL never leaves Rust; it is stripped before anything reaches
the model or Qt.

Quality settings are a Wi-Fi, a mobile and a download bitrate (each original,
or one of 64/96/128/160/192/256/320 kbps) and a codec-fallback switch. The
mobile choice applies only on a known cellular link; Wi-Fi, Ethernet and an
unknown network use the Wi-Fi choice. The mobile default is 320 kbps when
nothing is stored; an explicitly stored "original" stays original.

A plan transcodes when the streaming cap is nonzero, or when codec fallback is
on and the original's (container, codec) pair is not decodable:

| Codec | Containers |
|---|---|
| MP3 | mp3 |
| FLAC | flac |
| AAC | mp4/m4a/m4b/aac |
| ALAC | mp4/m4a/m4b |
| Vorbis | ogg |
| PCM s16/24/32le | wav |

Plex reports these as lowercase identifiers. Anything else, including empty
values, is transcoded.

**Progressive transcode** (`quality::transcode_url`):
- `/music/:/transcode/universal/start.m3u8?path=/library/metadata/K&protocol=http`
- `directPlay=0&directStream=0&directStreamAudio=0&audioCodec=mp3`
- `musicBitrate`, `minAudioBitrate` and `maxAudioBitrate` set to the cap
- `X-Plex-Client-Profile-Extra`: an mp3 musicProfile and a bitrate upper bound
- a fresh `session` UUID and an `offset` in seconds

With `protocol=http` the body is a progressive MP3, not an HLS playlist (Plex
for Kodi `audioobject.py`; Music Assistant Plex provider). A transcode cannot
seek by byte, so every load or seek creates a new session at a time offset. The
old session is closed in the background with
`/video/:/transcode/universal/stop?session=…` (OpenPHT
`PlexMediaServerClient.cpp`); the server also expires idle sessions.

### 4.3 Gapless preparation

After any command that starts or changes playback, the worker computes the
successor speculatively, without changing the queue or radio, and hands its plan
to the engine, after the reply presenting the current track. Only one successor
is predecoded at a time. Its queue position, radio refill and new occurrence are
committed only when the engine reports that it became audible.

### 4.4 Radio

`src/radio.rs` owns the radio kinds and source URIs.

- **Station radio** (artist and discovery stations) keeps the server
  `playQueueID`. Its window is replenished one unseen `playQueueItemID` per
  advance. History is trimmed so current, future and recent rows fit 11 rows.
  When a station occurrence completes naturally, a `stopped` timeline with
  `continuing=1` is reported before the refill.
- **Sonic track radio** chooses nearest tracks and remembers up to 200 recent
  seeds.
- **Album radio** expands whole next albums and may exceed the window.
- Unsupported seeds and failed continuations keep the previous queue
  (transactional).

Radio refuses shuffle and repeat. A failed completion report blocks
continuation only for an authorization failure.

**Offline and failure continuation.** In explicit offline mode, on a known
disconnected network, after a network or 5xx refill error, and at the end of the
window after any non-auth refill failure (including the empty station of a
re-created library), the radio window is traversed in playback order, skipping
entries without complete cached audio and wrapping.

The window is not trimmed then, so cached history stays available. With nothing
cached, playback ends with either the gone notice or the refill error. An ended
station's Next resumes from its last audible item.

**Background refill.** A move whose next item has complete cached audio does
not wait for the server: the refill is owed (`Radio.refill.pending`, at most 12,
persisted with the session) and the worker fetches it on a separate thread. The
worker never blocks on that request. The result is applied between worker
messages, and only to the state it was fetched for: the same radio instance
(random per radio), source version, queue revision and server. Otherwise it is
fetched again. It only appends, so queued queue commands keep their indices. A
prepared successor of the same radio state receives the same items, so its
commit keeps them; a successor already shown waits for its commit and a new
fetch. A failed background refill clears the debt; the next move refills as
usual. A move onto an uncached successor or at the end of the window still
refills before moving, so its network-failure fallback above applies. The
station completion report of an automatic move is still sent before the move.

**Refill center.** The refill is centred on the current item if it is not gone.
Otherwise it uses the last held non-gone item, because PMS answers nothing after
a centre it no longer holds.

### 4.5 Health and gone items

Checks start only after a failure: a cache-ahead download answered 404 (or 400
for a transcode), a gapless successor failed to pre-load, the current stream
failed after the cache and transcode fallbacks, or a metadata fetch answered
404. The check is a status-only `GET /library/metadata/{key}` (5 s, token in a
header). At most 10 probes run per rolling minute and 2 at a time, with part of
that budget reserved for current-track failures. A key is not probed again for 60 s after
an inconclusive result, and an Auth result suspends probing. Results from before
a queue, server or Core change are discarded.

**Gone marks** (`src/health.rs`) last 10 minutes, because a rescan can restore
an item. They are cleared on connect, select server and logout, and dropped when
a queue replacement succeeds. An explicit Select, Play or Resume of a gone item
clears its mark and retries it.

Next (manual or automatic), Previous, removing the active item and successor
preparation step over gone items that are not served from the cache, in the
direction of travel. Forward with nothing playable left is a natural end with a
gone notice, leaving the cursor on the last audible item; backward with nothing
behind is an error, and the queue is restored. Cache-ahead looks past gone keys,
so its window keeps its size. Gone rows are dimmed and labelled "No longer on
the server" on both shells. A successor that fails to pre-load is dropped
silently and checked, and preparation continues past it once the verdict is
Gone, so a run of deleted tracks is found while the current track still plays.

**Current-track verdict.** A Gone verdict skips automatically only if the item
is still current, no user playback request is pending, and the target needs no
metadata fetch. Otherwise the user sees "This track is no longer on the server".
If the verdict is inconclusive, or none arrives within 8 s, the original error
is shown.

### 4.6 Held stream failures and restarts

A current-track stream failure is held instead of discarded, so recovery can
never override a later user control. When the current decoder fails, the
successor is dropped, the output plays what was already submitted, and the
engine publishes a held state that preserves position, listened time and play
intent. While a failure is held, Play/Pause change the held intent, Seek moves
the held position, and Stop or a new load end it. The worker resolves it
(recover, replace or abandon); a failure that nothing resolves is given up after
60 s. Toggle and MPRIS status count buffering and held failures as Playing.

The recovery chain for the current item:
1. **Cache.** The complete cached file, if one exists.
2. **Transcode restart** (only when the failing source was a transcode). The
   stream restarts at the held position, never within 5 s + crossfade of the
   end. A restart needs new audio from the failed attempt; at most 4 attempts
   are made without 10 s of new audio, with backoff 0, 2, 4 and 8 s. While
   paused or known offline, the restart waits for Play or the network without
   spending an attempt. A pending user request decides instead of a restart.
3. **Transcode** of an original that failed.
4. **Gone check** (§4.5), then skip or report.

**Early transcode end.** PMS can end a transcode cleanly before the track's
duration. That is treated as truncation and restarted as above. If a restart
ends early again at the same point, the media is shorter than its metadata, and
it is treated as the real end: the listen is recorded and playback advances
(keeping a paused state), or ends without a banner if nothing follows.

### 4.7 Listens, history and timelines

The audio engine computes listened time per occurrence from output frames, so
seeks do not count; Core qualifies and records it. A play qualifies at
≥ min(half the duration, 240 s). Qualified plays are stored in `listens` (at
most 10 000, for insights) and in the `history` outbox (at most 2 000).
Delivered entries are evicted first; when the outbox is full of undelivered
plays, recording fails until a sync.

History sync scrobbles up to 20 undelivered plays per pass. It runs every 30 s
while online, and when the user asks. The scrobble endpoint cannot restore the
original listen time, and an acknowledgement lost in transit can cause a
duplicate.

While a track is loaded and playing, a `playing` timeline is sent about every
5 s, within the "normally every 10 s" guidance at https://developer.plex.tv/pms/.
Timelines are not sent for cached playback, in explicit offline mode, or on a
known-offline network.

## 5. Audio engine

### 5.1 Structure

```
plexfreq-audio actor: controls, pump, DSP, crossfade, PulseAudio mainloop dispatch
  ├─ Decoder (current)   ── plexfreq-decode: Symphonia → trim → stereo → rubato 48 kHz
  │     └─ http::Source (blocking Read+Seek) ⇐ byte window ⇐ async reader task (plexfreq-rt)
  ├─ Decoder (successor) ── same pipeline, 1 MiB window until promoted
  └─ Output: PulseAudio stream | Fake (real-time, silent) | Capture (instant, recorded)
```

Controls reach the actor over a bounded channel; the engine publishes a snapshot
and a change watch (transport fields without position) that drives MPRIS, the
heartbeat and the download yield without polling. `PLEXFREQ_AUDIO_SINK=fake`
selects the fake sink.

Output is fixed at 48 kHz, stereo, float32. There is no bit-perfect or
sample-rate-matching claim.

### 5.2 HTTP input (`src/audio/http.rs`)

An async reader appends response bytes to a bounded window: 4 MiB for the
current track and 1 MiB for a successor until promotion. It has a 5 s connect
timeout and a 10 s read timeout, with redirects disabled. The token is sent as
a header; the decoder never sees a URL. A read outside the window, or more than
64 KiB ahead of it, starts a new request at that offset, and bytes of a
superseded request are never used.

Ranged resume follows RFC 9110 (§13.1.5 If-Range, §14.4 Content-Range,
§15.3.7): a nonzero offset requires a strong ETag or Last-Modified validator and
is sent with `Range` + `If-Range`; `Content-Range` is validated, including `*`
for an unknown total; if the server ignores the range and answers 200, the
prefix is skipped only when the validator matches.

Retries:
- Until bytes have entered the window, 3 retries are allowed (4 requests in
  total, 1/2/4 s apart). Exhausting them fails the decoder, so the runtime can
  choose a fallback. Entering the window is the only proof required; it does
  not imply successful decoding or audible output.
- After that, transient network and 5xx failures of a byte-resumable original
  keep retrying the same representation, backing off 1/2/4/8 s (capped at 8 s),
  until a seek or Stop. A response that makes no progress loses this status.
- A progressive transcode is not resumable by bytes. Its broken transfer is a
  stream failure, handled by the restart in §4.6.

A broken transfer still hands over the bytes it already received, so playback
continues to the heard position before the failure surfaces.

### 5.3 Decoding (`src/audio/decode.rs`, `mp4.rs`, `resample.rs`)

Symphonia 0.6.1 is built pure Rust, with default features off and `flac, mp3,
aac, alac, isomp4, ogg, vorbis, wav, pcm` enabled. It has no Opus or WMA
decoder (docs.rs/symphonia 0.6.1; crate sources).

Each decoder picks the first audio track with a compiled decoder (ignoring
cover-art video tracks) and seeks sample-accurately for resume. Mono is
duplicated to stereo; more than 2 channels get an ITU-style downmix normalized
to full scale, with LFE dropped. Symphonia's probe treats a read error as end of
stream, so an HTTP failure during probing is logged alongside the "no suitable
format reader" error.

**Gapless trimming.**
- Packet `trim_start`/`trim_end` are applied where the decoder does not already
  do it. MP3 (LAME/Xing delay) and Vorbis trim in the decoder; AAC does not.
- Symphonia's MP4 demuxer parses `elst` but does not apply it ("Since edits are
  not currently supported", `symphonia-format-isomp4`). `mp4.rs` therefore taps
  the bytes the demuxer already reads, with no extra HTTP request, and derives
  delay and valid length from iTunSMPB or the first non-empty edit.

**Resampling.** rubato 5.0.1 asynchronous sinc resampler (128 taps, no FFT
dependency; rubato README and `Async` source). Edges are extended rather than
zero-padded, the filter delay is discarded, and the output length is exactly
round(n × ratio).

### 5.4 Flow control and buffering (`src/audio/mod.rs`)

Decoders keep 2 s of PCM ahead plus the crossfade tail. A remote successor of a
remote current track is not opened until the current track is well buffered or
close to its end, so the successor never competes with the audible stream for
bandwidth. Local successors start at once.

**Resume gates.** A fresh remote load or seek stays corked until 500 ms is
decoded (the Start gate), above the 300 ms prebuffer. When nothing is pushable
and the output is starved, the stream corks (the Stall gate) and resumes with
5 s of playable audio, at the decoded end, or after 15 s of unpaused waiting
with ≥ 500 ms buffered. A slow link therefore gives a few long pauses instead of repeated 300 ms
stutters. Local streams are never gated. The engine reports `starved` when play
is intended and a Stall gate is set or a Start gate has lasted over 1 s; this
drives the download yield (§6.1). If the output starves during a crossfade wait,
that boundary's fade is dropped.

**Crossfade.** Equal-power, 0–12 s, off by default. It is suppressed between
tracks of the same album and never exceeds a short, fully decoded successor.

**Position.** `pa_stream_get_time` gives the server read index minus sink
latency, interpolated with `INTERPOLATE_TIMING` + `AUTO_TIMING_UPDATE`. The
reported position never exceeds submitted audio, small backward jitter is
clamped, and a backward jump of at least 1 s rebases it.

### 5.5 PulseAudio output (`src/audio/pulse.rs`, `output.rs`)

Each engine has one libpulse context on a non-threaded `pa_mainloop`, dispatched
by the actor without blocking, so all callbacks run on the actor thread. Each
dispatch iterates until no event source fires: on a socket-only transport (no
shm/srbchannel), libpulse's `pstream.c` reads one packet per iteration (libpulse
16.1 `do_pstream_read_write`).

Each load or seek creates a float32 stereo 48 kHz playback stream, started
corked.
- Buffer attributes: `tlength` = 300 ms, `minreq` = 20 ms, `prebuf` = -1
  (equal to tlength), `maxlength` = server maximum.
- `ADJUST_LATENCY` is not set, so `tlength` is the stream's own buffer rather
  than total latency including the sink (PulseAudio 17.0 `pulse/stream.h`,
  `def.h`).
- The actor writes while `pa_stream_writable_size()` > 0, rather than trusting a
  client-side estimate that drifts on Bluetooth and after a sink move.

Stream properties: `media.role` = `x-maemo` when `/etc/os-release` has
`ID=sailfishos`, otherwise `music`; `media.name` and `application.name` =
`PlexFreq`; `application.id` = `harbour-plexfreq`.

On Sailfish, mainvolume and stream-restore recognize `x-maemo`
(`/etc/pulse/mainvolume-listening-time-notifier.conf` `role-list = x-maemo`;
pulseaudio-modules-nemo `module-meego-mainvolume.c`). The user-session policy
then owns volume keys and routing. The app writes no policy.

Pause, a recording hold and the gates cork the stream; load, seek and stop
disconnect it, discarding queued audio. A policy `request-cork` event clears
play intent; `request-uncork` is ignored, because it is not permission to undo a
user or disconnect pause. A stream failure (daemon restart) reports "Audio
output is unavailable", and the next load reconnects. Volume is Rust DSP gain,
not a stream volume.

### 5.6 DSP (`src/audio/dsp.rs`)

- 10-band peaking EQ at Q = 1, ±12 dB per band.
- Normalization from the Part stream `gain`/`albumGain`: Track; Album (falling
  back to track gain); Auto (album gain for identified single-album queues and
  album radio, track gain otherwise).
- Optional headroom of 0–12 dB, then volume gain. Float peaks above full scale
  are preserved and non-finite samples are sanitized.

Invalid configurations are rejected with "Audio settings exceed the supported
range".

## 6. Caching and offline

### 6.1 Audio cache (`src/cache.rs`)

The cache lives in `<state-dir>/audio-cache` (directory 0700, files 0600, one
process-exclusive writer). The default budget is 512 MiB with 5 tracks of
look-ahead. Entries are namespaced by SHA-256(server origin ‖ token), so
different servers or accounts never share them, and keyed by ratingKey, Part
path and quality, so original and transcoded media never share a key. The
index stores no token or authenticated URL.

Each download batch is the queue's actual upcoming order plus explicit
downloads; a new schedule replaces it. Validators, ranges and `Content-Range`
follow the same rules as streaming, and responses must be identity-encoded
audio. PMS `Part.size` is only a hint: the served byte count can differ, so the
received size is what is committed. Completion is an atomic rename plus index
commit, and partial files are never handed to a decoder.

A transcoded partial restarts rather than byte-resumes. Originals that cannot be
decoded locally are downloaded as 320 kbps MP3 transcodes when the download
quality is "original".

Eviction and housekeeping:
- LRU eviction protects the scheduled batch (normally the current track and the
  look-ahead; automatic originals are not scheduled under a streaming cap, see
  "Mobile cap") and permanent pins.
- The quota includes partial files and excludes symlink targets.
- Downloads of unknown size reserve space in windows of at least 4 MiB.
- A malformed index is quarantined to a backup. Unindexed media is preserved
  for 7 days for re-indexing.

A batch makes up to 4 passes, waiting 30 s after a pass that left transient
failures (interrupted, incomplete, network, 5xx). Cancellation and holds are not
failures.

**Refusal cooldown.** Refusals that would only repeat on retry (HTTP 4xx except
401, 404, 408 and 429; non-audio or content-encoded responses) are remembered for 30 min per item and quality. Later batches skip them
and show the stored error. The cooldown is cleared by any explicit download
command, by changing the cache settings, and by clearing the cache. Local
errors (disk, capacity) never cool down.

**Gate and hold.** Downloads wait while the Wi-Fi-only policy meets a
non-Wi-Fi network, while the user has paused them, in explicit offline mode, and
during the yield hold.

The yield hold is set while the engine reports `starved` (§5.4). It is released
after 10 s of healthy playing, at once when nothing plays, and at exit. It never
cancels downloads or resets progress:
- An original with a validator and known size drops its connection at once and
  resumes by range after release, spending no attempt.
- Other downloads finish their current item.
- Every item waits for release before it starts.

The Downloads pages show "Downloads wait while the stream catches up" while held
with work pending.

**Mobile cap.** Automatic cache-ahead of originals is skipped while a streaming
cap applies and download quality is original. Explicit downloads follow their
own policy.

### 6.2 Download plans and groups (`src/daily.rs`)

Pinned groups are albums, playlists, single tracks, Plex stations, and artist,
album and track radio plans. Each keeps its membership and track plans in the
session. Plan refresh runs transactionally on its own thread, never replaces the
playback queue, and honours the network gates. Automatic refresh attempts for a
group are at least 900 s apart, in addition to the configured refresh interval
(fixed throttling, not exponential backoff).

Permanent pins are protected independently of the scheduled batch. Overlapping
groups keep shared downloads when one group is removed. On restart, protection
is rebuilt and retained plans are scheduled again.

### 6.3 Offline metadata and browsing (`src/offline.rs`)

Raw responses are stored as private snapshots scoped to the credential, up to
64 MiB including saved lyrics. When over the limit, files with the oldest
modification time are pruned first; reading a snapshot refreshes its time at
most once an hour, so pruning approximates least recent use.

On network or 5xx failures, saved pages may be used and the reply is flagged
offline; authorization failures never fall back silently. Explicit offline mode
bypasses Plex and cancels plan refresh, audio downloads and artwork downloads.
Offline browsing merges saved metadata with the completed-audio catalogue, 100
items per page, sorted by lowercase title then ratingKey so pages agree on one
order. Details use saved pages or known parent-child relationships; unvisited
data is never claimed to exist.

When saving snapshots fails, the Downloads pages show "Offline library data
could not be saved: …".

### 6.4 Artwork

Rust references each image as a `file://` URL of the private artwork cache when
the file is cached, otherwise (online with a token) as a credential-free id
`image://plexfreq/<first 16 hex of the namespace>/<hex of the server-relative
path>` after the path has passed URL confinement, otherwise not at all. The
namespace prefix keeps the same path on another server or account a distinct
image in Qt's cache.

The Rust artwork source holds the credential snapshot. It is updated before
every reply, empty while signed out or offline, and cleared on exit, so a
retained image provider cannot keep fetching with the credential. Fetches
reject ids that are malformed or belong to another namespace, download with the
token in a header (5 s, `image/*` only, at most 4 MiB, redirects disabled), and
discard the result if the account changed meanwhile.

Core also caches artwork for returned items: at most 4 MiB per image and
128 MiB in total. MPRIS exports only `file:` artwork.

The C++ provider is a `QQuickAsyncImageProvider`, available since Qt 5.6 (Qt 5.6
`QQuickAsyncImageProvider` docs). Requests run on a bounded thread pool, so a
stalled download never blocks shutdown or other images. Sources over 8192 px per
side or 16 Mpx are refused, and images are only ever scaled down (1024 px on the
longest side by default, never above 2048 px).

## 7. Network policy

The network observer reports three facts for the default route (online, Wi-Fi,
cellular). It re-queries on D-Bus signals and every 15 s, with a 2 s deadline
per call.

- **ConnMan.** `net.connman.Manager.GetServices` returns services sorted
  favourite-first. The first `ready`/`online` service is the default route.
  `vpn` services are skipped so the carrier link underneath decides. `wifi` is
  `Type == "wifi"` and `cellular` is `Type == "cellular"`. Sources: ConnMan
  `doc/manager-api.txt` and `service-api.txt`.
- **NetworkManager** is the fallback when ConnMan does not answer within 2 s.
  `PrimaryConnectionType` is `802-11-wireless` for Wi-Fi and `gsm`/`cdma` for
  cellular, and `State` ≥ 50 means online (NM_STATE_CONNECTED_LOCAL). For NM's
  own VPNs the primary connection type is that of the underlying link; a
  VPN-typed primary counts as neither Wi-Fi nor cellular. Source: https://networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.html
  and `nm-dbus-types.html`.
- **Not cellular.** Bluetooth tethering and Wi-Fi hotspots count as not
  cellular.

A failed or late query is unknown, never offline, and the last known facts are
kept. On a change, Core updates the download gate, reschedules cache-ahead,
cancels refresh and artwork downloads when downloads are no longer allowed, and
restarts plan refresh when coming online.

Facts are not persisted, so the Wi-Fi quality choice applies until the first
observation after start. A successor prepared before a network change keeps its
quality.

## 8. Platform integration

### 8.1 MPRIS

`org.mpris.MediaPlayer2.plexfreq` on the session bus, at
`/org/mpris/MediaPlayer2`, exports Root and Player through zbus. Spec:
https://specifications.freedesktop.org/mpris-spec/latest/Player_Interface.html.

`PlaybackStatus` follows logical intent: Playing while loaded with play intent
(including buffering, gated or held failure), Paused when paused, Stopped
otherwise. Metadata carries title, album, artist, length and a track id per
occurrence; `mpris:artUrl` is a local `file:` URL only. `Next`/`Previous` keep
the paused state, as the spec requires, and `Raise` brings the window to the
front. `PropertiesChanged` carries only properties whose value actually changed,
so pause, volume and queue changes never re-send Metadata. `Position` is read on
demand; user seeks emit `Seeked`.

**AVRCP resync.** On Sailfish, BlueZ's `mpris-proxy` bridges MPRIS to AVRCP. In
BlueZ `profiles/audio/media.c`, every received `Metadata` ends with
`mp->position = 0; g_timer_start(mp->timer)` plus a track-changed event, even
when the track did not change. BlueZ `tools/mpris-proxy.c` forwards
PropertiesChanged and every Player signal verbatim, and runs `GetAll` only at
registration (BlueZ upstream 5.87; Sailfish fork github.com/sailfishos/bluez5).
Re-sending Metadata mid-track would therefore reset the car's position to 0:00.
So after each Metadata emission PlexFreq sends a `Seeked` carrying the real
position of the track that Metadata named (skipped in the first second). The
spec defines `Seeked` as a position change inconsistent with normal playback,
so a scrobbler may see this resync as a seek.

### 8.2 Bluetooth

The BlueZ observer reads `org.bluez.Device1` objects with audio UUIDs (A2DP
sink 110b, headset 1108, handsfree 111e) and `org.bluez.MediaTransport1` states
through `org.freedesktop.DBus.ObjectManager` (BlueZ `doc/org.bluez.Device.rst`,
`org.bluez.MediaTransport.rst`).

Playback pauses when a tracked device disconnects or its audio transport
disappears while playing. Device1 can stay connected through another profile
after the audio profile is gone. An idle transport, a read error or an unknown
fact is not a disconnect. Switching between accessories forgets the previous
route, and reconnection never resumes playback.

### 8.3 Recording hold

A read-only libpulse subscription watches sources and source-outputs. An
uncorked source-output on a non-monitor source is an active recording. Only
indexes and flags are read, never samples or stream properties, and a failed
query keeps the last state.

While something records, the output freezes and the player shows paused. When
the recording ends, only the retained intent resumes: a Pause, Stop or
Bluetooth-disconnect pause during the hold cancels the resume, and Play during
the hold stays inaudible until release.

Under Sailjail the `Audio` permission exposes the PulseAudio socket
(sailfishos/sailjail-permissions `permissions/Audio.permission`).

### 8.4 Diagnostics observers

A periodic heartbeat logs transport state, BlueZ counts and PulseAudio facts
from read-only libpulse samples. It never creates streams, starts a daemon or
changes routing, and it logs technical fields only, with sink names reduced to
a class. `pactl` is not available inside the Sailjail namespace, which is why
this uses libpulse. `examples/pulse-diagnostics.rs` probes the same code.

### 8.5 Sailfish specifics

- **Foundation.** Qt 5.6, QtQuick 2.6, Silica and libsailfishapp
  (https://docs.sailfishos.org/Develop/Apps/).
- **QML API limits.** Not available on Qt 5.6: Controls 2, new-style
  Connections, `Column.forceLayout`, `Qt.callLater`.
- **Silica quirks.** BackgroundItem reparents children into a content item, so
  bind to explicit ids rather than `parent`. SectionHeader adds an x margin.
  SliderBase writes `value` imperatively while dragging, which breaks bindings.
  Loader geometry is applied to the loaded item, so `IntrinsicLoader` sizes from
  `implicitHeight` (Qt 5.6 Loader docs).
- **Permissions.** The desktop file declares
  `[X-Sailjail] Permissions=Internet;Audio;Bluetooth` with
  `OrganizationName=org.plexfreq` and `ApplicationName=harbour-plexfreq`
  (https://docs.sailfishos.org/Develop/Apps/Application_Permissions/).
  Sandboxing applies only to a normally installed app launched through
  Sailjail. Sailjail rejects an `Exec` that does not match the installed
  desktop file, so an unpacked binary cannot be tested in the sandbox.
- **Harbour libraries.** Harbour validation (`mb2 check --suites harbour`)
  rejects libgstreamer-1.0, libgstbase-1.0 and libgstapp-1.0, and accepts
  libpulse. The allowed NEEDED set is libsailfishapp, libpulse, Qt5
  Quick/Gui/Qml/Core, libstdc++, libm, libgcc_s, libc and ld-linux.

Build, toolchain and packaging details are in [development.md](development.md).

## 9. UI adapter

**Backend** (`app/src/backend.*`) creates the runtime with the state directory
(`QStandardPaths::AppDataLocation`, or `PLEXFREQ_STATE_DIR` for isolated tests)
and polls it every 100 ms. It exposes the merged reply state, cache status,
busy/error flags, playback fields and two list models, notifying only on
change. Lyric and similar-artist replies never enter the shared state, and
their failures never set the global error.

**EntryModel** reconciles immutable lists by identity (type, ratingKey, key,
`playQueueItemID`, `playlistItemID`), so existing rows survive updates.

**Session.qml** owns routes, history, paging, search debounce, alphabet rail
offsets, sign-in polling and shared presentation helpers. `Navigation.qml` is
the shared destination/action catalogue.

**Translations** (`app/src/i18n.cpp`). The locale is taken from
`PLEXFREQ_LANGUAGE`, then the environment, then (on Sailfish, when no locale
variable is set, for example in an SSH launch) `LC_MESSAGES` or `LANG` from
`/etc/locale.conf` (https://man7.org/linux/man-pages/man5/locale.conf.5.html),
with codeset and modifier suffixes stripped. The base-language catalogue is
installed before the territory one, with English as the fallback. Rust error
strings are translated in the `Core` context, including parameterized forms
("Plex returned HTTP %1").

## 10. Persistence and diagnostics

### Session files (`src/store.rs`)

| File | Content |
|---|---|
| `session.json` | Settings and the playback checkpoint, including the client id and account/server tokens. Rewritten atomically only when its bytes change. |
| `queue.json` | Queue and radio. Rewritten only when they change. A queue revision mirrored in `session.json` ensures a saved position is never applied to another queue. |
| `listens.jsonl`, `history.jsonl` | Append-only, with the same caps as in memory. Damaged lines are skipped, and the files are compacted atomically when damaged or too large. |

Writes go to a synced 0600 temp file that is renamed into place, then the
directory is synced. Stale temp files, which may contain tokens, are removed at
startup. A malformed document is saved to a unique 0600 corruption backup
before defaults are used. The read-only inspect mode never writes, migrates or
compacts.

### Logs (`src/diagnostics.rs`)

On Sailfish, PlexFreq keeps a persistent log unless `PLEXFREQ_STATE_DIR` is set.
The log directory is `~/Documents/PlexFreq` if it can be created, otherwise
`<AppDataLocation>/logs`. There is one file per local date
(`plexfreq-YYYY-MM-DD.log`), each at most 8 MiB with one backup, and 7 dates are
kept. On the desktop, output goes to the terminal.

Redaction removes full URLs and the rest of any line carrying a token-like
value. It is best effort: framework output may still carry media URLs, so log
files are not treated as redacted and should be checked before sharing.

## 11. Security and credential handling

- **Where tokens live.** Rust owns credential persistence and use; tokens are
  stored only in the 0600 session files. Published models and replies omit
  credentials: stream URLs are stripped from plans, server summaries omit
  tokens, and artwork uses `image://plexfreq` ids resolved in Rust. The one
  exception is the manual "connect directly" form, where QML collects the token
  the user types and the C++ adapter forwards it to Rust in the connect request
  (`app/qml/pages/SettingsPage.qml`, `app/qml/desktop/Main.qml`,
  `app/src/backend.cpp`).
- **How tokens travel.** In `X-Plex-Token` headers, on clients with redirects
  disabled, to origin-confined paths. Advertised keys may not inject `X-Plex-*`
  parameters.
- **Isolation.** Response bodies are size-bounded (§3), and cache, offline
  snapshots and artwork are namespaced per credential (§6).
- **Read-only platform access.** No root, privileged helper or system-service
  modification. Platform access is read-only (ConnMan/NM, BlueZ, libpulse
  introspection) except the app's own playback stream and its MPRIS name.
