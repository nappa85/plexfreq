# Architecture

## Pure-Rust decoding and PulseAudio output (2026-10-16)

GStreamer is removed (Harbour rejects libgstreamer/libgstbase/libgstapp). The audio
actor, Control/Event/Snapshot/Handle API, segment/transition accounting, crossfade,
gain and EQ are unchanged; only the decoder, HTTP input and output sink were replaced.

```
 actor thread (plexfreq-audio): commands, pump, DSP, PulseAudio mainloop dispatch
   ├─ Decoder (current) ── thread plexfreq-decode: Symphonia demux/decode
   │     └─ http::Source (blocking Read+Seek) ⇐ byte window ⇐ async HTTP task (plexfreq-rt)
   ├─ Decoder (prepared successor) ── its own thread, same pipeline
   └─ Output: PulseAudio stream | Fake (real-time) | Capture (instant, recorded)
```

- Decoding: `src/audio/decode.rs`. One thread per Decoder probes with Symphonia 0.6.1
  (features flac, mp3, aac, alac, isomp4, ogg, vorbis, wav, pcm), selects the first
  audio track with a compiled decoder (a cover-art video track is ignored), seeks
  sample-accurately for resume, decodes, trims, maps channels to stereo (mono
  duplicated; >2 channels ITU-style downmix normalized to full scale; LFE dropped)
  and resamples to 48 kHz. It blocks only on a condvar (PCM ready ≥ the actor's
  requested lead: 2 s plus crossfade tail) or on input bytes, re-checking stop at
  least every 100 ms; Decoder::drop stops the reader and joins the thread.
- Trimming: packet trim_start/trim_end are applied when the decoder has not already
  done so (MP3/Vorbis apply them; AAC does not), and a block is capped at the packet
  duration (a shortened final MP4 sample marks padding). Symphonia's MP4 demuxer
  parses but ignores edit lists, so `src/audio/mp4.rs` taps the bytes the demuxer
  already reads (no extra HTTP range request), keeps `moov` and derives delay/valid
  length from iTunSMPB or the first non-empty `elst` edit (media duration minus delay
  when it agrees with the millisecond movie-timescale edit duration).
- Resampling: `src/audio/resample.rs`, rubato 5.0.1 `Async::new_sinc` (128 taps,
  linear interpolation of a 256× oversampled Blackman-Harris² sinc, no FFT
  dependency, comparable to GStreamer audioresample's default quality). The first
  frame is extended backwards and the last frame forwards instead of zero padding,
  the filter delay is discarded and the output length is exactly round(n × ratio).
- HTTP input: `src/audio/http.rs`. The async task on the shared current-thread runtime
  appends response bytes to a bounded 4 MiB window (Mutex + Condvar for the decoder,
  tokio Notify for room); it never blocks. Seeks move the decoder's logical position
  only; a read outside the window (or >64 KiB ahead of it) bumps a generation and
  requests that offset, so stale bytes of a superseded request are never appended.
  Probe-time End/Start round trips cost no request. Retries, validators, Content-Range
  (`*` included), 200-ignoring-Range skipping, progressive transcodes (not
  byte-seekable; resume via time offset) and the session closer are unchanged.
- Output: `src/audio/output.rs` + `src/audio/pulse.rs`. One libpulse context per
  engine (non-threaded `pa_mainloop`, dispatched without blocking by the actor every
  pump, so callbacks run on the actor thread). Each load/seek creates a float32 stereo
  48 kHz playback stream (START_CORKED, INTERPOLATE_TIMING, AUTO_TIMING_UPDATE,
  ADJUST_LATENCY; tlength 300 ms, minreq 20 ms as pulsesink buffer-time/latency-time
  did; prebuf server default). Pause/recording hold/starvation cork; Play uncorks;
  load/seek/stop disconnect the stream (queued audio discarded immediately). End of
  stream drains; the drain callback sets EOF. Stream properties: media.role (x-maemo on
  Sailfish, music elsewhere), media.name, application.name=PlexFreq,
  application.id=harbour-plexfreq. Policy request-cork clears play intent; request-uncork
  is ignored (as REQUEST_STATE was). Stream failure (daemon restart, kill) reports
  "Audio output is unavailable"; the next load reconnects. Volume stays Rust DSP gain.
  Each actor service dispatches the mainloop until no event source fires (at most
  64 iterations): on the socket-only transport (no shm/srbchannel) one iteration
  reads about one server packet, and a single one per pump left write requests stale
  behind timing replies after a seek (underflow ~300 ms into most new streams, found
  by the server-backed tests, 2026-10-16; not reproducible with memfd/srbchannel).
- Position: `pa_stream_get_time` (server read index minus sink latency, interpolated)
  in frames, through the unchanged PositionClock (failed query keeps the last value,
  jitter clamped, ≥1 s backwards jumps rebased, capped at submitted frames). Queued
  bytes = (submitted − position) × 8, so the 0.5 s backpressure includes server and
  sink latency. Snapshot.playing requires uncorked and started (not prebuffering or
  underrun, via started/underflow callbacks); otherwise play intent shows buffering.
- Format policy: `src/audio/formats.rs` is the single (container, audioCodec) table
  derived from the compiled decoders; anything else, including empty/unknown values,
  is transcoded for streaming, and original-quality downloads of such tracks are
  server MP3 transcodes at 320 kbps (`cache::download_quality`). The runtime's single
  decode-failure → transcode fallback is retained.

## Typed command admission / worker channel (2026-10-15)

`Runtime::submit` (and MPRIS/Bluetooth through `Shared::send`) parses the wire
JSON into `crate::Command` (`src/command.rs`) once, at admission. Malformed,
unknown and `_`-prefixed ops now fail synchronously there (`accepted:false`);
transport controls (`audio_pause/stop/seek/volume/play/toggle`) are `Command`
variants executed on the calling thread. `Command::class()` is an exhaustive
match (no wildcard) giving listing/view-bound/passive/transport/re-prepare/
session-reset/engine routing, so a new op cannot be added unclassified; `op()`
comes from strum. The worker channel is `WorkerMsg { Request(Box<Request>),
Checkpoint(Checkpoint), Shutdown }`: `Request` carries the `Command`, the request
as submitted (echoed as the event's `request`; Qt reads only `op`) and typed
routing facts (view generation, busy/page counters, page start, start-paused).
QML's `_background`/`_refresh`/`_replace` stay wire flags read once at
admission; `_paused` is now a typed MPRIS argument. Events carry their view
generation beside the JSON (`Outgoing`) for delivery-time discard. Event and
reply shapes (`_update`, `_audio_error`, `_discarded`, `_pageStart`,
`_listAction`, correlated keys) are unchanged.

Fixed-value strings are enums with serde spellings identical to the old
strings: `model::ItemKind` (Plex `type` of items and library sections; unknown
types keep their exact spelling in `Other(String)` and order by spelling),
`queue::Repeat`, `radio::RadioKind`, `CollectionView`, `DownloadAction`,
`PlaylistAction`, `SmartAction`, `features::AlbumType` (also the Qt
"AlbumType" key), `PlaybackSource` (written by Core, read back by the runtime's
decode-failure fallback), `DownloadKind` (plan/group kinds with exhaustive
minute ranges and radio seeds) and `GroupId` (`kind:key`, persisted as that
string; its derived order equals string order so `session.json` maps iterate
unchanged; unparseable persisted entries are dropped instead of rejecting the
document). The `strum` crate (0.27, derive) provides Display/FromStr/
IntoStaticStr/EnumIter; tests round-trip every variant through strum and serde.
Specific user-facing messages for unknown enum values ("Unknown repeat mode",
"Unknown download action", ...) are mapped at admission.

## Output position domain across sink resets (2026-10-15)

Each Rust Output owns a position clock mapping queried sink frames into its
appsrc/gapless segment coordinate domain. Failed queries retain the last position;
minor backwards jitter is clamped. A backwards jump of at least one second rebases
the offset at the last observed position. Results never exceed submitted frames.
Load/seek replaces Output and its mapping. No wall-clock progress is fabricated.
Raw query availability/frame/offset diagnostics distinguish a reset from ongoing
query failure on the next installed incident. This is local reset-path hardening;
the Oct 15 phone trace establishes broken timing but lacks raw query evidence.

## Restored playback presentation (2026-10-13)

Runtime::poll projects Core's saved resumePosition and selected track duration
into the outgoing playback DTO when its audio occurrence is zero. Engine snapshots
used for checkpoints remain untouched. Loaded/playing/seekable flags continue to
describe the empty engine, so presentation does not fabricate a decoder or autoplay.
Active and stopped nonzero occurrences continue to report their actual audio state.
Qt's existing property/signal adapter presents the DTO on both shells.

## Idle startup checkpoints / durable resume (2026-10-13)

Rust runtime audio occurrence zero means no playback has been loaded. `record`
rejects it even when the worker's current occurrence is also zero. Periodic,
transport and shutdown checkpoints therefore cannot overwrite the selected saved
track's durable position/history with a decoder-free startup snapshot. Nonzero
matching occurrences still save position zero for genuine seeks/stops. Startup
continues to publish saved metadata without autoplay; ordinary resume loads its
saved position. No Qt/QML checkpoint policy is added.

## Production-defect recovery / transparent float DSP (2026-10-13)

Rust session/cache readers recover from malformed whole documents after saving
their original bytes to a unique 0600 corruption backup using shared atomic writes.
The invalid active file is removed only after backup success. Ordinary I/O failures
remain errors. Read-only cache inspection does not write or quarantine. A cache
quarantine marker protects unindexed media from startup orphan cleanup across
reopen; it remains unplayable without valid metadata and counts toward quota.
Symlink targets are excluded from disk accounting just as from lookup and GC.
These contracts supersede earlier strict whole-index startup failure below.

Ratings tolerate unusable non-finite strings as zero. Smart-rule DTOs always
provide a valid limit; unsupported limits remain noneditable. Queue history trims
at natural end without inventing a current selection. Crossfade zeroes unmatched
output samples. DSP gain/EQ preserves finite float peaks above full scale and
sanitizes non-finite results; user-selected headroom remains explicit. Downstream
limiting/physical peak behavior is a separate design and validation question.

Persistent diagnostics strip controls before key matching, redact bare/quoted
credential assignments with identifier boundaries, and retain the existing
whole-tail removal for credential-bearing records. Presentation remains Qt/QML-owned.

## Buffered original-stream outage recovery (2026-10-13)

The Rust HTTP reader retains appsrc/decoder state during transient network/5xx
failures for original streams. A nonzero resume offset requires a representation
validator; Range/If-Range and total/validator checks prevent mixing different media.
Reconnect waits grow 1/2/4/8 seconds and remain capped at eight seconds until seek
or Stop cancels them. Delivered bytes reset the consecutive-failure counter.
Temporary failures send neither EOS nor a fatal bus error, so buffered PCM can
continue and the audio actor's existing empty-output buffering state resumes when
more bytes arrive. Permanent response failures, invalid representations and
non-resumable progressive streams retain explicit failure behavior. HTTP work and
retry sleeps remain off the audio actor; memory stays bounded by input watermarks.

## Queue presentation / isolated cache-entry recovery (2026-10-12)

Queue presentation materializes playback order into a linear editable queue and
clears its shuffle policy. Core snapshots take the displayed items/current from
that copy but the shuffle indicator from the source queue. Same-row QueueMove
validates indexes and preserves the source queue and radio without materialization.
Repeat-all previous wraps across the playback-order boundary.

Cache index syntax and outer map shape remain strict. Individual malformed Entry
values are skipped with count-only diagnostics; independently valid entries still
undergo file/length validation and remain available. A manifest write/shutdown
persists the recovered map. This differs from whole-index corruption, which fails
before orphan cleanup and preserves evidence.

Lyric JSON syntax failure is plain content; valid unsupported JSON is a protocol
error. PMS fractional starts use nearest-millisecond rounding, rejecting negative
or unrepresentable float values and falling back from line to first-span starts.

## Terminal stream cancellation / viewport feedback (2026-10-12)

The HTTP reader's watch value None is terminal cancellation. seek-data updates
the offset conditionally under the channel write borrow and rejects a stopped
reader, preventing teardown-time seeks from resurrecting requests. Stop publishes
None even after receiver loss and supplies appsrc EOS before decoder NULL teardown.
Existing nonblocking, bounded input feeding remains on the Rust HTTP worker.

Backend exposes Rust audio Snapshot.buffering as a Qt property using the existing
playbackChanged signal. Sailfish Library/detail and Now playing indicators sit
outside their scroll content and read live busy/buffering rather than retained
presentation snapshots; desktop uses its fixed top indicator. Native outgoing
pages mark departure before route changes and suppress restorePage until their
next activation, so early request completion cannot undo an intended forward push.
Core request diagnostics record operation/success/elapsed time, excluding request
arguments. Blocking metadata/reporting still shares Core's request worker; these
changes do not make all metadata requests concurrent or remove their HTTP deadlines.

## Production review admission / transport / storage bounds (2026-10-12)

Runtime list generations publish only after successful request admission under a
short admission mutex shared with generation readers. An unavailable queue cannot
invalidate accepted replies. Audio structural controls retain a bounded FIFO;
volume has a latest-value mailbox with one outstanding wake. Load failure returns
an error before publishing occurrence identity; rejected successor controls are
retried by the Core worker. Decoder dynamic pads query possible caps and defer
unknown media types until CAPS, rather than discarding an unnegotiated audio pad.

The HTTP reader alone produces decoder input. It bounds queued bytes explicitly
with nonblocking pushes and seek/stop-aware waits. Playback/cache/probe share a
normalized media MIME policy. Plex account/library JSON and async cache metadata
are bounded to 8 MiB before deserialization. server_path accepts only query-free,
origin-confined paths, rejecting literal/encoded traversal and separators;
advertised station/artwork/filter queries have a separate credential-rejecting
validator. Internal cleanup/filter requests pass query parameters separately.

Cache index errors are explicit and precede orphan deletion. LRU touch is memory
only until a manifest write/shutdown. Terminal downloads are excluded from retry
passes; transient failures permit at most three retries per admitted batch.
Unknown-size downloads reserve quota in 4 MiB windows (smaller fallback near full);
eviction accounts disk once per pass. Protection identity is a namespace/key tuple.
Lookup filesystem validation is outside the state mutex; mutation/status I/O still
needs a larger storage-ownership refactor. A common page envelope uses raw server
count independently of displayed rows and never advertises more at zero progress.


## Rolling radio window / offline continuation (2026-10-12)

Rust `Core::advance_next` replenishes station radio one unseen playQueueItemID per
advance, including speculative native successor preparation. The prepared queue
is still committed only when its occurrence becomes audible. Station startup
(artist and discovery station paths) retains at most eleven tracks starting at
the selected occurrence. Advancement trims old history so current + future +
recent history fit eleven rows. Track sonic radio replenishes each advance;
album sonic radio preserves whole-album groups and may exceed eleven rows.

An empty/failed speculative refill retains an already available successor.
Explicit offline mode, a known disconnected network observation, or a network/5xx
refill error traverses the radio window in playback order, skips entries without
usable completed audio and wraps, including repeating the sole cached track.
The same cache namespace/length validation as ordinary local playback applies;
partial files and unrelated cached music are never selected. Offline cycling
retains the queue and radio source, so online advancement can resume replenishment.
Network facts are distinguished from the initial unknown state. Timeline reporting
is skipped when disconnected; network/5xx completion-report failures do not block
direct automatic continuation. Queue policy remains entirely Rust-owned.

## Recording fixture synchronization (2026-10-12)

The decoder-free recording fixture uses a capture sink to await actual initial PCM
delivery, forcing exhaustion before the recording hold. Fresh PCM supplies preroll
while held: PAUSED commits without delivering it, then recording release commits
PLAYING and delivers that block. Explicit Pause still cancels retained resume intent.
This extends the earlier final-Pause preroll fix to recording release and leaves
production actor/recording policy unchanged.

## Release distribution (2026-10-12)

Tag releases publish the normally installed Sailfish aarch64 RPM and an Ubuntu
24.04-built Linux amd64 desktop tar.gz. CPack's TGZ generator consumes the existing
CMake install layout with relative bin/share paths. QML/translations are embedded;
the archive relies on compatible host Qt6 and audio libraries/plugins. CI extracts
and smoke-tests the desktop archive before publication. Rootless phone archives are no longer generated;
Sailfish integration evidence must come from normal Sailjail installation/launch.

## Recording fixture preroll (2026-10-12)

The decoder-free recording actor fixture supplies fresh PCM before awaiting the
final PAUSED transition. Non-live GStreamer sinks require preroll to commit that
state; a previously played block may already have drained. The regression also
awaits PLAYING after release, then verifies Pause cancels retained resume intent.

## Temporary recording holds / bounded Bluetooth lifetime (2026-10-09)

Rust `runtime/pulse/recording.rs` owns a persistent read-only libpulse subscription
to sources and source-outputs. Two bounded lists identify uncorked non-monitor
recording streams by indexes/flags only. No microphone samples or stream properties
are read. Subscription changes trigger reconciliation; an idle loop wakes at 100 ms
and refreshes every two seconds. Failed queries preserve the last hold until a
successful observation; cancellation/retry is worker-owned under Audio permission.

Recording changes go directly to the independent audio handle. The actor retains
logical play intent separately from its recording hold, freezes the output and
publishes paused rather than buffering. Ending the hold resumes only retained
intent: manual/ Bluetooth-disconnect Pause or Stop cancels resume; Play during the
hold remains inaudible until release. Source loads/seeks preserve the hold. Native
REQUEST_STATE(PAUSED) is serviced before PCM/backpressure checks, with no unsolicited
uncork resume. Clock-loss recovery cannot override a pause or recording hold.
Paused snapshots reset the progress diagnostic baseline, so a recording hold is
not mislabeled as stalled playback even while logical play intent is retained.

Bluetooth facts include existing transport devices, including idle transports.
Removing the selected device's audio transport now pauses even if Device1 remains
connected via another profile. Signals are continuously drained while asynchronous
ObjectManager queries await replies, avoiding zbus's bounded signal-queue/reply
deadlock. Connection, subscription, queries and deregistration have two-second
deadlines; shutdown is checked every 100 ms. A draining clone keeps cleanup replies
deliverable while the match is removed before disconnect. Diagnostic BlueZ RPCs
are also bounded. Stage-specific shutdown logs record worker joins and Core exit.

Final installed recording pause/resume and close/reopen are user-confirmed and
log-verified; car-profile-loss policy and stalled-RPC/signal-flood shutdown have
local fixtures. Historical hang attribution remains probable, without a backtrace.

## Native PulseAudio introspection (2026-10-07)

`runtime/pulse.rs` replaces the sandbox-inaccessible pactl observer. Rust links
libpulse directly and confines context/mainloop/operations to the existing
diagnostic worker. Each sample opens the local `$XDG_RUNTIME_DIR/pulse/native`
Unix socket (UID runtime fallback when unset), with NOAUTOSPAWN. Nonblocking
iterations share a two-second connection/query deadline and check shutdown every
iteration. A new connection per sample permits recovery after daemon failure.

Concurrent sink-input/sink list queries must both finish successfully. At most
32 entries per list are processed; overflow, timeout, denial or connection loss
produces an unavailable record. Callback data is borrowed only during dispatch;
technical fields are copied into bounded summaries and arbitrary strings are
not persisted. Volume is the numeric channel min/max percentage. Stream matching
retains the exact application.name=PlexFreq whitelist. Sink names are classified
as bluetooth/null/native-other but never emitted. Indexes correlate stream routes
with sink state, mute, actual/configured latency and sample specifications.

Cancellation disables both callbacks before their stack userdata is destroyed;
context teardown precedes mainloop teardown. No observer creates an audio stream,
starts a daemon or changes routing/volume. C++/QML and playback policy retain their
existing roles. `examples/pulse-diagnostics.rs` provides a read-only probe of this
same code. Phone rootless smoke/probe pass; updated normally installed Sailjail
execution remains pending RPM installation.

## Installed diagnostics boundary / afternoon incident (2026-10-07)

Normally installed Sailjail execution has now been inspected: diagnostics-v2
audio/BlueZ records work, but `/usr/bin/pactl` is absent from the running app's
filesystem namespace although it exists over SSH. Thus `runtime/platform.rs`'s
subprocess observer cannot currently retain installed-app PulseAudio snapshots.
Rootless log/transport tests do not cover that boundary. The native libpulse
replacement above uses the existing Audio permission's socket access and retains
technical-field filtering off the audio/GUI actors.

Pipeline position and `heard_ms` are output-clock-derived estimates, not acoustic
measurements. The 15:46 incident has advancing position, healthy local decoder
queues and no pipeline warnings while the user reports stutter. BlueZ active
transport counts likewise do not guarantee successful packet delivery. Correlated
ElectricEel BLE failures and recovery following service/application restarts favor
shared-stack trouble but do not isolate its origin. Playback/disconnect policy is
unchanged by this read-only investigation.

## Rotating multi-day diagnostics (2026-10-07)

Rust `diagnostics.rs` captures framework and Rust stdout/stderr through a process
pipe, assembles bounded lines, redacts URLs/token assignments, and writes each record
to its current local calendar date. Timestamps include UTC offset, monotonic elapsed
time and PID. Rotation runs independently of Qt event delivery. The Qt adapter
selects Documents/PlexFreq or the existing AppData/logs fallback and owns one opaque
log handle until backend and application teardown have completed. Shutdown restores
stdout/stderr and joins the reader after draining queued records.

Seven log dates are retained, with an 8 MiB active file and one 8 MiB backup per
date (about 112 MiB maximum under normal operation). An oversized unterminated line
is discarded through its newline, keeping memory bounded without persisting split
secrets. Isolated-state tests do not enable persistent user logging.

Audio diagnostics log controls, source class/resume, pipeline state/warnings,
clock/latency events, HTTP status/offset/bytes, audible transitions and PCM queue
levels. Snapshots and a two-second no-position-progress detector report stall and
recovery without changing transport policy. Active heartbeats are ten seconds;
idle heartbeats sixty. Repeated decoder/output warnings are sampled after their
first ten occurrences while total warning/QoS counts remain in snapshots.

A separate Rust platform worker observes BlueZ transport counts, load and whitelisted
PulseAudio stream/sink fields. Read-only pactl queries are capped at two seconds and
256 KiB; unavailable/denied snapshots are logged. These queries never run on audio
or GUI actors and do not alter routing, pairing or services. Sampling wakes early
on playback-state changes. Normally installed sandbox access is a separate runtime
check; the app logs its own pipeline diagnostics even if pactl access is unavailable.

## Overnight diagnostic scope (2026-10-07)

Persistent Sailfish logging opens the dated file once at application startup.
An app retained overnight continues writing to that startup-date file; there is
no midnight rotation in the investigated build (superseded above). Output bus errors/clock loss and HTTP retries
are logged, but downstream Bluetooth starvation without a bus error is not
positively identified by these records. The OS owns Bluetooth transport/routing;
PlexFreq's observer only applies disconnect pause policy. A default PulseAudio
sink is not necessarily the route of the existing PlexFreq stream.

## Car-trip playback resilience and branding (2026-10-06)

Play explicitly resumes the Rust output pipeline before applying PCM backpressure,
so a full paused queue can drain without reloading the track. Output bus handling
recovers CLOCK_LOST through PAUSED/PLAYING only while play is desired and services
LATENCY recalculation. Bluetooth reconnection itself still does not request play.

Output appsrc retains at most about 0.75 s of PCM (0.5 s scheduling watermark),
with bounded eight-block actor batches to recover from delayed wakes. PulseAudio
requests 300 ms buffering and 20 ms latency. Each current/successor decoder retains
two seconds of PCM plus its crossfade tail; HTTP input holds up to 4 MiB each.
Original-stream HTTP retries are cancellable and bounded to three (1/2/4 s), resume
only delivered bytes and require matching validators at nonzero offsets. They do
not turn unfinished cache files into playable files or byte-resume transcodes.

Sailfish daily diagnostics fall back from Documents/PlexFreq to
`<AppDataLocation>/logs` if the Documents directory is unavailable in the sandbox.
Generated PNG launcher assets and an embedded Qt icon derive from root `logo.png`;
the regeneration tool is `tools/build-icons.py`. Rust retains all transport policy.

Shared TextLink.qml renders compact underlined Show all and Read more/less actions
on both platforms. Native adapters supply font/color and minimum touch height;
the shared component handles mouse, keyboard and accessibility activation.

Unfiltered title-sorted artist browse publishes its first bounded 100-item Plex page
immediately. Session then requests subsequent pages automatically; runtime marks
their nonzero offsets as background work and EntryModel appends them. The alphabet
rail uses server offsets without issuing replacement requests. A shortcut beyond
the loaded prefix remains pending until its page arrives. The rail's current letter
follows the first card in the visible row; touch selection is a temporary preview.
Footer grid access is explicit because QML IDs inside the footer component are not
in the outer ListView's runtime scope.

Rust marks verified `music.recent.added.<section>` and
`music.recent.played.<section>` discovery hubs after validating the numeric suffix.
Both shells retain the flattened outer model, section header and Show all action;
the first item in either contiguous hub hosts a noninteractive nested card grid and
the remaining duplicate outer rows collapse. Grid and host both clip; the host adds
bottom separation after the measured card rows so the next section cannot overlap.
Other Discover hubs remain rows.
Standalone added/played collection routes also use the route grid and now participate
in runtime listing append semantics. Favorites remains a list.
Sailfish's thin translation adapter falls back to read-only `/etc/locale.conf`
LC_MESSAGES/LANG when the launcher supplies no locale environment. Explicit
PLEXFREQ_LANGUAGE or environment locale retains precedence; codeset suffixes are
removed for catalogue lookup. No system settings are written.

Discovery group headers reserve title width from the button's actual left edge.
Sailfish overrides SectionHeader's built-in x margin and clips long titles; desktop
uses its layout's width allocation without extra left padding. Show all is a shared
translated label in Navigation. Qt5 fixture geometry now uses the production-like
QQuickView hierarchy and SDK-only quick-private polish; no private Qt dependency is
added to the production application.

## Shared navigation/action catalogue

`app/qml/shared/Navigation.qml` is the common presentation catalogue for the three
primary destinations, page actions and secondary destinations. Stable IDs define
order; `definition(id)` supplies translated labels, selection, visibility and
enabled state. `activate(id)` checks the current capability before forwarding to
Session or the existing backend command. Offline and Back shortcuts use the same
definitions. Session owns route/history transitions; Rust owns data and enforcement.

Desktop's `NavigationMenu.qml` uses Controls2 Instantiator/insertItem/removeItem;
its primary row uses Repeater. Sailfish's LibraryPage uses native MenuItem and
BackgroundItem repeaters. Both adapters handle only layout and native signals for
Connection/Back. Sailfish supplies retained BrowsePageView DTOs, active-page state
and whether the page can navigate root destinations. Stable ID models keep widgets
alive when backend busy/capabilities/labels change. This replaces duplicated primary
and main-menu definitions in the platform screens.

Silica primary captions/highlights reference their destination control ID explicitly:
BackgroundItem's default content parent is an internal item, not the control itself.
The shared catalogue is now running in the phone's rootless bundle. Native stack
and shared-policy checks pass; geometry fixture supplies the real Silica context
and explicitly polishes a controlled Qt window for deterministic size assertions.

Discovery group navigation belongs to section headers. Session's presentation-only
`groupHub` resolves the common advertised hub key from the displayed group DTOs;
ambiguous keys produce no shared link. Desktop uses current DTOs; Sailfish uses
the page-local retained DTOs. Rust still validates and pages the hub endpoint.

Desktop/mobile navigation alignment: desktop now exposes equal-size Discover,
Library and Playlists destination controls in one full-width row. Selection follows
the current route or its parent destination for detail pages. Back belongs to a
separate contextual row. Two menu buttons mirror Sailfish's split: page operations
(⋮) and secondary destinations/settings (☰). Queue is a normal destination, not a
checkable toggle. This supersedes the earlier desktop standalone Home-button layout.

Desktop navigation exposes Discovery Home directly at every connected destination,
one Back control and a Library entry point; Queue is a menu destination. Session's
`goHome` can reset history, while `browseLibrary` distinguishes entering Library,
refining the current browse and a fresh browse. Existing no-argument home/browse
entry points retain their behavior. Detail headers own detail titles, library
selectors own category labels; contextual headings cover discovery/queue/collections.

Discovery is the default initial destination after Session resolves music libraries.
Sailfish Library uses an inline scrolling header and a collapsed control summary;
IntrinsicLoader expands search/browse/sort on demand. Header/tabs use the full page
width, while only list rows reserve space for the alphabet rail. That rail is a
list-viewport child bounded below the visible header and above the playback dock.

Sailfish destinations (2026-10-05): root headers expose Discovery Home, Library and
Playlists directly. Pull-down contains contextual actions, pull-up secondary
destinations/settings, and the playback dock exposes Queue. Browse/sort controls
are library-route-only. An active page releases its retained presentation on a
matching route completion, even when unrelated work remains busy; idle transitions
retry restoration. Deactivating/off-stack pages keep their transition snapshots.
Navigation remains in Session, data/grouping in Rust.

## Current runtime (Rust-owned, 2026-10-04)

Next recommendations (2026-10-05): `refresh.rs` serializes automatic metadata plans
on a separate Rust thread using read-only planner state. Core commits current
namespace/recipe/generation results, honoring network gates/backoff. `catalogue.rs`
owns reusable normalized text/order; snapshot stamp, audio revision and plan epoch
invalidate it. `filters.rs` owns validated scopes/choices/query URIs and smart
mutations; `discovery.rs` owns scoped layout, hub confinement, measured insights
and multi-waypoint paths. Qt/QML only edit and present these DTOs.

`bluetooth.rs` tracks audio accessories; a BlueZ signal worker pauses the independent
audio handle on tracked disconnect. Read errors/idle are not disconnects; reconnect
never auto-plays. Async match removal precedes connection teardown. Bluetooth
permission is declared. PCM clipping uses decoded segment limits to honor container
priming/padding independently of Plex DB duration.

This expansion is deployed in the phone's existing rootless bundle. Final local
gates and Qt5.6 controller/navigation/MPRIS checks pass; live read-only filter
choices and genre/year browsing pass. Smart mutations, physical Bluetooth
disconnect behavior and installed sandbox permissions have distinct validation
scope; see validation.md.

Sailfish navigation (2026-10-05): each stacked library/detail page has a
`BrowsePageView` presentation snapshot and a stable, page-local `EntryModel`.
Session emits `beforeViewChange` before mutating the shared route or submitting
a replacement request. Outgoing views retain their heading/detail/rows/alphabet
through the native transition; deactivation also retains content for back/peek.
Back restoration keeps that snapshot until the matching list request completes.
Actions and playback still use Session/Rust; these snapshots contain presentation
DTOs only and are not a second queue/navigation policy implementation.
The snapshot adapter is validated against native Silica push/pop/reload and deployed
in the phone's user-owned rootless bundle; see validation.md for exact runtime scope.

Daily-use expansion: `src/quality.rs` owns quality policy/progressive transcode
URLs and session confinement; `src/daily.rs` owns discovery, Sonic Adventure and
bounded durable download plans. `Settings.quality` and `download_plans` default
cleanly for old sessions. Cache entries/partials include representation quality
so original and transcoded media do not share a cache key. Transcoded partials
restart rather than byte-resume a freshly generated representation.

Streaming still prefers complete local files. Wi-Fi/mobile caps select subsequent
remote plans; explicit downloads use their own cap. A progressive decoder uses
server time offsets and a fresh session for every load/seek. A bounded, Rust-owned
cleanup worker closes discarded sessions without network waits in the audio actor;
shutdown performs bounded best-effort cleanup. Cache download cleanup stays on
the cache worker. The diagnostic `probe_quality` reads at most 256 KiB in memory
and stops its own server session; it does not play/scrobble or persist that audio.

Discovery returns flattened Rust DTOs tagged with server hub titles; Qt only
exposes a `groupTitle` presentation role. Listing generations also cover discovery,
sonic neighbors/adventures and paged offline search. Station metadata tolerates
PMS's string-valued radio flag. Download plan specifications retain source, duration
and refresh time separately from ordered track membership. Planning/refresh is
transactional with respect to group membership and never replaces the playback queue.

DSP retains both track/album gains, switching gain policy on the audio actor.
Auto selects album gain for identified single-album queues/album radio, track gain
otherwise. Optional 0–12 dB headroom precedes the existing sample clamp. Idle/paused
audio control waits are 100 ms and command-wakeable; active scheduling remains 5 ms.
MPRIS polling copies a small projection instead of full queues and publishes
Next/Previous capability changes. Physical headset/call/routing tests remain distinct.

Automation uses independent Rust/Qt/QML/i18n/Sailfish-cross jobs in CI; release
packaging reuses these gates. Qt checks use local/offscreen/fake-sink fixtures,
distinct from phone routing and live-account validation. Production-review fixes
(2026-10-04): queue/cache limits enforced in Rust, malformed server entries
skipped, artwork pending epoch-guarded, audio checkpoints overflow without
blocking, ConnMan wifi uses the primary route, shared `mutex_lock` helper owns
poison recovery.
Review follow-ups (2026-10-04): natural-end queue edits preserve occurrence
identity; offline detail fallback is paged; replacement-aware snapshot quotas and
temporary-file reclamation/accounting cover interrupted atomic writes. Streaming
and cache share Content-Range/validator helpers; full-response seek-prefix skipping
requires a matching validator. List generations are checked both on execution and
event delivery, including failures, and page completion carries `_pageStart`.
Qt polls at 100 ms; download rows reuse one validated cache-status snapshot.
Hardening (2026-10-04): corrupt queue cursors/orders rejected without worker
panics, unknown-duration resume/position preserved, track gain preferred
globally, bad next-track prepare keeps current audio, empty station keys fall
back to sonic, MPRIS transport independent of list busy state, audio pipeline
uses fallible construction, shared atomic-write/numeric helpers.

```
Qt5.6/Silica or Qt6/Controls QML
 → small QObject/model/translation/window bridge (no QThread or QMediaPlayer)
 → Runtime C ABI submit/poll
 → Rust Core request actor + independent Rust audio actor/HTTP readers
 → Symphonia decoder threads → Rust resample/PCM/DSP → libpulse playback stream (music role)
 → Rust zbus MPRIS and read-only network workers
```

Rust owns worker lifetime and maintenance, request classification/generations,
queue/radio/successor preparation, media HTTP/header credentials, decoded-frame
scheduling, transport/position/heard-time, gain/EQ/crossfade, MPRIS and network facts.
Qt only exposes properties, reconciles presentation models, translates UI/error
messages and activates the window. Playback URLs/PCM no longer enter Qt audio APIs.
Artwork still has a Qt Image fallback until its private Rust cache is ready.

The blocking Core HTTP domain cannot stall audio transport/output. Prepared next
queue snapshots are committed when their PCM becomes audible; the GUI can read
audio snapshots independently of Core metadata work. One successor is predecoded
with bounded buffering. PCM output is fixed 48 kHz stereo F32; EQ/gain/sample-clamp
are Rust math and crossfades use equal-power curves. Output negotiation through
PulseAudio is not a bit-perfect/sample-rate-matching claim.

Translation catalogues and the build/check dictionary workflow are in
`app/translations`, `tools/translations.json` and `tools/build-qm.sh`. Full locale,
base language and English fallback is installed before QML. All 39 reference
locales are packaged and embedded; context/placeholder coverage is checked.

Earlier sections below record the previous Qt Multimedia baseline and how it was
extended; `docs/audio-engine-plan.md` describes the replacement and its limits.

Native stream role is selected in Rust from OS identity: Sailfish x-maemo for its
mainvolume/stream-restore policy; desktop music. The app does not intercept hardware
keys or alter system policy. Reply correlation includes failures for optional lyric/
similar-artist requests; QML rejects stale keys and completes current loading states.
Seek sliders synchronize idle values from playbackChanged after native drag handlers
remove their initial value binding, retaining QML-only presentation ownership.
Plex optional gain/albumGain analysis accepts finite JSON numbers/numeric strings;
unusable gain is omitted instead of invalidating track and lyric metadata.
Optional Plex gain/albumGain metadata accepts finite numbers and numeric strings;
missing/unusable analysis must not prevent track/lyric metadata retrieval.

```
Silica (Qt5.6) / Controls2 (Qt6) QML
        ↓ shared Session.qml navigation + browser login polling
Backend QObject — Qt Multimedia platform adapter
        ↓ queued work / queued completion
CoreThread — one Rust handle, lifetime confined to worker
        ↓ opaque C ABI: pf_new / pf_call / pf_free / pf_string_free
Rust Core → Plex HTTP client, normalized models, session store, playback queue
```

Rust is the application backend. C++ contains no Plex endpoint, token storage,
queue ordering or discovery policy. The native decoder/output adapter is Qt
Multimedia so Sailfish's existing media integration is available without a
privileged service. Shared UI logic only manages navigation and maps user input
to backend commands.

## Source map

- `src/plex.rs`: request headers, bounded HTTP, PIN/discovery and origin-safe URLs.
- `src/model.rs`: typed Plex JSON structures.
- `src/queue.rs`: selection, shuffle permutation/history, repeat/off/one/all.
- `src/store.rs`: durable atomic session storage; client UUID persists.
- `src/lib.rs`: commands, music pagination, metadata resolution, stream planning.
- `src/ffi.rs`, `app/src/plexfreq_core.h`: paired ownership contract.
- `app/src/backend.*`: worker lifetime, Qt5/6 player adapter, QML state.
- `app/qml/shared/Session.qml`: navigation/history, paging, browser authorization.
- `app/qml/desktop/`, `app/qml/pages/`: platform-specific views.

## Command contract

Requests are JSON objects with `op` in snake_case, parsed into `Command` at
runtime admission (invalid requests are rejected synchronously). Returns
`{"ok":true,"data":{...}}` or `{"ok":false,"error":"human-readable"}`.

Operations: `status`, `login`, `poll_login`, `servers`, `select_server` (index),
`connect` (url/token), `libraries`, `browse` (section/kind/query/start), `children`
(key/start), `playlists` (start), `play` (items/index), `select_track` (index),
`next` (automatic), `previous`, `shuffle` (enabled), `repeat` (mode), `logout`.
`play` enqueues the loaded tracks only; use Load more to expand before playback.
Metadata is fetched on demand when a track lacks media parts.

User skip ignores repeat-one; automatic EndOfMedia honors it. Shuffle retains
the playing track and visits every track once in the generated permutation.
Previous follows that permutation. Queue ends with no selected track; repeat-all
wraps. Pause/seek/volume act directly on the audio adapter, independently of HTTP.

## Threading and memory

The QThread's `run()` initializes, calls and frees Rust. Inputs and outputs are
copied byte arrays; no borrowed pointer crosses a thread. Every returned Rust
string is freed by Rust. C ABI catches unwinds so no panic unwinds into C++.
Shutdown waits for the bounded request to finish and discards pending requests.
Large-library requests use bounded server pages. Artist browse displays page one,
then fills automatically through background page requests; other large views remain
scroll-triggered incremental pages. UI disables foreground request-producing actions
while a command is pending.

## Extension boundaries

Add backend behavior and tests in Rust first. Keep native audio integration in
the shim or add a dedicated Rust playback engine once gapless/DSP requirements
justify it. Add device-facing capabilities only after checking the Qt5.6 API and
permissions. Do not import Silica into shared/desktop QML.

## Cross-platform runtime fixture

`tests/qt_backend.cpp` runs unchanged against desktop Qt6 and device Qt5 using
the same Rust library and `Backend` source. `tests/sailfish/` supplies a separate
qmake/spec build; `tools/build-device-tests.sh` builds it without adding QtTest
dependencies to the production app. It runs from a user-owned device directory,
serves generated media over loopback and keeps session state in QTemporaryDir.
This isolates the native audio adapter from real Plex credentials and content.

## Account response handling

Plex account resources are heterogeneous. `Plex::resources` filters the exact
`server` capability before decoding the strict server fields (name, identifier,
access token, connections). Player tokens can be missing/null and are irrelevant
to music-server discovery. JSON failures carry only static operation labels,
never raw response/error values. UI server summaries still omit credentials.

The `savedAccountDiscovery` native test is explicitly opt-in through
`PLEXFREQ_ACCOUNT_CHECK_STATE_DIR` and a selected QtTest case. It checks an
existing authorized account without collecting credentials into fixture files;
default local checks skip it and use synthetic loopback responses.

Server choices are presented as prominent selection cards under a dedicated
heading in each connection screen. The Sailfish page uses native BackgroundItem
press handling; desktop uses ItemDelegate inside a scrolling panel. The UI still
passes the selected resource index to Rust; it does not choose connection URLs
or handle server tokens.

## Radio and streaming

`src/radio.rs` owns radio-kind/source metadata, station-key discovery and safe
source URI construction. `Core` starts/extends radio transactionally: an
unsupported seed or failed continuation retains the previous track/queue.

- Station sources retain a server playQueueID; tracks carry playQueueItemID.
  Refills exclude already-loaded occurrences, not repeated song ratingKeys.
- Sonic sources retain a last seed and up to 200 recent seed IDs. Track radio
  chooses same-type nearest tracks; album radio expands one whole next album.
- Queue history retains up to 100 tracks behind the current selection after
  advancement. Future metadata is loaded in station windows or full album groups.
- QML sends `radio` with numeric key and artist/album/track kind. `stop_radio`
  clears playback. Responses include a small `radio` summary (kind/title/source).
- Qt reports its actual station state/time through `timeline`. Rust supplies the
  server queue/occurrence fields and owns the HTTP request. A ten-second native
  timer drives periodic reporting. Native transition/end events report the completed
  station occurrence as stopped with `continuing=1` before successor preparation or
  an end-of-window refill; terminal automatic refill failures are surfaced rather
  than silently parking playback. No QML HTTP/media policy was added.

Only the selected track's media is handed to Qt. Queue/radio preparation fetches
metadata, not audio. There is no offline cache; sequential Qt playback still does
not guarantee gapless transitions. Radio continuation can incur network latency.

Installed Sailfish launches append stdout/stderr diagnostics to the private
daily file `~/Documents/PlexFreq/plexfreq-YYYY-MM-DD.log`. Isolated-state smoke tests
do not write this user log. Decoder and output errors include the underlying
Symphonia/libpulse reason while the user-facing response stays stable and
translated. Remote media credentials never reach a decoder: Rust's HTTP reader
supplies bytes through a Read/Seek window (GStreamer appsrc before 2026-10-16).

## Artist-first navigation and detail models

- Root route is always an artist search/list. Artist activation opens a `detail`
  route with artist metadata and album children. Album activation opens another
  detail route with album metadata and tracks.
- `detail(key,start)` fetches metadata on the first page only; subsequent child
  pages append without clearing the header. Keys are validated numeric IDs.
- Item models retain summary, art, Image URLs, year, track index and disc index.
  Rust normalizes an authenticated photo gallery, deduplicates URLs and rejects
  cross-origin paths. Optional detail values can be missing/null; metadata numbers
  are signed. QML only presents returned fields.
- Shared Session owns route/history and emits navigation intent. Sailfish maps
  that intent to separate Artist/Album stack pages and restores parent routes on
  return. Desktop reuses the same history with an in-list header.
- `RadioIcon.qml` is a QtQuick2.6 scene-graph antenna/waves, wrapped by each platform's native button.
  Radio seed type is retained explicitly; a lifetime subscription is not inferred
  from a per-item analysis field. The recommendation endpoint decides availability.

Detail headers use the shared `IntrinsicLoader.qml` size contract: explicit
width, height from the loaded item's **implicitHeight**, and clipping to that
region. Never bind Loader height to its item's actual height, because Loader
assigns that height back to the item. The following list rows need the full
intrinsic header size for placement. Row artwork is constrained to row bounds.

## Audio cache

`src/cache.rs` owns the file/index/download policy. The initial private cache is
`<state-dir>/audio-cache` (normally under per-user AppData), with a 512 MiB default
budget and a five-track look-ahead. This is not a Plex Sync/download-queue client;
it directly caches the original Part representation already used for playback.

- Separate Rust thread/current-thread Tokio runtime, async reqwest and cancellable
  generation watch. Plex commands/UI do not wait for whole media downloads.
- Jobs are replaced as playback moves; order follows the queue's actual shuffle
  permutation. An explicit download list is retained in the current Core and
  merged with priority current/upcoming tracks.
- Strong ETag or Last-Modified If-Range, validated Content-Range/length, identity
  encoding, served-representation size checks, and safe restart when a server returns 200
  instead of the requested partial range. Non-audio error responses are rejected.
  Partial files are never handed to the decoder.
  PMS database Part.size is a hint, not the completion authority: the observed
  server serves a slightly different byte count. Committed metadata retains the
  actual received size.
- 0600 media/metadata in a 0700 directory; SHA-256 namespace includes origin/token
  and file key includes ratingKey/Part path. No raw token or authenticated media
  URL is stored in the index. Different credentials/origins do not share entries.
- Atomic completion rename and index commit; completed-file length checked on
  lookup. LRU eviction protects current/upcoming/explicitly requested tracks.
  The limit includes partial audio, not small JSON/lock-file overhead.
- Process-exclusive cache file lock. On drop, cancel jobs, close the job channel
  and join the worker before releasing the lock. Interrupted-job metadata permits
  startup recovery. Diagnostic `pf_new_inspect` opens read-only cache state and
  never starts media jobs or modifies LRU state.
- Core prefers local completed files before metadata/network resolution. Cache
  catalogue is persisted. The C++ adapter can retry a failed remote source from
  a completed local file and restore position after decoder loading.
- Cached station playback does not contact the timeline endpoint; offline play
  history sync remains future work. New radio recommendations require a server.
- Cache status uses a separate QObject `cache` property/notification so polling
  does not reload biographies/gallery/models. Passive polls preserve user errors.

Commands: `cache_status`, `cache_config` (enabled/limit_mb/ahead), `cache_tracks`,
`cached_tracks`, `cached_playback`, `clear_cache`. Artwork/library snapshots,
permanent download pins, complete queue/session persistence, and network-type
controls are separate follow-ups.

## Reactive UI and browsing updates

The C++ adapter tracks total work separately from foreground busy and loadingMore.
Passive polling/timeline/alphabet requests do not alter user-visible busy state.
Cache notifications and generic state changes are emitted only for actual changes.
EntryModel is a presentation adapter over immutable Rust-returned QVariant DTOs:
append notifications and identity-based reconciliation preserve existing rows.

List-request generations reject stale results after navigation. Explicit replace
markers distinguish a letter jump/back-restored nonzero page from incremental
append. Shared Session deduplicates pending page starts, debounces artist search,
and snapshots detail metadata independently of generic list/state updates.

Rust `alphabet(section)` reads/caches server firstCharacter groups with artist
type/titleSort ordering. Session uses those offsets against the incrementally filled
artist model, retaining a target until enough background pages have arrived.
The legacy `jump_artist` command remains protocol-covered but normal UI shortcuts no
longer replace the model. Filtered searches hide the rail. Recently added albums and
recently played tracks share the responsive artwork/title grid; their normal paging
starts at 75% and preserves the outer scroll offset while cards are appended.

## Similar-artist navigation

`similar_artists(key)` is an optional Rust metadata request to the documented
PMS `/library/metadata/N/similar` endpoint. Its separate response contains
`similarKey`, `similarArtists` and `similarAvailable`, never a replacement `items`
list. Results are deduplicated numeric library artists, capped at 12, with the
existing authenticated/origin-confined artwork normalization.

Session requests similarity after the initial artist detail succeeds and keeps
it separate from detail snapshots. C++ treats this request as passive, excludes
its data from global state, suppresses global errors and discards replies from
older view generations. Both platform headers activate cards through the same
artist route/history path as normal library entries. Empty/unavailable similarity
does not prevent browsing the artist's albums.

## Queue persistence and media controls

Private Settings now holds raw playback state (Queue, exact shuffle traversal,
current occurrence, repeat, Radio, position and playback generation) and persistent
download plans/groups. Queue DTOs follow actual playback order and normalize
artwork on demand; generated framework URLs never enter session snapshots.
`enqueue`, `enqueue_album`, `queue_move`, `queue_remove`, `play_album`, `resume`
and `save_playback` keep this policy in Rust. Removing the final active occurrence
stops rather than replaying an earlier track. Playback generations reject stale
position checkpoints, including consecutive occurrences of the same rating key.

The C++ adapter checkpoints native position periodically/on pause/seek/shutdown,
and restores position only after native media becomes seekable. Startup publishes
the saved track without a decoder URL or autoplay. CoreThread drains accepted
requests at normal shutdown so the final checkpoint can finish.

`app/src/mpris.*` exports Root/Player adaptors on the session bus under
`org.mpris.MediaPlayer2.plexfreq`. Typed metadata and capabilities reflect Backend;
transport methods invoke existing adapter/Rust commands. Position/length are
microseconds, SetPosition checks occurrence identity and Seeked reports seeks.
Authenticated URLs are excluded; only cached local artwork is exported.

## Now-playing, discovery and offline metadata

Shared Session correlates lyrics to the current rating key, with passive requests
and separate lyric state. Platform-specific now-playing views remain within their
own import sets. `src/lyrics.rs` parses plain/LRC text, multiple timestamps and
offsets. Rust fetches the advertised Part.Stream type-4 key with a 512 KiB bound.

`search` flattens music hubs from `/hubs/search`; `collection` supplies paged
favorites/added/played views; `favorite` uses a documented rating PUT. Local DTOs
and cached snapshots update after successful rating changes. The played view is
PMS history; ordinary and offline playback history synchronization is still a
follow-up beyond existing station timeline reporting.

`src/offline.rs` stores raw credential-scoped request snapshots (64 MiB including
retrieved lyrics). Network/5xx failures may use saved pages; authorization failures
do not silently fall back. Explicit offline mode bypasses Plex and cancels audio
downloads. `offline_browse` merges saved metadata and completed audio catalogue;
details use saved pages or known parent-child relationships. Unvisited data is
not claimed to be present.

A separate bounded Rust artwork worker uses header authentication, rejects
redirects/cross-origin paths and stores private local images (4 MiB/image, 128 MiB
total). Artwork publication is generation-guarded on connection changes/logout.
C++ schedules returned metadata for artwork caching without owning HTTP policy.

Pins retain group membership plus raw track plans. Scheduling protects permanent
pins independently of temporary queue protection; cancellation does not unpin.
Restart rebuilds protection and schedules retained plans. Overlapping album/
playlist/track groups preserve shared downloads when one group is removed.

### Lyric representations and playlist totals

`lyrics::decode` selects plain/LRC versus PMS JSON. The latter extracts
MediaContainer.Lyrics.Line.Span text, including empty stanza lines. Arbitrary JSON
is never treated as display text. Offline retrieval repairs old caches containing
literal JSON through the same normalization path.

Items retain optional leafCount/durationInSeconds; playlist DTOs add totalDuration
in milliseconds. Explicitly empty playlists have zero duration; missing metadata
for nonempty playlists stays null. Shared Session formats total count/time for
list rows and preserves the playlist seed while children paginate.

Track DTOs distinguish albumArtwork (origin-confined parentThumb, falling back to
track artwork). Passive cache status can update a newly available local album
image without reloading the queue. Sailfish CoverBackground binds that image with
bounded geometry above captions and native cover actions.

## Playlist management, history and browsing/mixes

`src/features.rs` implements regular playlist mutation, playlist metadata/items,
chooser pagination, sorted library browsing, bounded album grouping, mix building,
download group presentation/actions and history delivery. Playlist entry identity
is playlistItemID, not ratingKey; smart contents remain read-only. Changed metadata/
item/list snapshots are invalidated. QML dialogs use separate chooser state rather
than replacing the current browser model.

Native playback sampling accumulates elapsed time only when position advances
while playing, and resets the position baseline on seeks. Playback occurrence IDs
survive pause/resume/fallback but change for a new track occurrence. Rust qualifies
listens, persists scoped history and records delivery acknowledgements. Ordinary
remote playback sends timelines; cached/offline playback records a durable outbox
and syncs scrobbles after reconnection. The replay endpoint cannot guarantee original
server timestamps or exactly-once delivery after an acknowledgement is lost.

Artists/albums/tracks can be browsed by title, added time and (where applicable)
year. Optional artist-album grouping loads up to 1000 albums and exposes a separate
albumGroup presentation role. Autoplay asks for actual nearest recommendations,
then can fall back to the advertised artist station. Multi-seed mixes interleave
shuffled per-seed pools and remove duplicate tracks; this policy is Rust-owned.

## Network-aware download cancellation

QtDBus observes ConnMan/NetworkManager read-only and reports connection facts. Rust
owns Wi-Fi-only/pause policy and the download scheduler. `DownloadControl` is a
separate Arc-owned cancellation/gate handle cloned on the Core thread; the C ABI
permits concurrent fact/policy hints without borrowing Core on the GUI thread.
CoreThread coordinates its lifetime under its request mutex. Accepted early hints
are retained, and stale queued observations do not overwrite newer atomic facts.

The audio worker checks the gate before batches/jobs and selects cancellation
during HTTP; artwork jobs are generation-invalidated (an already active bounded
image request may finish but cannot publish after cancellation). Core stays
thread-confined. GUI policy observation never changes system network settings.

See `audio-engine-plan.md` for the current Rust/C++ boundary and a proposed future
Rust-controlled audio pipeline. The current engine remains Qt Multimedia.

## Session persistence split (2026-10-15)

Session state is split: `session.json` (small settings and checkpoint, written
atomically only when its bytes change), `queue.json` (queue + Radio, rewritten only
when the queue's revision (replaced by every `Queue` mutation) or the Radio
fingerprint changes; its revision is mirrored in `session.json` and
a mismatch drops the stale position), and append-only `listens.jsonl` /
`history.jsonl` (newest record per namespace+occurrence wins on replay, replay uses
the same cap eviction as memory, damaged lines are skipped, atomic compaction when
damaged, above 2x cap or diverged). A checkpoint writes at most one log line (with
`sync_data`) plus ~0.7 KB `session.json`. The owner migrates old all-in-one sessions
at startup by writing the new files first. `Core::inspect` never writes.

## Shared I/O runtime (2026-10-15)

One process-wide tokio current-thread runtime (thread `plexfreq-rt`, `src/rt.rs`)
runs D-Bus observers, MPRIS, the diagnostic heartbeat, cache/artwork downloads and
audio HTTP readers as tasks; filesystem and libpulse steps use the bounded blocking
pool (`rt::unblock`). Core stays synchronous and drives Plex requests (one pooled
async reqwest client) with `rt::block_on` on its own thread. Owners stop tasks
deterministically (Halt watch where async D-Bus cleanup is needed, abort otherwise);
zbus (tokio feature) runs its internal tasks on the shared runtime. No blocking
filesystem/D-Bus/libpulse call may run on `plexfreq-rt`; use `rt::unblock`. MPRIS emits on an audio transport change
watch (position excluded), model revisions and seeks, with a 1 s fallback. View-bound
commands abort in-flight HTTP when navigation moves past their view; the reply is
discarded. The audio actor (which also dispatches its libpulse playback mainloop),
one decoder thread per current/prepared track, libpulse recording mainloop, log
capture and Core worker remain dedicated threads.
Decision (2026-10-15): stay on one current-thread runtime. Throughput is ample; the
risk is a blocking call landing on `plexfreq-rt`. If audio stalls or I/O latency
show up on the phone, switch to a 2-worker multi-thread runtime (tokio feature
`rt-multi-thread`, builder change in `src/rt.rs`; tasks are already `Send`).
