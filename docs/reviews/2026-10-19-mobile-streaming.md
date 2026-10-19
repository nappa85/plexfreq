# Mobile streaming robustness — review record

## Context (2026-10-19 phone log, read-only)

The user started an artist station (October Tide) at 08:05:18, 40 s after the
phone fell back from Wi-Fi to mobile data (`network change wifi=false
online=true` at 08:04:38). Mobile quality was `mobileKbps: 0` (original).
- **Current track:** the original FLAC, 54.6 MB over 7:13 (about 1 Mbit/s).
  The gapless successor (24 MB) started its HTTP download 1 s after the current
  track's decoder was created, before the current track had even opened.
- **Interruptions:** "audio stream interrupted" appeared repeatedly, with 63
  ranged retries through about 08:15 at 1-4 s backoff each. There were 4
  stalls and 6 output underflows in the first 16 s. Playback re-uncorked
  after each stall as soon as any PCM was pushed, and PulseAudio's prebuf is
  about 300 ms. The result was repeated short stutters.
- **Later tracks** played from the cache-ahead copies.

User decisions (2026-10-19):
- plan the three edits:
  - delay the successor pre-load;
  - require more buffered audio before resuming;
  - pause cache-ahead while the live stream is starving;
- set the mobile-data default to 320 kbps.

Code facts (exploration, `src/audio/*`, `src/runtime*.rs`, `src/cache.rs`,
`src/runtime/platform.rs`):
- **Successor pre-load:**
  - `runtime::prepare` → `engine.handle.prepare(Some)` →
    `Actor::prepare` → `Decoder::new` → `http::Reader::new` starts the
    successor's HTTP at once.
  - There is no buffer or remaining-time condition.
  - The reader reads at most 4 MiB ahead (`CAPACITY`).
- **Decoding and output:**
  - Decode-ahead is 2 s plus the crossfade tail.
  - Any pushed block uncorks.
  - Pulse uses `tlength` 300 ms and `prebuf` = tlength.
- **Retries:**
  - Original files: unbounded once bytes arrived; backoff 1/2/4/8 s.
  - **Transcodes (progressive): never retried.** The runtime fallback after a
    failed transcode tries only the cache, then reports.
- **Cache-ahead:**
  - One worker, one item at a time.
  - Gated only by pause and Wi-Fi-only; nothing reacts to live buffering.
- **Network facts:**
  - ConnMan: the first ready or online service's `Type == "wifi"`.
  - NetworkManager: `PrimaryConnectionType == "802-11-wireless"`.
  - The app keeps only `network_wifi`, so every non-Wi-Fi link counts as
    mobile. That includes Ethernet and an unknown network (initially false).
- **`quality.streaming(wifi)`** has two users:
  - the live source (`lib.rs:911`);
  - cache-ahead suppression when capped (`lib.rs:987`).

## Plan M1

### 0. Mobile default 320 kbps, applied to cellular only
- **Default:** `quality::Config::default().mobile_kbps = 320`. Stored sessions
  keep their explicit value: serde fills only missing fields. QML fallback
  objects (`Session.qml`, `AudioSettings.qml`, `AudioSettingsPage.qml`) use
  320.
- **A new network fact `cellular`:**
  - ConnMan: the default-route service `Type == "cellular"`.
  - NetworkManager: `PrimaryConnectionType` in `{"gsm", "cdma", "bluetooth"}`.
    A `Metered` property of 1 or 3 ("yes", "guess-yes") also counts.
  - Carried through `network_state`
    (`{"wifi","online","cellular","live_hint"}`). It defaults to false when
    absent, so older callers keep working.
  - Stored as `Core::network_cellular`, and replayed after a Core rebuild with
    the other facts.
- **`quality.streaming(...)` takes `cellular`:**
  - mobile kbps only on a known cellular link;
  - Wi-Fi kbps otherwise, including Ethernet and an unknown network.
- **`wifi_only` downloads are unchanged** (they still require Wi-Fi).
- **Cache-ahead suppression** (`lib.rs:987`) uses the same predicate.

### 1. Transcode interruption → restart at the current position
`Event::Error` for a current-track failure whose source is `Transcode`:
- **Restart:** start a new transcode at `snapshot.position`, through the
  existing `TranscodedPlayback` plan with `resumePosition`. That's the same
  path as the original-to-transcode fallback.
- **Limit:** at most 3 restarts per occurrence within 60 s.
- **Then:** fall back to the cache (unchanged), then report.
- **Separate from stage 1:** this counter is independent of the stage-1 gone
  check, which still runs once every source has failed.

### 2. Successor pre-load waits for a healthy current track (engine)
- **Hold the source:** `Actor::prepare(Some(remote source))` stores it as
  pending and does not create the decoder yet.
- **Start when any of these holds:**
  - the current source is local, or its reader has completed;
  - remaining time ≤ `PRELOAD_LEAD` (60 s) plus the crossfade tail;
  - the current decoder has `PRELOAD_HEALTHY` (20 s) of audio ahead.
- **Then:** the pending decoder is created at that point.
- **Local (cache) successors** are created at once, as today.
- **Unchanged:** Seek, Stop and replacement keep their successor handling
  (`take_successor` returns the pending source as `Err(source)` today).
- **Accepted:** on a link slower than the bitrate the successor may start
  late, and gapless can degrade to a short gap. The successor would have
  competed with the current track anyway.

### 3. Resume only with a buffer (engine)
- **Starving:** when the output has drained (`queued_bytes() == 0`, corked as
  today), the actor enters `rebuffering`.
- **Uncork again only when either holds:**
  - decoded PCM ≥ `RESUME_BUFFER` (3 s);
  - the decoder reached end of stream.
- **First start** of a remote track uses `START_BUFFER` (1 s). Local files are
  unchanged.
- **Buffering state:** `publish` keeps reporting `buffering` while
  rebuffering, so the UI and MPRIS are unchanged.
- **Memory:** decode-ahead rises from 2 s to max(2 s, 3 s + tail) only while
  rebuffering.

### 4. Cache-ahead yields to a starving live stream
- **Hold flag:** `DownloadControl` gains `hold(bool)`.
  - The runtime sets it when the engine reports `desired && buffering` for
    more than 1 s, or when a remote current track has less than 5 s
    decoded-ahead.
  - It clears the flag after 10 s of continuous playing.
- **Worker:** while held, the cache worker parks between 64 KiB chunks. It
  waits on the gate, with no cancel and no restart, and the open response is
  kept. If the server times the response out (`read_timeout` 10 s), the
  existing resume/retry applies.
- **Manual pins and refresh** follow the same hold, since they compete for the
  same link.

### 5. Tests
- **Network facts:**
  - `connman_facts` and the NetworkManager mapping (cellular, Ethernet, Wi-Fi,
    metered);
  - `streaming` on cellular, Ethernet and unknown;
  - a missing field defaults to 320, and a stored 0 is kept;
  - cache-ahead suppression only on cellular.
- **Runtime fixture:** a transcode that drops mid-stream restarts at the
  position. Three drops lead to a report.
- **Engine (fake sink, scripted HTTP fixture with throttling):**
  - the successor's HTTP is not requested before the lead or healthy
    condition, and is requested at once for a local successor;
  - after a forced underrun the output stays corked until 3 s are decoded, and
    `buffering` is true meanwhile;
  - a track whose remaining audio is under 3 s resumes at EOF.
- **Cache:** held downloads make no progress while held, resume without a
  restart, and a manual pin is held too.

### 6. Docs and gates
- `docs/architecture.md` (audio, cache, network facts),
  `docs/validation.md`, and the `docs/roadmap.md` Open items.
- `./tools/check.sh`, `./tools/build-sailfish.sh`, then a phone run on mobile
  data.

## Round 1 reviews (plan M1)

Reviewers:
- **Sol** (main chain): 3 Blocking, 10 Major.
- **Fresh Sonnet subagent:** 2 Blocking, 11 Major.
- **Third chain:** unavailable (exit 3). Recorded.

Engine parts 2-4 (findings, kept for the part B plan):
- **Unreachable signals.** The decoded-ahead budget is only 2 s plus the
  crossfade, so the "<5 s decoded" hold trigger is always true and the "20 s
  healthy" pre-load condition can never be met (O B1, O M2, S B1, S M1).
- **Stall detection.** `queued_bytes()==0` is not a reliable PulseAudio
  starvation signal; use the backend's underflow/running facts instead (O B3,
  S M4).
- **Uncork sites.** Every uncork site needs a central resume gate: Play,
  recording release and crossfade (O M4, S M3).
- **Short EOF tails.** A tail shorter than PulseAudio's prebuf, before a
  prepared successor, needs explicit handling (O M5).
- **Pending successor.** It needs explicit lifecycle handling. The
  alternative is to gate `http::Reader` start instead (O M6, S M2).
- **Cache hold release.** The hold has no release on stop, pause or error
  (O M12, S B2).
- **Hold vs cancellation.** The hold must be separate from `Gate::allowed()`
  and stay cancellable, and a held response may still be lost (O M10/M11,
  S m4).

User decisions (2026-10-19):
- **Split the work.** Part A now: default, network facts and transcode
  restart. Part B (engine) gets a separate plan, tested against real
  PulseAudio.
- **320 kbps for new installs only.** There is no migration of a stored
  `mobileKbps: 0` (S M7).
- **An unknown network uses the Wi-Fi setting** (S M9).

Part A findings and dispositions:

| # | Finding | Sev | Disposition (M2) |
| --- | --- | --- | --- |
| A1 | Restart budget keyed to the engine occurrence resets on every restart (`start_plan` bumps `serial`) (O B7, S M5) | Blocking | Keyed to Core's logical `playback.occurrence` (UUID, unchanged by same-item fallbacks), with a rolling 60 s window |
| A2 | Restart before cache wastes time; startup failures with no audio should go to the gone check (S M6) | Major | Order: cache, then restart, then today's path. A restart needs audio already heard in this occurrence |
| A3 | `TranscodedPlayback` is gated by `codec_fallback`, unrelated to a quality-selected transcode (O M8) | Major | A separate internal `RestartTranscode` command, allowed only when the current plan is a transcode |
| A4 | Restart must respect user intent (Pause/Stop/Next after the failure) (O M9) | Major | The existing `Playback::superseded(acted)` check at fire time; the paused flag comes from the event |
| A5 | Restarts need a backoff (S M5) | Major | First restart immediate; then 2 s and 4 s via a worker deadline, rechecked for supersession at fire time |
| A6 | QML `old.mobileKbps || 320` would overwrite an explicit 0 (S M8) | Major | `=== undefined ? 320 : value` in the three QML files |
| A7 | Cellular vs metered conflated; Bluetooth, VPN (O M13, S M10) | Major | `cellular` = link technology only: ConnMan `cellular`; NM `gsm`/`cdma`. Metered is not used, and Bluetooth/VPN are not cellular (documented). ConnMan skips `vpn` services when picking the default route |
| A8 | Plumbing: `changed` must include cellular (cancels and reschedules the batch); the response, the initial model and `recover()` replay (`unwrap_or(false)`); observer dedup (S M11, O M14) | Major | Adopted |
| A9 | A prepared successor keeps the old quality after a network change (S M11) | Major | `network_state` re-prepares when the effective streaming kbps changes |
| A10 | Off-by-one in the restart test (O B7) | Minor | Three restarts; the fourth failure reports |
| A11 | Labels "Wi-Fi/Mobile audio" now mean "not cellular/cellular" (S m6) | Minor | Labels kept: the semantics match the user's wording; documented |

## Plan M2 — Part A (default, network facts, transcode restart)

### 0. Default and network facts
- **Default:** `quality::Config::default().mobile_kbps = 320`. A missing field
  (new install, or a pre-quality session) gets 320; a stored value is kept.
  QML fallbacks use `x === undefined ? 320 : x`.
- **Platform:** `connman_facts` returns `(wifi, cellular, online)` from the
  first ready/online service that is not `vpn`. The NetworkManager fallback
  sets `cellular` for `PrimaryConnectionType` `gsm`/`cdma`. The observer
  compares the triple.
- **Command:** `Command::NetworkState` gains `#[serde(default)] cellular`.
  - `changed` includes it.
  - Core stores `network_cellular`.
  - The response carries `networkCellular`.
  - `recover()` replays it with `unwrap_or(false)`.
- **Quality choice:** `quality.streaming(cellular)` returns mobile kbps only
  on a known cellular link. Wi-Fi, Ethernet and unknown networks get Wi-Fi
  kbps. The cache-ahead suppression follows it.
- **Re-prepare:** when the effective streaming kbps changes, the runtime
  re-prepares the gapless successor.
- **Unchanged:** `wifi_only` still requires Wi-Fi.

### 1. Transcode restart
In `Event::Error` for the current track, with the source `Transcode` and the
cache fallback failed:
- **Restart condition:** some audio was heard in this occurrence
  (`snapshot.listened > 0` or `position > 0`), and fewer than 3 restarts for
  Core's `playback.occurrence` in the last 60 s.
- **Restart:** `Command::RestartTranscode` (internal), i.e. `playback_plan`
  with `force_transcode`, allowed when the current plan's source is a
  transcode, regardless of `codec_fallback`. It starts at `snapshot.position`
  with the event's paused flag.
- **Backoff:** restart 1 is immediate. Restarts 2 and 3 wait 2 s and 4 s via a
  worker deadline (`pending_restart`). At fire time the restart re-checks
  `superseded(acted)`, the current key and the occurrence. If superseded, it
  is dropped silently: the user acted.
- **Otherwise:** today's path, i.e. the stage-1 gone check, then the original
  error.

### 2. Tests
- `connman_facts`: Wi-Fi, cellular, Ethernet, a VPN listed first, none.
- The NetworkManager mapping, as a pure function extracted for the test.
- `streaming`: cellular, Ethernet, unknown.
- Settings: a missing `mobileKbps` gives 320; a stored 0 is kept.
- Core: a cellular change cancels and reschedules cache-ahead; Wi-Fi to
  cellular suppresses automatic prefetch; `networkCellular` is in the
  response.
- Runtime: `recover()` replays cellular. A change of effective kbps
  re-prepares with a transcode URL.
- Runtime fixture (a transcode route that drops the body after N bytes):
  - one drop → restart at the position, with no banner;
  - four drops → the error banner, after restarts with backoff;
  - Pause during the backoff → no restart;
  - a drop before any audio → no restart (today's path);
  - `codecFallback:false` with a quality transcode → still restarted.
- Existing quality and parity tests are updated for the default.

### 3. Docs and gates
`docs/architecture.md` (network facts, quality, restart), `docs/research.md`
(the ConnMan/NM type values used), `docs/validation.md`, and the
`docs/roadmap.md` Open items (part B, with its findings). Then
`./tools/check.sh`, `./tools/build-sailfish.sh`, and the phone.

## Round 2 reviews (plan M2, part A)

Reviewers: Sol (main chain): 1 Blocking; fresh Sonnet subagent: no Blocking;
third chain: preflight timeouts (recorded as unavailable unless it completes).

| # | Finding | Sev | Disposition (M2.1) |
| --- | --- | --- | --- |
| B2-1 | Re-preparing on a network change can discard an already promoted successor (Transition not yet handled) → lost advance or duplicate (O 1); also needs an idle guard (O 5); no runtime hook exists (S F8) | Blocking | **A9 dropped.** A prepared successor keeps its quality; the next preparation uses the new one. Documented |
| B2-2 | `RestartTranscode` must be in Core's occurrence-preserving `resuming` list, else a new UUID resets the budget and listened (S F1, O 3) | Major | Adopted; `pub(crate)` occurrence getter; test |
| B2-3 | `position > 0` / `listened > 0` are seeded by resume: not evidence of new audio (S F2, O 4) | Major | Gate on audible progress since the last (re)start: the worker records `listened` at each start of the occurrence and requires an increase |
| B2-4 | A restart dropped on Pause/Play leaves a dead engine; Seek during backoff ignored (S F3, O 2) | Major | While a restart is pending, the worker applies transport to it: pause/play/toggle set its paused flag, seek sets its position, stop cancels it. It is dropped only when Core's key/occurrence changed or the item is gone (→ gone path) |
| B2-5 | 0/2/4 s backoff too short for dead zones; frequent drops with real progress hit the cap (S F4) | Major | Backoff 0/2/4/8 s, at most 4 restarts without progress (≥10 s of new audio resets the count). While `network_known && !online` the restart waits for the next online `NetworkState` (no budget used), up to 60 s, then reports |
| B2-6 | QML literal fallbacks still 0 (S F5) | Major | All literal fallbacks and `||` patterns use 320 / `=== undefined` |
| B2-7 | A session without a `quality` key gets 320 (S F6) | Minor | Accepted and documented: no stored choice means the default |
| B2-8 | Cold start before the first fact streams original on cellular (S F7) | Minor | Accepted (user decision: unknown → Wi-Fi setting); recorded |
| B2-9 | NM `vpn`/`wireguard` primary hides cellular; Bluetooth/gadget untested (S F9) | Minor | Mapping function tested; VPN primary → unknown (Wi-Fi setting), documented |
| B2-10 | A truncated transcode ending cleanly arrives as `End` (S F10) | Minor | Not observed; open item for part B |
| B2-11 | A failure inside the last seconds/crossfade replays the old ending (S F11) | Minor | No restart when less than 5 s + crossfade remains (today's path) |
| B2-12 | Budget storage, window tests, `Class::PLAIN`, op-name reachability (O 6, S notes) | Minor | One slot (current occurrence) in worker state; clock-tested window; `RestartTranscode` is `PLAIN`, the transcode check lives in the runtime (`playback_source`), Core refuses it offline; reachable by name like `transcoded_playback` (accepted) |

## Plan M2.1 — Part A (M2 with round-2 amendments)

§0 as M2 without the re-prepare; `NetworkState` has no runtime side effect
beyond today's. §1 restart:
- Trigger: current-track `Event::Error`, source `Transcode`, cache fallback
  failed, listened increased since the last start of this occurrence, more
  than 5 s + crossfade left (when the duration is known), item not gone.
- Budget (worker, one slot keyed by Core's occurrence): up to 4 restarts
  without progress; ≥10 s of new audio since the previous start resets it.
- Schedule 0/2/4/8 s via `pending_restart {occurrence, key, position, paused,
  due}`; offline (known) waits for online (≤60 s) without spending budget.
- Transport while pending updates it (pause/play/toggle/seek) or cancels it
  (stop); navigation (key/occurrence change) or a gone mark drops it.
- Fires `Command::RestartTranscode` (in `resuming`; `playback_plan(false,
  true)`; refused offline) with `resumePosition`/`resumeListened`, then
  `start_plan(paused)`; on failure today's path.
Tests: M2 list minus re-prepare, plus occurrence stability across the three
same-item commands and a fresh UUID on repeat-one Next; no restart without new
audio (resume offset > 0); pause/seek/stop during backoff; offline wait and
resume on online; progress resets the budget; window/near-end cases; QML
undefined vs 0.

## Round 3 reviews (plan M2.1)

Reviewers:
- **Sol** (main chain): 1 Blocking, 4 Major.
- **Fresh Sonnet subagent:** 1 Blocking, 6 Major.
- **Third chain:** unavailable (preflight timeouts). Recorded.

Both Blocking findings concern the **delayed (pending) restart**:
- Pause, Play, Seek and Stop run on the caller's thread straight on the engine
  (`runtime.rs:263-320`), so the worker cannot observe them.
- Play on an unloaded engine enqueues `Resume`.
- Seek reloads the failed source.
- Actions can land between the error and the pending state.

Related Majors:
- the successor is not rebuilt after a delayed restart;
- a gone verdict during the wait is dropped;
- the baseline has no defined source;
- requiring progress before every attempt defeats the backoff.

Disposition: **the delayed restart is dropped** (M2.2). The restart happens
synchronously in the `Event::Error` handler, as one more step of the existing
same-item fallback chain. That chain already merges the plan, publishes the
update, re-prepares the successor and falls through to the stage-1 gone check.
No pending state exists for user actions to race.

| # | Finding | Sev | Disposition (M2.2) |
| --- | --- | --- | --- |
| B3-1 | Transport invisible to a pending restart (S F1, O 1); Seek and Resume interplay (S F2/F3) | Blocking | No pending restart (synchronous) |
| B3-2 | Progress before every attempt defeats retries (O 2) | Major | An episode: entering needs new audio since this decoder's start; then up to 4 attempts without audio; 10 s of new audio ends the episode |
| B3-3 | Baseline source (O 5, S F4) | Major | Baseline = the `resumeListened` passed with this start (seed), kept by the worker with the engine id. Progress = `snapshot.listened - seed`, only when `snapshot.id == failed id`. Gapless commits and Seek start a new decoder: their seed is the plan's `resumeListened` (0 for a new occurrence); Seek keeps the episode |
| B3-4 | Successor and gone handling after a delayed restart (O 3/4, S F5/F7) | Major | Moot: the existing fallback continuation runs |
| B3-5 | "Refused offline" ambiguous (S F6) | Major | `RestartTranscode` is refused in offline mode and when the network is known offline. That falls to today's path (cache fallback was tried first; then gone check or banner) |
| B3-6 | Dead zones longer than the attempts still end with the banner | Note | Accepted: immediate attempts only help with short drops. Delayed recovery needs a transport channel; recorded for part B |

## Plan M2.2 — Part A (implementation basis)

§0 as M2.1: 320 default for new installs, `cellular` fact, quality by
cellular, QML fallbacks, no re-prepare.

§1, in the `Event::Error` same-item fallback (`runtime.rs` ~945-985):
- **Order:** `CachedPlayback`, then (source Transcode) `RestartTranscode`, then
  (source not Transcode) `TranscodedPlayback`, then today's path.
- **`RestartTranscode` admission** requires all of:
  - the source is `Transcode`;
  - `snapshot.id == id`;
  - more than 5 s plus the crossfade left, when the duration is known;
  - the restart episode allows it.
- **The episode** is one worker slot `{occurrence, seed, attempts}`, keyed by
  Core's `playback.occurrence` (`RestartTranscode` is in `resuming`):
  - it starts when `snapshot.listened - seed > 0`;
  - it allows up to 4 attempts;
  - it resets when `snapshot.listened - seed ≥ 10 s` at a later error, or when
    the occurrence changes.
- **Core** refuses `RestartTranscode` when `offline_mode` is on or the network
  is known to be offline.
- **Seed:** each `start_plan` from the error handler, a request or a
  transition records the seed from the plan's `resumeListened` for the new
  engine id.

Tests:
- §0 tests as in M2.
- Occurrence and `resumeListened` stable across Cached, Transcoded and
  RestartTranscode; a fresh UUID on a repeat-one Next.
- Runtime fixture, with a transcode route that drops after N bytes:
  - one drop → a restart at the position, with no banner and the successor
    still prepared;
  - the restarted stream dying before audio → further attempts, up to 4, then
    the banner;
  - 10 s of progress resets the count;
  - a failure before any audio → no restart (today's path);
  - known offline → no restart;
  - `codecFallback:false` → still restarted;
  - near the end → no restart.

## Round 4 reviews (plan M2.2)

Reviewers:
- **Sol** (main chain): 1 Blocking.
- **Fresh Sonnet subagent:** no Blocking, 3 Major.
- **Third chain:** not run (unavailable in rounds 1-3).

| # | Finding | Sev | Disposition (M2.3) |
| --- | --- | --- | --- |
| B4-1 | Pause, Stop or Seek between the actor's error and the worker's recovery load is overridden by the stale `paused` (existing fallbacks too); a worker-side check leaves a check-to-enqueue race (O 1, S M-1) | Blocking | **Engine-side guard:** recovery loads go through `Control::Recover(source, paused, acted)`; the actor applies it only if `acted` is unchanged since the error. Seek now counts as a user action (`transport()` bumps `acted` for Seek). Used for the cached, transcoded and restart fallbacks |
| B4-2 | The seed cannot follow Seek, and transitions have no `start_plan`; the plan's sources are wrong (O 2/3, S M-2) | Major | The engine reports it: `Snapshot.seed` = the current decoder's `source.listened` (Seek re-seeds; a gapless successor's seed is 0). Progress = `listened - seed` with `snapshot.id == id` |
| B4-3 | Episode cases unstated (S M-3) | Major | Specified below |
| B4-4 | Successor hold lost after a fallback; `recovery.waiting`; crossfade source; plumbing (S m-2..m-6) | Minor | Hold moved to the new `current`; waiting cleared on a successful fallback; crossfade from `core.audio_config()`; unknown duration allows; `RestartTranscode` in `resuming`, `PLAIN`, refused in offline mode or known offline |

## Plan M2.3 — Part A §1 (implementation basis; §0 as M2.2)

**Engine:**
- `Handle::recover(source, paused, acted)` sends `Control::Recover`. The actor
  loads the source only if `self.acted == acted`; otherwise it logs "recovery
  superseded" and stays unloaded. A user Play then takes today's `Resume`
  path, since the engine is not loaded.
- `Snapshot.seed` is the front segment's `source.listened`.

**Runtime:**
- **Seek:** `transport()` bumps `acted` for `AudioSeek` too.
- **Recovery load:** `start_plan` takes `guard: Option<u64>`. Fallback loads
  pass `Some(event.acted)`.

**Episode:** one worker slot, `{occurrence, attempts}`, keyed by Core's
`playback.occurrence` through a `pub(crate)` getter. On a current-track error
with source Transcode, `snapshot.id == id`, and the cache fallback failed:
- **progress** = `snapshot.listened - snapshot.seed`;
- **new episode** (none for this occurrence): admitted if progress > 0, and
  `attempts = 1`;
- **progress ≥ 10 s:** `attempts = 1`;
- **otherwise:** `attempts += 1`;
- **admit** while `attempts ≤ 4`. Exhausted stays exhausted for this
  occurrence;
- **near the end:** none when `duration > 0` and
  `duration - position ≤ 5 s + crossfade`.

Admitted → `RestartTranscode` (`playback_plan(false, true)` at the position,
with `resumeListened`), then the existing continuation (guarded start, merge,
update, prepare). Otherwise, or on failure, today's path. The gone check
follows the fallbacks unchanged.

**Tests (additions):**
- An error with `acted` bumped before the recovery is applied: no load (engine
  test).
- Seek counts as an action.
- `seed` after Seek, Resume and a transition.
- Episode counting: a run under 10 s counts, ≥ 10 s resets, exhausted stays.
- A new occurrence starts a fresh episode.
- Refusals: offline, offline mode, gone item.
- The successor hold survives a fallback.

## Round 5 reviews (plan M2.3) and decision

Reviewers: Sol (main chain) 2 Blocking; fresh Sonnet subagent 1 Blocking.
- **The guard is not enough** (O 1, S M1). A control admitted before the
  error, but applied after it, still matches the `acted` stamp.
- **Seek reloads the dead source** (S B1). On an errored engine, Seek reloads
  the failed source, and with Seek counting as an action that reload wins over
  the recovery.
- **A refused recovery desynchronizes the worker** (O 2, S M2). The worker
  has already moved to the new serial, presented plan and successor.
- **Early failures** (O 3, S M4). A decoder that fails before its first
  publication leaves a stale snapshot.
- **Episode precedence** (O 4, S M5).

Both reviewers recommend an **engine-owned recovery intent**. The actor records
the failure and the pause intent; Play, Pause, Stop and Seek update it; and the
actor decides whether to recover, acknowledging the outcome to the worker. That
also fixes the same race in today's cached/transcoded fallback.

**User decision (2026-10-19):** the transcode restart (§1) moves to part B,
together with the other engine work. Part A ships §0 only: network facts, the
cellular quality choice and the 320 default. Until part B, a dropped transcode
fails as today and Play resumes it.

## Implementation review, part A, round 1

Reviewers:
- **Sol** (with the diff): no Blocking.
- **Fresh Sonnet subagent:** no Blocking. It ran `network_tests` (2) and
  `parity` (24), all passing.
- **Third chain:** unavailable.

| # | Finding | Sev | Disposition |
| --- | --- | --- | --- |
| IA-1 | The observer marks a fact as seen even when a full queue refused the request, so the change is never retried (O) | Major | Fixed: dedup advances only after a successful admission; the next signal or 15 s refresh retries |
| IA-2 | No behavioural coverage of a cellular-only change, cache-ahead suppression, missing-field default (O, S 5) | Major | Added `src/network_quality_tests.rs`: on cellular with a cap, no prefetch; a cellular-only flip to Ethernet starts it. **Mutation-checked:** without `cellular` in `changed` the test fails. A missing `mobileKbps` deserializes to 320, a stored 0 stays 0. Not added: `recover()` replay of `networkCellular` (the model merge path is shared with `networkWifi`; recorded) |
| IA-3 | Platform values from reviewer knowledge, not documented sources (O) | Minor | Fixed: NetworkManager D-Bus docs cited (primary connection is the VPN's underlying link; NMState; NMMetered); ConnMan measured on the phone (a ready `vpn` listed first, then `wifi` online). `docs/research.md` |
| IA-4 | "Sessions that never stored one" too broad: old sessions store 0 explicitly (S 1) | Major | Fixed wording: new installs only, no migration |
| IA-5 | NM VPN over cellular gets the Wi-Fi choice (S 2) | Major | Rejected by source: NM's `PrimaryConnection` is the underlying link for a default-route VPN. Documented |
| IA-6 | Bluetooth tether and hotspot not cellular; plan had listed NM `bluetooth`/`Metered` (S 3) | Minor | Accepted limit, documented |
| IA-7 | Cold start uses the Wi-Fi choice until the first fact (S 4) | Minor | Accepted (user decision), documented |
| IA-8 | ConnMan VPN skip changes Wi-Fi-only downloads with a VPN over Wi-Fi (S 7) | Note | Intended (it was a bug on the user's phone), documented |
| IA-9 | QML compatible; `qt_backend.cpp` fixture unaffected (S 5/6) | Note | — |

# Part B

User decision (2026-10-19): go on with part B after part A. Part B is split
into steps, each with its own reviewed plan:
- **E1:** engine-owned recovery, plus the transcode restart.
- **E2:** successor pre-load timing and the resume threshold.
- **E3:** cache-ahead yields to a starving stream.

## Plan E1 — Engine-owned recovery

### Problem
On a current-track streaming failure, `Actor::error()` clears the decoder,
output and intent, then emits `Event::Error {paused, acted}`. The worker,
another thread, then decides on a fallback and loads it. User controls run on
the caller's thread straight into the actor, so any decision taken from the
event's stamp can be overtaken:
- Pause or Stop are lost;
- Seek reloads the dead source;
- Play takes `Resume`.

Rounds 4-5 showed that worker-side guards cannot close this.

### Design: the actor keeps a recoverable failure
1. **The failed state.** On a fill failure of the current decoder with
   "Audio decoding or streaming failed", the actor does not discard the track.
   It enters `failed: Some(Failed {id, source, position, listened, progress,
   paused, since})`.
   - `position` and `listened` are the current media position and listened
     time.
   - `progress` is the listened time since this decoder's start
     (`listened - segment.source.listened`).
   - `paused` is `!desired`.

   Decoder, successor and output are dropped as today. The segment is kept
   for the snapshot. The actor emits `Event::Error {id, message, paused,
   acted, progress, recoverable: true}`. Other failures (load/output
   "Audio playback failed", a successor failure) are unchanged
   (`recoverable: false`).
2. **Controls while failed.**
   - **Pause / Play:** set `failed.paused` and `desired`. Nothing is loaded.
   - **Seek(p):** sets `failed.position = p`. It emits `Seeked(p)` and does
     not reload the dead source.
   - **Stop, Load, Shutdown:** clear `failed`. Load also clears it.
   - **Recording hold:** unchanged; it applies to the recovered load.
3. **Snapshot while failed:**
   - `loaded: true`, so transport keeps routing Play/Pause to the actor
     instead of starting `Resume`;
   - `playing: false`, `buffering: !failed.paused`, `paused: failed.paused`;
   - `position: failed.position`, and `error` empty.

   The UI shows buffering or paused at the right position.
4. **`Control::Recover {id, source: Option<Source>}`**, sent by the worker.
   - **Failure no longer current** (`failed` is None or a different `id`):
     the actor emits `Event::Recovery {id, accepted: false}` and does nothing
     else.
   - **`Some(source)`:**
     - **Load:** the actor sets `source.resume = failed.position` and
       `source.listened = failed.listened`, then loads it with
       `paused = failed.paused`. Its `source.id` is the new engine id.
     - **Accepted:** it clears `failed` and emits
       `Event::Recovery {id, accepted: true}`.
   - **`None` (abandon):** the actor clears `failed`, sets `loaded: false`
     and the error string (today's final state), and emits
     `Event::Recovery {id, accepted: true}`.
5. **Safety timeout.** A failure the worker never resolves is abandoned
   by the actor itself after `RECOVERY_LIMIT` = 75 s. That is above any worker
   wait (an 8 s verdict, a 60 s offline wait plus backoff).

### Worker changes (`runtime.rs`)
- **Recoverable `Event::Error`, same fallback chain:**
  `CachedPlayback`, then (source Transcode, admitted by the episode)
  `RestartTranscode`, then (source not Transcode) `TranscodedPlayback`.
  - **The load** goes through `recover_plan(...)`, which sends
    `Control::Recover(id, Some(source))` instead of `load`.
  - **Book-keeping:** `current`, `presented` and `prepared` are updated
    tentatively, as `start_plan` does today. The hold is re-keyed and
    `recovery.waiting` cleared.
- **`Event::Recovery {accepted: false}`:** the user stopped or loaded
  something else. If `current` is still the tentative id, the worker resets
  `current` to 0, the presented plan and `prepared`.
- **Every path that gives up on a recoverable failure** sends
  `Recover(id, None)`. That covers the original-error banner, the
  deadline/refused gone-check path, and `settle_failure` reporting.
  `skip` (Next/Previous) loads instead, which clears the failure itself.
- **The stage-1 gone check is unchanged otherwise.** While the verdict is
  pending the actor shows buffering/paused instead of an error, and
  `superseded(acted)` still guards the automatic skip.

### Transcode restart (the M2.3 episode, with actor data)
- **Progress** comes from the event: `progress` is exact, including a failure
  before the first publish.
- **Episode** (one worker slot keyed by Core's `playback.occurrence`;
  `RestartTranscode` is in `resuming`):
  - progress > 0 starts an episode, or resets an exhausted one;
  - progress ≥ 10 s resets `attempts` to 1;
  - otherwise `attempts` increases by 1;
  - restarts are admitted while `attempts ≤ 4`.
- **Near the end:** no restart when `duration > 0` and
  `duration - position ≤ 5 s + crossfade`.
- **Backoff:** attempt 1 is immediate, attempts 2-4 wait 2, 4 and 8 s. That
  is safe now, because the actor holds the user's intent meanwhile. The wait
  is a worker deadline `{id, due}`. At fire time the worker re-plans
  `RestartTranscode` and sends `Recover`, and the actor refuses it if the
  user moved on.
- **Network known offline:** the worker waits for an online `NetworkState`
  for up to 60 s without spending attempts, then restarts or abandons.
- **Core** refuses `RestartTranscode` in offline mode. The worker does not
  call it while known offline.

### Tests
- **Engine** (fake sink, a scripted HTTP source that fails mid-stream):
  - the failed state, with its snapshot fields;
  - Pause, Play and Seek while failed update the intent;
  - a Seek while failed does not reload;
  - Stop then `Recover` → refused;
  - `Recover(Some)` loads at the failed position with the failed intent;
  - `Recover(None)` → the unloaded error state;
  - the safety timeout (with a clock injected or a short limit under test);
  - `progress` correct after Seek, Resume and a failure before the first
    publish.
- **Runtime fixture:**
  - the existing fallback tests stay green;
  - Pause during the fallback window → the recovered load is paused, with
    no override;
  - Stop during the window → stopped, with nothing loaded and the worker
    state reset;
  - a transcode drop → a restart at the position;
  - a drop before the restarted stream produces audio → backoff attempts up
    to 4, then the banner;
  - a Pause during the backoff is honoured;
  - known offline → waits, then resumes on online;
  - near the end → no restart;
  - stage-1 gone tests unchanged, plus no stuck buffering after a gone
    verdict.
- **Real PulseAudio** (`pulse_server_tests.rs`): a recovered load after a
  failure plays (output recreated).

## Round E1 reviews

Reviewers:
- **Sol** (main chain): 2 Blocking, 6 Major.
- **Fresh Sonnet subagent:** 2 Blocking, 8 Major.
- **Third chain:** unavailable (exit 3). Recorded.

Both confirm that the actor-owned intent closes the same-item races, because
controls and `Recover` share one ordered channel.

| # | Finding | Sev | Disposition (E1.1) |
| --- | --- | --- | --- |
| E1-1 | Gone-skip still loads with the stale `waiting.paused` (O 1, S M1) | Blocking | Every automatic replacement goes through the actor: `Resolve::Replace` |
| E1-2 | A promoted-but-not-audible successor failing is misattributed; the worker treats it as a prepared failure; actor stuck (O 2, S B1) | Blocking | Three failure kinds, distinguished by the actor (§2) |
| E1-3 | Not every worker exit ends the failed state (S B2, O 4) | Blocking/Major | One worker `Failure` slot with one `resolve()`; every exit goes through it; a test per exit |
| E1-4 | Tentative publication is not a transaction; checkpoints of the failed id are lost (O 3, S minor) | Major | The worker keeps `current` = failed id and commits only on `Event::Resolved {Loaded}` |
| E1-5 | Failure facts must be immutable and published before the event (O 5, S M3) | Major | Adopted |
| E1-6 | Timeout and load-failure outcomes undefined; limit arithmetic (O 6, S M7/M8) | Major | Explicit outcomes; one absolute worker deadline; actor limit above it |
| E1-7 | Toggle and MPRIS cannot express retained intent; recording; seekable (O 7, S M2) | Major | `Snapshot.failed`; transport and MPRIS use the intent |
| E1-8 | Episode precedence allows unlimited short-progress retries (O 8, S minor) | Major | Ordered transitions (§5) |
| E1-9 | Delayed restart replans against a moved Core (O 9, S M4) | Major | Validate failure id, key, occurrence and epoch before any Core call; cancel on navigation, epoch, gone or offline mode |
| E1-10 | Other engine users (`daily.rs`, tests) never resolve (S M5) | Major | Opt-in: `Engine::with_recovery`; `Engine::new` keeps today's behaviour |
| E1-11 | After abandon, or for a non-recoverable error, Seek reloads the dead source (S M6) | Major | Abandon and non-recoverable errors clear `segments`; Seek needs `current` or `failed` |
| E1-12 | Queue-full `try_send`; Prepare while failed; a paused stream dropping consumes attempts; string-typed kind; event-type plumbing (S minor) | Minor | §6 |
| E1-13 | Deterministic tests with barriers, a truncating fixture, terminal-state assertions (O 10, S tests) | Minor | §7 |

## Plan E1.1 — Engine-owned failure resolution

### 1. Actor failed state (opt-in via `Engine::with_recovery`)
- **Recoverable failures** are classified by kind, not string: a fill
  failure (`ErrorKind::Stream`) of the *audible* decoder. Output and load
  errors (`ErrorKind::Playback`) keep today's path.
- **The failure record:** the actor computes
  `Failed {id, key, position, listened, duration, progress, paused, since}`
  from the front segment's timeline:
  - `progress` = listened minus this decoder's seed;
  - `paused` = `!desired`.

  It then drops the decoder, successor and output, keeps the front segment,
  publishes the failed snapshot, and only then sends `Event::Error {id, kind,
  recoverable: true, position, listened, duration, progress, paused}`.
  `desired` stays false; `failed.paused` holds the intent.
- **The failed snapshot:**
  - `id`, `position`, `duration`, `listened` and `seekable`;
  - `loaded: true`, `failed: true` and `playing: false`;
  - `paused: failed.paused || recording`;
  - `buffering: !paused && !recording`;
  - an empty error.
- **Controls while failed:**
  - **Play / Pause:** set `failed.paused`.
  - **Seek(p):** clamps like a normal Seek, sets `failed.position` and emits
    `Seeked`.
  - **Stop, Load, Shutdown:** clear `failed`.
  - **Prepare:** ignored.
  - **Recording:** flag only.
- **Abandon and non-recoverable errors** clear `segments`, so a later Seek
  cannot reload a dead source. Seek requires `current` or `failed`.

### 2. Failure kinds at a gapless boundary
- **Prepared successor (not promoted):** fails as today (`Next track
  decoding failed`, worker retire/hold).
- **Promoted successor whose segment is not audible yet**
  (`segments.len() > 1`, failing decoder = back segment):
  - The actor drops the successor segment and decoder, keeps the output, and
    lets the queued tail of the audible track play out.
  - It emits the successor failure as `Event::Error {kind: Successor}`.
  - The audible track then ends naturally (`Event::End`), so the worker's
    existing End → Next path re-plans the item, which may fail again as an
    audible failure. The stage-1 hold applies.
- **Audible decoder:** the failed state above.

### 3. `Control::Resolve {failed: id, resolution}`
- **Resolutions:**
  - **`Recover(source)`:** the same item. The source gets the actor's
    position, listened and intent.
  - **`Replace(source)`:** another item, the gone-skip. It keeps the source's
    own resume and listened, and takes the actor's intent.
  - **`Abandon`:** today's unloaded error state, with segments cleared.
- **Outcomes:** the actor answers `Event::Resolved {failed, outcome}`:
  - `Loaded {new}`;
  - `Refused`: the failure is no longer current;
  - `Abandoned`;
  - `LoadFailed {new}`: the decoder or output could not be built, so the
    actor goes to the unloaded error state;
  - `Expired`: the actor's own limit, 90 s from the failure.

### 4. Worker: one failure slot, one resolution
- **The slot:** `failure: Option<Failure {id, key, occurrence, epoch, facts,
  stage, deadline}>`, with `deadline` = failure + 60 s, absolute. Each stage
  ends by calling `resolve(Resolution)`:
  - **`Fallback`:** cache → restart → transcode;
  - **`Verdict`:** the stage-1 gone check (`recovery.waiting`);
  - **`Backoff {due}`;**
  - **`Offline`:** waiting for online;
  - **`Paused`:** see below.
- **`resolve()`** keeps the pending plan data and sends `Resolve`. Nothing
  is published and `current` stays the failed id. On `Resolved`:
  - **`Loaded {new}`:** commit as `start_plan` does (presented, `current =
    new`, merge, update), re-key the hold, clear `waiting`, prepare the
    successor.
  - **`Refused`:** drop the pending data. The user acted, and the actor
    state is already theirs. Send `prepare(None)`.
  - **`Abandoned`, `LoadFailed`, `Expired`:** the banner with the stage's
    message, or the original one.
- **Every former `audio_error` for a current-track failure goes through
  `resolve(Abandon)`.** That covers:
  - the handler tail (cache source, no key, refused probe, stale id);
  - the orphaned verdict;
  - every `settle_failure` and `skip` exit, including Core error, panic,
    source build failure, the empty plan (gone end), and the superseded case
    (Abandon: the user acted, so the engine stays as the user left it).
- **`skip()` builds a plan and resolves with `Replace`.** Its old
  `start_plan(waiting.paused)` is gone.
- **Validation before any Core call at a deadline or stage change:**
  `failure.id == snapshot.id && snapshot.failed`, plus Core's current key,
  occurrence and health epoch unchanged. Otherwise `resolve(Abandon)`.
- **Cancellation:**
  - A worker Load (request, End, skip) supersedes the slot. The actor
    answers its later Resolve with `Refused`.
  - An epoch change or offline-mode switch triggers `resolve(Abandon)`.
  - A gone verdict moves to the gone path.
- **Paused failures:** while `snapshot.failed && snapshot.paused`, no restart
  attempt is spent. The cache fallback is still tried at once, and other
  stages wait (polling the snapshot each loop pass) until the intent is
  playing or the absolute deadline passes. Then `Abandon`; a later Play takes
  `Resume`.
- **Queue-full admission:** a `try_send` failure of `Resolve` is retried on
  the next loop pass until the deadline.

### 5. Transcode restart episode (ordered transitions)
Keyed by Core's `playback.occurrence`:
1. a new occurrence starts a fresh budget;
2. progress ≥ 10 s sets `attempts = 1`;
3. otherwise `attempts += 1`;
4. admit while `attempts ≤ 4`.

Exhaustion holds until (1) or (2). Near the end (`duration > 0` and
`duration - position ≤ 5 s + crossfade`) there is no restart. Backoff is 0,
2, 4 and 8 s. When the network is known offline, the stage waits for online
without spending attempts. `RestartTranscode` is in `resuming`, and Core
refuses it in offline mode.

### 6. Transport and presentation
- **Toggle:** `transport()` uses the intent. While failed, toggle sends Pause
  when `!snapshot.paused`, else Play. Play/Pause never take the `Resume`
  path while `snapshot.failed`.
- **MPRIS:** `PlaybackStatus` reports Paused while paused, otherwise
  Playing, which includes buffering while failed. Its Next/Previous keep the
  intent.
- **`Change`** includes `failed`.

### 7. Tests
- **Actor unit tests** (direct `control()` calls, a truncating source, an
  injected limit):
  - each control while failed;
  - each `Resolve` with each outcome;
  - Seek while failed and after Abandon;
  - the snapshot projection, with recording;
  - progress after Seek, Resume and a failure before the first publish;
  - a promoted-successor failure (tail plays out, End);
  - a prepared-successor failure;
  - `Engine::new` unchanged.
- **Runtime fixture** (per-request truncation, barriers around Resolve):
  - controls before and after the failure, and before the Resolve outcome;
  - Pause or Stop during the window and the backoff;
  - the stage-1 gone tests, asserting the terminal snapshot;
  - each `resolve(Abandon)` exit leaves no failed state;
  - the transcode restart episode, offline wait, paused failure,
    exhausted budget, near the end;
  - a checkpoint of the failed id is kept on Refused.
- **Real PulseAudio:** a recovered load after a failure plays.

## Round E1.1 reviews

Reviewers:
- **Sol** (main chain): 3 Blocking, 4 Major.
- **Fresh Sonnet subagent:** 2 Blocking, 5 Major.
- **Third chain:** not run.

| # | Finding | Sev | Disposition (E1.2) |
| --- | --- | --- | --- |
| E2-1 | §2 tail-to-End is not executable: `pump` returns without `current`; nothing finishes the output (O 3, S B1) | Blocking | §2 replaced: drain-then-fail (below) |
| E2-2 | Successor PCM is already queued at a promoted failure (up to about 6 s); classification raced against `publish()` (S B2, O 4) | Blocking | Same: a failure is raised only when playback reaches the failing decoder's last queued frame, so Transition happens naturally first |
| E2-3 | Delayed outcomes can overwrite a newer load (O 1) | Blocking | `Resolve` carries a `seq`; outcomes apply only if the worker slot still holds it; one `begin_load()` helper clears the slot at every site that writes `current` (S M3) |
| E2-4 | Gone-skip mutates Core before acknowledgement; a checkpoint credits the wrong item (O 2, S M2) | Blocking | Before Core `Next`/`Previous` the worker records the failed item's checkpoint from the failure facts. Non-`Loaded` outcomes present Core's new position unloaded (as today's superseded skip), with a banner only for `LoadFailed`/`Expired` |
| E2-5 | `acted` makes a plain Pause abandon (S M1) | Major | Supersession = a queued worker request (`queued_transport > 0`) or an actor refusal. Pause/Play/Seek are engine-held intent, not supersession |
| E2-6 | Recording hides logical intent; MPRIS status/next/previous (O 5, S M5) | Major | `Snapshot.intent` (logical play intent, independent of recording or failure). Toggle, MPRIS Next/Previous and the status use it |
| E2-7 | Paused gating vs gone verdict ordering (O 6) | Major | Health results are consumed immediately; pause gates only restart attempts |
| E2-8 | Admitted `Resolve` past its deadline; draining starves the actor timeout (O 7) | Major | `Resolve` carries an absolute expiry checked by the actor; the failure expiry is also checked while draining controls |
| E2-9 | `poll()` window between `Loaded` and commit (S M4) | Major | The plan is inserted under the `presented` lock before sending `Resolve` (as `start_plan`), and rolled back on non-`Loaded`; checkpoints in the window are accepted as lost |
| E2-10 | Abandon after a long pause shows a banner; `SEEK_FAILED`; Bluetooth policy sees `loaded` (S minor) | Minor | Paused expiry unloads silently; seek failures are not recoverable; Bluetooth auto-pause of a failed loaded track is intended |
| E2-11 | Test barriers; deterministic Fake timing (O 8, S tests) | Minor | §7 |

## Plan E1.2 — Engine-owned failure resolution (drain, then fail)

This replaces E1.1 §2. The other E1.1 sections stand with the amendments in
the table above.

**Drain then fail** (actor, opt-in):
- **On a fill failure of the current decoder** (`ErrorKind::Stream`, not a
  seek failure), the actor stops decoding: `current = None`, the successor
  is dropped and `failing = Some({id, end_frame})`. `end_frame` is the
  output frame after the decoder's last pushed PCM, and the decoder's
  segment gets `end = end_frame`.
- **The output keeps playing the queued audio.** `publish()` promotes
  segments as usual, so a successor whose audio was already queued becomes
  audible and fires `Transition` normally.
- **When the output position reaches `end_frame`** (or the queue is empty),
  the actor enters the failed state for the segment at the front, which is
  then the failing decoder's own segment. Facts come from that segment's
  timeline at `end_frame`. The actor publishes the failed snapshot, then
  sends `Event::Error`.
- **Paused while draining:** the drain resumes on Play. A Seek while draining
  cancels the drain, enters the failed state at the target and emits
  `Seeked`. A Stop or Load clears it.
- **The prepared, not-promoted successor** fails as today.

**No early cut-off and a single kind:** there's no special gapless case and
no early cut-off: the user hears everything that was decoded. The position
at failure is exact.

Tests (in addition to E1.1 §7):
- **Actor (Fake sink, paused where timing must not advance):**
  - a failure with queued PCM → drain, then failed at `end_frame`, with exact
    facts;
  - a promoted successor failing with queued frames → Transition, then the
    failed state for the successor;
  - Seek, Pause and Stop while draining;
  - the crossfade overlap.
- **Runtime:**
  - a delayed `Resolved` after a newer load is ignored (`seq`);
  - a skip refused by Stop → Core at B, presented unloaded, with A's
    checkpoint kept;
  - Pause during the verdict → the skip loads paused (no abandon);
  - recording → toggle → release;
  - a Resolve expiry race.

## Round E1.2 reviews

Reviewers:
- **Sol** (main chain): 2 Blocking, 4 Major.
- **Fresh Sonnet subagent:** 1 Blocking, 6 Major.
- **Third chain:** not run.

| # | Finding | Sev | Disposition (E1.3) |
| --- | --- | --- | --- |
| E3-1 | The drain trigger (position or queue estimate) can hang on PulseAudio: prebuffer not reached, underflow or cork, sink move (S B1, O 1) | Blocking | Drain uses the backend's own drain (`output.finish()` → `eof` via the drained callback), handled before ordinary `End`; plus a drain bound = queued frames / rate + 3 s of un-paused time |
| E3-2 | During a pending gone-skip, `record(A)` reads Core's new current B (O 2) | Blocking | Once Core advances for a skip, recording for the failed id is suspended (A's checkpoint was saved from the failure facts before `Next`) until resolution |
| E3-3 | Play routed by a stale snapshot reaches an unloaded actor (O 3) | Major | An actor Play with nothing loaded, failed or draining emits `Event::PlayIdle`; the worker answers with `Resume` if it has nothing pending |
| E3-4 | Seek during drain with a promoted failing successor (S M2, O 4) | Major | If the failing segment is not the front: a normal Seek on the front (failing cleared, the successor re-prepared as today). Otherwise, failed at the target with listened sampled at the audible cursor |
| E3-5 | `intent` missing from `Change` and MPRIS dedup (O 5); the failed snapshot must be a `publish` branch, stable across ticks (S M3) | Major | Adopted |
| E3-6 | Fresh-episode admission lost `progress > 0` (O 6) | Major | Restored: a fresh episode needs progress > 0; zero-progress retries only inside an established episode |
| E3-7 | Recovery waits for the drain; a paused failure waits for Play (S M6) | Major | `Event::Error` is sent at failure start, with facts computed for `end_frame`; the actor holds a `Resolve` received while draining and applies it at drain end (with the intent and position then current) |
| E3-8 | `error()` id during drain; controls during drain (S M4/M5) | Major | `failing.id`; Prepare ignored; Load, Stop, Shutdown and Abandon clear `failing` |
| E3-9 | "Everything decoded" overstated (S M1, O 7) | Minor | Narrowed to already-submitted PCM: decoder-buffered PCM (up to 2 s + tail) is replayed by the recovery from the drain-end position |
| E3-10 | Stale crossfade `fade` when `next` fails mid-fade; zero-audio successor; SEEK_FAILED loop; worker `prepared` cleanup (S N1-N4) | Minor | Clear `fade`; a zero-audio successor still Transitions (listened 0); recovery-time SEEK_FAILED is non-recoverable; `prepared`/hold cleared on a recoverable Error |

## Plan E1.3 — consolidated

**Actor** (`Engine::with_recovery` only). A fill failure of the current
decoder (not a seek failure) makes the actor:
1. stop decoding, drop the successor, and clear a stale fade;
2. set `failing = {id, end_frame, deadline}`;
3. close the failing segment at `end_frame`;
4. call `output.finish()`;
5. send `Event::Error {recoverable, facts at end_frame, paused}` straight
   away.

While draining:
- `publish` promotes segments (a queued successor Transitions normally) and
  does not emit `End`.
- On `eof`, or past the drain bound, the actor applies a held `Resolve`, if
  any. Otherwise it enters the failed state at the front segment.
- Controls follow E3-4 and E3-8.

The failed state, `Resolve` and its outcomes, and the snapshot (with
`failed`, `intent` and `Change`) are as in E1.1, amended by E2-*.
`Event::PlayIdle` follows E3-3.

**Worker:**
- **The failure slot:** a single slot, with `seq` correlation and
  `begin_load()` clearing it at every site that writes `current`.
- **The fallback chain:** cache, then restart (episode as in E3-6), then
  transcode, with backoff and the offline wait.
- **Gone verdict and skip:** the verdict is consumed immediately. A skip
  records A's checkpoint first, suspends recording for A, then resolves with
  `Replace`.
- **Commit on `Loaded` only**, with the plan inserted before sending and
  rolled back if the load is not accepted.
- **Supersession** is a queued worker request.
- **Paused failures** spend no restart attempts. A paused expiry is silent.

**Tests:**
- E1.1 §7, plus E1.2 tests.
- Real PulseAudio: a drain with a sub-prebuffer tail, an underflowed stream
  and a corked stream all complete or hit the bound.
- Record suspension during a pending skip, with barriers before and after
  `Resolve`.
- `PlayIdle` after Stop or abandon.
- Seek during drain with the failing successor not yet front.
- `Change`/MPRIS emission on an intent-only change during recording.
- A cold-start transcode that never plays: no restarts, so stage-1 timing
  holds.
- A drain bound hit.

## Round E1.3 reviews and decision

Reviewers:
- **Sol** (main chain): 3 Blocking.
- **Fresh Sonnet subagent:** 1 Blocking, 5 Major.

Both confirm E3-1 (backend drain plus bound) and E3-2 (record suspension) are
closed. The new Blocking findings come from two E1.3 additions:
- **The early Error.** A promoted successor's Error arrives before its
  Transition, which misroutes it (S B1, O 2). Worker validation also fails
  during a valid drain (O 1).
- **`PlayIdle`.** The signal is lost while a stale failure is pending (O 3).

**User decision (2026-10-19): simplify and finish.**
- **Drop the early Error:** the failure is reported after the drain.
- **Drop `PlayIdle`:** the pre-existing stale-snapshot Play race is
  documented. A second Play takes `Resume`, as today.

## Plan E1.4 — E1.3 without the early Error and `PlayIdle`

Changes to E1.3:
- **When the Error is sent.** It is sent when the actor *enters the failed
  state*: at drain end (eof), at the drain bound, or at once if `finish()`
  fails or nothing was queued. The actor publishes the failed snapshot
  first. By then any queued successor has Transitioned, so the failure
  always belongs to the worker's committed `current`. That removes the
  misrouting and the "prepared" branch question. Facts are taken from the
  front segment at `end_frame`, including at the bound.
- **No held `Resolve`.** `Resolve` exists only after the Error, while the
  actor is failed. During the drain, Load, Stop and Shutdown clear `failing`.
  A Seek follows E3-4.
- **The drain.**
  - It uncorks before `finish()` when `desired && !recording`.
  - `publish` suppresses `End` while `failing`.
  - The bound counts un-paused, unheld time. A paused or held drain waits:
    nothing needs recovering until the user plays. The actor expiry, 90 s,
    starts at failed-state entry.
- **No `Event::PlayIdle`.** Pre-existing and documented: a Play routed by a
  stale `loaded` snapshot can reach an unloaded actor and be lost. Pressing
  Play again takes `Resume`.
- **Worker validation.** `snapshot.failed && snapshot.id == failure.id` holds
  when the Error arrives, because the actor publishes before sending. The
  worker uses only the event's facts for the checkpoint and the fallback
  position.
- **Record suspension** covers every entry point: `apply_checkpoint`
  (including overflow), maintenance, shutdown, and the
  Transition/End/Error records.

Accepted cost: recovery starts after the queued audio plays out. A drop with
a few seconds queued is recovered a few seconds later than a head start
would allow; the listener hears no gap in that queued audio.

Tests: E1.3's list, without `PlayIdle` and the held `Resolve`, plus:
- a promoted-successor failure: Transition, then Error, then recovery of B;
- a paused drain resumes on Play;
- `finish()` failing → immediately failed.

## Round E1.4 reviews

Reviewers:
- **Fresh Sonnet subagent:** **no Blocking**. Both E1.3-round Blocking findings
  are closed: Transition precedes Error on one channel, and validation holds at
  Error time.
- **Sol:** unavailable on the first run; the retry reported **one Blocking**
  (forced drain completion, merged into E5-1 below) and confirmed no other
  Blocking in the combined plan.
- **Third chain:** not run.

Amendments (E1.4.1, implementation basis):

| # | Finding (S) | Sev | Disposition |
| --- | --- | --- | --- |
| E5-1 | At the bound with a back-segment failure, facts at `end_frame` overshoot the front segment (S); forced completion (bound, `finish()` failure) does not guarantee Transition before Error (Sol retry: **Blocking**) | Blocking | **One finalization for every exit** (drain eof, bound, empty queue, `finish()` failure): promote the segment timeline through `end_frame`, exactly as `publish` does at eof, so every queued successor emits its `Transition` before the failed state is entered for the failing decoder's own segment. A forced completion counts the queued audio as heard (accepted: rare, and bounded by the queue length) |
| E5-2 | Tick ordering | Major | Failed-state entry runs after `publish()` promotion in the same tick. `publish` suppresses `End` while `failing` |
| E5-3 | "Nothing queued" detection; `finish()` Err | Major | "Nothing queued" = `output.frames == 0`. A `finish()` error enters the failed state at once, not through `self.error` |
| E5-4 | A recoverable Error not matching the worker's `current` | Major | `Resolve(Abandon)` and the banner, as today's stale-id path |
| E5-5 | Bound accounting | Major | Time accumulates only while `desired && !recording && !output corked`; a paused drain resumes on Play (tested) |
| E5-6 | Seek on the failing front during drain; `prepared` cleanup; record-suspension entry list (transport pre-record, `Seeked` paths) | Minor | Adopted |

## Implementation review E1, round 1

Reviewers:
- **Sol** (with the diff): 1 Blocking, 7 Major.
- **Fresh Sonnet subagent:** no Blocking, 2 Major. It ran `audio::recovery_tests`
  and `fixture_tests`: 26 passed.
- **Third chain:** unavailable (exit 3). Recorded.

| # | Finding | Sev | Disposition |
| --- | --- | --- | --- |
| IE-1 | A deadline give-up carried the already-passed expiry, so the engine refused it and the failure lingered silently until 90 s (O 1, S M1) | Blocking | Fixed: `Abandon` is always honoured; only replacements check expiry. The engine's `Expired` also settles a matching pending resolution. Test `giving_up_is_honoured_after_the_expiry_and_clears_buffering` |
| IE-2 | Stop during a held failure left `buffering = true`, so the spinner stayed on (O 3, S M2) | Major | Fixed: the no-output projection clears `buffering`. Test `stopping_a_held_failure_clears_buffering` |
| IE-3 | A full engine queue stranded the pending resolution (O 2) | Major | Fixed: `Handle::resolve` hands the resolution back; the worker retries it each pass |
| IE-4 | Deferred restart lacked validation: epoch, queued navigation, gone, offline (O 4) | Major | Fixed: the restart runs only with a play intent, online and no queued request; gone → the gone path. An epoch, key or occurrence change while the engine still holds the failure releases it (silent Abandon) |
| IE-5 | Restart attempts spent while paused or offline; recording read as pause (O 5, S m3) | Major | Fixed: a paused or offline failure is admitted (counted) only when it can run; the logical intent is used, not the recording-held `paused` |
| IE-6 | The successor hold was lost across a same-item recovery (O 6) | Major | Fixed: the hold is re-keyed to the new engine id on `Loaded` |
| IE-7 | An output failure during a promoted-successor drain was misclassified (O 7) | Major | Fixed: `error()` while draining finalizes the drain (`enter_failed(true)`: Transition before Error) |
| IE-8 | Tests bypass production classification; the drain test did not require submitted audio; bound not driven by `drain_tick` (O 8, S m4) | Major | Partly fixed: the PulseAudio test now requires `progress > 0`; `a_stalled_drain_is_forced_by_its_bound` drives `drain_tick`. **Not added (recorded):** deterministic worker-level barrier tests for deadline expiry, queue-full retry and late outcomes across newer loads |
| IE-9 | `send_resolve` could overwrite a pending resolution; plans left after `clear()` (S m1/m2) | Minor | `debug_assert`; a recoverable Error while pending is logged and ignored (outcomes precede new failures on the channel); lingering plans are harmless (ids are not reused) |

Tests after the fixes:
- `audio::recovery_tests` and `fixture_tests`: 29 passed.
- Real PulseAudio: 8 passed.

**Note from the gate run (2026-10-19).**
- **What happened:** `a_dropped_transcode_restarts_where_it_stopped` failed once
  in 5 parallel runs. Under load, the fixture cut the stream before any audio
  was submitted.
- **Why that is by design:** progress was 0, so the episode is not admitted
  ("a transcode that never played → gone check").
- **Fixture change:** it now holds the connection 1.5 s before cutting, as a
  real mid-track drop does. 5 of 5 parallel suite runs then passed.
- **Accepted limit, documented:** a drop before any audio of a track was
  played gets the gone check and the original banner, not a restart. Pressing
  Play resumes.

## Implementation review E1, round 2 (focused on the IE fixes)

Reviewers:
- **Sol:** 1 Blocking.
- **Fresh Sonnet subagent:** no Blocking. It ran 29 tests, all passing, and
  verified IE-1 to IE-9 in code. Its main finding equals Sol's Blocking one.

| # | Finding | Sev | Disposition |
| --- | --- | --- | --- |
| IE2-1 | A replacement that reaches the engine after its expiry is refused while the failure stays held; the worker dropped its slot, leaving it buffering until 90 s (O Blocking, S M1) | Blocking | Fixed: on `Refused`, if the engine still holds that failure, the worker sends `Abandon` (banner unless the intent is paused) and, for a `Replace`, presents Core's item unloaded |
| IE2-2 | Switching offline mode on did not cancel a held recovery (O M2) | Major | Fixed: offline mode releases it (silent `Abandon`) |
| IE2-3 | The PulseAudio drain test was scheduler-dependent (O minor) | Minor | Fixed: `truncating_server` holds the connection before cutting (playing case). The paused case cuts at once, because a paused decoder only reads its short read-ahead |
| IE2-4 | `prepare()` could run while a failure is held (S minor) | Minor | Fixed: preparation is skipped while a failure is held or being resolved |
| IE2-5 | `resolve` errors on a dead engine are retried forever; the `Expired` banner ignores pause (S minor) | Minor | Accepted: only with a dead engine thread; the banner then is harmless |

Gates after the fixes:
- `./tools/check.sh`: 397 Rust tests, 0 failed.
- Real PulseAudio: 8 passed.
- Not added (recorded): worker-level tests for late outcomes and queue-full
  retry.

## Plan E2/E3 — successor timing, resume buffer, cache-ahead yield

User decision (2026-10-19): "go on with E2 and E3". The round-1 engine findings
(see "Round 1 reviews (plan M1)") are the input; each is mapped below.

Code facts this plan relies on:
- **Reader:** `http::Reader` keeps at most `CAPACITY` (4 MiB) of undecoded
  input in its window. `window.eof` is set when the response completed;
  `window.total` is the Content-Length when known (originals, not
  progressive transcodes). The transcode URL carries `musicBitrate=<kbps>`.
- **Decoder:** `fill(n)` keeps up to `n` frames of PCM ready; today the
  current decoder is asked for `tail + DECODE_AHEAD_FRAMES` (2 s).
- **Output:** PulseAudio reports `running` (set by the started callback,
  cleared by underflow) and an `underflows` counter; `tlength` = 300 ms and
  `prebuf` = tlength. Test sinks consume in real time (Fake) or instantly
  (Capture).
- **Uncork sites in the actor:** `Control::Play`, `Control::Recording(false)`,
  the crossfade push and the normal push in `pump`. The starving branch corks
  when `queued_bytes() == 0`.
- **Runtime:** the worker loop runs at least every 25 ms and reads
  `engine.handle.snapshot()`. Cache-ahead and manual downloads share one
  worker (`cache::worker`) gated by `Gate::allowed()` (pause, Wi-Fi-only);
  `DownloadControl::stop_if_blocked` cancels when the gate closes.

### E2.1 Successor pre-load waits for a healthy current stream (actor)

- **Pending successor:** `Actor::prepare(Some(source))` with a remote
  source, while the current decoder is remote, stores it in a new
  `pending: Option<Source>` instead of creating the decoder. A local
  (`file:`) successor, or any successor of a local current track, is created
  at once (today's behaviour).
- **Start condition**, checked in `pump` after `current.fill` (so it is
  evaluated even while the output is full or corked), creates the decoder
  when any of these holds:
  1. the current reader completed (`window.eof`);
  2. the current reader's window holds at least `PRELOAD_AHEAD` of audio
     (20 s), or at least 3/4 of `CAPACITY` (the reader is throttled by the
     decoder, so the network is ahead);
  3. the current track's remaining decode time
     (`duration - resume - frames/RATE`) is at most `PRELOAD_LEAD` (60 s)
     plus the crossfade tail, **and** the actor is not rebuffering (E2.2).
     A duration of 0 (unknown) never satisfies this condition.
- **Audio time in the window:** bytes ÷ a rate estimate:
  - transcodes: `musicBitrate` × 125 bytes/s, from the URL;
  - originals: `total × 1000 / duration` bytes/s when both are known;
  - otherwise no estimate: only conditions 1 and 3 apply.
  `Reader` gains one accessor returning `(window bytes, eof, total)` under
  the window lock; `Decoder` gains `buffered_ms() -> Option<u64>` and
  `complete() -> bool` (true for a local file).
- **Lifecycle** (round-1 O M6/S M2), every path that clears `next` clears
  `pending`: `load`, `Stop`, `enter_failed`, `unload`, `Prepare(None)`, and a
  replacing `Prepare(Some)`. `take_successor` returns a pending source as
  `Err(source)`, so a seek re-prepares it (and re-defers it, since the new
  current is remote). `Prepare` while failing/failed is ignored (unchanged).
  A failed deferred `Decoder::new` sends today's "Next track decoding
  failed" event.
- **Diagnostics:** `audio successor deferred occurrence=…` and
  `audio successor started occurrence=… reason=complete|ahead|lead|local`.
- **End without a successor decoder:** if the current decoder reaches EOF
  with the successor still pending (only possible when the reader
  completed in the same tick), the decoder is created in that tick before
  the gapless check; otherwise the natural End path starts it on a fresh
  output, as today when no successor was prepared.
- **Accepted:** on a link slower than the bitrate the successor starts late
  and gapless can degrade to a short gap at the boundary.

### E2.2 Resume only with a buffer (actor)

- **One gate:** a new `gate: Option<Gate>` on the actor, with
  `Gate::Start` (a fresh remote load or seek) and `Gate::Stall` (the stream
  ran dry). While a gate is set the output stays corked. Every uncork site
  (`Control::Play`, `Control::Recording(false)`, crossfade push, normal push)
  goes through one `fn release(&mut self)` that uncorks only when
  `desired && !recording && gate.is_none()` (round-1 O M4/S M3).
- **Entering `Stall`** (round-1 O B3/S M4): while uncorked and desired, the
  current decoder has no PCM to push (`remaining() == 0`, not `eof`) and the
  output is starved:
  - PulseAudio: an underflow was reported since the last uncork
    (`Output::starved()` compares the counter saved by `set_playing(true)`);
  - test sinks: `queued_bytes() == 0` (today's condition).
  The actor corks the output (queued audio, if any, is retained) and logs
  `audio rebuffer start occurrence=… position_ms=…`.
- **Entering `Start`:** `load` sets it when the decoder is remote. A local
  decoder gets no gate.
- **Clearing a gate**, checked in `pump` right after `current.fill`:
  - decoded PCM ready (`current.remaining()`) ≥ `START_BUFFER` (1 s) for
    `Start` or `RESUME_BUFFER` (5 s) for `Stall`; or
  - the current decoder reached EOF (short tails, round-1 O M5: whatever is
    left plays, then the existing finish/gapless path runs); or
  - the current reader completed and all its bytes are decoded.
  Then `release()` runs and the actor logs
  `audio rebuffer end occurrence=… waited_ms=… buffered_ms=…`.
- **Decode budget:** while a gate is set, the current decoder is filled to
  `tail + max(DECODE_AHEAD, threshold)` frames (5 s of stereo f32 is about
  1.9 MB), back to today's budget once cleared.
- **Interaction with transport:** Pause keeps the gate (the output is corked
  anyway); Play with a gate set does not uncork, `pump` releases when the
  threshold is met. Seek and Load reset it (`Start` for a remote decoder).
  Stop and failure paths clear it. A drain of a failing decoder (E1) clears
  the gate: the queued audio plays out.
- **Presentation:** `publish` already reports `buffering = desired &&
  !recording && !playing`, so a gated output shows buffering with no change.
  A new `Snapshot.starved` (and `Change.starved`) is true while a `Stall`
  gate is set, or a `Start` gate has lasted more than 1 s, with play intent.
  It drives E3 only; QML and MPRIS are unchanged.

### E3 Cache-ahead yields to a starving stream

- **Hold, separate from `Gate::allowed()`** (round-1 O M10/S m4):
  `cache::Gate` gains `held: tokio::sync::watch::Sender<bool>`, set by
  `DownloadControl::hold(bool)`. `allowed()` and `stop_if_blocked` ignore it,
  so a hold never cancels, never resets `pending`/`active_key`, and never
  drops a partial file.
- **Worker:** before starting each item and before each `response.chunk()`
  read, the download loop awaits the hold's release with `select!` on the
  cancel watch (a cancel still ends the download promptly). The open response
  is kept. If the server drops it during a long hold, the existing
  interruption/range-resume path applies (it costs one of the batch's
  4 attempts; accepted). Manual downloads share the worker and are held too.
  Artwork fetches are not held (small, separate gate use).
- **Runtime policy** (round-1 O M12/S B2), in the worker loop:
  - set the hold when the engine snapshot reports `starved`;
  - keep it until the engine has reported play intent, loaded and not
    starved for 10 s (`HOLD_RELEASE`);
  - release at once when the engine is unloaded, paused (no intent), held
    in a failure (E1), or stopped, on `resets_session`, and when the
    download control is replaced (session rebuild).
  The worker logs `cache hold active=…` on each change.
- **Cache status:** unchanged (a held download still reports downloading).

### Tests

Actor (`audio::*_tests`, Fake/Capture sinks, local HTTP fixtures):
- a remote successor is not requested while the current stream is slow and
  far from its end; it is requested once the current reader completes, and
  at once for a short current track (lead) and for a local successor;
- a seek and a Stop drop a pending successor (no request is ever made);
- a throttled current stream (1 s served, a 3 s stall, then the rest):
  the output stays corked until 5 s are decoded or EOF, `buffering` and
  `starved` are true meanwhile, and Play during the stall does not uncork;
- a remote load stays corked until 1 s is decoded; a local load is not gated;
- a track whose remaining audio after a stall is under 5 s resumes at EOF.

Real PulseAudio (`pulse_server_tests`):
- a stream that stalls mid-track reports at most one underflow for the
  stall and resumes with the full buffer (today: repeated underflows).

Cache (`cache.rs`):
- a held download makes no progress, then resumes on the same response
  (one request in the server log); a cancel while held ends it promptly;
  a manual download is held too.

Runtime (`fixture_tests` or a unit test of the hold policy):
- `starved` sets the hold; 10 s of playing, a pause, a stop and a held
  failure each release it.

### Docs and gates
- `docs/architecture.md` (audio pre-load and resume gate, cache hold),
  `docs/validation.md`, `docs/roadmap.md` Open items.
- `./tools/check.sh`, the real-PulseAudio suite, `./tools/build-sailfish.sh`.
  A phone run on mobile data remains pending (the user asked for no deploy).

## Round E2/E3 reviews

Reviewers:
- **Sol:** 0 Blocking, 5 Major, 2 Minor. Verdict: revise.
- **Fresh Sonnet subagent:** 0 Blocking, 5 Major, 12 Minor. Verdict: not
  ready as written.
- **Third chain:** unavailable (all three models failed preflight, exit 3).
  Recorded.

| # | Finding | Sev | Disposition (E2.1) |
| --- | --- | --- | --- |
| P1 | With crossfade on, a starved decoder parks at `remaining == tail`, so a `remaining()==0` stall trigger never fires; a stalled successor in the crossfade wait is another no-progress path (O1, S M1) | Major | Stall is entered from the existing no-progress branches (nothing pushable, not finished), measured on the decoder being waited on |
| P2 | Gate thresholds count the reserved crossfade tail, so a gate can clear with nothing pushable and flap (S M2) | Major | Thresholds use pushable audio: `remaining - eff_tail` before EOF, the successor's PCM in the crossfade wait |
| P3 | Releasing at EOF does not play a tail under PulseAudio's prebuf, notably before a stalled successor (O2) | Major | Every gate release calls `pa_stream_trigger` (documented: starts playback, disabling prebuffering for that start) |
| P4 | "Underflow since the last uncork" stays latched after PulseAudio restarts by itself; the first glitch is heard before the gate engages (O3, S M4) | Major | `Output::starved()` = uncorked, not running, and the last stream fact was an underflow (cleared by the started callback); test sinks: uncorked and nothing queued. Only remote decoders are gated |
| P5 | The cache-hold policy in the worker loop is delayed by synchronous Core requests (O4, S m9) | Major | The policy runs in its own task on the shared runtime, driven by engine changes and a timer; the wanted state is stored in `Shared` and re-applied to a replaced download control |
| P6 | Reader EOF/window may describe probe ranges, not playback input; the bitrate estimate is a heuristic (O5, S m2, S m5) | Major | Health is evaluated only after the decoder has produced PCM; `complete` also needs the window end to equal `total` when known; the estimate is documented as advisory, with `3/4 CAPACITY` as the main signal |
| P7 | A pulled-forward successor reader still takes 4 MiB at full speed (S M3) | Major | Successor readers get a 1 MiB window until they become the current decoder, then `CAPACITY` |
| P8 | Test sinks and PulseAudio use different stall detectors; production path barely tested (S M5) | Major | One `Output::starved()` behind every backend; real-PulseAudio tests for stall, crossfade-on stall, seek and policy cork during a stall |
| P9 | Seek test contradicts the pending lifecycle (O6) | Minor | Fixed: a far seek keeps and re-defers the pending successor, a near-end seek starts it, Stop and a replacing Prepare drop it |
| P10 | A held transcode cache download that the server drops restarts from zero (O7, S m10) | Minor | Mid-body hold only for range-resumable originals; transcode downloads are held only between items |
| P11 | Uncork list incomplete (`start_drain`, crossfade branch); `Recording(true)` needs a cork path (S m1) | Minor | `cork()`/`release()` pair; `start_drain` clears the gate first (its queued audio must play) |
| P12 | Decoder EOF with the successor still pending is not limited to one tick (S m2) | Minor | `current.eof` is a start condition, checked before the gapless block |
| P13 | `Change.starved` wakes MPRIS subscribers (S m3) | Minor | Kept: the cache-hold task needs the wake; the MPRIS key already ignores fields it does not present |
| P14 | Start gate adds latency to every remote seek (S m6) | Minor | `START_BUFFER` is 500 ms (above the 300 ms prebuf); Start and Stall log differently |
| P15 | No bound on a gate: a link at half the bitrate stays buffering for long (S m7) | Minor | After `GATE_PATIENCE` (15 s) the threshold drops to `START_BUFFER` |
| P16 | Late "Next track decoding failed" from a deferred create (S m8) | Minor | Test: a 404 at deferred creation leads to exactly one recovery check |
| P17 | Manual downloads are held with no UI (S m11) | Minor | Accepted and documented: the hold lasts at most the stall plus 10 s |
| P18 | Reader accessor duplication; "reader completed and decoded" equals decoder EOF (S) | Minor | Extend `Reader::diagnostics`; the redundant clear condition is dropped |

Also checked: cache-ahead does not run on cellular when a mobile cap is set
and downloads are originals (`lib.rs:992`), and Wi-Fi-only downloads block it
by default. E3 therefore matters on slow Wi-Fi and when mobile downloads are
allowed.

PulseAudio facts (libpulse `pa_buffer_attr` and `pa_stream_trigger`
documentation): after an underrun, playback stops until `prebuf` bytes are
queued again; `pa_stream_trigger` requests an immediate start, disabling
prebuffering for that start.

## Plan E2.1/E3.1 — the plan above with the round amendments

Only the changes to "Plan E2/E3" are listed; everything else stands.

### E2.1 Successor timing
- **Start conditions** (any): the current decoder reached EOF; the current
  reader completed (`eof`, and its window end equals `total` when known);
  the window holds ≥ 20 s by the advisory rate estimate, or ≥ 3/4 of
  `CAPACITY`; remaining decode time ≤ 60 s plus the tail, with no `Stall`
  gate in the last 10 s. The reader conditions count only after the current
  decoder has produced PCM.
- **Successor window:** `Reader::new` takes a capacity. A successor decoder
  is created with 1 MiB and raised to `CAPACITY` when it becomes the current
  decoder (the gapless join in `pump`, or a load).
- **Accessor:** `Reader::diagnostics` is extended with `eof` and `total`.

### E2.2 Resume gate
- **Gated decoders:** only remote ones. The waited-on decoder is the current
  one, or the successor during the crossfade wait.
- **Pushable audio:** `remaining - eff_tail` before EOF, `remaining` at EOF;
  the successor's `remaining` in the crossfade wait.
- **Stall entry:** at the existing no-progress branches (normal path with
  nothing pushable and not finished; crossfade wait for successor PCM),
  when `Output::starved()`:
  - PulseAudio: uncorked, `!running`, and the last stream fact was an
    underflow (`Events.underrun`, set by the underflow callback, cleared by
    the started callback);
  - test sinks: uncorked and `queued_bytes() == 0`.
- **Release:** pushable ≥ threshold (Start 500 ms, Stall 5 s), or the
  waited-on decoder reached EOF, or `GATE_PATIENCE` (15 s) passed with
  ≥ 500 ms pushable. `release()` uncorks and calls `Output::trigger()`
  (`pa_stream_trigger`; a no-op for test sinks).
- **Uncork/cork:** `release()` is the only uncork for Play, recording end,
  and both pushes; `cork()` for Pause, recording start and stall entry.
  `start_drain` clears the gate before its uncork.
- **Logs:** `audio buffer start kind=start|stall …` and
  `audio buffer end kind=… waited_ms=… buffered_ms=…`.

### E3.1 Cache hold
- **Mid-body hold** only for range-resumable original downloads; transcode
  downloads finish the current item, and the hold applies before the next.
- **Policy task:** spawned with the worker on the shared runtime. It
  subscribes to `engine.handle.changes()` (with `starved` in `Change`) and
  sleeps until the release deadline. It writes the wanted state to
  `Shared::cache_hold` and applies it to `shared.control`; installing a new
  control applies the stored state.
- **Release:** play intent, loaded, not failed and not starved for 10 s; at
  once on unloaded, paused, failed or stopped.

### Tests added to the plan's list
- A crossfade-on stall enters `Stall` and resumes only with 5 s pushable.
- A stall waiting on a successor in the crossfade wait.
- A 200 ms tail before a stalled successor plays after release (real
  PulseAudio, via trigger).
- No false `Stall` after PulseAudio restarts by itself.
- Seek and policy cork during a `Stall` (real PulseAudio).
- Successor window capped at 1 MiB until promotion.
- A deferred successor that 404s leads to exactly one recovery check.
- `GATE_PATIENCE` lowers the threshold.
- The cache hold applies to a replaced control and is not delayed by a busy
  worker loop.
- A transcode cache download is not held mid-body.

## Round E2.1/E3.1 reviews

Reviewers:
- **Sol:** 0 Blocking, 5 Major, 1 Minor. Verdict: revise.
- **Fresh Sonnet subagent:** 0 Blocking, 4 Major, 8 Minor. It confirmed
  P4, P5, P7, P9, P11-P13 and P18, and that the `plexfreq-rt` thread is not
  blocked by Core requests. Verdict: revise.
- **Third chain:** not retried (unavailable this session).

| # | Finding | Sev | Disposition (E2.2/E3.2) |
| --- | --- | --- | --- |
| Q1 | Gating the crossfade wait flaps (release with nothing joinable for tails ≥ 6 s) or never clears (successor filled only to the tail) (O2, S M1) | Major | The crossfade wait is never gated. If the output starves while waiting for successor PCM, that boundary's fade is dropped: the old tail plays raw, the gapless join follows, and a starving successor then stalls as the current decoder |
| Q2 | Trigger on every `release()` disables prebuffering on ordinary pushes; trigger before PCM is submitted underruns (O1, S M3) | Major | One-shot: after the first push following a gate release, and after the raw tail of a dropped fade. A failed trigger is logged, never an error. `pa_stream_trigger` is added to the FFI use (libpulse-sys has it) |
| Q3 | `underrun` must flow through `Stream::service` and `Output::service`; it stays true until the started callback (S M3) | Major | Carried through both; cleared locally on release |
| Q4 | Capture sinks would be "starved" at every decoder lag (S M2) | Major | Capture never reports starved; only Fake and PulseAudio model stalls |
| Q5 | Hold replay on a replaced control races (O3) | Major | Both the policy task and the installers update the wanted state and apply it while holding the `control` mutex |
| Q6 | The policy task has no owner or shutdown (O4) | Major | An `rt::Task` owned by the worker (abort on drop), dropped before exit; the worker releases the hold at exit. It also ends when the change channel closes |
| Q7 | A hold can miss a blocked body read; and a paused read does not stop the server sending until the TCP receive buffer (megabytes) fills (O5, S M4) | Major | Pausing reads is replaced by dropping the connection. Originals: the read is selected against the hold, the response is dropped, and the item re-runs with a range resume after release, without spending an attempt. Transcodes cannot resume, so they are only held between items. Documented: E3 helps originals and item starts |
| Q8 | The completion accessor needs the window end (O6) | Minor | `Reader::facts()` computes `complete` and the buffered bytes under the window lock |
| Q9 | A far seek keeps a created, untouched successor downloading (S m1) | Minor | For a remote current, the seek turns a created successor back into a pending source |
| Q10 | `GATE_PATIENCE` counted paused time (S m2) | Minor | Only un-paused, non-recording time counts |
| Q11 | Window capacity needs a mutable field, a wake, and all promotion sites (S m3) | Minor | `Buffer.capacity` (atomic) plus `room.notify_one()`; raised at the gapless join, the only promotion of a created successor once Q9 applies. The `http.rs` capacity test is updated and a raise test added |
| Q12 | A promoted empty successor waits 5 s (S m4) | Minor | Intended: it is a real stall of a remote stream; documented |
| Q13 | `starved` for a paused Start gate (S m7) | Minor | `starved` requires play intent; a satisfied gate clears while paused |

## Plan E2.2/E3.2 — consolidated (implementation basis)

### E2a Successor timing (actor and reader)
- **Pending successor:** a remote successor of a remote current decoder
  is stored as `pending: Option<Source>`. A local successor, or any
  successor of a local current decoder, is created at once.
- **Creation** happens in `pump`, after `current.fill`, when any of these
  holds:
  1. the current decoder reached EOF;
  2. the current decoder has produced PCM, and either its reader is
     complete (`eof`, and the window end equals `total` when known), or its
     window holds ≥ 3/4 of `CAPACITY`, or ≥ 20 s by the advisory rate
     (`musicBitrate` × 125 B/s for transcodes; `total`/duration for
     originals);
  3. remaining decode time ≤ 60 s plus the tail, with no stall in the last
     10 s.
- **Successor window:** a successor reader is created with a 1 MiB
  window; the gapless join raises it to `CAPACITY`.
- **Lifecycle:** `pending` is cleared wherever `next` is: load, Stop,
  failure, unload, `Prepare(None)`, a replacing Prepare. A seek keeps it
  pending; with a remote current, a created successor becomes pending
  again. A failed deferred creation sends today's "Next track decoding
  failed".

### E2b Resume gate (actor and output)
- **Gate:** `Start` after a remote load or seek, `Stall` after starvation.
  Only remote current decoders are gated.
- **Pushable audio:** `remaining - eff_tail` before EOF, `remaining` at EOF.
- **Stall entry:** in the normal no-progress branch (nothing pushable, not
  finished), with `Output::starved()`:
  - PulseAudio: uncorked, `!running` and `underrun`;
  - Fake: uncorked and nothing queued;
  - Capture: never.
  On entry the actor corks.
- **Release:**
  - pushable ≥ 500 ms (Start) or 5 s (Stall); or
  - the decoder reached EOF; or
  - 15 s of un-paused, non-recording waiting with ≥ 500 ms pushable.
  Release uncorks and clears `underrun` locally. A one-shot trigger
  follows the first push.
- **Decode budget while gated:** `tail + max(DECODE_AHEAD, threshold)`.
- **Crossfade wait** (current at EOF, successor not ready for the tail):
  - never gated;
  - if the output starves there, the boundary's fade is dropped
    (`fade_dropped`, reset with the next successor);
  - the raw tail is pushed and triggered once;
  - the join follows.
- **Sites:**
  - `release()` is the only uncork: Play, recording end, and both pushes;
  - `cork()` handles Pause, recording start and stall entry;
  - `start_drain` clears the gate before its uncork;
  - load and seek reset the gate; Stop and failure clear it.
- **Presentation:** `Snapshot.starved` and `Change.starved` are true with
  play intent while a `Stall` gate is set, or a `Start` gate has lasted
  over 1 s. `buffering` is unchanged.

### E3 Cache yield (cache and runtime)
- **Hold signal:** a hold `watch` lives in `cache::Gate`, set by
  `DownloadControl::hold(bool)`. It is independent of `allowed()` and
  cancellation.
- **Download loop:**
  - before each item, wait for release or cancel;
  - while reading an original's body, `select!` the chunk read against the
    hold. When the hold rises, drop the response (the partial is kept) and
    return `Failure::Held`; the item re-runs after release with a range
    resume, without spending an attempt;
  - transcode downloads are not interrupted mid-body.
  Manual downloads follow the same rules.
- **Policy task:**
  - owned by the runtime worker and driven by engine changes plus a timer;
  - hold while `starved`;
  - release after 10 s with intent, loaded, not failed and not starved, or
    at once when not loaded, paused, failed or stopped;
  - the wanted state is in `Shared`, written and applied under the
    `control` mutex, and installers apply it under the same lock;
  - the worker releases the hold at exit.
- **Logs:** `cache hold active=…`.

### Tests
- **Actor (Fake sink, local HTTP fixtures):**
  - a successor's HTTP is deferred for a slow, long current track, and
    starts once the current is complete, at once for a short current
    track, and at once for a local successor;
  - Stop and a replacing Prepare never request it;
  - a far seek keeps it pending;
  - a deferred 404 sends one "Next track decoding failed";
  - the Stall gate holds until 5 s pushable or EOF, also with crossfade on;
    Play during a stall does not uncork; patience lowers the threshold and
    ignores paused time;
  - the Start gate needs 500 ms; local loads are not gated;
  - a starved crossfade wait drops the fade, and the raw tail and the
    successor play.
- **Reader:** the successor window cap is honoured, and the raise frees it.
- **Real PulseAudio:**
  - a mid-track stall gives one underflow, then resumes after the buffer;
  - no false stall after PulseAudio restarts by itself;
  - a short tail before a stalled successor plays after the trigger;
  - a policy cork during a stall.
- **Cache:**
  - a held original download drops its response and resumes by range with
    no attempt spent;
  - a transcode download finishes its item, then waits;
  - a cancel while held ends it;
  - a manual download is held.
- **Runtime:** the hold task holds on `starved`, releases on pause, on stop
  and after 10 s of play, re-applies to a replaced control, and ends with
  the worker.

## Round E2.2/E3.2 reviews and amendments (E2.3/E3.3)

Reviewers:
- **Sol:** 1 Major. Q1-Q6 and Q8-Q13 confirmed. Verdict: revise.
- **Fresh Sonnet subagent:** 2 Major, 7 Minor. Q1-Q13 resolved. Verdict:
  revise; the Minors can go in as implementation notes.
- **Third chain:** unavailable this session.

| # | Finding | Sev | Disposition |
| --- | --- | --- | --- |
| R1 | An "original" is not necessarily range-resumable: no validator, or a server answering a range with 200, restarts from zero, and `Held` restarts were unbounded (O, S M1) | Major | A download is interrupted mid-body only if it is an original (quality 0) with a validator and a known total. If a resume after a hold comes back as a full 200 response, that item is not interrupted again in this batch. Otherwise the hold applies between items. Fixtures: with ETag, without validator, range answered with 200 |
| R2 | The estimate-based cork (`queued_bytes()==0`) at the no-progress branch would pre-empt `starved()` (S M2) | Major | Replaced: the no-progress branch acts only on `Output::starved()`. A remote decoder enters `Stall`; a local one corks as today. The next push uncorks through `release()` when no gate is set. Test: the estimate reaching 0 before an underflow does not cork |
| R3 | One-shot trigger after one 10.7 ms block can underflow again (S m1) | Minor | The trigger is armed by the release and fires at the end of that pump batch, if PCM was pushed |
| R4 | The threshold excludes the crossfade reservation, so a Stall with crossfade needs tail + 5 s (S m2) | Minor | Kept, documented: the reserved tail is not playable before EOF; `GATE_PATIENCE` bounds it. The default crossfade is 0 |
| R5 | `eff_tail` is computed before the successor is created (S m3) | Minor | Creation runs before `eff_tail` is computed |
| R6 | A held download still shows as downloading (S m4) | Minor | Status gains `held: true` while held; the UI is unchanged (recorded) |
| R7 | The `Held` loop is unspecified (S m5) | Minor | An inner loop around `download`: no `failed`, no `terminal`; the transcode stop request still runs |
| R8 | The Start gate changes remote fixture timing (S m6) | Minor | Affected tests are updated and listed in the implementation record |
| R9 | 3/4 `CAPACITY` fires early on lossless (S m7) | Minor | Accepted: a full window means the link is ahead; documented as an OR |

## Implementation review E2/E3, round 1

Reviewers:
- **Sol** (with the diff): 0 Blocking, 3 Major, 1 Minor. Verdict: revise.
- **Fresh Sonnet subagent:** 0 Blocking, 1 Major (reproduced as a flaky
  `tests/cache.rs` run), 4 Minor. It ran the actor and cache tests and
  confirmed the gate state machine, the pending lifecycle, the trigger FFI,
  the reader capacity, the hold subscription and the yield task locking.
- **Third chain:** unavailable this session.

| # | Finding | Sev | Disposition |
| --- | --- | --- | --- |
| IE3-1 | A cancel landing with a hold release could lose the random `select!`, letting a cancelled batch send one request with the old token (S M1, reproduced) | Major | Fixed: `biased` with the cancel first in `unheld`, the chunk-loop select and `cancellable`; the batch epoch is re-checked after a hold wait. `tests/cache.rs` run 5 times in parallel afterwards |
| IE3-2 | A recording hold (intent kept, output paused) kept downloads held for 10 s (O M1) | Major | Fixed: healthy playing also requires `!paused`; runtime test for the recording snapshot |
| IE3-3 | The range-ignored flag was reset on each retry pass (O M2) | Major | Fixed: a per-batch set; unit test across a retry pass (test-only 300 ms pass delay), mutation-checked |
| IE3-4 | No test discriminated the sub-prebuffer trigger (O M3) | Major | Added a real-PulseAudio test: a 200 ms tail after a stall, before a successor with no audio; mutation-checked |
| IE3-5 | Successor start tests could pass through the wrong condition (O m) | Minor | Completion, input health and lead are isolated and asserted by the recorded reason (the lead time is a field); the two health predicates, window fill and the bitrate estimate, are accepted together |
| IE3-6 | A trigger armed with nothing to push could fire on a later unrelated push (S m2) | Minor | Fixed: armed only with pushable audio |
| IE3-7 | `starved()` can hold while < 300 ms are queued below the prebuffer (S m3) | Minor | Accepted: it is a real stall of a remote stream; the gate then waits for 5 s |
| IE3-8 | Each held re-entry re-runs `download()` from the top (S m4) | Minor | Accepted: one cycle per hold episode, bounded |
| IE3-9 | `downloading`/`received` stay set while held (S m5) | Minor | Accepted: `held` is reported; the partial and resume record stay consistent (verified by the reviewer) |

## Implementation review E2/E3, round 2 (narrow confirmation)

- **Sol** (with the diff): no Blocking/Major. IE3-1 to IE3-4 and IE3-6
  fixed; IE3-5 substantially fixed (window fill and the bitrate estimate are
  asserted together; wording corrected above).
- **Fresh Sonnet subagent:** no Blocking/Major. It ran the actor, hold and
  yield tests (13 passed), `tests/cache.rs` 5 times (15 passed each), and the
  trigger test under the isolated PulseAudio server. Note: the trigger test
  only proves anything under `tools/pulse-test-server.sh` (it skips
  otherwise); the throttled actor tests depend on wall-clock pacing.

Gates after the fixes:
- `./tools/check.sh`: 420 Rust tests, 0 failed.
- 5 parallel runs of the library tests plus `tests/cache.rs`: all green.
- The cancel-while-held test 40 times: 0 failures.
- Real PulseAudio: 13 passed.

## CI failure after release 0.1.7 (2026-10-19)

**What failed.** `pulse_trigger_plays_a_tail_below_the_prebuffer` failed on
CI in the "memfd/srbchannel" job. It was reproduced locally: 3 of 3 runs
failed with `PLEXFREQ_PULSE_SHM=0`, and 1 of 3 with shm.

**Diagnosis (diagnostics forced on in a local run).** The trigger did play
the 200 ms tail.
- After the tail, PulseAudio underflowed. The successor, still without
  audio, entered its Stall gate and was corked.
- The corked stream's timing stayed a few frames short of the boundary, so
  the `Transition` was never reported.
- The first fix (settle the position on any stall) was refuted by the
  mutation check. Without the trigger the test then passed, because
  `starved()` (`!running && underrun`) also held while the tail sat
  unplayed below the prebuffer: the risk the reviewer had flagged as IE3-7.

**Fix (after review, see below).**
- Stall detection keeps `!running && underrun`.
- Once the server's queue is empty, the output position settles at the
  submitted frames. The check runs at stall entry and on every tick while a
  Stall gate is set: `Output::settle_drained`, with `Stream::empty` meaning
  writable ≥ the negotiated `tlength` from `pa_stream_get_buffer_attr`.
- Audio queued below the prebuffer is therefore still a stall (the gate and
  the trigger release it), but it is never counted as played.

**Checks.**
- Both CI PulseAudio commands (shm and `PLEXFREQ_PULSE_SHM=0`): 17 passed
  each.
- The stall tests: 4 runs (2 per mode) green.
- Trigger mutation: fails with shm off and passes with shm on. Over shm the
  server started the short tail by itself here; CI runs both modes.
- `./tools/check.sh`: 420 passed.

Narrow review of the first version of this fix:
- **Sol:** 1 Major. Requiring an empty queue for `starved()` would never
  gate a stall that had a sub-prebuffer trickle queued, which brings back the
  old stutter (no gate, no patience, no trigger). It also noted that
  `writable() >= tlength` is libpulse's request accounting, not a synchronous
  queue query, and that "played" means "left the stream's queue": sink
  latency still applies.
- **Fresh Sonnet subagent:** the same Major, independently. It also found
  the emptiness test otherwise sound (null attribute and errors fail safe),
  confirmed the settle is monotonic, and noted a misplaced doc comment.

Dispositions:
- The Major is fixed by the split above: stall detection is back to the
  reviewed condition, and only the settle needs an empty queue, re-checked
  while gated in case the request arrives late.
- The wording "true by construction" is withdrawn: the settle is a bounded
  approximation (sink latency).
- The doc comment is moved.
- No test covers a trickle followed by a dead source; that case now takes
  the same path as before this fix.

Re-run after the split:
- The 5 stall/trigger tests: 2 runs per mode, all green.
- The trigger mutation still fails with shm off.
