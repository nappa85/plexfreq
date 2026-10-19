# Stale queue recovery after server re-key — 2026-10-18

## Incident

See `docs/validation.md` "Stale queue after server library re-key". The Music
section was recreated on the server (section 7 `createdAt` 2026-10-18 09:51; new
part timestamps 2026-10-17 21:08). Every queued ratingKey and part returns 404. Only
cached tracks play. The first uncached successor (old 33963, now 98244) fails both
direct (404) and transcode (400) attempts, and playback stops. The log shows a
misleading Symphonia probe error.

Server facts measured from the phone (one PMS, one library):
- `/library/metadata/{old}` → 404; `/library/metadata/98244` → 200 with
  `guid=plex://track/5d07cd9d403c640290f2af75`.
- `/hubs/search?query=Blood Drenched Execution&limit=10` → the new track (ratingKey,
  title, grandparentTitle, parentTitle, duration, Part key).
- `/library/sections/7/all?type=10&guid=<guid>` and `…&title=…&grandparentTitle=…`
  → the new track (not used by plan v2).
- Transcode `start.m3u8?path=/library/metadata/{old}` → 400 (not 404). The direct
  status alone therefore can't prove "gone"; a metadata lookup can.
- Persisted queue items carry no `guid` (`Item` has no such field today).

## Plan v1 — reviewed in round 1

v1 proposed:
- a new `Command::RecoverStaleTrack`, inserted between the cache and transcode fallbacks;
- guid-first resolution per music section, with a title/artist/album fallback;
- skip-on-Gone via `Next { automatic: true }`, capped at 5;
- a new transient notice string, with HTTP status/source-error diagnostics;
- a new `guid` field on `Item`.

## Round 1 reviews (plan v1)

Reviewers: OpenAI Sol (`openai/gpt-6.1-sol`, `--chain main`), a fresh Sonnet
subagent (read-only), and Space Bunny (`opencode/space-bunny-free`, `--chain third`,
the first that worked). All three: not ready to implement. Every finding below was
adjudicated against the code. "S" = Sonnet, "O" = Sol, "T" = third.

| # | Finding (reviewers) | Sev | Adjudication | Disposition in v2 |
| --- | --- | --- | --- | --- |
| 1 | A new Command is wire-reachable (`Command::parse` rejects only `_`/unknown ops, `command.rs:604-630`). NotStale/Gone as `Error` leak untranslated text (S B2, T B3) | Blocking | Valid | No new Command. `pub(crate) fn` on Core returns an internal enum (like `prepare_next`, `lib.rs:220`) |
| 2 | Not in the `persist`/`resuming` lists (`lib.rs:1395-1454`): new occurrence, zeroed listened, queue not saved (S B1, O 3, T B2/M8) | Blocking | Valid | Resolution only replaces the item. The plan is rebuilt by the existing `Command::Resume`, which is in both lists |
| 3 | `Next{automatic:true}` honours repeat-one (`queue.rs:381`). Radio timeline/refill/autoplay side effects. Queue end not detected (O 1/8, S M3/M6, T M5) | Blocking | Valid | Skip = non-automatic `Next` after an explicit `at_end` precheck; radio sessions are excluded from recovery and skip |
| 4 | The resolver is unbounded off the view scope (`plex.rs:152-178`; 15 s per request) and blocks shutdown (T B1, O 5, S M1) | Blocking | Valid | One overall 5 s budget; at most 2 requests; check `shared.stop`; recheck a transport epoch before acting |
| 5 | Items without Media/Part 404 inside `playback_plan` (`lib.rs:843-855`), with no `Event::Error` (O 2) | Blocking | Valid | Same resolution inline in `playback_plan` for that branch |
| 6 | Prepared successor failure emits `Next track decoding failed` (`audio/mod.rs:616-624`) → `_audio_error` banner while the current track plays (S M4, O 4) | Major | Valid | Retire the prepared successor silently and don't re-prepare that item; recovery runs at the transition |
| 7 | Trigger too broad: any decode failure → network lookups (S M1) | Major | Partly valid: bounded by the cache-miss/stream-only guards and one metadata GET | Kept, with a once-per-queue-item guard and the 5 s budget |
| 8 | guid path is dead for the incident; `local://` guids are not stable; section scan with stale section ids; paging/ambiguity (S M2, O 6, T M4/M11/n22) | Major | Valid | Drop guid and the section scan. One live `/hubs/search` (not `container_cached`); exact multi-field match; fail closed |
| 9 | A unique text match may be the wrong recording (O 7) | Major | Valid | Also require equal `index`/`parentIndex` when known, and all identity fields non-empty |
| 10 | No notice surface on NowPlaying; `ok:false` sets the sticky `m_error` (`backend.cpp:63-65`) (S M5, O 9, T M9) | Major | Valid | No transient notice. Skips are logged; a terminal stop uses the existing `_audio_error` with one new `Error::Input` string |
| 11 | Translation needs all 39 locales in `tools/translations.json`; extraction only sees `Error::Input` literals (`translations.py:35`) (T M10, O 9) | Major | Valid | The single new string is an `Error::Input` literal, authored for all 39 locales |
| 12 | 200 with no Track item treated as NotStale (T M12, S m5) | Major | Valid | 200 without the matching Track → Gone. 401/403/5xx/network → NotStale (existing path) |
| 13 | Listen/history recorded under the dead key, then the same occurrence under the new key (T M7) | Major | Valid in principle; listened ≈ 0 at start | Re-key entries of the current occurrence on replacement; test exactly one listen and one history entry |
| 14 | Skip path leaves `prepared`/`presented.plans` stale; gapless lost (T M6) | Major | Valid | Clear `prepared` and re-run `prepare()` after the skip, as `Event::End` does (`runtime.rs:759`) |
| 15 | Diagnostics: first-io-error wrapper can misattribute exploratory seeks; no occurrence correlation (O 11, T m14/m15, S m3) | Minor | Valid | Status-only log in `Reader::request` with the occurrence id; probe failure reads the Reader's recorded static error (`window.error`) instead of a wrapper |
| 16 | guid erased by cache substitution; guid leaks into payloads (O 10, S m2, T m17) | Minor | Moot | No `guid` field in v2 |
| 17 | Cache orphaning of old-key entries (S m1) | Minor | Valid; out of scope | Follow-up in roadmap Open items |
| 18 | Docs obligations: research/architecture/validation/roadmap (T m20) | Minor | Valid | Added to the plan |
| 19 | Sequencing cache → recover → transcode is sound (S n1) | Note | — | Kept |

## Plan v2

### Scope
1. Re-resolve the current queued track when the server no longer has its ratingKey.
2. If it can't be resolved, skip it (bounded) instead of stopping.
3. Log the real HTTP failure.

Out of scope (roadmap follow-ups): library-cache/section invalidation on section
re-creation; bulk re-resolution of the whole queue; cache-entry re-keying for
recovered items; station/radio queues (recovery and skip disabled while
`radio.is_some()`, existing behaviour kept).

### A. Diagnostics (`src/audio/http.rs`, `src/audio/decode.rs`)
- Pass the occurrence id into `http::Reader::new`. In `request()`, log
  `HTTP audio request failed occurrence=<id> status=<u16>` for 4xx (`Attempt::Failed`)
  and 5xx (`Interrupted`) responses. Status integer and static strings only.
- On probe/packet failure, also log the Reader's recorded static error
  (`window.error`, set by `fail()`), if any:
  `audio source failed occurrence=<id>: <msg>`. Engine error strings are unchanged.

### B. Core resolution (`src/lib.rs`, new `pub(crate) fn recover_stale_current(&mut self, budget: Instant) -> Stale`)
`enum Stale { NotStale, Resolved, Gone }`. These are not `Error` variants and not a Command.
1. `NotStale` immediately if any holds: offline/offline-mode; radio active; no
   current track; the current track is cache-served; this queue item (by
   `play_queue_item_id` or index+ratingKey) was already checked in this session; the
   budget is spent.
2. `GET /library/metadata/{key}` via `self.plex.container` (live, not cached).
   200 with the matching Track → `NotStale`. HTTP 404, or 200 with no matching
   Track → stale. Anything else (401/403/5xx/network/timeout) → `NotStale`.
3. Stale: if title, grandparentTitle and parentTitle are all non-empty, run one
   live `/hubs/search?query=<title>&limit=100`. Keep Track items with a numeric
   ratingKey ≠ old key whose title, grandparentTitle and parentTitle match exactly
   (case-insensitive, trimmed); `index`/`parentIndex` equal when both are > 0; and
   |duration Δ| ≤ 2000 ms when both are > 0. Dedupe by ratingKey. Exactly one
   candidate with a Media Part → `Resolved`. Otherwise (none, several, error,
   budget spent) → `Gone`.
4. `Resolved`: keep `play_queue_item_id`, `queue.replace_track(new)`. Re-key
   `settings.listens`/`settings.history` entries whose occurrence equals the current
   playback occurrence (key/title/album/artist fields). Mark the item checked. No
   plan is built here.
5. `playback_plan` branch for items without Media/Part (`lib.rs:843`): on a 404 from
   that metadata fetch, call the same resolver (step 3/4) inline. On `Gone`, return
   `Error::Input("This track is no longer on the server")`.

### C. Runtime (`src/runtime.rs`)
- `Event::Error`, `id == current`, generic decode failure, source ≠ Cache (the
  existing guard). Order:
  1. `CachedPlayback` (existing).
  2. `core.recover_stale_current(now + 5 s)`.
     - `Resolved`: execute `Command::Resume`. It is in `persist` + `resuming`, so the
       queue is saved and occurrence/listened kept. Set `resumePosition`/
       `resumeListened` from the snapshot, as the cached fallback does, then
       `start_plan` + `prepare()`.
     - `Gone`: skip (below).
     - `NotStale`: fall through.
  3. `TranscodedPlayback` (existing). Then `_audio_error` (existing).
- Before acting on a `Resolved`/`Gone` result, re-read a transport epoch (a
  `Shared` counter bumped on every admitted transport request; add one if none
  exists) taken before the resolver ran. If it changed, apply the queue
  replacement but don't start or skip, so the user's newer intent wins.
- Skip: if `queue.at_end()` and repeat ≠ All, or the consecutive-skip counter has
  reached `min(queue.len(), 5)`, emit `_audio_error` with
  `"This track is no longer on the server"` and stop. Otherwise execute
  `Command::Next { automatic: false }` (ignores repeat-one; no autoplay/radio
  because radio is excluded and the queue isn't at its end), then `start_plan`,
  `prepared = None`, `prepare()`. The counter is a worker local, reset on any
  admitted transport request and when the snapshot reports `listened > 0`.
- Prepared successor failure (`id` == prepared id, `Next track decoding failed`):
  log it, drop `prepared` and its `presented.plans` entry, and remember that queue
  item so `retry_prepare` doesn't rebuild it. No `_audio_error`. The transition then
  takes the normal current-track path above.
- Shutdown: the resolver checks `shared.stop` before each request. Worst case is 2
  requests in a 5 s budget.

### D. Translation
One new Core string, `Error::Input("This track is no longer on the server")`,
authored for all 39 locales in `tools/translations.json`. Regenerate with
`bash tools/build-qm.sh`, then `--check`. No QML change.

### E. Docs
- `docs/research.md`: the PMS facts above (404 vs transcode 400, `/hubs/search`
  shape, section re-creation).
- `docs/architecture.md`: the error fallback order and the skip policy.
- `docs/validation.md`: actual check results.
- Roadmap Open items: follow-ups and live verification.

### Tests (local first; synthetic tokens; fixture server in `src/runtime/fixture_tests.rs` style)
- Resolver:
  - 200 → NotStale; 401/5xx/timeout → NotStale; 404 and 200-empty → stale.
  - Exact match → Resolved; `playQueueItemID` kept; queue.json contains the new key
    after `Resume`; occurrence and listened unchanged; exactly one listen and one
    history entry.
  - Two matches, a duplicate title on a different disc/index, duration mismatch,
    missing identity fields, malformed candidates → Gone.
  - Radio active, offline, cache-served → NotStale with no request made.
  - Already-checked item → no second lookup.
  - Budget exhausted → no further requests.
- Runtime:
  - Stale current → resolved plan plays, no `_audio_error`.
  - Unresolvable → skip, with repeat-one, repeat-all and shuffle.
  - Fully stale queue stops at the cap with exactly one `_audio_error`.
  - At queue end → stop with error.
  - Transport during a delayed resolver → no start/skip.
  - Stale prepared successor → predecessor continues, no `_audio_error`, recovery
    at the transition.
  - Non-stale decode failure → existing transcode fallback unchanged.
  - No-Media item 404 inside `Next` → resolved inline.
- Diagnostics: a 404 logs status and occurrence; the synthetic token never appears
  in the captured log.
- Gates: `./tools/check.sh`, `bash tools/build-qm.sh --check`,
  `./tools/build-sailfish.sh`.
- Phone (normally installed RPM, Sailjail launch): keep the current stale queue as
  the live reproduction. Expect "Blood Drenched Execution" to resolve to 98244 and
  play. A successful build is not a hardware test.

## Round 2 reviews (plan v2)

Same three reviewers (Sol, fresh Sonnet subagent, Space Bunny). Verdict: not ready
to implement (Sol 4 Blocking, Space Bunny 2, Sonnet 1). Round-1 dispositions 1, 9,
10, 11, 12 and 16-19 were confirmed sound. `Command::Resume` rebuilding the
plan after `replace_track` was confirmed correct (occurrence/listened/resume kept,
generation bumped, cache rescheduled, queue persisted). Non-automatic `Next` as the
skip primitive was confirmed correct (shuffle order, ignores repeat-one, no
autoplay/timeline, no listen credit).

| # | Finding (reviewers) | Sev | Adjudication |
| --- | --- | --- | --- |
| R2-1 | Inline resolution in `playback_plan` runs inside `prepare_next` (cloned queue, `lib.rs:229-235`) and before the new occurrence is minted (`lib.rs:1448-1452`). Re-keying would rewrite the predecessor's listens/history; speculative replacements and "checked" marks leak (S B1, O 1, T M4) | Blocking | Valid. Resolution must run only for the established current occurrence; `prepare_next` must not resolve |
| R2-2 | The 5 s budget is not a deadline: `Plex::container` has no deadline (`plex.rs:95,272`), one request can take 15 s, and shutdown joins the worker (`runtime.rs:489-499`) (S M1, O 4, T B2) | Blocking | Valid. Needs a deadline/stop-aware request (`tokio::time::timeout_at` + stop) and a `network_known && !online` guard |
| R2-3 | Transport epoch unsound: engine-class controls run in `Shared::transport` (`runtime.rs:252-303`) and bypass the queued path; `Resume` is PLAIN; requests admitted but still queued are invisible; check-to-load race (S M2, O 2, T m1) | Blocking | Valid. Must bump in `admit` before the engine early return, compare under `shared.admission`, and treat already-queued requests as superseding |
| R2-4 | Recovered/skip plans are never published: `start_plan` sets `presented.shown = current` (`runtime.rs:1219-1225`), so `poll` won't merge. The existing fallback explicitly does `merge` + `update(visible_plan)` (`runtime.rs:804-805`) (T B1) | Blocking | Valid. Mirror the fallback's publish |
| R2-5 | No-Media item Gone at natural end: `Event::End` swallows non-radio `Next` errors (`runtime.rs:762-766`), so playback stops silently, the original symptom; no bounded skip path (O 3, S M4, T M3) | Blocking | Valid |
| R2-6 | A boolean "checked" guard turns a revisited Gone into NotStale → transcode 400 → misleading error; network blips disable recovery (S M6, T M1, O 8) | Major | Valid. Cache typed verdicts keyed by `play_queue_item_id`/ratingKey; mark only on definitive outcomes |
| R2-7 | Prepared-successor suppression via `retry_prepare` does nothing: `prepare()` has ~5-8 call sites (S M5, T M2, O 8) | Major | Valid. The blacklist must live in Core and be consulted by `prepare_next` |
| R2-8 | `/hubs/search?limit=100` is relevance-ranked, a per-hub cap, mixed kinds; it can't prove uniqueness, and candidates must be read from `hubs[*].items` + `items` (O 5, T M6, S minor) | Major | Valid. Treat truncation as inconclusive; add `type=10`; consider a live section-scoped exact filter |
| R2-9 | Missing candidate discriminators (0 = unknown) bypass the index/duration check (O 6) | Major | Valid. A known old value requires an equal candidate value |
| R2-10 | Epoch-cancelled recovery loses the repair (not persisted) (O 7, S M2, T m3) | Major | Valid |
| R2-11 | Re-key omits `artist_key`/`album_key` (`features.rs:451-452`) (T M7); never re-key `delivered` entries (S M3) | Major | Valid |
| R2-12 | `snapshot.paused` is always false after `audio::error()` clears it (`audio/mod.rs:560-572`), so a paused session auto-plays after the fallback. Pre-existing for the cached fallback (S M2) | Major | Valid, pre-existing |
| R2-13 | Skip `Err` path unspecified; counter reset via `listened > 0` is wrong (includes resumed listened) (S M4, T M5, m4) | Major/Minor | Valid |
| R2-14 | Diagnostics need a `Source::recorded_error()` accessor and occurrence threading through `Reader::new`; 5xx logs once per retry (T n2, S minor) | Minor | Valid |
| R2-15 | Fixture server answers every request 200 `{"size":0}` (`fixture_tests.rs:52-82`); 404/search cases need per-path routing (T m7) | Minor | Valid |

Status: **blocked pending a scope decision** (see roadmap Open items). No code
has been changed.

## Scope decision (user, 2026-10-18)

Staged. Stage 1 (this plan, v3): diagnostics, a deadline-bounded staleness check,
a bounded skip on confirmed-gone tracks, prepared-successor failure handling, and
correct publishing. Stage 2 (separate plan and review): re-resolve a stale ratingKey
to its new key. Stage 1 alone does not make a fully stale queue playable: it skips
the gone tracks (bounded) and stops with a clear error instead of stalling.

## Plan v3 — Stage 1

Findings addressed: R2-2/3/4/5 (partly)/6/7/10 (n/a: no replacement)/12/13/14/15.
R2-1/8/9/11 move to stage 2 with resolution.

### A. Diagnostics (`src/audio/http.rs`, `src/audio/decode.rs`)
- `http::Reader::new` gains the occurrence id. `request()` logs
  `HTTP audio request failed occurrence=<id> status=<u16> retry=<n>` for 4xx
  (`Attempt::Failed`) and 5xx (`Interrupted`) responses. Status integer, counters
  and static strings only, never `self.uri`.
- Add `http::Source::recorded_error() -> Option<&'static str>` (reads `window.error`).
  On probe/packet failure, decode.rs logs
  `audio source failed occurrence=<id>: <static msg>` when one is recorded.

### B. Deadline-aware request (`src/plex.rs`)
- `Plex::container_until(base, token, path, params, deadline: Instant,
  stop: &AtomicBool) -> Result<Container>`. Same `request()` body; the future is
  wrapped in `tokio::time::timeout_at(deadline)` and raced against a 50 ms `stop`
  poll (inside `crate::rt::block_on`, view scope ignored). Expiry or stop →
  `Error::Network`. It covers headers and body.

### C. Core gone-check (`src/lib.rs`)
- `pub(crate) fn check_current_gone(&mut self, deadline: Instant, stop: &AtomicBool) -> bool`
  (not a Command; no `Error` variants for verdicts). Returns false without a
  request if any holds: `offline_mode`; `network_known && !network_online`; radio
  active; no current track; `cached_playback`; `base()` or `numeric(key)` fails.
- One `container_until("/library/metadata/{key}")`. Gone ⇔ `Error::Http(404)`, or
  Ok with no item whose `rating_key == key` and kind Track. Any other outcome → false.
- Gone items go into `gone: HashSet<ItemId>` (`ItemId` = `play_queue_item_id` if
  any, plus `rating_key`). Only definitive Gone is stored. The set is cleared on
  Connect/SelectServer/Logout. A cached Gone returns true without a request.
- `pub(crate) fn current_gone_cached(&self) -> bool`.
- `playback_plan` no-Media branch (`lib.rs:843-855`): a 404 or no-matching-Track
  response → insert into `gone` and return
  `Error::Input("This track is no longer on the server")` (the one new string).
  No resolution and no other side effects there.
- Prepared successor suppression: `failed_prepare: Option<ItemId>`, set by
  `pub(crate) fn note_prepare_failed(&mut self, id)` from the runtime. In
  `prepare_next` (`lib.rs:220`), return `Ok(None)` when the successor matches
  `failed_prepare` or is in `gone`. Cleared when that item becomes current
  (`commit_prepared`/`next`) and on `Queue::replace`/Connect/Logout.

### D. Intent tracking (`src/runtime.rs`, `Shared`)
- Two `AtomicU64`s: `intent_admitted` and `intent_consumed`.
- In `admit()`, before the engine early return, for `class.engine || class.transport`
  or `Command::Resume` (this includes the AudioPlay→Resume re-admit): take
  `shared.admission`, bump `intent_admitted`, and for `class.engine` also bump
  `intent_consumed` (applied immediately). Queued requests carry their admitted
  value; the worker sets `intent_consumed` to it when it dequeues the request.
- `superseded(before) = admitted != before || admitted != consumed`. That is: newer
  intent arrived, or user intent is still queued. Read `before` before the
  snapshot.

### E. Runtime error path (`src/runtime.rs` `Event::Error`)
- Prepared successor (`Some(id) == prepared id`, any message): log it, drop
  `prepared` and its `presented.plans` entry, `core.note_prepare_failed(item)`, no
  `_audio_error`. Events for an older, already-replaced prepared id: log only.
- Current (`id == current`), generic decode failure, source ≠ Cache:
  1. `CachedPlayback` (existing).
  2. `before = intent_admitted`. Then
     `core.check_current_gone(now + 4 s, &shared.stop)`. If Gone → skip (below).
     Else → existing `TranscodedPlayback` → existing `_audio_error`.
- Skip (worker local `skips`, cap `min(queue.len(), 50)`):
  - If `superseded(before)`: no skip, publish nothing new, emit `_audio_error` with
    the gone message, so the user's intent runs next.
  - If `queue.at_end()` with repeat ≠ All, or `skips >= cap`: emit `_audio_error`
    with the gone message and stop.
  - Otherwise `Command::Next { automatic: false }`. While the new current item is
    in `gone` (cached, no I/O) and the cap allows, `Next` again. On `Err` →
    `_audio_error` with that error and stop.
  - Then, under `shared.admission`, re-check `superseded(before)`. If superseded:
    don't load, publish the plan (`merge` + `update(visible_plan)`) so the UI
    matches Core. Otherwise `start_plan(&data, …, last_paused)`, `merge`,
    `update(visible_plan)`, `prepared = None`, `prepare()`. The Next has already
    persisted the queue (`Next` is in `persist`).
  - `skips += 1` per skipped item. Reset on `Event::Transition`/`Event::End` and
    on any consumed request with intent.
- `last_paused`: a worker local holding the paused flag passed to the last
  `start_plan`. It replaces `snapshot.paused` (cleared by `audio::error()`,
  `audio/mod.rs:560-572`) in both the existing cached/transcode fallback and the
  skip. This also fixes the pre-existing auto-play bug.
- `Event::End` (`runtime.rs:762-766`): a non-radio `Next` error is no longer
  swallowed: log it and emit `_audio_error`. (Skipping no-Media gone items at a
  natural end is stage 2. Stage 1 makes the stop visible rather than silent.)
- Shutdown: the only new I/O is one `container_until` bounded by min(4 s,
  shared.stop); skips over cached-Gone items do no I/O.

### F. Translation and docs
- `Error::Input("This track is no longer on the server")`, authored for all 39
  locales in `tools/translations.json`. `bash tools/build-qm.sh`, then `--check`.
- research.md (PMS facts), architecture.md (fallback order, skip policy, intent
  tracking), validation.md (actual results), roadmap Open items (stage 2,
  limitations).

### Tests
Fixture server: route by path; per-path status/body; a hang mode.
- `container_until`: deadline expiry and a stop flag end a hung request within
  budget + 100 ms; the synthetic token is absent from logs.
- `check_current_gone`:
  - 404 → true; 200 without matching Track → true; 200 match → false.
  - 401/500/timeout → false and not cached.
  - Offline/radio/cached-playback → no request.
  - Cached Gone → no request.
  - Cleared on SelectServer.
- `playback_plan` no-Media 404 → the gone error; the queue is rolled back by `next()`.
- `prepare_next` skips `failed_prepare` and gone items and does no network I/O.
- Runtime:
  - Stale current → skip to the next playable track, published ratingKey matches,
    no `_audio_error`.
  - Repeat-one stale → skips anyway; repeat-all fully-stale → stops at the cap with
    exactly one `_audio_error`; shuffle follows the order.
  - At queue end → gone error.
  - Pause (engine) during a delayed check → no skip or load; queued Next pending
    at check start → no skip.
  - A stale prepared successor → predecessor continues, no `_audio_error`, no
    re-prepare loop on queue edits; at the transition the current path above runs.
  - Non-gone failure → existing transcode fallback unchanged.
  - Paused session → the fallback load stays paused.
  - `Event::End` with a failing non-radio Next → `_audio_error`.
  - Shutdown during a hung check → worker joins within budget.
- Diagnostics: 404 logs status + occurrence; a probe failure logs the recorded
  source error.
- Gates: `./tools/check.sh`, `bash tools/build-qm.sh --check`,
  `./tools/build-sailfish.sh`. Phone: installed RPM under Sailjail. With the existing
  stale queue, expect bounded skips (or one clear stop) instead of a silent stall.

## Round 3 reviews (plan v3, stage 1)

Same three reviewers. Verdict: not ready (Sol 3 Blocking, Space Bunny 3, Sonnet 1).
Confirmed sound:
- `container_until` mechanics (`timeout_at` around send + `bounded_body`; the runtime
  is `enable_all`).
- No lock-order deadlock (engine `Handle` calls are `try_send`).
- Non-automatic `Next` side-effect freedom on a non-radio queue.
- Making non-radio `Event::End` errors visible.
- The translation literal.

| # | Finding (reviewers) | Sev | Adjudication |
| --- | --- | --- | --- |
| R3-1 | admitted/consumed counters desync: dequeue assignment after an engine bump, a bump before a failed `try_send`, non-intent requests → permanent "superseded" (S B1, O 1) | Blocking | Valid |
| R3-2 | Volume/Seek are `Class::ENGINE` and abort the skip; superseded after a committed `Next` leaves Core advanced, the engine empty and no banner (S M2, T B3, O 7) | Blocking | Valid |
| R3-3 | Skip loop runs off the queue end: `advance` → `current=None`, `playback_plan` returns `Ok` with an empty stream (`lib.rs:818-822`), and `start_plan` stops silently (T B1, O 6) | Blocking | Valid |
| R3-4 | Holding `shared.admission` across `start_plan` + `prepare()` freezes `poll`/`send` during no-Media metadata I/O (`runtime.rs:182,311`; `lib.rs:845`) (T B2, O 8) | Blocking | Valid |
| R3-5 | Only Gone skips were gated; cached/transcode fallback loads still override newer intent (O 2) | Blocking | Valid, but pre-existing behaviour; not widened by v4 |
| R3-6 | Speculative `prepare_next` inserting into `gone` poisons verdicts, including radio (O 4) | Blocking | Valid |
| R3-7 | `last_paused` is load-time state; user Pause/Play after load inverts it, a regression (S M1, O 3, T M1) | Major | Valid. Capture `paused` in `Event::Error` before `Actor::error` clears it |
| R3-8 | Suppression checked after `advance_next` still does plan I/O; a single slot reopens R2-7; clear list incomplete (O 5, T M4, S m2) | Major | Valid |
| R3-9 | The 4 s check before transcode stalls the worker on every non-stale failure (S M3) | Major | Valid. Check only after the existing fallbacks fail |
| R3-10 | The superseded branch leaves `prepared`/`presented.plans` stale; `start_plan` `Err` unspecified (T M3/M5, S m5) | Major | Valid |
| R3-11 | Unrecognised error ids: keep the current banner behaviour explicit (T M2, S m3) | Minor | Valid |
| R3-12 | `gone` lifetime (session-permanent), fixture catch-all 200 `{"size":0}` now reads as Gone, single-source the string const, `request` refactor instead of duplication (T m4/m6/m7/m8, S m6) | Minor | Valid |

## Plan v4 — Stage 1 (asynchronous gone check)

v3's synchronous check required an intent-supersession protocol (R3-1/2/4/5).
v4 removes the race instead of synchronising it: the worker never blocks on the
check. The result comes back through the worker's own FIFO message channel, so
any user request admitted earlier is handled first, in order. The fallback order
for non-stale failures is unchanged (R3-9).

### A. Diagnostics (unchanged from v3 A)
Occurrence id into `http::Reader::new`. Status-only
`HTTP audio request failed occurrence= status= retry=` log. A
`Source::recorded_error()` accessor; probe/packet failures log the recorded
static source error. Never `self.uri`.

### B. Async metadata request (`src/plex.rs`)
Refactor `request()` into an async `fetch()` future (credential header, `status()`
mapping and `bounded_body` stay single-sourced) plus the existing sync wrapper
via `run()`. New `pub(crate) fn container_future(&self, base, token, path)
-> impl Future<Output = Result<Container>> + Send + 'static` (owned inputs). It is
bounded by the client's 15 s total timeout.

### C. Error event carries pause intent (`src/audio/mod.rs`)
`Event::Error { id, message, paused }`, where `paused` is captured in `Actor::error`
(and the prepare/`pump` successor error sites) as `!self.desired || self.recording`
before they are cleared. The existing cached/transcode fallback uses it instead of
`snapshot.paused` (always false after `error()`, R2-12). The skip uses it too. This
captures the user's actual Play/Pause state at failure time (R3-7).

### D. Core (`src/lib.rs`)
- `pub(crate) fn gone_probe(&self) -> Option<(ItemId, impl Future)>`. `None` (no
  check) if any holds: `offline_mode`; `network_known && !network_online`; radio
  active; `cached_playback`; no current track; `base()`/`numeric` fails. Otherwise
  the current item's identity (`ItemId` = `play_queue_item_id` + `rating_key`)
  and a `container_future("/library/metadata/{key}")`.
- Verdict classification, as a pure function with unit tests: `Gone` ⇔
  `Error::Http(404)`, or Ok with an empty `Metadata` array (container `size == 0`).
  Everything else (match, other items, 401/403/5xx/network/timeout/parse) →
  `Alive/Unknown`.
- `gone: HashSet<ItemId>`. It is populated only by the worker from a verdict for
  the established current occurrence, never from planning or preparation (R3-6).
  Cleared on Connect/SelectServer/Logout and on any queue replacement
  (Play/PlayAlbum/Station/Radio/Mix/PlayDownload) (R3-12).
- `playback_plan` no-Media branch: map `Error::Http(404)` to the one new string
  `GONE_TRACK` (`pub(crate) const` with an `Error::Input(GONE_TRACK)` literal at the
  use site for extraction). No caching or other side effects.

### E. Runtime (`src/runtime.rs`)
- `WorkerMsg::Gone { occurrence: u64, item: ItemId, gone: bool }`.
- The existing `Event::Error` current-track path (cache → transcode) is unchanged
  until it would emit the final `_audio_error`. At that point:
  - If `core.gone_probe()` is `Some` and `item` is already in `gone`, skip
    immediately (below).
  - If it is `Some` and not cached: spawn the future on `crate::rt::spawn`. On
    completion it `try_send`s `WorkerMsg::Gone`; failure (queue full or closed) is
    dropped. Set the worker-local
    `pending = Some((occurrence, item, paused, Instant::now() + 16 s))` and emit
    nothing yet.
  - If it is `None`: emit `_audio_error` as today.
- Handling `WorkerMsg::Gone` (dequeued in order with requests):
  - It is stale unless `pending` matches `occurrence` and `current == occurrence`.
    A stale message is ignored. Any `start_plan` clears `pending`, so a user
    request that started something else in between wins naturally.
  - When it matches: take `pending`. If `gone`, insert into `core.gone` and skip.
    Otherwise emit `_audio_error` with the original message.
- Pending expiry: each worker loop iteration (25 ms tick) checks the deadline. On
  expiry, take `pending` and emit `_audio_error` with the original message. The
  worker owns the timeout, so a lost message cannot suppress the banner.
- Skip (one function; worker-local `skips`, cap `min(queue.len(), 50)`; reset on
  `Event::Transition`/`Event::End`):
  - Loop:
    1. If `skips >= cap`, stop.
    2. `skips += 1`.
    3. `Command::Next { automatic: false }`. `Err` → stop with that error.
    4. If the result has no track, an empty stream, or `current` is None, stop.
    5. If the new current item is not in `gone`, break.
  - Stop = `_audio_error` with `GONE_TRACK` (or the `Next` error), plus publish
    the Core state (`merge` + `update(visible_plan)`), `prepared = None`, and
    `engine.handle.prepare(None)`. Never a silent stop (R3-3).
  - Break → `start_plan(&data, …, paused)`. On `Err`, emit `_audio_error`, clear
    `prepared`, and don't prepare (R3-10). On success: `merge`,
    `update(visible_plan)`, `prepared = None`, `prepare()`, mirroring the existing
    fallback publish (R2-4).
  - No locks are held across any of this (R3-4). `Next` is already in `persist`.
- Prepared successor error (`id` == the live prepared serial): log it,
  `engine.handle.prepare(None)`, drop `prepared` and its `presented.plans` entry, and
  set the worker-local `prepare_suppressed = Some(current)`. `prepare()` (the single
  function used by every call site) returns early while
  `prepare_suppressed == Some(current)`. This is cleared automatically when
  `current` changes. No banner. Gapless for that one transition is lost; the
  successor still plays via `Event::End` → `Next` (R3-8). Other unrecognised ids
  keep today's banner (R3-11).
- `Event::End` non-radio `Next` error: log it and emit `_audio_error` (instead of
  swallowing it). This is an intended behaviour change: offline "not downloaded" at a
  natural end becomes visible too.
- Shutdown: the worker never waits on the check. The spawned task is dropped with
  the runtime, or its `try_send` fails harmlessly.

### F. Translation and docs (as v3 F)

### Known limitations (documented, stage 2 or later)
- A no-Media gone item is reported, not skipped (`next()` rolls back on `Err`).
- `playback_plan`'s no-Media fetch remains a pre-existing blocking 15 s request.
- Gone verdicts last until a queue replacement or server change.
- Fallback loads for non-stale failures still don't consult newer intent
  (pre-existing, R3-5).

### Tests
- Unit:
  - Verdict classification table.
  - `gone_probe` guards (offline/unknown network/radio/cached/no track → None).
  - `gone` cleared on queue replacement and SelectServer.
  - The `fetch` refactor keeps existing plex tests green.
- Fixture server: per-path routing, status and body, a hang mode. Existing
  catch-alls stay 200 `{"size":0}` only where no gone check can run, or get an
  explicit metadata route (R3-12).
- Runtime:
  - Stale current → after the async verdict, skip to the next playable track; the
    published ratingKey matches; no `_audio_error`.
  - `[A gone, B gone]` with repeat off → exactly one `_audio_error`, published
    `track: null`, engine stopped.
  - Repeat-one stale → skips; repeat-all fully stale → stops at the cap with one
    `_audio_error`.
  - Shuffle → follows the order.
  - User `Next`/`SelectTrack` admitted before the verdict → its plan plays and the
    verdict is ignored.
  - Volume/Seek during the check → no effect on the skip.
  - Paused at failure → the skip loads paused. Playing → loads playing.
  - Hung check → `_audio_error` at 16 s; worker requests keep being served
    meanwhile.
  - Shutdown with a hung check → the worker joins promptly.
  - Stale prepared successor → the predecessor continues, no banner, no re-prepare
    on queue edits, the successor attempted at End.
  - Non-gone failure → existing fallback and timing unchanged.
  - `Event::End` with a failing non-radio Next → `_audio_error`.
- Diagnostics: a 404 logs status + occurrence; the synthetic token is absent from
  logs.
- Gates: `./tools/check.sh`, `bash tools/build-qm.sh --check`,
  `./tools/build-sailfish.sh`. Phone: installed RPM under Sailjail.

## Round 4 reviews (plan v4, stage 1)

Same three reviewers. Sonnet: no Blocking. Sol: 3 Blocking. Space Bunny: 5 Blocking.
All reviewers confirmed sound:
- FIFO ordering of the verdict behind earlier queued requests (`admit` uses
  `try_send` under `admission`).
- Skip-loop termination in every repeat/shuffle/end state, including
  `current=None` → empty stream.
- Radio exclusion.
- `paused` captured before mutation at the three `Event::Error` sites.
- `prepare()` as the single preparation choke point.
- No poisoning from `prepare_next`.
- Security posture (token in a header only).

| # | Finding (reviewers) | Sev | Adjudication / v5 amendment |
| --- | --- | --- | --- |
| R4-1 | `rt::spawn` returns a `Task` whose `Drop` aborts (`rt.rs:101-110,143-147`). Discarding it cancels every probe (S m3, O 2, T B3) | Blocking | Valid. The `Task` is owned by `pending` |
| R4-2 | 200 with an empty or missing `Metadata`/`size` deserializes leniently to "empty" (`model.rs:229-235,446-458`), so a live or malformed reply could become Gone and be cached (S M2, O 3, T B5) | Blocking | Valid. Gone ⇔ HTTP 404 only. The probe needs only the status, so no body is decoded (also fixes S m8) |
| R4-3 | `Error::Input(GONE_TRACK)` with a const is not extracted (`translations.py:35`) (O 7, T B4) | Blocking | Valid. Inline literal at each use site |
| R4-4 | Probe placement read as "at the unconditional `_audio_error`" (`runtime.rs:817`) widens the trigger to output/control errors and stale ids (S m1, T B1) | Blocking | Valid. Probe only inside the existing guard (`id == current`, decode-failure message, source ≠ Cache), after both fallbacks fail |
| R4-5 | Engine-class Stop/Pause/Play bypass the FIFO (`runtime.rs:252-303`), so a verdict can restart playback after Stop (S m7, O 1, T B2/M4) | Blocking | Valid. Add a transport epoch (below) |
| R4-6 | The verdict isn't tied to Core's current item; a queue edit or failed request can move Core without a new occurrence (S M1, O 4) | Major | Valid. Also require `core.current_item_id() == item` |
| R4-7 | `pending` lacks the original message, task, generation; not cleared on Logout/SelectServer/Connect; a second arm unspecified (O 4, T M1) | Major | Valid |
| R4-8 | The recording hold is mistaken for Pause (`!desired \|\| recording`) (O 5, S note) | Major | Valid. `paused = !desired` |
| R4-9 | `skips` leaks across user-started playback (O 6, T M5, S m6) | Major | Valid. Reset on a successful request-path `start_plan` |
| R4-10 | A 15 s client timeout vs a 16 s deadline is too tight; expiry and verdict could both fire (S m2, T M2) | Major | Valid. The probe has its own 5 s timeout; deadline 8 s; `pending` is taken exactly once |
| R4-11 | `Event::End` change widens banners to offline "not downloaded" (T M7) | Major | Valid. Surface only the gone error at End |
| R4-12 | No-Media successor in the skip loop: a blocking fetch, then rollback/stop (T M3) | Major | Known limitation (stage 2); the loop stops with the gone banner. Documented |
| R4-13 | Occurrence-wide prepare suppression loses gapless for a later good successor after a queue edit (T M6) | Major→Minor | Accepted cost: one non-gapless transition. Documented. Clear-then-return ordering kept (S m9) |
| R4-14 | Skip-loop break ignores a cache-served gone item; queue-end stop leaves Core advanced and persisted (S m4, m5) | Minor | Valid. Break if `cached_playback`; check `at_end && repeat != All` before each `Next` |
| R4-15 | Undeliverable verdict log, `describe()` arm, stop-reason log, fixture catch-alls, roadmap note (T M8/m1/m3/m4/m5, S m10) | Minor | Valid |

## Plan v5 — Stage 1 (v4 with amendments)

Everything in v4 stands except as amended here.

- **C (pause intent):** `Event::Error { id, message, paused }` with
  `paused = !self.desired`, captured before mutation at `audio/mod.rs:437`, `:560`
  and `:619`. The recording hold stays a separate actor gate.
- **B (probe request):** `Plex::probe_status(&self, url, token) -> impl Future<Output =
  Result<u16>> + Send + 'static`. It clones the client and headers and attaches the
  token header; it is built from the same `builder` (single-sourced) and does not
  touch `self.scope`/`run()`. A GET is wrapped in `tokio::time::timeout(5 s)`; the
  body is discarded unread. Classification is a pure function: `404` → Gone.
  Anything else (2xx, 401/403, 5xx, network error, timeout) → Unknown.
- **D (gone set):** populated only from a Gone verdict for the established current
  item. Cleared on Connect/SelectServer/Logout and on queue replacement. No 200-empty
  rule.
- **Translation:** every use site writes `Error::Input("This track is no longer on the
  server")` inline. Verify the source appears in the Core context of the generated
  `.ts` and in all 39 catalogues.
- **E (arming):** only inside the existing guard (`id == current`, message
  `"Audio decoding or streaming failed"`, source ≠ Cache), when both the cached and
  transcode fallbacks have failed. Other errors keep today's immediate banner.
  - Already Gone-cached → skip now.
  - Otherwise replace any existing `pending` (dropping its `Task` aborts it) with
    `Pending { task, generation, occurrence, item, message, paused, epoch, deadline:
    now + 8 s }`. `generation` is a worker counter.
- **Transport epoch:** `Shared::transport_epoch: AtomicU64`, bumped in
  `Shared::transport` for AudioStop, AudioPause, AudioPlay and AudioToggle
  (not Volume/Seek). It is snapshotted at arming.
- **Verdict handling:** `WorkerMsg::Gone { generation, gone }`. Take `pending` once.
  It is valid only if the generation matches, `current == occurrence`,
  `core.current_item_id() == item`, and the transport epoch is unchanged.
  - A Gone verdict always inserts `item` into `gone` (harmless).
  - Valid + Gone → skip with `paused`.
  - Epoch changed (the user pressed Stop/Pause/Play) → no skip or load. Emit the
    banner (`"This track is no longer on the server"` if Gone, else the original
    message), so the user's newer intent stands and the failure is still visible.
  - Generation/occurrence/item mismatch → ignored silently (the user moved on).
  - Valid + Unknown → banner with the original message.
- **Expiry:** at the top of each worker loop iteration (before any `continue`),
  `pending` past its deadline is taken, dropped, and its original message emitted.
  `pending` is also cleared, with no banner, by a successful `start_plan` and by
  Connect/SelectServer/Logout. An undeliverable verdict (`try_send` error) is
  logged.
- **Skip loop:**
  1. Stop if `skips >= cap`.
  2. Stop (queue untouched) if `queue.at_end() && repeat != All`.
  3. `skips += 1`, then `Next { automatic: false }`. An `Err` (including the no-Media
     gone error) stops.
  4. Stop if the result is empty or there is no current track.
  5. Break if `cached_playback` or the new current item isn't in `gone`.

  `skips` resets on Transition/End and on a successful request-path `start_plan`.
  Each stop logs its reason (`cap|end|next_err|empty`) with skips/cap.
- **`Event::End`:** only an error whose message is the gone string is surfaced as
  `_audio_error`. Other non-radio `Next` errors stay swallowed as today, so offline
  is unchanged.
- **Prepare suppression:** `prepare()` first performs its existing clear (engine
  `prepare(None)`, dropping `prepared` and its plan), then returns early while
  `prepare_suppressed == Some(current)`. Accepted cost: gapless is lost for the rest
  of that track even if a queue edit replaces the bad successor.
- **Misc:** a `describe()` arm for `WorkerMsg::Gone`. Fixtures that drive a
  current-track decode failure need explicit metadata routes; with 404-only, the
  existing 200 catch-all reads as Unknown, so `long_gapless_*` is unaffected. Remove
  the `snapshot.paused` follow-up from the roadmap once fixed.
- **Added tests:**
  - A 200 with an empty or missing `Metadata` → Unknown.
  - A probe never fires for output/control errors or stale ids.
  - The `Task` is retained (a delayed 404 still yields a skip).
  - Stop, Pause and Play during `pending` → no skip, banner shown, engine state
    respected.
  - Verdict item ≠ Core's current item → ignored.
  - Logout during `pending` → no banner afterwards.
  - Recording hold during failure → the skip resumes after release.
  - Queue-end stop leaves the failed item current.
  - Re-arming replaces and aborts the old probe.
  - Expiry vs a late verdict → exactly one outcome.
  - The gone string is present in the generated catalogues.

## Round 5 reviews (plan v5, stage 1)

Same three reviewers. Sonnet: no Blocking. Sol: 2 Blocking. Space Bunny: 1 Blocking.
Round-4 amendments R4-1..R4-4 and R4-6..R4-15 were confirmed closed, as were
`probe_status` ownership (`Send + 'static` via an owned `RequestBuilder`) and the
shutdown path.

| # | Finding (reviewers) | Sev | Adjudication / v6 amendment |
| --- | --- | --- | --- |
| R5-1 | "Take `pending` once" happens before the generation check: a queued stale verdict (probe aborted after `try_send`) destroys the live re-armed `pending`, leaving no skip, banner or expiry (S M2, O 2, T 1) | Blocking | Valid. Compare the generation against `pending.as_ref()` first; a mismatch is a pure no-op |
| R5-2 | Epoch snapshotted at arming is too late: a Stop between the actor error and arming is already included. A Stop after validation but before `start_plan` is overridden by the load (S M1, O 1) | Blocking | Valid. Stamp at failure time; serialize the final check + load with transport (below) |
| R5-3 | A queued user `Next`/`Previous` admitted while the verdict is queued behind events is applied after the skip: a double advance (T 3) | Major | Valid. Queued `class.transport` admissions bump the same epoch |
| R5-4 | Same-occurrence item mismatch (QueueRemove of the current item) loses the banner; expiry has no validity check; `recover()` doesn't clear `pending` (S m1/m2, T note) | Minor | Valid |
| R5-5 | Stop payload per reason undefined; the inherited v4 queue-end test contradicts v5 (O 3, T 2) | Major | Valid. Enumerated below; the v4 assertion is replaced |
| R5-6 | "Successful `start_plan` clears `pending`" also fires on an empty-stream stop (T 5) | Minor | Valid. Clear only when `current` changes |
| R5-7 | `Shared::transport` bump placement must precede the AudioPlay→Resume early return (T 4, S m3) | Minor | Valid |
| R5-8 | Gone-string comparison literals can drift; End arm ordering vs radio (T 7, S m6) | Minor | Valid |
| R5-9 | Required Core accessors not listed (T 8) | Minor | Valid |
| R5-10 | Banner latency for non-gone decode failures: up to 5 s probe / 8 s deadline (S m5); `paused` site wording (S m4) | Minor | Valid. Documented as intended; wording fixed |

## Plan v6 — Stage 1 (v5 with amendments)

Everything in v4 and v5 stands except as amended here.

- **Intent epoch (replaces the v5 transport epoch):**
  - `Shared::intent: Arc<AtomicU64>` and `Shared::transport_lock: Mutex<()>`.
  - **Bumps:**
    1. At the top of `Shared::transport`, under `transport_lock`, for AudioStop,
       AudioPause, AudioPlay and AudioToggle (not Volume/Seek). This happens
       before any branch, so it includes the AudioPlay→Resume re-admit. The lock
       is held across the bump and the immediate `handle.*()` call, which is a
       non-blocking `try_send` (`audio/mod.rs:156-176`). The lock is released
       before any re-admit.
    2. In `admit` for `class.transport` requests, after a successful `try_send`.
  - Worker-internal `handle.stop()`/`load()` calls never bump: the counter lives in
    `Shared`, not in `Handle`.
  - The audio actor receives a read-only clone of the `Arc` at engine creation.
    `Actor::error` records `intent.load()` into `Event::Error { id, message,
    paused, intent }`, captured at failure time. `paused` is captured only there:
    the `:437`/`:619` successor errors don't feed the current path.
  - Arming stores `pending.intent = event.intent`.
- **Verdict handling (replaces the v5 order):**
  1. If `pending.as_ref()` is None or its generation ≠ the message's generation,
     log and return. Don't touch `pending`. A stale message never inserts into `gone`.
  2. Take `pending`.
  3. If `current != occurrence`: silent (the user moved on). Insert into `gone`
     only if the server and queue are unchanged, i.e. no clear happened since
     arming (tracked by a `gone_epoch` in Core bumped by every clear).
  4. If `core.current_item_id() != item`: banner with the original message.
  5. If the verdict is Gone and the server/queue are unchanged, insert `item` into `gone`.
  6. If `shared.intent != pending.intent`: banner (the gone string if Gone, else
     the original message). No skip or load.
  7. Gone → skip. Unknown → banner with the original message.
- **Skip loop final load (closes R5-2's second window):**
  - Build each `Next` plan outside any lock.
  - Then, holding `shared.transport_lock`, re-check
    `shared.intent == pending.intent`, and call `start_plan` (its engine calls are
    `try_send`) only if it matches. Release the lock.
  - On a mismatch, don't load. Banner with the gone string and publish the Core
    state, so the newer user intent proceeds from a visible state.
  - Queued requests admitted after this check run after the skip, in FIFO order.
- **Stop payloads:**
  - End pre-check (step 2): no `Next` runs. Core is unchanged with the failed item
    current; the engine is already stopped. Publish nothing new, then
    `prepared = None`, `engine.handle.prepare(None)`, and the gone banner.
  - Cap or `next_err`: publish the Core state from the last successful `Next` plan
    (or none), plus the gone banner (or the `Next` error).
  - Empty (step 4): publish the empty plan (`track: null`) and the gone banner.
  - The v4 test "`[A gone, B gone]` → published `track: null`" is replaced with:
    "B stays current, exactly one gone banner, no further `Next`, engine stopped".
- **Pending lifecycle:** cleared silently when `current` changes (a new occurrence
  installed), in `recover()`, and on Connect/SelectServer/Logout. An empty-stream
  `start_plan` does not clear it. At expiry, emit the original message only if
  `current == occurrence`; otherwise drop silently.
- **Gone string:** `pub(crate) const GONE_TRACK: &str = "This track is no longer on
  the server";` for comparisons. Every `Error::Input("This track is no longer on
  the server")` stays an inline literal, and a unit test asserts the literal equals
  `GONE_TRACK`. The `Event::End` gone arm goes after the existing
  `Err(error) if radio` arm.
- **Core accessors (`pub(crate)`):**
  - `current_item_id()`, `queue_at_end()`, `queue_repeat()`, `queue_len()`;
  - `cached_playback()`, `gone_contains(&ItemId)`, `note_gone(ItemId, gone_epoch)`;
  - `gone_epoch()`, `gone_probe()`.
- **Latency (documented):** a non-gone decode failure on an uncached stream now
  shows its banner after the probe (usually < 1 s, at most 5 s probe / 8 s deadline).
  The v4 test line "non-gone timing unchanged" becomes "the fallback order is
  unchanged; the banner arrives after the probe".
- **Added tests:**
  - Stop between the actor error and arming → no skip.
  - Stop after verdict validation but before the load (via a hook) → no load.
  - A user `Next` queued behind the verdict → a single advance.
  - A stale-generation verdict after re-arm → the live `pending` still resolves.
  - A late verdict after SelectServer → `gone` not populated.
  - QueueRemove of the current item during `pending` → banner.
  - Expiry after `current` changed → no banner.
  - AudioPlay→Resume with the verdict first → banner, then Resume → cached-Gone
    immediate skip.
  - The literal equals `GONE_TRACK`.

## Round 6 reviews (plan v6, stage 1)

Same three reviewers. Sonnet: no Blocking. Space Bunny: no Blocking. Sol: 1
Blocking. R5-1, R5-4 and R5-6..R5-10 were confirmed closed. The lock graph was
checked: no deadlock or unbounded GUI stall, provided `prepare()` and `store::save`
stay outside `transport_lock`.

| # | Finding (reviewers) | Sev | Adjudication / v7 amendment |
| --- | --- | --- | --- |
| R6-1 | The latest shared atomic, sampled in `Actor::error`, can include a Stop/Pause that is admitted but still unprocessed in the actor's control queue: new intent, old `desired`. No bump/send ordering fixes this (O 1; S M1 and T F2 proposed bump-after-send, which narrows the window but does not close it) | Blocking | Valid. Use an actor-applied stamp (below) |
| R6-2 | The skip's internal `Next` commits and persists B before the final check; a superseded check leaves Core at B, and the queued user Next then goes to C. Queued admissions aren't serialized with the final check (O 2, S M2, T F4) | Major | Valid. Plan the skip tentatively and commit it only under the final check |
| R6-3 | End pre-check after loop advancement loses the last plan (O 3); `prepared` cleanup missing for some stop reasons (T F9) | Major | Moot under v7: a stop commits nothing |
| R6-4 | The cached-Gone immediate skip has no intent check (T F1, S m1) | Major | Valid. Both entry points share one function with an explicit stamp |
| R6-5 | Resume dropped from bumps (T F3); fallback must use `event.paused` (T F5); `gone_epoch` field (T F6, S m5); `pending` clearing mechanism (T F7, S m7); test hook (T F8, S m8); monotonic generation (T F10); `diagnostics::event` (T F12); `Engine` constructor and naming (S m4); guard drop before re-admit (S m2); expiry vs queued verdict (S m6); a long silent repeat-all chain (T F13); breadth of class.transport (S m3) | Minor/Note | Valid. Folded in below |

## Plan v7 — Stage 1 (v6 with amendments)

Everything in v4–v6 stands except as amended here.

- **Applied-intent stamps (replace the v6 epoch semantics):**
  - Counter and bumps:
    - `Shared::intent: AtomicU64` with `Shared::transport_lock: Mutex<()>`.
    - Bumps (`n = intent.fetch_add(1) + 1`) happen only under `transport_lock`:
      - in `Shared::transport` for AudioStop/Pause/Play/Toggle (not Volume/Seek),
        passing `n` with the control (`Control::Stop(n)` etc.);
      - in `admit` for `class.transport` requests and `Command::Resume`, before
        `try_send`, with the stamp carried in the `Request`.
    - Lock order is `admission → transport_lock`. `transport()` drops its guard in
      an explicit scope before the AudioPlay→Resume re-admit; a test checks that
      Play→Resume doesn't hang.
    - A failed `try_send` leaves an unapplied stamp. That is the safe direction:
      the next skip becomes a banner, until a later stamp is applied.
  - The actor keeps `applied_stamp = max(applied_stamp, stamp)`. It is updated when
    it processes a stamped control, a `Load(.., stamp)`, or a `Control::Ack(stamp)`.
  - The worker sends `handle.ack(request.stamp)` after it finishes each stamped
    request, whether or not that request loaded anything. Non-loading transport
    requests (queue edits, ClearCache) therefore don't leave the stamp lagging
    behind `intent`.
  - Load stamps:
    - Request-path loads carry the request's own stamp.
    - Other loads (End/Transition/fallback/skip) carry the worker-local
      `last_stamp`. A lower stamp is harmless under `max`.
  - `Event::Error { id, message, paused, applied }` reports `applied_stamp`, which
    is exactly the user intent the actor has processed.
  - Meaning: `shared.intent == applied` ⇔ no user intent was admitted that the actor
    has not applied (an unprocessed Stop, a queued Next, …). Only then may a skip load.
  - The field is named `stamp`/`applied_stamp`, because `Actor::intent` already
    means an occurrence id. `Engine::with_stamps(sink, …)` is a new constructor;
    `Engine::new` delegates to it, so the existing callers in `daily.rs`,
    `platform.rs` and the tests are untouched.
- **Tentative skip (replaces the internal `Next` loop):**
  - Generalize `prepare_next` into `plan_advance(&mut self, automatic: bool,
    skip_gone: bool, cap: usize) -> Result<Option<Prepared>>`. It uses the same
    clone/advance/restore pattern, so Core's queue, radio, cache flags and disk are
    unchanged.
  - `prepare_next()` = `plan_advance(true, false, 1)`, with identical behaviour (it
    keeps its `schedule_cache()` call; `plan_advance` itself doesn't schedule).
  - With `skip_gone`, it loops on the tentative queue:
    1. Stop with None if `at_end && repeat != All`.
    2. Advance non-automatically.
    3. Stop with None on `Err`, an empty stream or no track.
    4. Return the candidate if it isn't Gone-cached or is cache-served.
    5. Stop with None after `cap` iterations.
  - Split `commit_prepared` into `apply_prepared` (in-memory: queue/radio/cached,
    occurrence, generation; returns the plan with its stream kept when requested)
    and `persist_playback` (`store::save`). `commit_prepared` = apply + persist, so
    gapless behaviour is unchanged.
- **One skip function** is used by both the verdict path and the cached-Gone
  arming path, taking `(occurrence, item, paused, applied)`:
  1. Run `plan_advance(false, true, cap)` outside any lock. It may block on the
     known no-Media fetch, a documented limitation.
  2. If it returns None: Core is unchanged, with the failed item current. Then
     `prepared = None`, `engine.handle.prepare(None)`, and one gone banner. This
     replaces the v6 stop payloads (R6-3 moot).
  3. Under `transport_lock`, check `shared.intent == applied`, `current ==
     occurrence` and `core.current_item_id() == item`. If they hold:
     `apply_prepared`, then `start_plan(plan, paused, last_stamp)`, which is only
     `try_send`. Release the lock.
     - If they don't: drop the `Prepared` (Core unchanged), release the lock, and
       show a banner with the gone string. Any queued user intent then runs from the
       unchanged state, so there is no double advance (R6-2).
  4. After release: `persist_playback`, `merge` + `update(visible_plan)`,
     `prepared = None`, `prepare()`, `skips += 1`. `store::save` and `prepare()`
     never run under the lock.
- **Fallback loads** (cached/transcode) use `event.paused`, not `snapshot.paused`
  (the R2-12 fix, restated).
- **Pending:**
  - Fields: task, generation (a monotonic worker counter, never reset), occurrence,
    item, message, paused, applied, gone_epoch (read via `core.gone_epoch()` at
    arming), deadline.
  - Clearing is a single check at the top of each worker loop iteration:
    `pending.occurrence != current` → drop silently. This covers every site that
    changes `current`, including `recover()` and session resets. The expiry check
    sits beside it.
  - Known degradation (documented): after a long blocking request, expiry can fire
    before an already-queued verdict is read. The banner is then generic and `gone`
    is not populated.
- **Tests (added or replacing):**
  - Stop admitted after the actor drained its controls but before the decode error
    → `applied < intent` → no skip.
  - A user Next queued while the verdict is handled → Core unchanged by the skip, a
    single advance.
  - A non-loading transport request (QueueRemove) followed later by a stale failure
    → skip still allowed (ack applied).
  - Play→Resume doesn't hang.
  - The cached-Gone path is superseded by Stop → no load.
  - The fallback stays paused when paused at failure.
  - A dedicated `#[cfg(test)]` worker hook between `plan_advance` and the lock, for
    the "Stop after planning" case.
  - `[A gone, B gone]` with repeat off → Core and the published queue stay on A,
    one gone banner, engine stopped.
  - `prepare_next` behaviour unchanged (existing gapless tests).
- **Misc:**
  - New diagnostics use `crate::diagnostics::event`.
  - Document that a fully stale repeat-all queue can probe each item before the
    final banner (bounded by the cap), and that class.transport breadth only ever
    downgrades a skip to a banner.

## Round 7 reviews (plan v7, stage 1)

Same three reviewers. Sonnet: no Blocking. Space Bunny: no Blocking. Sol: 1
Blocking. Confirmed sound:
- tentative planning plus apply-under-lock closes R6-2;
- `plan_advance(true,false,1)` is behaviour-identical to `prepare_next`;
- the lock graph `admission → transport_lock → presented → model` is acyclic;
- R6-3 is moot.

| # | Finding (reviewers) | Sev | Adjudication |
| --- | --- | --- | --- |
| R7-1 | One counter under `max`: a newer, directly applied Pause (stamp 2) masks an older queued Next (stamp 1). The error reports applied = intent, so the cached-Gone skip double-advances (O 1 Blocking, S M1, T related F2) | Blocking | Valid. Needs separate request-completion accounting (worker-side admitted/done) next to actor-applied controls, or routing the cached-Gone path through the FIFO |
| R7-2 | Play/Pause applied between failure and verdict: the skip loads with the stale failure-time `paused` (T F3) | Major | Valid |
| R7-3 | `start_plan` Err after `apply_prepared` unspecified; `last_stamp` undefined; `persist_playback` failure handling; publish shape (T F1/F2, S m3) | Major | Valid |
| R7-4 | Tentative loop can run cap × 15 s no-Media fetches with no stop check (T F4, S m4) | Major | Valid. Stop on a no-Media candidate in skip mode |
| R7-5 | `recover()` doesn't change `current`, so pending survives a rebuild (O 2, T F7) | Major | Valid. Explicit clear |
| R7-6 | Skip accounting counts events, not items (O 3, T F5) | Major | Valid |
| R7-7 | Ack/try_send robustness; QueueRemove test contradiction; queue clone inside the lock; guard helper; successor Event fields; dead expiry check (S m1/m2, T F6/F8–F11) | Minor | Valid |

Status: blocked pending a user decision. Seven rounds have each closed the previous
findings, but the intent-supersession protocol keeps producing new edge cases.

## User decision after round 7 (2026-10-18)

Simplify, and accept the remaining transport races. Use queue compaction at
dequeue to avoid the double-Next issue. The v6/v7 intent-stamp protocol is dropped.

### Explicitly accepted risks (not waived silently)

- **AR-1:** an engine-class Stop/Pause pressed within a few milliseconds of a stale
  failure can be overridden by the automatic skip (the next track loads and plays).
  The best-effort counter below narrows the window from the probe duration (up to
  5 s) to milliseconds.
- **AR-2:** a Play/Pause applied between the failure and the skip is ignored: the
  skip uses the failure-time paused state.
- **AR-3:** a user transport request admitted after compaction has run (the worker
  has already decided to skip) is applied after the skip. That can be one extra
  advance.

## Plan v8 — Stage 1 (simplified)

Kept from v4–v7 unless changed here:
- A. Diagnostics: status-only, with the occurrence id and `diagnostics::event`.
- `Plex::probe_status`: 5 s, 404 → Gone, status only, `Task` owned by `pending`.
- `Event::Error.paused = !desired`, captured at all three sites. The fallback uses it
  (the R2-12 fix).
- The probe is armed only inside the existing decode-failure guard, after both
  fallbacks fail.
- `gone` set + `gone_epoch`; cleared on Connect/SelectServer/Logout/queue replacement.
- Prepared-successor failure: retire silently, with occurrence-wide `prepare()`
  suppression (clear first, then return).
- `Event::End` surfaces only the gone error, after the radio arm.
- Inline `Error::Input("This track is no longer on the server")` literals plus a
  `GONE_TRACK` const pinned by a test; all 39 locales.
- Tentative `plan_advance` / `apply_prepared` / `persist_playback` (R6-2): a stop
  commits nothing.

Dropped: the v6/v7 `intent` stamps, `Control` stamps, `Ack`, `applied_stamp`,
`transport_lock` and `Engine::with_stamps`.

### 1. One FIFO entry point
Both the probe verdict and the cached-Gone case arrive as
`WorkerMsg::Gone { generation, gone }`. When the item is already Gone-cached, arming
creates `pending` without a task and immediately `try_send`s
`Gone { generation, gone: true }` to its own channel. If that send fails, `pending`
expires into a banner. Either way it is ordered behind every request admitted
earlier (closes R7-1: a queued user Next is processed first, changes `current`, and
the verdict is then ignored).

### 2. Compaction at dequeue (user decision; reduces R7-1/AR-3)
When the worker dequeues `WorkerMsg::Gone` whose generation matches `pending`:
- First drain every message already in the channel (`try_recv` until empty) into a
  worker-local `backlog: VecDeque<WorkerMsg>`. The loop takes its next input from
  `backlog` before `recv_timeout`.
- If `backlog` contains a `Request` whose class is `transport`, or a `Resume`: take
  `pending`, insert into `gone` if Gone (gone_epoch unchanged), and drop the skip
  silently. The user's request then runs from the unchanged Core. Checkpoints and
  other messages keep their order.
- Otherwise continue with the validity checks.
Shutdown found in the drain is honoured immediately.

### 3. Best-effort Stop/Pause guard (narrows AR-1)
`Shared::user_stops: AtomicU64`, bumped in `Shared::transport` for AudioStop and
AudioPause. The worker reads it when it handles the `Event::Error` that arms
`pending` and stores it as `pending.stops`. If it differs at the verdict, the skip is
dropped and the banner is shown instead. No locks are added.

### 4. Verdict validity and skip
1. A generation mismatch, or no `pending` → no-op.
2. Take `pending`.
3. If `current != occurrence` → silent. If `core.current_item_id() != item` →
   banner (the original message).
4. If Gone and the gone_epoch is unchanged → insert into `gone`.
5. If the compaction rule (2) or the stops counter (3) says superseded → silent or
   banner as above.
6. Gone → skip. Unknown → banner (the original message).

Skip:
- `skips` counts skipped items (worker local). It resets on Transition/End and on
  a successful request-path `start_plan`.
- `cap = min(queue_len, 50)`. If `skips >= cap`, stop.
- `plan_advance(false, true, cap - skips)`:
  - tentative, as in v7, with the at-end and repeat rules;
  - stops with None on a candidate with no Media/Part (no fetch: R7-4);
  - returns `(Prepared, advanced)`.
- None → Core unchanged: `prepared = None`, `engine.handle.prepare(None)`, gone banner.
- Some:
  1. `apply_prepared` (in-memory only; the plan keeps its stream and gets the
     presentation queue plus occurrence/generation/`resumeListened = 0`).
  2. `start_plan(plan, paused)`.
     - On `Err`: Core stays at B. `persist_playback`, publish, then `_audio_error`
       with the error. No rollback; the user sees B with an error, so a retry is
       possible.
     - On `Ok`: `persist_playback` (failure logged, not fatal, as in `execute()`),
       `merge` + `update(visible_plan)`, `prepared = None`, `prepare()`,
       `skips += advanced`.

### 5. Pending lifecycle
- Top of the worker loop: `pending.occurrence != current` → drop silently.
- `recover()` (the panic branch, next to `*prepared = None`): drop `pending`
  explicitly (R7-5).
- Expiry: emit the original message.

### 6. Tests (replacing the v6/v7 stamp tests)
- Cached-Gone: queued user Next before the self-verdict → single advance.
- Compaction: a user Next queued behind the verdict → skip dropped, single advance.
- Compaction ignores non-transport requests (Browse) and checkpoints, and keeps
  their order.
- Stop/Pause during the probe → no skip, banner (best-effort counter).
- Skip accounting: a batch over several cached-Gone items charges `advanced`, and
  the cumulative bound holds over a fully stale repeat-all queue.
- A no-Media candidate in skip planning → stop, no fetch.
- `start_plan` Err after apply → B published, banner.
- A panic/recover with `pending` → no later banner.
- QueueRemove tests split: non-final active removal → successor plays, no banner;
  removal emptying the queue → banner.
- All earlier non-stamp tests (A, B, verdict classification, prepared successor,
  End surfacing, paused capture, recording release, expiry, shutdown, translation,
  `prepare_next` unchanged).

## Round 8 reviews (plan v8, stage 1)

Same three reviewers. Sonnet: no Blocking. Sol: 1 Blocking. Space Bunny: 1
Blocking, the same finding. AR-1..AR-3 were not re-raised. Confirmed sound:
- the self-sent verdict cannot deadlock (single consumer, capacity 256, Full →
  expiry);
- FIFO + generation closes R7-1;
- taking input from `backlog` neither spins nor starves;
- busy/foreground counters are unaffected (`Gone` is not a `Request`; `abandon`
  skips it);
- skip accounting terminates;
- `probe_status` ownership.

| # | Finding (reviewers) | Sev | Adjudication / v8.1 amendment |
| --- | --- | --- | --- |
| R8-1 | Compaction by `class.transport` drops the skip silently. QueueRemove of an unrelated item, ClearCache, StopRadio or a failing Next don't restart playback, leaving no skip, no banner and no expiry: a silent stall (S M1, O 1, T B1) | Blocking | Valid. Defer instead of drop (below) |
| R8-2 | `user_stops` sampled when the worker handles the event, not at failure; a busy worker widens AR-1 to seconds (O 2) | Major | Valid. The actor reads the counter in `Actor::error` |
| R8-3 | Drain-until-empty is unbounded under a sustained producer; Shutdown handling reorders checkpoints (O 3, S m1) | Major | Valid. Bounded drain, strict FIFO incl. Shutdown |
| R8-4 | v8's closed "kept" list dropped the `gone_probe` guards, the `None` → banner arming outcome, fixture routing, known limitations and docs obligations (T M2) | Major | Valid. Restore "everything in v4–v7 stands except as changed" |
| R8-5 | "No fetch" on a no-Media candidate has no mechanism; no stop check in `plan_advance` (T M3) | Major | Valid |
| R8-6 | `start_plan` Err: worker `current`/`presented`/`prepared` desync; A's records could land on B (S m2, T M5) | Major | Valid |
| R8-7 | `user_stops` must be `Arc<AtomicU64>` (T M4); `apply_prepared` resets (T m6); arming replaces `pending` (T m7); End payload (T m8); `Resume` re-arms (T m9); Bluetooth/MPRIS also bump (T m10); tests (T m11/m12); `describe()` (T m13); verdict handler outside `catch_unwind` (S m3) | Minor | Valid |

## Plan v8.1 — Stage 1 (v8 with amendments)

Everything in v4–v7 stands except as changed by v8 and v8.1. That includes the
`gone_probe` guards (offline mode, `network_known && !online`, radio,
`cached_playback`, no current track, `base`/`numeric` failure), the `None` →
immediate `_audio_error` arming outcome, fixture routing, the known limitations and
the docs obligations (R8-4). The v6/v7 stamp protocol stays dropped.

- **Compaction = deferral (replaces v8 §2; R8-1):**
  - On a matching `Gone` that has not been deferred yet, drain into `backlog` (see
    the bound below).
  - If the drained messages include any `Request`, append the same verdict to the
    back of `backlog`, marked `deferred`, and leave `pending` in place.
  - The requests then run first. When the deferred verdict comes back, it is not
    drained again; the normal validity checks decide:
    - `current != occurrence` → silent (the user's request really moved on);
    - otherwise skip, or banner (the original message, or the gone string).
  - A request that didn't change playback (QueueRemove of another item,
    ClearCache, a failing Next) therefore leads to the skip or banner. It can never
    stall silently.
  - The `class.transport` / `Resume` test is gone. Expiry still applies while
    deferred (`pending` remains, deadline unchanged).
- **Bounded drain (R8-3):**
  - At most 256 `try_recv` calls (the channel capacity) per drain; stop early when
    `shared.stop` is set.
  - Messages keep their exact order in `backlog`, including `Shutdown` and
    checkpoints. `Shutdown` is processed when it is reached in order.
  - Request counters are handled only by normal execution or `abandon`, never by
    moving a message into `backlog`. `abandon` at exit also walks `backlog`.
- **Stop/Pause counter at failure (R8-2):**
  - `Shared::user_stops: Arc<AtomicU64>`, bumped in `Shared::transport` for
    AudioStop/AudioPause. The audio actor receives a read-only `Arc` clone.
    `Engine::new` keeps its signature and gets a fresh counter;
    `Engine::with_user_stops` is used by the runtime.
  - `Actor::error` records it in `Event::Error { stops }` together with `paused`.
    `pending.stops = event.stops`.
  - At the verdict, a different counter value → banner, no skip.
  - Bluetooth auto-pause (`platform.rs:179`) and MPRIS Stop also bump it. That is
    the safe direction, listed next to AR-1.
- **No-Media in skip planning (R8-5):** `playback_plan` gains a `fetch_metadata:
  bool` parameter (true everywhere today). `plan_advance(.., skip_gone)` passes
  false, and a no-Media candidate then ends planning with None (the gone banner;
  documented: the candidate lacks Media, so the label may be imprecise).
  `plan_advance` checks `shared.stop` (passed as `&AtomicBool`) between iterations.
- **`start_plan` Err after apply (R8-6):** like the existing request-path
  `audio_failure` branch (`runtime.rs:~930`):
  - `engine.handle.stop()`, `current = 0` (so A's records can't land on B),
    `prepared = None`, `engine.handle.prepare(None)`, `presented` cleared;
  - `persist_playback`, publish Core (B), `_audio_error` with the error.
- **`apply_prepared`:** sets `resume_position = 0`, `settings.playback.listened = 0`,
  a new occurrence and generation (R8-7). Arming replaces any `pending`; dropping
  its `Task` aborts it.
- **`Event::End` gone arm:** after `Err(error) if radio`, before `Err(_) => {}`. The
  payload is `{"request":{"op":"next"},"response":{"ok":false,"error":<gone string>}}`,
  matching the existing arm.
- **Panic safety:** the verdict handler (`plan_advance`, `apply_prepared`,
  `persist_playback`) runs inside `catch_unwind`. A panic → `recover()` (which
  drops `pending`) + `_audio_error`.
- **`describe()`:** gets a `Gone` arm.
- **Tests (added):**
  - Compaction with a non-playback request (an unrelated QueueRemove, ClearCache,
    a failing Next) → the skip still happens, or exactly one banner; never silence.
  - Compaction with a user Next → a single advance (deferred verdict silent).
  - A sustained producer during the drain → the bound holds; shutdown is honoured
    in order.
  - `[Checkpoint(reset), Shutdown]` in the drain → the checkpoint is applied first.
  - A busy worker + Stop after failure → no skip (counter captured at failure).
  - A self-send on a full channel → expiry banner.
  - `start_plan` Err → `current` cleared, B published, banner.
  - A panic in the verdict handler → recovered, banner.
  - A no-Media candidate in skip planning → no HTTP request.

## Round 9 reviews (plan v8.1, stage 1)

(pending)

## Round 9 reviews (plan v8.1) — summary

- Sonnet: no Blocking; Minor M-1..M-6.
- Sol: no Blocking; one Major. Shutdown sets `stop` before sending `Shutdown`
  (`runtime.rs:489-495`), so queued checkpoints are skipped. This is pre-existing.
- Third chain: Space Bunny timed out (exit 124). The fallback model was still
  running when the user interrupted, to discuss button bursts. No third-chain
  result is recorded for round 9.

## User design decision (2026-10-18): background item health

The user raised rapid button bursts during an in-flight check. The decision:
- background checks run to completion on their own, and nothing waits for them;
- results are recorded on the queue item, not on "what is playing now";
- playback consults those records when it reaches an item;
- reaching a known-gone item by any path acts like a fresh failure: skip in the
  direction of travel (Previous goes back, everything else forward).

The app must stay responsive, so button bursts need no special handling. Plan v9
replaces v8/v8.1 compaction, deferral, the backlog and the tentative skip machinery.

### Accepted risks (carried from AR-1..AR-3, restated for v9)
- **AR-1:** a user action in the few milliseconds between the final "user acted?"
  check and the automatic skip command can be applied after the skip.
- **AR-2:** the automatic skip uses the paused state captured at failure.

## Plan v9 — Stage 1 (background item health)

### Kept unchanged (confirmed across rounds)
- **A. Diagnostics:**
  - occurrence id threaded into `http::Reader::new`;
  - a status-only `HTTP audio request failed occurrence= status= retry=` log;
  - `Source::recorded_error()`, logged on probe/packet failure;
  - all through `crate::diagnostics::event`; never `self.uri`.
- **`Event::Error { id, message, paused, acted }`:**
  - `paused = !self.desired`, captured before mutation at `audio/mod.rs:437/560/619`;
  - `acted` is defined below;
  - the cached/transcode fallback uses `event.paused` (R2-12 fix).
- **Gone string:** inline `Error::Input("This track is no longer on the server")`
  literals, a `GONE_TRACK` const pinned by a test, all 39 locales in
  `tools/translations.json`.
- **Radio:** radio sessions are excluded (no health jobs, no gone skipping). Their
  behaviour is unchanged.
- **Gapless retirement:** a failed prepared successor is retired silently (log,
  `handle.prepare(None)`, drop `prepared` and its `presented.plans` entry), with no
  banner.

### 1. Item health in Core (`src/lib.rs`)
- `ItemId = (play_queue_item_id, rating_key)`.
- `health: HashMap<ItemId, Health>`, with `enum Health { Gone, Resolved(Item) }`.
  - `Resolved` holds fresh metadata for a no-Media item.
  - Absent means unknown.
- `health_epoch: u64`, a monotonic counter (never reset; it survives `recover()` by
  being carried into the rebuilt Core). It is bumped, and `health` cleared, on
  Connect/SelectServer/Logout and any queue replacement
  (Play/PlayAlbum/Station/Radio/Mix/PlayDownload).
- `pub(crate) fn record_health(epoch, item, Health)`. It is ignored if the epoch is
  stale. `Resolved(fresh)` fills `media` into every queue item with that `ItemId`,
  keeping `play_queue_item_id`, and calls `touch()`.
- **"Act as fresh" navigation.** When `next`/`advance_next` (manual or automatic),
  `previous` or `select` land on an item that is `Gone` and not cache-served, they
  keep moving in the same direction:
  - forward for Next, automatic advance and Select;
  - backward for Previous;
  - repeat-one is ignored for a gone item.
- This runs on Core's real queue inside the command, so the normal `execute()`
  bookkeeping (persist, occurrence, generation) applies unchanged.
- The skip is bounded by `queue.len()` steps. It stops when there is no playable
  item in that direction: the end of the queue (repeat ≠ All), a backward step that
  doesn't move (cursor 0, `queue.rs:409-413`), or the bound is exhausted. On a stop
  the command restores the queue (the existing `next()` rollback pattern) and
  returns `Err(Error::Input("This track is no longer on the server"))`.
- `prepare_next` uses the same advance, so gapless preparation skips known-gone
  items naturally.

### 2. No synchronous metadata fetch in playback planning
`playback_plan`'s no-Media branch (`lib.rs:843-855`) no longer blocks:
1. If `health` has `Resolved(fresh)` → use it.
2. If `Gone` → the navigation above has already moved past it. If it is reached
   directly (e.g. Resume), return the gone error.
3. Otherwise return a plan with `"stream": ""` and `"resolving": <ItemId>`. Core's
   current item is set; nothing is loaded.

In offline mode the existing "not downloaded" error stays. Callers:
- the runtime (requests, End, Transition, fallback) handles `resolving` generically
  (section 4);
- `prepare_next` treats a `resolving` successor as "not prepared" and enqueues a
  health job for it, so it is resolved before playback reaches it.

### 3. Background health jobs (`src/runtime.rs`, `src/plex.rs`)
- Two job kinds:
  - **Check(item):** `Plex::probe_status(/library/metadata/{key})`, async, 5 s timeout,
    status only. 404 → `Gone`; anything else → no record.
  - **Fetch(item):** the async metadata GET, 5 s timeout, body decoded on the worker.
    404 → `Gone`. A matching Track with a Media Part → `Resolved(fresh)`. Otherwise
    the job fails, with no record and its error message kept.
- The worker owns `jobs: HashMap<ItemId, Task>`:
  - at most one job per item;
  - at most 2 running, with extra jobs waiting in a worker-local FIFO;
  - all dropped (aborted) on a health-epoch bump and on worker exit.
- Results come back as `WorkerMsg::Health { epoch, item, outcome }` via `try_send`.
  The worker applies them with `core.record_health`. It never waits for a job.
- **Triggers:**
  - (a) A current-track decode failure, inside the existing guard (`id == current`,
    the decode-failure message, source ≠ Cache), once the cached and transcode
    fallbacks have both failed → Check.
  - (b) A prepared-successor decode failure → Check, for that successor.
  - (c) A `resolving` plan, from any path → Fetch.
  - (d) A `resolving` successor in `prepare_next` → Fetch.
- No checks for: offline mode, `network_known && !online`, radio, cache-served
  items. With no job, (a) emits today's banner immediately.

### 4. Acting on results for the current item only
- **"User acted" counter:** `Shared::acted: Arc<AtomicU64>`, bumped
  - in `Shared::transport` for AudioStop/Pause/Play/Toggle (not Volume/Seek),
    before the `try_send`;
  - in `admit` for `class.transport` requests and `Resume`, before `try_send`.

  The audio actor holds a read-only clone (`Engine::with_acted`; `Engine::new` keeps
  its signature) and records it as `Event::Error.acted`.
- Worker-local `waiting: Option<Waiting { item, acted, direction, paused, message,
  deadline }>`, at most one at a time (replaced on re-arm):
  - for trigger (a): `acted = event.acted`; `direction` = Back if the request that
    started this occurrence was Previous, else Forward; `deadline` = now + 8 s;
  - for trigger (c) on the current item: `acted` = the counter value when the
    request finished, so the user's own Next isn't counted against itself;
    `message` = the job error.
- When a `Health` result for `waiting.item` arrives (or the deadline passes):
  - If `core.current_item_id() != item`: drop `waiting` silently. The user, or
    playback, moved on; the record is kept for later.
  - Else if `shared.acted != waiting.acted` (the user acted since): no automatic
    action. Show a banner if the outcome is Gone (the gone string) or failed (the
    message). A `Resolved` item stays loaded-but-idle; the user's own Play starts it.
  - Else:
    - **Gone** → execute `Next { automatic: false }`, or `Previous` for Back. "Act as
      fresh" navigation skips any further gone items. On Ok: `start_plan` +
      `merge`/`update` + `prepare()`, mirroring the request path. On Err → banner.
    - **Resolved** → execute `Resume`, then `start_plan` with `waiting.paused`.
    - **Failed / no record / deadline** → banner with the original message.
- `waiting` is also dropped on `recover()`, on a health-epoch bump and on
  Logout/SelectServer.
- **Prepared successor (b):** while its Check is running, `prepare()` returns early
  for the rest of the current occurrence (existing clear-then-return). When the
  result is Gone, call `prepare()` again; it now skips the item, so gapless
  continues past a dead track.
- **`Event::End`:** a gone error from the automatic `Next` (nothing playable ahead)
  is surfaced as `_audio_error` with the gone string, after the radio arm. Other
  errors behave as today.
- **Panic safety:** health-result handling and the skip run inside `catch_unwind`. A
  panic → `recover()` + publish status + `_audio_error`, mirroring the request path
  (`runtime.rs:1040-1056`).

### 5. Shutdown
- Jobs are aborted by dropping `jobs`; a late `try_send` to a closed channel fails
  harmlessly. The worker never waits on jobs.
- (Pre-existing, R9 Sol Major) at exit, checkpoints already in the channel or the
  overflow are applied before the final snapshot record. This is a small separate
  fix, tested via the real `Runtime::drop` path.

### 6. Tests
- **Health:**
  - `record_health` with a stale epoch is ignored.
  - `Resolved` fills media for all items sharing the `ItemId`.
  - Cleared on queue replacement and server change.
- **Navigation:**
  - Next, automatic and Select over gone items → land on the next playable item.
  - Previous over gone items → go back further.
  - Repeat-one on a gone current → moves.
  - Repeat-all with everything gone → bounded, then the gone error, queue unchanged.
  - Cursor 0 with Previous on a gone item → gone error.
  - A cache-served gone item plays.
- **No-Media:**
  - Next onto a no-Media item returns immediately with `resolving`, no blocking HTTP.
  - Fetch Resolved + no user action → plays.
  - User acted → stays idle.
  - Fetch 404 → skip in direction.
  - Fetch error → banner.
  - A no-Media successor is fetched ahead, then gapless.
- **Current failure:**
  - 404 + no action → skip.
  - 404 + Pause/Stop/Next pressed during the check → no automatic action; banner
    only if still on the item.
  - Burst: 20 mixed Next/Prev/Pause/Stop presses during an in-flight check → every
    press is applied immediately, in order; at most 2 jobs running; the final state
    follows the user's last navigation.
  - A non-404 → banner (Unknown).
  - A hung server → banner at the deadline; the worker stays responsive.
  - Offline/radio → immediate banner.
- **Prepared successor:** retired silently, checked, Gone → re-prepared past it, the
  transition stays gapless.
- **Diagnostics:** a 404 logs status + occurrence; the synthetic token is absent
  from logs.
- **Shutdown and panic:**
  - Shutdown with running jobs → prompt join.
  - Checkpoints before Shutdown are applied.
  - A panic in result handling → recovered, banner.
- **Gates:** `./tools/check.sh`, `bash tools/build-qm.sh --check`,
  `./tools/build-sailfish.sh`.
- **Phone:** installed RPM under Sailjail.

### Known limitations
- Stage 1 detects gone tracks but does not re-resolve them to their new key
  (stage 2 will fill `Health::Resolved` for re-keyed items by search).
- Gone marks last until a queue replacement or server change.
- A track is only checked after it fails (or fails to pre-load); there is no bulk
  validation of the whole queue.

## Round 10 reviews (plan v9, stage 1)

Reviewers:
- Sol (`openai/gpt-6.1-sol`): 2 Blocking.
- Fresh Sonnet subagent: 1 Blocking.
- Third chain unavailable: Space Bunny and DeepSeek v4.1 Flash timed out (exit
  124); Muse Spark returned server errors (exit 3). Per AGENTS.md the round
  proceeds with the two main reviews; this is recorded here.

Confirmed sound:
- task-drop abort on shutdown;
- the navigation rollback pattern;
- cursor-0 detection by comparing the cursor;
- panic safety mirroring the request path;
- dropping `waiting` on current-item change (bursts are handled).

| # | Finding (reviewers) | Sev | Adjudication |
| --- | --- | --- | --- |
| R10-1 | A `resolving` plan (empty stream) skips the occurrence/listened/resume reset in `execute()` (`lib.rs:1443-1457`); a later `Resume` keeps the old values; `start_plan` empty → `current` keeps the old audio id, so `record()` writes A's listened/position against B (S B1, O 1) | Blocking | Valid. Part of the no-Media background-resolution scope |
| R10-2 | Health results via `try_send` on the bounded request channel can be lost; job slots leak (O 2, S M1/M3) | Blocking | Valid. Use a lossless results store drained each loop (like `checkpoint_overflow`); release slots on completion |
| R10-3 | `acted` baseline sampled at request completion absorbs later presses (S M2, O 3) | Major | Valid. Stamp the request at admission |
| R10-4 | Stale-epoch results can still act or remove jobs (O 4); jobs keyed by item only, no upgrade/priority (S M3) | Major | Valid |
| R10-5 | `resolving` with no job allowed (unknown network, radio) never terminates; `Resolved` + `acted` leaves the player idle with no notice; a Play press during resolve is consumed (S M1, O 5) | Major | Valid. No-Media scope |
| R10-6 | A successful look-ahead Fetch doesn't rebuild the prepared successor (O 6) | Major | Valid. No-Media scope |
| R10-7 | The gone guard must be central (after the cache fast path, regardless of Media); QueueRemove landing and Resume paths (O 7, S m5) | Major | Valid |
| R10-8 | Hop cost/`autoplay` growth/repeat-one in hops; natural end with a gone tail; `prepare_next` plumbing; trigger (b) message; fallback swallowing `resolving`; epoch ownership across `recover()`; memory; UX; Fetch decode thread (S m1–m9) | Minor | Valid |

Status: blocked pending a user decision on whether the no-Media background
resolution stays in stage 1.

## User decision after round 10 (2026-10-18)

The user asked why queue items lack file details. Findings:
- `display_item` serializes full items, `Media` included (`lib.rs:555-556`), and
  QML sends rows back to `Command::Play` unchanged, so UI-started queues carry
  details.
- Bare items are built only for offline catalogue album/artist placeholders
  (`lib.rs:673`, `daily.rs:796-812`) and test fixtures.

Decision:
- Health should come from background work. Cache-ahead already downloads the
  upcoming tracks (`schedule_cache` → `queue.upcoming(cache.ahead + 1)`), and the
  cache classifies 404 as terminal (`cache.rs:1414-1424`).
- Reaching an unchecked track without details keeps today's synchronous fetch
  (accepted wait). Skipping ahead would usually hit the same situation; Plexamp also
  stops on such errors.
- The v9 non-blocking `resolving` contract is dropped. That removes R10-1, R10-5
  and R10-6.

## Plan v9.1 — Stage 1 (v9 with amendments)

Everything in v9 stands except as changed here.

### Removed from v9
- The `resolving` plan, trigger (c) and the current-item Fetch/`Resolved` autoplay.
- `playback_plan`'s no-Media branch stays synchronous, as today.

### Health sources (all background; none on the playback path)
1. **Cache-ahead download failure.**
   - The cache worker gets a `suspects: HashSet<String>` (rating keys) beside
     `errors`. It is filled when a download fails with `Error::Http(404)`, or
     `Error::Http(400)` on a transcode download, for the current batch epoch.
   - `Cache::take_suspects()` drains it. The worker polls it once per loop.
   - Each suspect that is still in the queue gets a **Check** (a metadata status
     probe; 404 → Gone). A part 404 is never treated as Gone by itself.
2. **Upcoming check.**
   - The candidates are the current item plus `cache.ahead + 1` upcoming items
     (minimum 2).
   - When cache-ahead won't download them (cache disabled, downloads not allowed,
     or the mobile cap at `lib.rs:936-940`), each unchecked candidate gets a Check.
   - Each unchecked candidate without Media gets a **Fetch** (fills details:
     `Resolved` → `record_health` fills `media`). It is never current-blocking: if
     playback reaches the item first, the synchronous fetch runs as today, and the
     job result is merely recorded.
   - Recomputed by `core.health_candidates()` after every request, Transition and
     End. At most one check per item per epoch.
3. **Prepared-successor decode failure** → Check (v9 b). Match the
   `"Next track decoding failed"` message explicitly (`audio/mod.rs:437,619`).
4. **Current-track decode failure** → Check (v9 a), at priority (exempt from the
   cap), inside the existing guard after the cached and transcode fallbacks fail.

`Health` gains `Alive`, recorded on a 200 so items are not re-checked. Health
records are skipped for radio, offline mode, `network_known && !online` and
cache-served items.

### Job lifecycle (R10-2, R10-4)
- Results go into a lossless `Shared::health_results: Arc<Mutex<VecDeque<HealthResult
  { epoch, item, kind, outcome }>>>`, drained every worker loop iteration. Every
  job future has its own 5 s timeout and always pushes a result, Unknown on
  timeout or error. No `try_send` is used.
- **Epoch:**
  - `Shared::health_epoch: Arc<AtomicU64>`, monotonic. It is bumped, and Core's
    `health` cleared, on Connect/SelectServer/Logout, queue replacement and
    `recover()`.
  - Core holds a clone, so the epoch survives a Core rebuild.
  - Results with a stale epoch are discarded before any other handling, including
    job bookkeeping.
  - On a bump, all running jobs are dropped (aborted) and the result store cleared.
- **Job table:**
  - `jobs: HashMap<(epoch, ItemId, kind), Task>`.
  - An entry is removed when its result is drained (whether it is applied or
    discarded), or on an epoch bump.
  - At most 2 normal jobs run, from a FIFO. Current-failure Checks run immediately
    beside them. A Check for an item with a running normal Check attaches to it
    (marked priority) instead of starting another.

### Acting on a current-failure result (v9 §4, trigger (a) only)
- `waiting` as in v9, armed only by trigger (a), with `acted` and `paused` from
  `Event::Error`, `direction` and an 8 s deadline.
- The automatic skip requires all of the following:
  - the current item is still `waiting.item`;
  - `shared.acted == waiting.acted`;
  - `shared.queued_transport == 0`.

  `queued_transport: Arc<AtomicUsize>` counts transport-class requests (plus
  Resume) that have been admitted but not yet dequeued. It is incremented after a
  successful `try_send` in `admit` and decremented at dequeue. Results don't arrive
  in FIFO order with requests, so this prevents a queued user Next from being
  applied after an automatic skip (a double advance). The remaining millisecond
  window is AR-1.
- Otherwise the v9 rules apply: drop silently if the user moved; banner if they
  acted while still on the item; Unknown/deadline → banner.

### Central gone guard (R10-7)
- `playback_plan` checks `health` right after the cache fast path, regardless of
  Media: Gone → `Err(gone)`. The cache fast path still wins.
- All Core paths that pick a new current item go through one helper,
  `settle(direction)`, which hops over Gone items:
  - next/automatic advance;
  - previous;
  - select;
  - QueueRemove of the active item;
  - `prepare_next`.
- Hops use `queue.advance(false)` or `previous()` plus the health/cache check only,
  without building a plan per hop. They are capped at `min(queue.len(), 50)`, and
  autoplay or radio refill is never started from a hop (`m1`).
- No playable item in the direction:
  - for user commands: restore the queue and return `Err(gone)`;
  - for automatic End: apply natural end (`current = None`, as `queue.rs:386-388`)
    and surface the gone banner (`m2`).
- Resume on a Gone current item → `Err(gone)` immediately, without a Check.
- **Fallbacks (`m5`):** `CachedPlayback` stays first. `TranscodedPlayback` on a Gone
  item returns `Err(gone)` instead of trying.

### Other round-10 minors
- `Health` stores only the marker for `Resolved`, after filling media (`m7`).
- Fetch bodies are decoded on the worker (bounded by `JSON_LIMIT`); `plexfreq-rt`
  only transfers (`m9`).
- New Plex APIs:
  - `probe_status` (status only);
  - `fetch_metadata_bytes` (status + bounded body).
- UX: no new QML in stage 1 (`m8`, gone items are not visually marked; follow-up).

### Tests (changes relative to v9)
- **Removed:** the no-Media `resolving` tests.
- **Added:**
  - A cache 404 for an upcoming item → Check → Gone → the End transition skips it
    seamlessly (gapless re-prepared).
  - A cache 400 on a transcode download → Check.
  - Upcoming check when the cache is disabled.
  - An upcoming no-Media item gets details in the background; reaching it first
    still uses the synchronous fetch.
  - Results are delivered while the request channel is full (no loss, slots
    released).
  - Stale-epoch results after a queue replacement → no action, no job-table
    corruption.
  - A queued user Next while a current-failure result arrives → no automatic skip,
    a single advance.
  - QueueRemove of the active item onto a gone item → hops forward.
  - A natural end with a gone tail → queue end + banner.
  - Resume on a gone current item → error, no request.
  - `TranscodedPlayback` on a gone item → gone error.
  - `recover()` bumps the epoch and drops jobs.
  - An item checked Alive is not re-checked.

## Round 11 reviews (plan v9.1, stage 1)

Reviewers:
- Sol: 1 Blocking.
- Fresh Sonnet subagent: 2 Blocking.
- Third chain unavailable: every model timed out or returned server errors
  (exit 3). Recorded per AGENTS.md.

Closed/moot:
- R10-2: lossless store + epoch-keyed jobs.
- R10-3: moot.
- R10-4: stale-epoch rejection, Shared epoch.
- R10-7/8: in design, except the terminal End below.

No GUI blocking, safe shutdown, and no self-triggering loop were confirmed.

| # | Finding (reviewers) | Sev | Adjudication / v9.2 amendment |
| --- | --- | --- | --- |
| R11-1 | Unknown outcomes (5xx/401/timeout) are not remembered; candidates are recomputed after every request, so the same items are re-probed. Cache suspects are re-created on every `schedule_cache` (S B1) | Blocking | Valid. Attempt record with backoff, plus a per-epoch budget |
| R11-2 | Priority current-failure Checks have no aggregate bound; a button burst over failing items → a probe burst (O 1) | Blocking | Valid. One global cap including priority, with a reserved slot |
| R11-3 | Natural End: the sync no-Media fetch returns Http(404)/Network, swallowed by `Err(_) => {}` (`runtime.rs:765`): silent stop (S B2) | Blocking | Valid |
| R11-4 | Gone recorded before the current failure: no new result arrives, so the deadline gives a banner, not a skip (O 2); stale Alive suppresses the priority Check (S M1) | Major | Valid |
| R11-5 | `queued_transport` incremented after `try_send` can underflow (O 3, S m3) | Major | Valid |
| R11-6 | A gone tail at automatic End: `next()` rollback and `execute()` early return skip persistence and publication (O 4) | Major | Valid |
| R11-7 | A non-Plex 404 (proxy/wrong host) marks items Gone (S M2) | Major | Valid |
| R11-8 | Drain before the Checkpoint `continue`; spawn candidates before `prepare()`; epoch plumbing in `Core::new`/`inspect`, bump only after a successful replacement; job key by rating_key; Check/Fetch dedupe; candidate predicate incl. `cache_suspended` and lookup(touch=false); re-prepare on any Gone for the prepared item; autoplay at a gone tail documented; direction per occurrence; decode size cap; job futures free of `Plex::run`, drop-guard result (S m1–m11) | Minor | Valid |

## Plan v9.2 — Stage 1 (v9.1 with amendments)

Everything in v9 and v9.1 stands except as changed here.

- **Attempt record and rate (R11-1):**
  - Core keeps `attempts: HashMap<rating_key, Attempt { at, failures }>`. It is
    cleared with `health` on an epoch bump and never by job-table removal.
  - A background candidate or suspect is checked only if it has no health record
    and is not in backoff: 60 s after the first Unknown, then 5 min, capped there.
    401/403 suspends health checks for the whole epoch.
  - Per-epoch budget: at most 30 normal checks per rolling minute; beyond that,
    candidates wait.
  - Cache suspects are deduplicated through the same record, so a re-created
    suspect for an attempted key is ignored.
- **Global cap (R11-2):**
  - 3 running jobs in total, one slot reserved for current-failure Checks.
  - A second current-failure Check while the reserved slot is busy replaces the
    queued priority request, not the running one. Only the newest current failure
    waits; older ones keep their deadline and banner.
  - A running normal Check for the same key is attached and promoted, including
    one still in the FIFO.
  - Check and Fetch for the same key are deduplicated: a Fetch also yields
    Gone/Alive, so a pending Check is dropped when a Fetch is queued.
- **Synchronous no-Media 404 (R11-3):**
  - A `playback_plan` sync fetch returning a Plex-shaped 404 (see R11-7) records
    `Gone`, and the calling navigation continues `settle()` in the same direction.
  - Other errors propagate as today.
  - In the `Event::End` arm, every non-radio `Next` error is now surfaced as
    `_audio_error` with its message, not swallowed. This intentionally includes
    offline "not downloaded" at a natural end (accepted behaviour change).
- **Arming with existing health (R11-4):**
  - Trigger (a) consults health first.
  - Gone → the automatic-skip checks (current item, `acted`, `queued_transport`)
    apply immediately, with no job.
  - Alive or Unknown → a new priority Check always runs; it overrides `Alive`.
  - A running job for that key → attach.
- **`queued_transport` (R11-5):**
  - Incremented before `try_send`, under the admission lock, and decremented on
    send failure.
  - The `Request` carries a `counted: bool` flag, so the dequeue decrement uses the
    same predicate.
- **Gone tail at automatic End (R11-6):**
  - `next(automatic)` returns a terminal success when only Gone items remain ahead.
    The queue is set to natural end (`current = None`), and an empty-stream plan
    carries `"notice": "gone"`. `execute()` then persists and publishes it normally.
  - The runtime stops the engine, clears `prepared`, and emits `_audio_error` with the
    gone string.
  - User commands keep the rollback + `Err(gone)`.
- **Plex-shaped 404 (R11-7):** Gone only if the 404 response carries the
  `X-Plex-Protocol` header or a `MediaContainer` body (checked within the bounded
  read). Otherwise Unknown.
- **Minors (R11-8):**
  - **Worker loop:**
    - The health-result drain and the `waiting` deadline run before the request
      `match`, before any `continue`.
    - Health candidates are spawned before `prepare()`.
  - **Epoch:**
    - `Core::new`/`inspect` take an optional shared epoch, defaulting to a private
      one for tests and the refresh worker.
    - `recover()` attaches `shared.health_epoch`.
    - The epoch bump happens only after a successful queue replacement.
  - **Keys and lookups:**
    - Health, attempts and jobs are keyed by `rating_key`.
    - The candidate predicate is factored out of `schedule_cache`, including
      `cache_suspended`.
    - The cache-served exclusion uses `lookup(.., false)`.
  - **Prepared successor:** `prepare()` re-runs on any Gone result for the
    prepared successor's key.
  - **Direction:** `direction` is per occurrence. The request path sets it from the
    command; End, Transition and fallbacks set Forward.
  - **Fetch decoding:** bodies over 1 MiB are discarded (Unknown).
  - **Job futures:** free async functions capturing `(client, base, token)`. No
    `Plex::run` or `bind_view`. A drop guard pushes Unknown on abort or panic.
  - **Documented:** autoplay doesn't start when the tail is all Gone (playback ends
    with the banner).
- **Tests (added):**
  - 503 storm: bounded probes per minute, and backoff respected.
  - 401: checks suspended for the epoch.
  - A cache suspect re-created after an attempt → no new probe.
  - A burst over 20 failing items → at most 3 running jobs, one waiting priority.
  - End with a no-Media 404 successor → skip.
  - End with a network error → banner.
  - Gone before the current failure → immediate skip, no job.
  - Stale Alive → priority Check runs.
  - `queued_transport` barrier test (no underflow).
  - Gone tail at End → persisted natural end; a fresh Core reload shows it; banner.
  - A proxy-style 404 without Plex markers → Unknown.
  - Checkpoint flood doesn't starve the result drain.

## Round 12 reviews (plan v9.2, stage 1)

Reviewers:
- Sol: 1 Blocking.
- Fresh Sonnet subagent: 1 Blocking.
- Third chain unavailable again (exit 3). Recorded.

Confirmed sound:
- single epoch writer;
- result store bounded by the job count;
- prompt shutdown;
- per-key backoff;
- no self-triggering loop;
- End-arm surfacing closes R11-3;
- the gone-tail terminal success fits the existing natural-end shape.

| # | Finding (reviewers) | Sev | Adjudication |
| --- | --- | --- | --- |
| R12-1 | The automatic skip from a Gone last track: `Next{automatic:false}` → natural end, `Ok`, empty stream → silent stop (S) | Blocking | Valid. Treat an empty-stream skip result as a gone banner |
| R12-2 | Priority checks have a concurrency cap but no rate/backoff; fast 503s plus navigation → unbounded rate; 401 suspension precedence (O 1) | Blocking | Valid. Universal auth suspension, priority budget, same-key cooldown |
| R12-3 | `queued_transport` decremented at dequeue: a request dequeued in the same iteration as the result drain allows a double advance (S) | Major | Valid. Drain before `recv`, or decrement after execution |
| R12-4 | Sync fetch can't see 404 headers (`status()` discards them); PMS 404 shape unverified (S) | Major | Valid. **Measured (below):** PMS 404 carries `X-Plex-Protocol: 1.0` with an HTML body and no MediaContainer. Detection must use the header, and the sync path needs a shape-aware call |
| R12-5 | Cache-ahead re-downloads dead items on every `schedule_cache` (new batch, per-batch `terminal`, `cache.rs:1096-1146,1362`) (S) | Major | Valid. Pre-existing, but needs a bound |
| R12-6 | `prepare_next` sync-fetch per hop blocks the worker; hop cap 50 vs a longer queue (S) | Major | Valid |
| R12-7 | Gone is sticky/silent; Select of a Gone item can never retry (S) | Major | Valid |
| R12-8 | A fallback resets the Previous direction (O 2) | Major | Valid |
| R12-9 | Background-filled media lost at `commit_prepared` (whole-queue replacement) (O 3, S) | Major | Valid |
| R12-10 | waiting/replacement wording; gone-tail cursor restore; budget edges; fixture variants (S) | Minor | Valid |

**Measured 2026-10-18 (read-only GET from the phone, token as a header only):**

| Request | Status | Type | Header |
| --- | --- | --- | --- |
| `/library/metadata/33963` (stale) | 404 | `text/html` | `X-Plex-Protocol: 1.0` |
| `/library/metadata/98244` (live) | 206 | `application/json` | — |
| `/library/parts/34331/…/file.mp3` (stale) | 404 | `text/html` | `X-Plex-Protocol: 1.0` |

Status: paused for a user check-in after 12 rounds.

## User decision after round 12 (2026-10-18): minimal stage 1 (option B)

Stage 1 keeps only:
- failure-triggered background checks;
- gone marks with a TTL;
- skipping in the direction of travel.

Deferred to later stages:
- proactive checks of upcoming tracks when cache-ahead is off;
- background fetching of missing file details;
- the cache-ahead retry fix (except for excluding gone keys);
- prepared-queue media reconciliation (moot without background fetch);
- the pre-existing shutdown checkpoint ordering (R9).

Plan v10 is self-contained and supersedes v4–v9.2. Accepted risks AR-1/AR-2 (as
restated for v9) still apply.

## Plan v10 — Stage 1 (minimal, self-contained)

### 1. Diagnostics (`src/audio/http.rs`, `src/audio/decode.rs`)
- Occurrence id passed into `http::Reader::new`.
- A status-only `HTTP audio request failed occurrence= status= retry=` log for
  4xx/5xx.
- `Source::recorded_error()`; on probe/packet failure, log
  `audio source failed occurrence=: <static msg>`.
- All logging via `crate::diagnostics::event`. Never `self.uri` (it carries the
  token).

### 2. Gone marks (`src/lib.rs`)
- `gone: HashMap<rating_key, Instant>`; an entry counts only while younger than
  10 min (TTL).
- `attempts: HashMap<rating_key, Instant>`: a 60 s cooldown after an Unknown result.
- **Epoch:**
  - `Shared::health_epoch: Arc<AtomicU64>`, also held by Core.
  - `Core::new`/`inspect` take an optional epoch, defaulting to a private one.
  - Bumped, with `gone`/`attempts` cleared, after a successful
    Connect/SelectServer/Logout or queue replacement
    (Play/PlayAlbum/Station/Radio/Mix/PlayDownload), and in `recover()`.
- Never used for radio sessions.
- `schedule_cache` excludes gone keys from the cache-ahead list, so dead parts are
  not re-downloaded on every request.

### 3. Sources of checks (all failure-triggered)
1. **Cache-ahead.** A download failing with `Error::Http(404)`, or
   `Error::Http(400)` on a transcode download, adds the rating key to
   `Cache::suspects`. `take_suspects()` is drained by the worker at the top of
   each loop. A suspect still in the queue gets a Check.
2. **Prepared successor.** An `Event::Error` for the live prepared serial
   (`"Next track decoding failed"`, `audio/mod.rs:437,619`) gets a Check for that
   successor.
3. **Current track.** Inside the existing guard (`id == current`, message
   `"Audio decoding or streaming failed"`, source ≠ Cache), once the cached and
   transcode fallbacks have both failed, a Check runs.

Not checked: offline mode, `network_known && !online`, radio, cache-served items.
In those cases source 3 shows today's banner immediately.

### 4. Check jobs (`src/plex.rs`, `src/runtime.rs`)
- `Plex::probe_status(client, base, token, key)`: a free async function. It
  captures owned values, never touches `Plex::run` or `bind_view`, and sends the
  token in a header only.
  - GET `/library/metadata/{key}`, 5 s timeout, status and headers only; the body
    is discarded.
  - **Gone** ⇔ 404 **and** an `X-Plex-Protocol` header (measured: PMS 404s carry
    `X-Plex-Protocol: 1.0` with an HTML body).
  - **Auth** ⇔ 401/403.
  - Everything else (2xx, 5xx, a header-less 404, timeout, error) → **Unknown**.
- Results go into a lossless `Shared::health_results: Arc<Mutex<VecDeque<Result
  { epoch, key, outcome }>>>`. A drop guard pushes Unknown on abort or panic.
- The worker holds `jobs: HashMap<(epoch, key), Task>`.
  - One job per key; at most 2 running in total; extra requests are dropped,
    except as noted for source 3.
  - Entries are removed when their result is drained, or when the epoch bumps
    (which aborts the jobs).
- **Rate limits:**
  - A rolling budget of 10 checks per minute across all sources.
  - A per-key cooldown after Unknown (`attempts`).
  - A key that is already Gone (within its TTL) is never re-checked.
  - Auth → all checks suspended for the epoch.
  - When source 3 cannot get a check (budget, cooldown, suspension, no slot, or
    the slots are busy), the banner is shown immediately. A source-3 request for a
    key with a running job attaches to it.
- **Draining.** At the top of every worker loop iteration, before `recv_timeout`,
  the worker:
  1. drains `health_results` (stale epochs are discarded first);
  2. drains `take_suspects()`;
  3. handles `waiting` (section 5) and its deadline.

  This runs before the `Checkpoint` arm's `continue` can skip it, and before any
  request is dequeued in that iteration.
- A Gone result for the prepared successor's key → `prepare()` re-runs, and now
  skips it.

### 5. Current-failure handling (`src/runtime.rs`)
- **Pause intent and the "user acted" counter:**
  - `Event::Error { id, message, paused, acted }`.
  - `paused = !self.desired`, captured before mutation at all three error sites.
    The existing cached/transcode fallback uses `event.paused` instead of
    `snapshot.paused`.
  - `Shared::acted: Arc<AtomicU64>` is bumped before `try_send` in
    `Shared::transport` (AudioStop/Pause/Play/Toggle; not Volume/Seek) and in
    `admit` (class.transport and Resume).
  - The actor reads it through a clone passed in by `Engine::with_acted`;
    `Engine::new` keeps its signature.
- **Queued-request counter:** `Shared::queued_transport: Arc<AtomicUsize>`,
  incremented under the admission lock before `try_send` (decremented again if the
  send fails). `Request.counted` marks counted requests; the worker decrements at
  dequeue.
- **Direction:** a worker-local `direction` per occurrence. Previous → Back; every
  other request, End and Transition → Forward. A same-item fallback (cached or
  transcode) keeps the direction of the occurrence it replaces.
- **Arming (source 3):**
  - If the key is already Gone → evaluate immediately (below).
  - Otherwise start or attach a Check and set `waiting = { key, acted, paused,
    direction, message, deadline: now + 8 s }`. A new arm replaces `waiting`.
- **Evaluating** a result, or the deadline:
  1. The current item's key ≠ `waiting.key` → drop silently.
  2. `shared.acted != waiting.acted` or `queued_transport > 0` → no automatic
     action. Show a banner: the gone string if Gone, otherwise the original
     message.
  3. **Gone** → execute `Next { automatic: false }` (Forward) or `Previous`
     (Back), through the normal request bookkeeping.
     - Ok with a stream → `start_plan(paused)`, `merge`, `update`, `prepare()`.
     - Ok with an empty stream (natural end reached) → stop, publish, gone banner
       (closes R12-1).
     - Err → banner.
  4. **Unknown/Auth/deadline** → banner with the original message.
- `waiting` is also dropped on `recover()` and on an epoch bump.
- Result handling and the skip run inside `catch_unwind`. A panic → `recover()`,
  publish status, then `_audio_error`.

### 6. Navigation skips gone items (`src/lib.rs`, `src/queue.rs`)
- One helper, `settle(direction)`, after the queue picks a new current item. It
  hops over items that are Gone (within the TTL) and not cache-served
  (`cache.lookup(.., false)`).
  - Hops use `queue.advance(false)` or `previous()` only: in memory, with no plan
    building, no sync fetch, no autoplay and no radio refill.
  - The hop limit is `queue.len()`.
  - A Back hop that doesn't move (cursor 0, `queue.rs:409-413`) stops.
- Used by: Next (manual and automatic), Previous, QueueRemove of the active item,
  and `prepare_next`.
- **Explicit Select or Resume of a Gone item** clears its mark and tries it: the
  user's explicit intent retries. If it fails again, it is re-checked normally.
- **Nothing playable in the direction:**
  - user command → restore the queue, return
    `Err(Error::Input("This track is no longer on the server"))`;
  - automatic End → natural end: `current = None`, with the cursor restored to the
    last audible slot. The empty-stream plan carries `notice: "gone"`.
    `execute()` persists and publishes it; the runtime stops the engine, clears
    `prepared` and emits the gone banner.
- `playback_plan` checks Gone right after the cache fast path, regardless of
  Media. The cache fast path wins. `TranscodedPlayback` on a Gone item returns the
  gone error.
- `Event::End` surfaces every non-radio `Next` error as `_audio_error`, after the
  radio arm. This intentionally includes offline "not downloaded".
- Documented: autoplay doesn't start when the tail is all Gone.

### 7. Prepared successor
- **Retirement on failure (source 2):** log it, `engine.handle.prepare(None)`, and
  drop `prepared` and its `presented.plans` entry, without a banner.
- **Suppression:** `prepare()` returns early, after its existing clear, for the
  rest of the current occurrence, until the Check result arrives. If the result is
  Gone, `prepare()` re-runs and skips the item (gapless kept). If it is Unknown,
  suppression stays until the occurrence changes.

### 8. Translation
- Inline `Error::Input("This track is no longer on the server")` literals, plus a
  `GONE_TRACK` const for comparisons, pinned by a test.
- All 39 locales in `tools/translations.json`; `bash tools/build-qm.sh --check`.

### 9. Tests (fixture server: per-path routing; 404 with/without `X-Plex-Protocol`; 503; 401; a hold-open mode)
- **Diagnostics:** status and occurrence logged; the synthetic token is absent
  from logs.
- **Probe:** classification table; timeout; a drop guard on abort.
- **Incident replay:** a cache 404 for an upcoming item → Check → Gone → End skips
  it with gapless re-prepare.
- **Current failure:**
  - 404 → skip forward.
  - Previous then failure → skip back, also through a transcode fallback.
  - Already Gone at arming → immediate skip.
  - Unknown → banner.
  - A hung server → banner at the deadline, worker responsive.
- **User action:** Pause/Stop/Next during the check → no automatic action, banner
  only if still on the item. A queued Next at result time → a single advance.
- **Burst:** 20 mixed presses → applied in order, at most 2 jobs, at most 10
  checks/min; excess failures banner immediately.
- **Server trouble:**
  - 503 storm → the budget and cooldown bound total requests over time.
  - 401 → suspended, immediate banners.
  - A proxy-style 404 without the header → Unknown.
- **Natural end and edges:**
  - Last track Gone → natural end + banner; persisted (a fresh Core reload shows
    it).
  - Gone tail at End → the same; Previous returns to the last audible track.
  - Select of a Gone item → retried.
  - TTL expiry → retried.
  - Cache-served Gone → plays.
  - QueueRemove onto Gone → hops.
  - Repeat-all fully Gone → bounded, then the gone error.
- **Epoch and recovery:** stale-epoch results ignored after a queue replacement;
  `recover()` drops jobs and `waiting`.
- **Prepared successor:** failure → silent retirement, Check, Gone → re-prepared;
  Unknown → no re-prepare loop.
- **Translation:** the gone string is present in the catalogues.

### 10. Gates and verification
- `./tools/check.sh`, `bash tools/build-qm.sh --check`, `./tools/build-sailfish.sh`.
- Phone: installed RPM under Sailjail. A successful build is not a hardware test.
- Results go in `docs/validation.md`, the behaviour in `docs/architecture.md`, and
  follow-ups in the roadmap Open items.

### Deferred (roadmap follow-ups)
- Stage 2: re-resolve gone items to their new key.
- Proactive upcoming checks when cache-ahead is off.
- Background detail fetch.
- Cache-ahead retry policy.
- Shutdown checkpoint ordering (R9).
- Visual marking of gone items.

## Round 13 reviews (plan v10, stage 1)

Reviewers:
- Sol: 1 Blocking.
- Fresh Sonnet subagent: no Blocking.
- Third chain unavailable (exit 3). Recorded.

Sonnet verified as sound:
- asynchronous load failures reach the source-3 guard string;
- probe tasks run off the worker;
- `Actor::error` clears current/next, so a Gone skip cannot double-advance with the
  engine;
- the drain-before-`recv` ordering closes R12-3;
- server load is bounded (about 3 requests per dead item, 10 probes/min);
- repeat/cursor-0 termination;
- `catch_unwind` recovery;
- banner translation via `backend.cpp:65`.

| # | Finding (reviewers) | Sev | Adjudication / v10.1 amendment |
| --- | --- | --- | --- |
| R13-1 | The automatic skip checks `acted` and then runs `Next`, whose `playback_plan` may block up to 15 s on a no-Media target's sync fetch (`lib.rs:843-855`); a Stop during that wait is overridden by the later load. AR-1 widens to seconds (O 1) | Blocking | Valid |
| R13-2 | Gone is cleared only after a successful queue replacement, but Play/PlayAlbum of a Gone key fails on the Gone guard, so it can never be retried (O 2) | Major | Valid |
| R13-3 | Source-3 budget starvation by cache suspects (S M1) | Major | Valid |
| R13-4 | QueueRemove rollback undoes the user's removal of a dead active track (S M2) | Major | Valid |
| R13-5 | A no-Media landing item with a sync 404 gets a banner but no check (S M3) | Major | Valid |
| R13-6 | `notice: "gone"` leaks into the shared model via `merge` (S M4) | Major | Valid |
| R13-7 | Direction assigned only on accepted new occurrences (O 3); `Core::new` signature (S m1); probe key `numeric` (m2); stale successor-error banners (m3); `schedule_cache` filter-before-take and cancel-on-empty (m4); radio bypass of `settle` and the guard (m5); lookup only for Gone items (m6); Previous after a source-3 natural end (m7) | Minor | Valid |

## Plan v10.1 — Stage 1 (v10 with amendments)

Everything in v10 stands except as changed here.

- **Automatic skip without a sync wait (R13-1):**
  - Before executing the skip, `core.peek_settle(direction)` computes the target on
    a queue clone, in memory (health and cache only).
  - The skip runs only if the target is cache-served or has Media/Part, so
    `playback_plan` needs no network. A no-Media or no-target result → banner, no
    automatic action.
  - After `execute()` returns, re-check `current key == waiting.key`-equivalent
    state, `acted` and `queued_transport` immediately before `start_plan`.
  - If either moved, don't load: publish the new Core state (already advanced
    and persisted by `execute()`), with no banner. The user's newer action then
    applies to it.
  - The remaining window is the in-memory `execute()` plus `try_send` (AR-1).
- **Replacement with Gone keys (R13-2):**
  - Queue-replacement commands (Play/PlayAlbum/Station/Radio/Mix/PlayDownload)
    move `gone`/`attempts` aside before planning.
  - On success they bump the epoch (marks stay cleared). On failure they restore
    the saved maps.
- **Source-3 reservation (R13-3):**
  - The 10/min budget reserves 3 checks for source 3, and one of the 2 job slots
    is reserved for it.
  - Suspects start at most 1 new check per drain.
- **QueueRemove (R13-4):** removing the active item keeps the removal. If nothing
  playable remains ahead, apply natural end (stop, `current = None`) with the gone
  banner, instead of the rollback.
- **No-Media sync 404 (R13-5):** when the `playback_plan` sync fetch returns
  `Error::Http(404)`, queue a source-3-style Check for that key, and show the
  banner. The Check result is only recorded (no automatic action), so later
  navigation skips the key.
  - The pre-existing 15 s sync fetch remains for landing items; noted in Open items.
- **No model leak (R13-6):** the gone notice is carried out of band. Core returns
  it in a field that the runtime strips before `merge`/`visible_plan`, the same way
  `stream` is handled.
- **Minors (R13-7):**
  - **Direction:** set only when a new occurrence is accepted (successful
    load/commit). Status, NetworkState, failed commands, reprepare and same-item
    fallbacks preserve it.
  - **Core epoch:** `Core::set_health_epoch(Arc)` instead of a signature change,
    called by the worker for the initial Core and the `recover()` Core.
  - **Probe key:** `numeric(&key)?` before building the probe path.
  - **Stale successor errors:** an `Event::Error` whose id is neither current nor
    the live prepared serial, with the successor message, is dropped silently
    (logged).
  - **`schedule_cache`:** filter Gone keys before `take(ahead+1)`. When the
    filtered list is empty, cancel the previous batch explicitly.
  - **Radio:** while `radio.is_some()`, `settle` and the `playback_plan` Gone guard
    are bypassed.
  - **Cache lookups:** `cache.lookup(.., false)` is called only for Gone candidates.
  - **Previous after a source-3 natural end:** lands on the failed item, then
    Back-`settle` hops off it. Tested.
- **Added tests:**
  - A held metadata response for a no-Media skip target, with Stop during the
    wait → no automatic skip (banner).
  - `acted` changed after `execute()` → no load.
  - Play/PlayAlbum of a Gone key → plays.
  - A failed Play restores the marks.
  - A suspect flood still leaves source-3 capacity.
  - QueueRemove of a dead active track at the tail → removed, natural end, banner.
  - A sync no-Media 404 → Check recorded → the next navigation skips the key.
  - The model never contains `notice`.
  - Previous → Status → failure → skip back.

## Round 14 reviews (plan v10.1 amendments, focused)

Reviewers:
- Sol: **no Blocking**, 1 Major.
- Fresh Sonnet subagent: **no Blocking**, 4 Major + minors.
- Third chain unavailable (exit 3). Recorded.

Both confirm R13-1 closed in substance, and R13-2..R13-7 closed at plan level.

| # | Finding (reviewers) | Sev | v10.2 disposition |
| --- | --- | --- | --- |
| R14-1 | "No-target → banner only" suppresses the v10 source-3 natural end (Gone last track / Gone tail) (O, S F1) | Major | Only a playable target **without Media** suppresses the automatic action. A no-target result commits the network-free natural end (persisted, published, gone banner), with the intent check before the engine stop |
| R14-2 | Post-`execute()` "don't load" branch underspecified: Core at B with a new occurrence, nothing loaded, runtime `current` on A (S F2) | Major | On the intent change, publish Core state with `stream` stripped, set the runtime `current = 0`, clear `presented.plans` and `prepared`, and leave the engine idle. The occurrence was already rotated by `execute()`; it is accepted that B gets no listen until played. Compare only `acted`/`queued_transport` (the key clause is dropped). Transport Play then finds nothing loaded and re-admits Resume (`runtime.rs:277-288`), which loads B. Test it |
| R14-3 | Save/restore of marks inside command arms is double-handled by PlayAlbum → Play (S F4) | Major | One scoped guard in `execute()` around `execute_inner` for the replacement command set: restore on Err/panic, clear on Ok. The epoch bump stays in the worker after Ok. Replaying an album with a dead track re-fails once per replay (recorded) |
| R14-4 | R13-4 test trivial; banner only when a Gone hop happened; request-path banner; non-Gone Err still rolls back (S F5) | Major | Test "remove active A with tail [B Gone] → removal kept, natural end, banner" plus "remove last item → no banner". The runtime emits the banner from `notice` on the request path too. Non-Gone Err rollback is the intended residual |
| R14-5 | `peek_settle` shares the exact cache predicate with `playback_plan`; residual eviction window (S F3) | Minor | Shared helper; the residual is noted under AR-1 |
| R14-6 | R13-5 channel: Core `pending_checks` drained by the worker, bypassing the in-queue filter, suspect class (S F6) | Minor | Adopted |
| R14-7 | `notice` stripped in `visible_plan` itself, next to `stream`, after extracting it into the banner (S F7, O) | Minor | Adopted. Test every path (event, request, presented, model) |
| R14-8 | Source 2 is non-reserved; excess suspects dropped (S F8) | Minor | Adopted |
| R14-9 | `schedule_cache`: emptiness checked after the `manual_cache` append; window bounded by queue length (S F9) | Minor | Adopted |
| R14-10 | Vacuous held-response test reworded; radio rationale; same Arc in `recover()`; stale-successor drop keyed on message + id (S F10) | Minor | Adopted |

**Plan status:** v10 + v10.1 + v10.2 dispositions above make up the approved stage-1
plan. No Blocking finding remains after rounds 13–14. Implementation starts per the
user's instruction (2026-10-18). Implementation review follows per AGENTS.md.

## Implementation review, round 1 (stage 1 code)

Scope: the working-tree diff vs HEAD plus the new `src/health.rs`,
`src/health_tests.rs` and `src/runtime/recovery.rs`.

Reviewers:
- Sol (with the diff): 1 Blocking.
- Fresh Sonnet subagent: no Blocking (ran `cargo test --lib`: 166 passed).
- Third chain unavailable (exit 3). Recorded.

**Plan deviation (adjudicated, kept):** forward navigation over a gone tail
(user Next, automatic Next, QueueRemove, and an all-gone repeat-all queue) ends
naturally with the gone notice. v10 restored the queue for user commands. This
matches Next on a last track, is persisted, and is never silent. Previous with
nothing behind still restores the queue and reports.

| # | Finding (reviewers) | Sev | Disposition |
| --- | --- | --- | --- |
| I1-1 | `hold_prepare` set only when the probe was admitted; a refused probe (budget, slot, cooldown, Auth) lets every queue edit re-fetch the failed successor (O) | Blocking | Fixed: hold set unconditionally. Test `a_failed_successor_is_not_fetched_again_on_queue_edits` (checks refused with 401, 3 enqueues → successor part fetched once) |
| I1-2 | Gone-tail natural end keeps the cursor on the last gone hop; after TTL expiry or a restart Previous lands on a gone item (O) | Major | Fixed: `Queue::finish_at(audible)`; QueueRemove uses the slot before the removed item. Test: Previous after a restart resumes track 1 |
| I1-3 | Missing adversarial coverage (O, S) | Major | Added: hung check → original error after the 5 s probe timeout, worker responsive; 401 → original error, no skip; no-Media skip target → gone banner, no automatic fetch after the check; prepare-hold request count; failed command leaves no notice. Not added (recorded): a deterministic barrier test of the post-`execute()` intent change, a queued Next exactly at verdict time, a 20-press burst through the worker (budget and slots unit-tested in `recovery.rs`) |
| I1-4 | Stray gone banner on a discarded/stale/failed reply; notice left after rollback (S) | Minor | Fixed: `execute()` clears the notice on Err; the request path shows the banner only for an ok, fresh reply |
| I1-5 | `waiting` silently dropped on an epoch bump (e.g. same-server Connect) (S) | Minor | Fixed: the orphaned failure reports its original message if still current |
| I1-6 | HTTP failure log lacks the retry counter (O) | Minor | Fixed |
| I1-7 | Translations are keyword fragments (O) | Minor | Rejected: project convention (`tools/author-translations.py`: technical messages use compact localized clauses); Italian is a full sentence |
| I1-8 | `hold_prepare` not released on Unknown; suspects beyond one per pass dropped; `gone` map not pruned; ClearCache drops `waiting` silently (S) | Minor/Note | Accepted as specified (plan §7, R14-8); bounded by queue size; ClearCache is user-initiated |
| I1-9 | Docs unfinished (O) | Minor | Fixed: `docs/architecture.md` section added; `docs/validation.md` updated with gate results |

## Implementation review, round 2 (focused on the fixes)

Reviewers:
- Sol (with the diff): **no Blocking**.
- Fresh Sonnet subagent: **no Blocking** (ran 42 targeted tests: passed).
- Third chain unavailable (exit 3). Recorded.

Both confirm that I1-1, I1-2 (forward path), I1-4, I1-5 and I1-6 are correctly
wired, with no URI or token in the new log line.

| # | Finding | Sev | Disposition |
| --- | --- | --- | --- |
| I2-1 | QueueRemove of the active item at slot 0 before a gone tail has no audible slot; after a restart, Previous lands on the gone item (O Major, S Minor) | Major | Accepted (recorded): bounded and never silent. The item fails, is re-checked, and the gone banner is shown. Representing "no audible slot" needs new Queue state; follow-up |
| I2-2 | The I1-1 test admitted the probe (401 arrives after admission), so it did not discriminate (O, S) | Minor | Fixed: checks refused via offline network facts (`may_probe` false). **Mutation-checked:** reverting to the conditional hold makes the test fail (successor fetched 4×); with the fix it passes |
| I2-3 | The I1-4 test was vacuous; I1-5 untested (O, S) | Minor | Fixed: the notice is raised, then a failing Play clears it; the epoch-reset unit test asserts the orphaned failure is handed over |
| I2-4 | `sync` could overwrite a pending orphan; a stale `waiting` is replaced silently (S) | Note | Accepted: practically unreachable; the replaced entry is no longer current |

**Status:** no Blocking finding remains in the implementation review. Not yet done:
phone validation (normally installed RPM under Sailjail).

## Phone validation of stage 1 (2026-10-19) and the radio gap

The 0.1.6-1 build was installed normally (pkcon) on the phone. Pressing play on
the Cannibal Corpse track: direct stream 404, transcode fallback 400, then the
explicit "Audio decoding or streaming failed" banner. No health check started,
because the persisted queue is an **artist station** (In Flames, playQueue 6225)
and stage 1 excludes radio by design (round 3). The 18:30 incident was on this
station too (`op=radio` precedes it in the 10-17/10-18 logs). Read-only checks
from the phone (token read in the phone shell, never printed): all 11 local
radio items answer 404 to `/library/metadata/{key}`; `/playQueues/6225` answers
200 with **zero items**. Library recreation emptied the server station, so
skipping alone cannot recover it: the refill has nothing to give.

User decision (2026-10-19): this is a corner case. A station that cannot fetch
more data may stop, or better behave as when offline, cycling through the
already downloaded radio tracks.

## Plan R1 — Stage 1b: radio (stale station)

Goal: on radio, a confirmed-gone track and a station that can no longer supply
tracks behave like the existing offline continuation
(`Core::advance_cached_radio`: cycle the radio window in playback order over
fully cached items, wrapping, including a sole cached track). When nothing is
cached, end with the gone banner. No new UI strings, no new persisted state.

### 1. Lift the radio exclusions (`src/lib.rs`)

- `may_probe`: drop `radio.is_none()`. Probes (current-track failures, cache
  suspects, failed gapless successors, no-Media 404) now also run on radio,
  under the same budget/slots/cooldown.
- `skippable`: drop `radio.is_none()` (gone and not cache-served).
- `playback_plan`: the gone guard and the no-Media 404 `request_probe` apply to
  radio as well.
- Epoch/marks: `start_radio` already goes through the `execute()` replacement
  guard (Radio/Station/StopRadio/Mix), so marks and checks reset with a new
  station.

### 2. A station that cannot supply tracks continues like offline (`advance_next`)

Today a refill error with `Network | Http(500..=599)` tries
`advance_cached_radio()` and plays from the cache; other errors are ignored
unless the queue is at its end. Add a predicate `station_unavailable(&Error)`:
the existing network/5xx cases, plus the cases that mean the source is gone:
- Station: `/playQueues/{id}` answers 404, or answers with no item after the
  center (`extend_radio` returns a distinct `Error::Input(RADIO_EXHAUSTED)`
  ("Radio returned no playable tracks", already translated) when the filtered
  list is empty, before `queue.append`).
- Sonic: `nearest(seed)` answers 404 (the seed was re-keyed/deleted).

Other errors (protocol, 401/403, 400) keep today's behaviour: ignored while
items remain ahead, reported at the end. No sticky "stalled" flag: every radio
advance tries one refill (one request per track, as today), so a station that
comes back resumes normally.

### 3. Gone current radio track (runtime, unchanged code paths)

With §1 the existing stage-1 path applies: all sources fail → Priority probe →
Gone → `peek_skip`/`skip` with `Next { automatic: false }` (no timeline
"stopped" report for the failed occurrence; the refill still runs). The refill
reaches §2: for the incident (empty station, some cached tracks) the skip lands
on the next cached radio item. For a healthy station with one deleted track, the
refill succeeds and the skip lands on the next radio item, hopping marked-gone
ones (`settle_forward`).

`peek_skip` models only `queue.advance`; on radio it may say `End`/`Ready`
where the real Next refills or cycles cached items. Both lead to `skip`, whose
result is authoritative (an empty plan shows the gone banner), so no radio
modelling is added. `NeedsMetadata` still reports instead of skipping (station
items normally carry Media).

### 4. Gone tail on radio with nothing cached

`settle_forward` ends at the audible slot with the notice, as in stage 1: the
station stops with "This track is no longer on the server". In the radio
`Event::End` arm the gone notice already produces the banner (it is read before
the `radio` error arm). Each not-yet-marked gone item still costs one failed
stream and one Priority probe; the 10/min budget bounds it, after which the
original error is shown. Accepted: corner case, never silent.

### 5. Speculative preparation

`prepare_next` runs `advance_next(true, false)` on a copy; with §2 it may
prepare a cached item when the refill says the station is unavailable. That is
the same as offline today, and the copy is discarded unless committed.

### 6. Tests

Core (`src/health_tests.rs`, radio fixtures with a local server):
- radio queue, current gone, station `/playQueues/{id}` empty, one later item
  cached → `Next{automatic:false}` lands on the cached item; no error.
- same, station 404 → cached item.
- same, nothing cached, all ahead gone-marked → plan `stream == ""`, notice.
- healthy refill (one new item) with the next item gone-marked → lands past it.
- refill 400 (not unavailable) with items ahead → today's behaviour (advances
  normally, no cache cycling).
- sonic radio, `nearest` 404 → cached item.
Runtime fixture (`src/runtime/fixture_tests.rs`): the incident on a station —
current 404/transcode 400, probe Gone, empty playQueue, a cached item → the
cached item plays, no error banner.
Existing radio offline-continuation tests must stay green.

### 7. Docs and gates

`docs/architecture.md` stale-queue section: radio paragraph replaces "Radio
sessions never use marks". `docs/validation.md`: phone result above and gates.
`./tools/check.sh`, `./tools/build-sailfish.sh`, then a phone run on the
installed station.

## Round R1 reviews (radio plan R1)

Reviewers:
- Sol (`--chain main`, with the diff).
- A fresh Sonnet subagent.
- Third chain: **Space Bunny** (`opencode/space-bunny-free`), available this time.

| # | Finding (reviewers) | Sev | Disposition |
| --- | --- | --- | --- |
| RR1-1 | An empty refill *delta* (every returned item already known) is routine on a healthy station; classifying it as unavailable jumps to cached history and breaks order and repeat-off (O B1, T M2, S F1) | Blocking | Valid. R2 drops error classification: cycle only when no playable successor exists |
| RR1-2 | `next(automatic)` sends the station "stopped" timeline first and aborts on any non-Network/5xx error, so a track end on a stale station never reaches the refill (T B1, S F2) | Blocking | Valid. R2 §3 |
| RR1-3 | The refill measurement omitted `center`; the classification signals are unmeasured (T B2) | Blocking | Moot in R2 (no classification). A retry from the phone failed (DNS on the phone); recorded |
| RR1-4 | A radio gone-tail `finish_at` leaves `current` None: the refill (center), `advance_cached_radio` and `at_end` all stop working (T B3, O M3, S F6) | Blocking | Valid. R2 §2: on radio the cached loop is tried before ending; an ended stale station stays ended (explicit) |
| RR1-5 | At end with nothing cached, the refill error is returned, not the gone banner; `Err` drops the notice (O M2, T M1, S F3) | Major | Valid. R2 §2 returns the gone outcome (Ok, empty plan, notice) when the current item is gone |
| RR1-6 | `peek_skip` does not model radio refill/cycling (O M4, S F4) | Major | Partly: R2 §4 |
| RR1-7 | Probe budget halts an all-gone chain mid-way (T M4, S F8, O 6) | Major | Accepted and documented (corner case, never silent); cached cycling makes it rare because the loop does not need probes |
| RR1-8 | No radio playback tests exist; the Core harness has no HTTP server (T M3) | Major | Valid. R2 §6: characterisation tests and a routing fixture first |
| RR1-9 | `RADIO_EXHAUSTED` shares text with a protocol error (T M5) | Major | Moot (no classification) |
| RR1-10 | Request-count claims, sonic gaps, repeat-one, Previous on radio, timeline/scrobble of the skipped occurrence, docs list (O 5, T m1-m6, S F5/F7) | Minor | Docs and tests in R2; behaviour accepted as listed in §5 |

## Plan R2 — Stage 1b: radio (stale station)

Principle: **decide from local facts, not from the server's refill error.**
On radio, if no playable successor is left, behave as offline (cycle fully
cached radio items); if nothing is cached either, end with the gone banner.

### 1. Lift the radio exclusions (`src/lib.rs`)
`may_probe`, `skippable`, the `playback_plan` gone guard and the no-Media 404
`request_probe` stop excluding radio. `start_radio`/StopRadio already go
through the `execute()` replacement guard, so marks reset with a new station
(tested).

### 2. Radio continuation in `advance_next` / `settle_forward`
The refill code and its `Network | 5xx → advance_cached_radio` path stay as they
are. A refill error is still ignored while the queue is not at its end. Change
only the outcome after the forward move:
- `settle_forward(audible)` on **radio**, when the hops find nothing playable
  ahead (the tail is gone, or the queue is at its end after a failed refill):
  re-select the `audible` slot, then `advance_cached_radio()` (existing:
  presentation order from the audible item, wrapping, cached items only;
  gone-but-cached items are playable). If it finds an item, play it, with no
  notice. Otherwise `finish_at(audible)` plus the notice (as stage 1).
- In `advance_next`, the `at_end` refill-error return becomes: if the current
  item is gone-marked (the skip case), do the same as above
  (`advance_cached_radio`, else the gone outcome) instead of returning the
  refill error. Otherwise keep today's `Err` (an unrelated end-of-station
  error stays visible as today).
- A healthy station never reaches this: its refill appends, or its successor
  is not gone (RR1-1).
- An ended stale station (`current` None) stays ended: Next from there runs
  today's logic and ends again. Accepted and documented: the user restarts
  the station. It is never silent and persists like stage 1.

### 3. Best-effort station completion report (`Core::next`)
The automatic "stopped" timeline report before advancing tolerates any error
except `Unauthorized`/`Http(401|403)` (an auth failure must still surface).
It is logged; "reporting must not prevent local continuation" (as the
existing comment says). Test: station, `cached_playback` false, timeline 404,
→ still advances.

### 4. Runtime decision on radio (`Playback::settle_failure`)
On radio, `Skip::End` and `Skip::Ready` both run the skip (as today);
`NeedsMetadata` still reports. The skip result is authoritative: an empty plan
shows the gone banner, and a cached item plays. No radio modelling is added to
`peek_skip`. Recorded: a successful refill may append an item without Media,
and executing Next then fetches its metadata synchronously, which the user
accepted for stage 1.

### 5. Accepted consequences (documented)
- Probe budget: an all-gone, nothing-cached radio window ends with the
  original error after about 10 failures per minute. With cached items, the
  cycle needs no probes.
- Previous on radio only steps back through history (no refill, no cycling).
- Repeat-one radio: automatic End skips the radio branch (unchanged); the
  failure skip uses `automatic:false`.
- The skipped occurrence gets no "stopped" report; the server station keeps
  pointing at it. Plays stay undelivered as today when reports fail.
- Each advance on a dead station still costs one refill request, plus one
  from speculative preparation, as today.
- Sonic radio: same local rule (no classification of `nearest`).

### 6. Tests
First, characterisation tests of today's radio behaviour (none exist):
offline cycling (wrap, sole cached track), and a refill error with items ahead
→ normal advance. Then a per-path routing HTTP fixture for Core
(`/playQueues/{id}`, `/:/timeline`, metadata), and:
- a healthy station with only already-known items in the refill and items
  ahead → normal advance, no cycling (RR1-1);
- a stale station: current gone, all ahead gone-marked, one cached item →
  Next lands on it; same when at end with the refill empty, 404 or 400;
- nothing cached → empty plan plus notice (singleton window and last slot);
- timeline 404 on automatic End → advances (RR1-2); timeline 401 → error;
- a new station clears the marks;
- repeat-all with cycling terminates.
Runtime fixture: the incident on a station (404, transcode 400, probe Gone,
empty refill, one cached item) → the cached item plays, with no banner;
nothing cached → gone banner.

### 7. Docs and gates
`docs/architecture.md`: the stale-queue section (radio paragraph) and
"Rolling radio window / offline continuation" (the new trigger).
`docs/research.md`: the empty station (playQueue 200, zero items) and the
pending center-request measurement. `docs/validation.md` and the
`docs/roadmap.md` Open items. Gates: `./tools/check.sh`,
`./tools/build-sailfish.sh`, then the phone.

## Round R2 reviews (radio plan R2)

Reviewers: Sol (main chain), a fresh Sonnet subagent, Space Bunny (third chain).
- **Sonnet:** no Blocking.
- **Sol:** no Blocking.
- **Space Bunny:** two Blocking.

| # | Finding (reviewers) | Sev | Disposition |
| --- | --- | --- | --- |
| RR2-1 | The continuation is specified on two paths; the at-end case returns `Err` before `settle_forward`, so one path is dead (T B1, S M1) | Blocking | Valid. R3 §2: one helper, one call site |
| RR2-2 | Reaching the cached loop requires every item ahead to be probed; the 10/min budget (3 reserved) stops the chain first, including with cache-ahead off or suspended. The §5 claim "no probes needed" was false (T B2, S M2, O 3) | Blocking | Valid. R3 §2: a failed refill plus a gone current item goes straight to the cached loop, without walking the window |
| RR2-3 | A natural End at the window end on a dead station (current not marked, e.g. an unmarked cached last item, or sonic radio) still stops with the raw error (S M1, O 1, T M1/M2) | Major | Valid. R3 §2: at the end, any non-auth refill failure tries the cached loop |
| RR2-4 | The new loop path ran `trim_history`, unlike offline cycling, pruning cached history in album windows (O 2) | Major | Valid. R3: cached-loop targets return early, like the offline path |
| RR2-5 | "Gone outcome" undefined; runtime fixture cannot reach the cached item as written; no request-count tests (T M3/M4/M5) | Major | Valid. R3 §2/§6 |
| RR2-6 | Ended station: Next from `current` None is a silent no-op, or wraps under repeat-all (S M4, O 4, T m2) | Minor | Valid. R3 §2: radio re-selects the audible slot first |
| RR2-7 | `Http(401|403)` never constructed (plex maps to `Unauthorized`); §3 too broad for local errors (T m7, S M7) | Minor | Valid. R3 §3 |
| RR2-8 | `physical_index` for re-selection; repeat-one unreachable on radio (`Repeat` rejected); cached→gone lookup race; `trim_history` after a backward cycle; extra lookups; cache-ahead cancelled on an all-gone window; prepared-successor and TranscodedPlayback changes unstated; "healthy path byte-for-byte" too strong (S M3/M5/M6, T m1/m3-m6/m8) | Minor | R3 §5 states/accepts; tests in §6 |

## Plan R3 — Stage 1b: radio (stale station)

Principle: decide from local facts. **Dead-station evidence** for one advance
on radio is *the refill attempt just failed with a non-auth error* and either:
- the current item is gone-marked (the failure skip), or
- the queue is at its end (no successor at all).

On that evidence, behave as offline: cycle the fully cached radio items. If
nothing is cached, take the gone outcome when the current item is gone, else
keep today's error.

### 1. Lift the radio exclusions (`src/lib.rs`)
These four stop excluding radio:
- `may_probe`;
- `skippable`;
- the `playback_plan` gone guard (so `TranscodedPlayback` on a gone radio item returns `GONE_TRACK` without trying);
- the no-Media 404 `request_probe`.

Consequences, stated and tested:
- Background probes now run on radio (cache suspects, failed gapless successors).
- Previous on radio steps back over gone history.
- `schedule_cache` cancels cache-ahead when the whole window is gone.

Healthy radio with no marks behaves exactly as today (characterisation test).

### 2. One radio continuation helper (`advance_next`)
In the radio branch of `advance_next`, before the refill:
- **Ended radio:** if `queue.track()` is None (an ended station, also after a
  reload), re-select `physical_index(queue.cursor())` (the audible slot kept by
  `finish_at`). The refill has a center again, and the rules below apply.
- **Remember the refill:** keep its result as `refill_failed: Option<Error>`.
  `Unauthorized` is never treated as evidence and keeps today's handling.

Then one helper `radio_fallback(&mut self, schedule) -> Option<Result<Value>>`,
called from exactly one place: right after the refill, replacing today's
`Network|5xx → advance_cached_radio` arm and the `at_end → return Err` arm:
- **Network/5xx:** unchanged. Try `advance_cached_radio()` and on success return
  `playback_with_cache` (early return, no `trim_history`, as today).
- **Other non-auth failure with dead-station evidence:**
  - Try `advance_cached_radio()` and on success return as above, with no notice.
  - Otherwise, if the current item is gone-marked: the **gone outcome**. That is
    `finish_at(cursor)` (the audible slot), `health.set_notice()`, then
    `Ok(playback_with_cache)` with `stream == ""`, the same as the stage-1
    `settle_forward` end. Ok, so `next()` keeps it and the runtime shows the
    gone banner.
  - Otherwise (at the end, current not gone, nothing cached): today's `Err`.
- **Otherwise** (the refill succeeded, or failed while items remain ahead and
  the current item is not gone): fall through to today's
  `advance` → `settle_forward` → `trim_history`. On radio, `settle_forward`'s
  exhausted-tail case also takes the gone outcome, as in stage 1.

So the incident's skip goes straight to a cached item, with no walk through
unmarked items and no extra probes. A healthy station's empty delta with a
healthy current item mid-window changes nothing (RR1-1). A healthy station
with one deleted track (current gone) and a failing refill jumps to the next
cached item in presentation order. With cache-ahead that is usually the next
item. Accepted.

### 3. Station completion report (`Core::next`)
Tolerate `Network`, `Http(_)`, `Protocol` and `ProtocolAt` (logged). Keep
`Unauthorized`, `Input` and `Io` fatal, as today.

### 4. Runtime (`Playback::settle_failure`)
- Unchanged: `Ready`/`End` both run the skip.
- `NeedsMetadata` reports. That's rare on radio: station items carry Media.
- The skip result is authoritative.

### 5. Accepted consequences (documented)
- **Nothing cached, all-gone window with a successful refill:** the skip walks
  unmarked items, and the probe budget can end it with the original error.
  Never silent.
- **Cached loop on a station that refills again:** after a backward cycle,
  `trim_history` trims less, so the window can grow until the next forward move.
  Bounded by `MAX_QUEUE`.
- **Cached entry evicted between lookups:** a gone guard then makes the skip
  report `GONE_TRACK`. This is the race recorded as R14-5.
- **Request cost:** each advance on a dead station still costs one refill
  request, plus one from speculative preparation (counted in tests).
- **The skipped occurrence gets no "stopped" report.**
- **Repeat cannot be set on radio**, so the repeat-one note is dropped.
- **Persisted repeat-all radio queues:** bounded by `items().len()` (tested).

### 6. Tests
Characterisation first (none exist today):
- offline radio cycling: wrap, sole cached track, no trim;
- a refill error with items ahead: normal advance;
- healthy radio with no marks: today's behaviour.

A per-path routing HTTP fixture for Core: settings saved with the fixture
`server_url`, routing `/playQueues/{id}`, `/:/timeline` and metadata, and
counting requests. Then:
- **Healthy station:**
  - only already-known items in the refill, items ahead → normal advance;
  - one new item → appended.
- **Dead station:** refill empty, 404 or 400; current gone; unmarked uncached items ahead; a cached item further ahead or behind:
  - the skip lands on the cached item;
  - no probe of the items ahead;
  - one refill request per advance.
- **At the end:**
  - a non-gone current item and a failed refill → cached item;
  - nothing cached → today's `Err`;
  - a gone current item with nothing cached → gone outcome (empty plan plus notice).
- **Unmarked cached last item finishes, refill fails** → cycling continues (two transitions).
- **Album radio window above 11 rows** with two cached history items: several wraps keep both. Also through `prepare_next` plus `commit_prepared`, and after a reload.
- **Shuffle on:** re-selection uses `physical_index`.
- **Ended station** (`finish_at`): Next re-selects the audible slot and then cycles or reports. Also after a reload.
- **Refill `Unauthorized` with a gone current item** → the auth error stays visible.
- **Timeline:**
  - 404, 5xx and `ProtocolAt` on automatic End → advances;
  - `Unauthorized` → error.
- **Sonic track radio, seed cached, `nearest` 404** → cycles the cached seed.
- **A new station clears the marks.**
- **A persisted repeat-all radio queue terminates.**

Runtime fixture, the incident on a station (404, transcode 400, probe Gone, empty refill, a cached item two slots ahead with an unmarked item between):
- the cached item plays, there's no banner, and the probe count is one;
- nothing cached → gone banner.

### 7. Docs and gates
- **`docs/architecture.md`:** the stale-queue radio paragraph and the "Rolling radio window / offline continuation" trigger.
- **`docs/research.md`:** the empty station (200, zero items); the center-request measurement is still pending (the phone was off the network).
- **`docs/validation.md`** and the **`docs/roadmap.md`** Open items.
- **Gates:** `./tools/check.sh`, `./tools/build-sailfish.sh`, then the phone.

## Round R3 reviews (radio plan R3)

Reviewers: Sol (main chain), a fresh Sonnet subagent, Space Bunny (third chain).

| Reviewer | Blocking | Major |
| --- | --- | --- |
| Sol | 0 | 3 |
| Sonnet | 1 | 4 |
| Space Bunny | 1 | 3 |

**Measurement (2026-10-19)**, read-only GETs from the desktop session against
the same server (station 6225 present and empty; old key 29116 → 404 with
`X-Plex-Protocol`). Token used in-process only, never printed:
- Dead station 6225: `/playQueues/6225` (bare, or with any `center`, known,
  stale or absurd) → 200, `playQueueID` 6225, **zero items**.
- Healthy station 6237: `center=<held item>&includeBefore=0&includeAfter=1` →
  200, 50 items starting *after* the center.
- Same healthy station, unknown center → 200, **zero items**.
- Unknown playQueue → 404 with `X-Plex-Protocol`.

| # | Finding (reviewers) | Sev | Disposition |
| --- | --- | --- | --- |
| RR3-1 | "Refill failed" includes the routine empty delta (`append([])` → `Input`), so "current gone + refill failed" ends or jumps a healthy station with one deleted track and live successors (S B1, T B1) | Blocking | Valid. R4 removes the "current gone" shortcut and makes the empty delta impossible on a healthy station (center on the window tail) |
| RR3-2 | The center is the failed item; after a deletion it is unknown to PMS (T M1) | Major | Valid and measured: an unknown center returns zero items. R4 centers on the window's last item |
| RR3-3 | `tests/radio.rs` already has 8 radio tests with a strict ordered fixture (T M2) | Major | Valid; R4 extends it (status field), no new fixture |
| RR3-4 | Auth disposition ambiguous mid-window vs at end; Network/5xx with no cache undefined; `Err` predicate must be literally `at_end` (O 1/2, S M2, T M3) | Major | R4 §2 grid |
| RR3-5 | The exhausted-tail gone outcome still trims history (O 3); `settle_forward`'s tail has no cached loop (S M1) | Major | Valid. R4 §2: on radio the tail tries the cache, and the gone outcome skips `trim_history` |
| RR3-6 | Behaviour changes on healthy stations stated as "unchanged" (S M3) | Major | R4 §5 lists them |
| RR3-7 | Re-select guards (empty queue, corrupt cursor); refill skipped means no evidence; `timeline` `Input` from `base`/`numeric`; `peek_skip` NeedsMetadata; repeat/shuffle defensive; budget/request wording; cache cancel; missing tests incl. a fixture item lacking `play_queue_item_id` passing for the wrong reason; runtime prepared-successor test (S M4/m1-m3, T m1-m10, O 4) | Minor | R4 §2/§5/§6 |

## Plan R4 — Stage 1b: radio (stale station)

Evidence is local or measured. No "current item is gone" shortcut.

### 1. Lift the radio exclusions
As R3 §1. That covers `may_probe`, `skippable`, the `playback_plan` gone guard
and the no-Media 404 `request_probe`, with the same stated consequences.

### 2. Station refill and continuation (`extend_radio`, `advance_next`, `settle_forward`)

**a. Center on the tail.** `extend_radio` for a station centers on the last
queue item (physical order of appended station items) that has a
`play_queue_item_id` and is not gone-marked, instead of the current item. It
still filters known IDs and takes one.
- Measured: a held center returns the items after it, so a healthy station
  always yields an unseen item. A dead station returns zero items.
- The routine empty delta disappears (RR3-1).
- The empty filtered list keeps `Error::Input("Radio returned no playable
  tracks")`; it now means "the station has nothing after our tail".
- Existing `tests/radio.rs` expectations on the `center` parameter are updated.
  The current item equals the tail only when nothing is ahead.

**b. One continuation rule** after the refill attempt, replacing today's
`Network|5xx` arm and its `at_end → Err` arm:

| Refill | Items ahead (unmarked or not gone) | Result |
| --- | --- | --- |
| not attempted / ok | any | today: `advance` → `settle_forward` → `trim_history` |
| `Unauthorized` | yes | today: ignored, advance (visible on the next failure) |
| `Unauthorized` | no (`at_end`) | `Err(Unauthorized)` (today) |
| other error (Network, 5xx, empty, 404, protocol, sonic `nearest` failure) | yes | today: advance normally (the successor is tried; RR3-1) — **except** Network/5xx keep today's `advance_cached_radio` first |
| other error | no (`at_end`) | `advance_cached_radio()`; else **gone outcome** if the current item is gone, else `Err(error)` (today) |

The "except" row keeps today's offline behaviour byte for byte.

**c. Exhausted tail on radio.** In `settle_forward`, when every hop found a
gone item, re-select the audible slot (`physical_index(audible)`) and try
`advance_cached_radio()`. If it finds an item, play it with no notice.
Otherwise take the gone outcome.

**d. The gone outcome** (one helper):
- `finish_at(audible)` and `set_notice()`;
- then `Ok(playback_with_cache)` with `stream == ""`, returned early, with no
  `trim_history`.

A cached-loop target also returns early, with no trim, as the offline path does.

**e. Ended radio.** If `queue.track()` is None on radio, the queue is
non-empty, and `physical_index(min(cursor, len-1))` is valid: re-select it
before the radio branch. If not, keep today's behaviour.

The incident: the skip from a gone current (`Next{automatic:false}`) refills
centered on the window's last item, and the station returns zero items. Items
are still ahead, so the next item is tried. Each dead uncached item costs one
failed stream and a check, the hops pass the marked ones, and at the tail
(c) the cached loop starts. **Accepted (user, stage 1):** an all-dead window
with cache-ahead off can hit the 10/min check budget first, ending with the
original error. With cache-ahead on, suspects mark items ahead in the
background.

### 3. Station completion report (`Core::next`)
Only `Unauthorized` stays fatal. Any other error is logged and tolerated:
reporting must not prevent continuation (RR3-7, T m3).

### 4. Runtime
Unchanged from stage 1 (`Ready`/`End` skip; `NeedsMetadata` reports, rare on
radio; recorded).

### 5. Behaviour changes, stated
- **Healthy stations:** the refill center moves to the tail. Same items, the
  requests differ.
- **A healthy station that runs out at the end** (refill empty or failing):
  previously "Radio returned no playable tracks". Now it cycles cached items
  when it has any.
- **Next on an ended station** re-selects the last audible item.
- **Previous on radio** steps back over gone history.
- **Background probes** run on radio.
- **Cache-ahead** is cancelled for an all-gone window.
- **Repeat** cannot be set on radio, so repeat and shuffle tests are defensive.
- **The skipped occurrence** gets no "stopped" report.
- **Request count per advance on a dead station:**
  - one refill from the Next;
  - one refill from `prepare_next`;
  - up to one check per failed item;
  - `prepare()` also runs after queue edits.

### 6. Tests (extend `tests/radio.rs`, with a `status` field on `Response`)
All 8 existing tests are kept (updated for the tail center). Each case below
uses items with a `play_queue_item_id`:
- **Healthy station:**
  - the refill centers on the tail and appends one item;
  - one deleted current item with live successors → the next item plays (RR3-1);
  - a refill 500 mid-window → normal advance.
- **Dead station (refill empty or 404):**
  - mid-window → the next item is tried;
  - at the end with a cached item → cycles;
  - nothing cached and current gone → gone outcome;
  - nothing cached and current healthy → `Err` (today).
- **Exhausted tail** (all ahead gone-marked) with cached history → cycles. With
  an album window over 11 rows, several cycles keep the history, also after
  `prepare_next`/`commit_prepared` and a reload.
- **Unauthorized:** mid-window → advance; at the end → `Err(Unauthorized)`.
- **Ended station:** Next → re-select and continue. An empty queue and a
  corrupt cursor → today's behaviour.
- **Timeline:** 404, 5xx, `ProtocolAt` and `Input` → advance;
  `Unauthorized` → `Err`.
- **Sonic track radio:** seed cached and `nearest` 404 → cycles the seed.
- **A new station clears marks.**
- **A cache-served gone radio item plays.**

Runtime fixture (items with `play_queue_item_id`, counting `/playQueues`):
- **The incident:** 404, transcode 400, probe Gone, refill zero items, cached
  item at the tail → reached; the check and refill counts are asserted; no
  banner.
- **Nothing cached** → gone banner.
- **A failed prepared radio successor** → Gone → the successor commits once,
  with no listen credit for the skipped item.

### 7. Docs and gates
- `docs/research.md`: the measurements above.
- `docs/architecture.md`: the radio paragraph and the offline-continuation
  trigger.
- `docs/validation.md`, plus the `docs/roadmap.md` Open items.
- Then `./tools/check.sh`, `./tools/build-sailfish.sh` and the phone.

## Round R4 reviews (radio plan R4)

Reviewers:
- Sol (main chain): **no Blocking**.
- A fresh Sonnet subagent: **no Blocking**.
- Third chain unavailable (exit 3). Recorded.

**Measurement (2026-10-19, same method).** Healthy station 6237:
- Centering on the last item of a full listing (`431179`) → 200 with 50 items
  after it.
- Centering on the item before it → 50 items, starting at the last one.
- The total stayed 446, so these GETs did not grow the queue.

The tail center works on a healthy station. The case of a deleted-but-held
center is still unmeasured; R4.1 avoids depending on it.

| # | Finding (reviewers) | Sev | Disposition |
| --- | --- | --- | --- |
| RR4-1 | The tail center is riskier than the current center; a deleted unplayed tail could empty refills (S M1) | Major | R4.1: current center as today when the current item is not gone; the tail only when it is |
| RR4-2 | No eligible center when everything is marked (O 1, S m6) | Major | R4.1: fall back to the last held ID (one bounded attempt); `ProtocolAt` → "other error" row |
| RR4-3 | "Items ahead" undefined; `Unauthorized` with an all-gone window (S M2) | Major | R4.1: literally `queue.at_end()`; an all-gone window is handled by `settle_forward` (auth swallowed there, as today mid-window), tested |
| RR4-4 | Network/5xx at end changes too (gone outcome); timeline `Protocol` (S M3/M4) | Major/Minor | Stated; the timeline tolerates everything except `Unauthorized`; test for an item without `play_queue_item_id` |
| RR4-5 | 2c re-select can fail; the audible item can replay as the sole cached item (S m1/m2) | Minor | Re-select failure → gone outcome. Replay documented (offline-consistent; a cache failure banners, no loop) |
| RR4-6 | Tests: Network/5xx cache-first cases; internal harness for marks and prepared commits; budget exhaustion; Previous on radio; persistence; the exact center (O 2-4, S tests) | Minor | Adopted in R4.1 §6 |
| RR4-7 | Refill each advance while cycling (S m5) | Minor | Accepted (bounded, §5) |

## Plan R4.1 (R4 with the round R4 amendments) — implementation basis

**§2a center.**
- Current item not gone, with an ID: center on it (today's request, unchanged).
- Otherwise: the last queue item that is not gone and has an ID.
- Otherwise: the last item with an ID (one attempt).
- Otherwise: `ProtocolAt` → the "other error" row.

**§2b grid.** "Items ahead" means `!queue.at_end()`, literally. The at-end row:
- **Other errors and Network/5xx:** `advance_cached_radio()`. If that fails:
  gone outcome if the current item is gone, else `Err(error)`.
- **`Unauthorized`:** `Err` as today.

Mid-window, Network/5xx keep today's cache-first rule. Others advance.

**§2c.** A re-select failure takes the gone outcome.

**§3.** Only `Unauthorized` stays fatal for the completion report.

**§6 placement.**
- Station protocol and center cases: `tests/radio.rs`, with a `status` field on
  `Response`.
- Marks, gone tail, cache-served gone and prepared commit cases: internal
  module `src/radio_health_tests.rs`, reusing a small in-crate routing fixture.
- Runtime: `src/runtime/fixture_tests.rs`, including a budget-exhaustion case
  (original error, bounded counts, no automatic restart).

## Implementation review, stage 1b (radio), round 1

Reviewers:
- **Sol** (with the diff): no Blocking.
- **Fresh Sonnet subagent:** no Blocking. It ran the 13 new lib tests and the 8
  `tests/radio.rs` tests, all passing.
- **Third chain:** unavailable (exit 3). Recorded.

| # | Finding (reviewers) | Sev | Disposition |
| --- | --- | --- | --- |
| RI1-1 | A forward move during cycling that reaches another cached item (not the exhausted tail) still ran `trim_history` and dropped cached history; on a long window the refill is not even attempted (O 1) | Major | Fixed: no trim after a failed refill or a move that stepped over gone items. Test `cycling_a_dead_station_keeps_every_cached_item_of_a_long_window` (20 rows, cached 1 and 10, four cycles); it failed (11 rows) before the fix |
| RI1-2 | Missing tests (S 1, O 2): Network/5xx cache-first, mid-window Unauthorized, the timeline outcomes, center fallbacks, a runtime case before the stale tail, refill upper bound | Major | Added: `a_network_failure_keeps_the_offline_cache_first_rule`, `only_a_rejected_session_stops_at_the_completion_report` (500/404/400 advance, 401 stops, no refill), `the_refill_center_falls_back_to_held_items` (102, then 103 once), mid-window 401 advances, `a_dead_station_found_by_pre_loading_cycles_its_cached_track` (runtime, 2.5 s tracks, gone successors never shown, at most one check each), refill count bounded in the incident replay |
| RI1-3 | `a_rejected_session_stays_visible` passed for the wrong reason: 401 on every path (S 2) | Minor | Fixed: the timeline 401 is now tested with a station that answers 200 |
| RI1-4 | Offline Next on an ended station now cycles instead of failing; the window can grow when gone items keep being appended; the cursor after the at-end gone end rests on the gone item (S 3/4/5) | Minor | Documented in `docs/architecture.md` |
| RI1-5 | Doc omissions: the last-held-ID fallback; the Network/5xx exception (O 3) | Minor | Fixed |
| RI1-6 | Not added (recorded): a runtime budget-exhaustion case (budget and slots are unit-tested in `recovery.rs`, and the stage-1 refused-check runtime test covers the original-error path); listen credit for a skipped prepared radio occurrence; a new station clearing marks (shared replacement guard, tested for Play); an ended station with an empty queue or a corrupt cursor (guarded by `physical_index`) (S 1, O 2) | Minor | Accepted gaps, listed in the roadmap |
| RI1-7 | Gone-but-cached sole item repeats; `NeedsMetadata` on a radio tail banners; Unauthorized with an all-gone window shows as gone (S 6-8) | Note | As planned (RR4-5, R4 §4, RR4-3) |

**Mutation checks** (each reverted afterwards):
- disabling the at-end cached cycle → 3 tests fail;
- disabling the exhausted-tail cycle → 1 fails;
- centering on a gone current item → 2 fail.

**Pre-existing flaky test (recorded):**
`audio::http::tests::unreachable_source_reports_error_after_bounded_retries`
binds a free port, releases it, then expects connections to it to be refused.
A test binding in parallel can take the port. It failed once in a full run,
then passed 3/3 alone and in the full gate. Not caused by stage 1b.
