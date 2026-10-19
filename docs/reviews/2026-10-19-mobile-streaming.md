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
