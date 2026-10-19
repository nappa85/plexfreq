# Development roadmap

## Open items (reviewed 2026-10-16)

This list is authoritative. The dated entries below are history: their "Remaining"
and "Next" notes predate this review, and items marked done here are finished.

Done since those notes: deadline/cancellation-bound ConnMan and NetworkManager
calls with stalled-service shutdown fixtures; navigation cancels in-flight view
HTTP; decoding runs on dedicated decoder threads (Symphonia), with u128
frame-to-time arithmetic; `crate::mutex_lock` replaces the poison-lock idiom;
`GroupId`/`daily::pinned_keys` consolidate download-group bookkeeping; Harbour
validation runs in CI and release; the Linux desktop package is built in CI/release;
hosted CI runs.

Partly done:
- Worker panics: a panicking request is answered with an error. Still open:
  publish the failure and rebuild validated Core state instead of continuing with a
  possibly partially mutated Core.
- Listen checkpoints: per-occurrence sequence numbers stop stale checkpoints from
  rewinding the saved position. Still open: the overflow keeps only the latest 16
  checkpoints, so durable occurrence-aware checkpoints are still to do.
- Cache I/O: directory scans and file writes run in the blocking pool. Still open:
  transactional eviction on partial failure, bounded/coalesced download batches, a
  recovery/reclamation policy for quarantined unindexed media, and filesystem work
  outside the shared cache lock.

Engineering:
- Mobile streaming robustness (2026-10-19 stutter on mobile data):
  - **Part A, implemented:** cellular network fact, mobile quality only on
    cellular, 320 kbps default for new installs.
  - **Part B step E1, implemented:** engine-held stream failures with the
    actor-owned intent, plus the transcode restart. Plan rounds E1-E1.4, all
    recorded. The implementation review took 2 rounds plus a narrow
    confirmation: no Blocking left. Pending: a phone check on mobile data.
    Recorded test gap: worker-level tests for late outcomes and queue-full
    retry. Follow-up: the HTTP reader should hand over buffered bytes before
    reporting a broken transfer.
  - **Part B steps E2/E3, implemented:** the successor pre-load waits for a
    healthy current stream (1 MiB window until promoted), a single resume
    gate (500 ms start, 5 s after a PulseAudio underflow, a one-shot trigger
    below prebuf, the crossfade dropped when the successor starves), and
    downloads yield to a starving stream (resumable originals drop their
    connection, other downloads finish their item). Plan rounds E2-E3.3
    recorded in `docs/reviews/2026-10-19-mobile-streaming.md`. The
    implementation review took 1 round plus a narrow confirmation: no
    Blocking or Major left. Pending: a phone check on mobile data, and
    showing `held` in the download UI.
  - Also open: a truncated transcode that ends cleanly is treated as a track
    end (B2-10).
- Stale queue after a server library re-key (2026-10-18 incident). **Stage 1
  implemented and reviewed** (14 plan rounds + 2 implementation rounds, no Blocking
  left): failure-triggered background checks, 10 min gone marks, skipping in the
  direction of travel, and HTTP status diagnostics. See
  `docs/reviews/2026-10-18-stale-queue-recovery.md`.
  - Phone validation of stage 1 (2026-10-19, installed 0.1.6-1): the HTTP status
    logging works; the skip did not apply because the queue was an artist
    station, which stage 1 excluded.
  - **Stage 1b (radio)**: implemented. Plan reviewed in rounds R1–R4 (no Blocking
    left in R4; third chain unavailable in R4). A station that can no longer
    supply tracks cycles its cached radio items like offline, and ends with the
    gone notice when nothing is cached. Implementation review round 1: no
    Blocking (Sol, Sonnet; third chain unavailable); its findings are fixed or
    recorded. Pending: phone validation on the stale In Flames station.
    Accepted test gaps (RI1-6): runtime budget exhaustion, listen credit of a
    skipped prepared radio item, a new station clearing marks, ended station
    with an empty queue or a corrupt cursor. The center for a single deleted
    track on a healthy station is unmeasured.
  - Follow-ups:
    - stage 2: re-resolve gone items to their new key (guid/search);
    - proactive upcoming checks when cache-ahead is off;
    - background detail fetch for items without Media (the sync fetch remains);
    - cache-ahead retry policy;
    - shutdown checkpoint ordering (R9);
    - visual marking of gone items;
    - QueueRemove-at-slot-0 audible cursor (I2-1);
    - untested races: post-`execute()` intent change, a queued Next at verdict
      time, a burst through the worker.
- Replace token-bearing artwork DTOs with a Rust-owned credential handle; DTOs must
  not reach persistence or logs meanwhile.
- Credential/server-scoped station download group IDs.
- Observable lyric/snapshot quota failures; bounded indexed search/top-page work.
- Consolidate time/digest/storage helpers, PCM byte conversion and page-size policy.
- Gapless queue identity: keep only active/prepared occurrences, update the shown
  identity at the audible Transition, prove one UI update per transition over long
  queues.
- Isolate the Bluetooth fixture from live host PulseAudio state.

Device validation (normally installed RPM under Sailjail):
- Symphonia/libpulse engine: steady car playback with server flow control (0.1.6),
  resume after Bluetooth reconnect and sink moves, x-maemo routing/volume keys,
  call/alarm cork requests, long lock-screen sessions, CPU/battery of decoding plus
  resampling, Opus/WMA libraries via transcode, iTunes AAC gapless.
- Connectivity loss and recovery while streaming (progressive transcodes need their
  own restart policy); live smart-playlist mutations.

Features (before calling this a complete Plexamp clone):
- Guest DJ, Sonic Sage, broader sonic-radio coverage/weighting.
- Plex Home managed users/PIN switching, multi-server queue identity, a secure
  platform credential store and explicit token revocation.
- Automatic complete offline library sync, richer download management, offline
  play-history sync.
- Complex Boolean smart-playlist rule editing; richer personalized hubs/mixes.
- Throughput-adaptive streaming; release signing; native translation and
  accessibility review.

## History

GStreamer removal (2026-10-16): Harbour validation now passes. Decoding is Symphonia
(MP3/FLAC/AAC/ALAC/Vorbis/PCM) with MP4 gapless trimming and rubato resampling;
output is a libpulse stream. Unsupported/unknown formats stream as server transcodes
and original-quality downloads of them are 320 kbps MP3. Next: install the RPM on the
phone and validate x-maemo routing/volume keys, call/alarm cork requests, Bluetooth
reconnect, lock-screen sessions, CPU/battery of decoding+resampling, and Opus/WMA
libraries via transcode.

Car reconnect follow-up (2026-10-15): installed 0.1.3-2 logs show zero playback
time after the 08:45 reconnect, stale occurrence while prepared audio continues,
then silence after its exhaustion at 08:51. Rust output position tracking now
retains failed-query position and rebases large backwards clock jumps; synthetic
reset regression and 200-test/full host/desktop gate pass. Raw query diagnostics
were added because the old trace cannot distinguish clock reset from query failure.
Next: normally installed reconnect/long gapless retest; persistent failed queries
would need a separately validated recovery path. Packaging does not prove a cure.
GNU/Qt5.6 RPM 0.1.3-3 built and copied to phone Downloads with matching checksum;
normal installation/restart and user retest are pending.

Resume delivery/display follow-up (2026-10-13): the user's restart test still
ran a different 0.1.3-1 executable than the delivered fix, confirmed by file digests.
Package release is now 0.1.3-2 for an unambiguous upgrade. Rust also projects saved
position/length to Qt before a decoder loads, fixing startup 0:00/0:00. The extended
native seek/pause/close/idle/reopen/resume fixture and full host gate pass, as does
GNU/Qt5.6 packaging. 0.1.3-2 is in Downloads. Next: normal installation, confirm
installed package/file identity, then repeat the user's startup and resume check.

Resume-restart fix (2026-10-13): installed log confirms the 40-minute track's
21:06 position became zero across decoder-free restarts. Rust checkpoints now
reject the no-audio occurrence sentinel instead of saving the empty startup
snapshot over durable playback. Regression fails before/passes after the fix;
unfiltered 199-test host/desktop gate passes with an isolated host runtime,
and GNU/Qt5.6 RPM packaging passes. Fixed replacement is in phone Downloads.
Next: install normally, reopen and remain idle for more than five seconds, then
verify Play resumes the saved position; package build is not that runtime check.
The Bluetooth fixture's dependency on live host PulseAudio state remains a test
isolation follow-up; default-environment failures and actual checks are recorded
in validation.md.

Production-defect review (2026-10-13): all ten supplied regressions now pass.
Finite ratings, safe/noneditable malformed smart limits, private corrupt-state
recovery, dangling queue selection, natural-end trimming, zero-filled crossfade
tails, transparent float peaks and bare-credential/control sanitization are fixed.
Cache quota accounting also excludes symlink targets. Host Rust and desktop gates
pass after translation regeneration; actual intermediate failures are in validation.md.

Remaining review-reported work, requiring focused failure fixtures and contract review:

- Request responsiveness/durability: cancellable generation-aware listing HTTP off
  the Core actor, and durable occurrence-aware listen checkpoints rather than lossy
  overflow. Worker panic handling must publish failure and rebuild validated state;
  catching an unwind and blindly continuing a partially mutated Core is insufficient.
- Gapless plan lifetime: keep only active/prepared occurrences, update shown identity
  at audible Transition, and prove one UI update per transition over long queues.
- Cache ownership: move blocking filesystem work off the current-thread async loop
  and shared mutex, make eviction manifest commits transactional on partial failure,
  bound/coalesce download batches, and define verified completion for unknown-length
  EOF. Quarantined unindexed media requires an explicit recovery/reclamation policy.
- Platform lifetime: deadline/cancellation-bound ConnMan and NetworkManager calls,
  with stalled-service shutdown fixtures matching the existing BlueZ strategy.
- Audio scheduling: wide frame-to-nanosecond arithmetic, successor-aware tail release,
  replacement-load coalescing, decoder preparation off the actor and small capture
  projections outside its lock. Develop long-duration and saturation fixtures first.
- Offline/catalogue identity and bounds: credential/server-scoped station group IDs,
  observable lyric/snapshot quota failures and bounded indexed search/top-page work.
- Consolidation: shared time/digest/storage helpers, PCM byte conversion and page-size
  policy. Token-bearing artwork DTO removal needs a Rust-owned credential-handle
  contract; DTOs must not enter persistence/logging in the meantime. Downstream DSP
  limiting/headroom policy needs audible validation beyond transparent float tests.

Buffered-stream outage fix (2026-10-13): the 08:16 installed log confirms fatal
HTTP retry exhaustion while audio remained buffered. Resumable originals now keep
queued data and retry transient failures with cancellable capped backoff rather
than discarding the pipeline after three failures. Loopback byte continuity and
Stop regressions pass, as do the 198-test host gate/desktop CTest and GNU/Qt5.6 RPM
build. Next: normally installed phone validation through actual connectivity loss
and recovery; progressive transcodes still require their separate restart policy.

Production-logic review follow-up (2026-10-12): fixed the ten supplied queue,
lyrics, PIN/flag/gain, station-key and cache-entry regressions. Added Core-level
same-row/shuffle coverage and strengthened cache lookup/reopen and timestamp/path
boundaries. Focused 24-test run passes; full gate results are in validation.md.
Remaining review work: reliable admission/retry of Bluetooth/recording pause and
structural audio controls under saturation, pad-link failure diagnostics and typed
fallback causes. Develop failure fixtures before changing actor/transport policy.
Output appsrc already defaults to nonblocking per upstream documentation.
Assess shared HTTP normalization/auth/encoding helpers, digest utilities, queue
cursor helpers, FFI/QML adapters and numeric-parser consolidation independently;
preserve optional heterogeneous metadata tolerance and document required-field
contracts before replacing defaults. Time/MIME/locale dependency replacements
and storage-scan error reporting remain cleanup candidates, not verified regressions.

Network-delay follow-up (2026-10-12): installed 0.1.1 logs confirm a 15-second
audio-worker delay after fast cached track planning during remote failures.
Current input backpressure is already nonblocking; terminal cancellation now
rejects late seeks and releases appsrc consumers with EOS. Viewport-level live
busy/buffering indicators cover Sailfish Library/detail/Now playing and desktop.
Outgoing-page restoration is suppressed during forward departure; Rust request
timings now cover listing work. The 185-test host gate, desktop CTest and Qt5.6
production/fixture SDK builds pass. Next: normally installed phone retest of
remote-to-cache replacement, recent-artist navigation and spinner visibility;
the native early-reply fixture is compiled but not run, and no stack establishes
the installed stall's exact blocking call. Core metadata/reporting concurrency
remains a separate responsiveness improvement.

Production review hardening (2026-10-12): the five supplied regressions pass.
Request-generation admission, volume coalescing, dynamic-pad linking, cancellable
HTTP input backpressure, bounded JSON, explicit corrupt-cache failure, bounded
transient retry, amortized quota accounting and deferred LRU persistence are
implemented. Shared page envelopes/kind codes/MIME policy, tuple protection keys,
strict required PIN values and smart-filter parse guards address targeted cleanup.
Final host gate passes 174 Rust tests, 39 locales and desktop CTest 2/2; Sailfish
GNU/Qt5.6 RPM packaging passes.
Remaining: consolidate download-group bookkeeping/storage scans/poison-lock idioms,
move mutation/status filesystem I/O off shared cache locks, replace token-bearing
DTOs with credential handles, and improve optional-data diagnostics without rejecting
valid heterogeneous Plex pages. Physical/device validation remains separate from
the local fixture, host gate and SDK package results in validation.md.


Rolling radio / offline continuation (2026-10-12): station radio now adds one fresh
occurrence per advance and trims old history to an eleven-row rolling window.
Track radio replenishes each advance; album radio retains whole-album ordering.
Disconnected radio skips uncached entries and cycles completed audio in its
retained window, including after restart. Speculative refill and completion-report
network failures no longer prevent usable local continuation. Local rolling/cache
regressions, the 161-test/full desktop gate and Sailfish RPM build pass. Next:
normally installed phone validation through actual signal loss and reconnection,
and long-running live station listening; packaging alone is not that validation.

CI recording-release follow-up (2026-10-12): hosted run 38225181884 exposes the
remaining decoder-free fixture preroll race on PLAYING resume. Actual capture-sink
delivery now forces exhaustion; fresh held PCM verifies PAUSED/no delivery and
PLAYING/delivery after release. The unfixed deterministic case fails locally;
fixed test passes 100 times each on Gentoo and Ubuntu/GStreamer 1.24.2, with the
full host gate passing 159 Rust tests and CTest 2/2. Hosted rerun awaits publication.

Release distribution (2026-10-12): replace the rootless phone archive with a Linux
amd64 desktop tar.gz, alongside the Sailfish aarch64 RPM. Desktop packaging uses
CPack TGZ and an extracted-binary CI smoke gate, with host Qt6/audio dependencies. Sailfish
integration/permission checks require the normally installed Sailjail app. Actual
local packaging checks and hosted execution scope are recorded in validation.md.

CI follow-up (2026-10-12): regenerated the 39 compiled translation catalogues
after the v0.1.1 source bump and repaired recording-fixture preroll starvation.
Full local gate passes (159 Rust tests, translations, desktop CTest 2/2), as do
20 focused repetitions. Hosted CI verification remains pending publication.

Recording/car/shutdown follow-up (2026-10-09): implemented native policy pause plus
read-only libpulse recording observation. Recording now temporarily holds output,
automatically resumes retained intent afterward, and honors explicit pause/disconnect
during the hold. Audio-transport removal pauses even if another Bluetooth profile
keeps the device connected; idle alone remains distinct from disconnect. Fixed the
structural bounded-signal-queue/RPC hang path by continuously draining Bluetooth
signals during deadline-bound queries and cleanup. Shutdown-stage diagnostics identify
unfinished workers. Final 159-test/full desktop gate, SDK app/fixture builds, phone
generated recording/resume/override test, and production smoke pass. Behavioral RPM is
installed; user and retained logs confirm messenger pause/resume and clean ~102 ms
shutdown/reopen. Next: physical car retest and retained long-lived/high-traffic traces.
Today's five logged whole-device disconnects did pause/cork; the reported sustained
phone playback and historical hang's exact stack remain unproven. No service reset.
Final diagnostics-only refinement prevents intentional recording pauses from being
classified as stalls despite retained play intent. Extended regression, fmt/Clippy,
43 library tests and SDK production build pass; refreshed RPM is in Downloads for
installation. The behavioral build validated above is already installed and running.

Native diagnostic patch (2026-10-07): pactl subprocess dependency is replaced by
Rust/libpulse introspection with two-second connection/query bounds, shutdown
cancellation, 32-entry caps and technical-only summaries. Local regressions,
153-test/full desktop gate, final SDK app/fixture builds, phone read-only probe,
temporary-HOME production logging smoke and generated real-PulseAudio transport
pass. Updated RPM is in Downloads. Next: install/restart normally and verify
`PulseAudio backend=libpulse` plus route snapshots in the retained dated log under
Sailjail, then retain a connected stutter snapshot before user-led recovery.
The temporary probe cannot substitute for that installed launch (Exec mismatch).
No playback recovery behavior or Bluetooth-service reset is introduced.

Afternoon car-stop investigation (2026-10-07): current installed diagnostics-v2
records correlate first-stop resume at 15:46 with healthy cached decoding/output
progress, then second-stop BLE failures and Bluetooth restart at 16:19:12.
ElectricEel also restarted and subsequently resolved GATT/authenticated; PlexFreq
resumed its retained occurrence. Shared Bluetooth-stack/car trouble is favored,
but the failing component and any cross-app interference remain unproven.
Installed sandbox inspection confirms pactl is absent from the app namespace,
so historical PulseAudio snapshots are missing despite passing rootless smoke.
Native replacement is now implemented above under existing Audio permission;
next verify through a normally installed launch and retain
connected incident route/cork/latency evidence before user-led recovery. No audio
policy workaround or service-reset behavior was added on this evidence.

Multi-day diagnostics (2026-10-07): current-local-date rotation is implemented in
a Rust stdout/stderr capture worker, independent of GUI timers. Logs have local
timestamps/UTC offsets, monotonic elapsed time and PID, URL/token redaction, seven
retained dates and bounded active/backup files. Controls, pipeline warnings/states,
clock/latency, HTTP status/bytes, PCM queues and stall/recovery now accompany periodic
read-only Bluetooth/PulseAudio/load snapshots. All 150 Rust tests, desktop gates,
Sailfish production/fixture builds, phone logging smoke and synthetic real-PulseAudio
transport pass. New RPM is in Downloads. Next: install/restart the app, verify optional
PulseAudio snapshot access under normal Sailjail launch, then inspect the retained
records after a real recurrence. No extra diagnostic build should be needed merely
to distinguish decoder starvation, output backlog, stalled position and routing/cork.

Car-resume investigation (2026-10-07): user confirms yesterday's RPM installed;
severe morning stutter and another Bluetooth application's failure cleared after
Bluetooth restart. Read-only phone inspection finds an installed overnight app,
no recorded audio/HTTP/clock-loss errors in its Oct 6 startup-date log, and no
currently connected Bluetooth audio transport. Shared-stack trouble is favored,
but the failing layer is unproven and journals are inaccessible. Next: capture
PulseAudio stream route/cork/latency, BlueZ transport state and track-position
progress during a recurrence before recovery. Overnight logging currently keeps
the startup-date file rather than rotating at midnight.

Car-trip follow-up (2026-10-06): reproduced/fixed full-output pause/resume deadlock;
added output clock-loss recovery, larger bounded input/PCM headroom, catch-up batches
and validated original-stream retries. Root logo now supplies PNG launcher/window
icons. All 144 Rust tests, desktop gates, Sailfish package/fixture builds, phone
synthetic real-PulseAudio transport and private fallback-log smoke pass. New RPM is
in the phone's Downloads, awaiting user installation. Next: repeat a car stop/resume,
notification overlap and sustained trip; inspect retained diagnostics if playback
fails again. Post-trip cache had all 15 saved queue tracks, so cache exhaustion is
not established as the cause of the reported stop.

Sailfish builds now retain private diagnostics in one dated
`~/Documents/PlexFreq/plexfreq-YYYY-MM-DD.log` file per day, including underlying
GStreamer decoder/output errors. The previously failing cached FLAC is structurally
complete and decodes on the host; installed-phone reproduction remains next.

Automatic artist-station continuation now reports each naturally completed server
occurrence with `continuing=1` before successor preparation/end-of-window refill.
Strict local protocol, full host gates and Qt5/Sailfish builds pass; terminal refill
errors are visible instead of silently stopping. Real long-session Plex/device
confirmation remains.

Library now displays its first artist page immediately and fills the remaining
artists through automatic background page requests. Alphabet shortcuts wait for an
unloaded target while that fill proceeds. Discover recent-grid hosts clip their cards
and reserve bottom spacing before the next section. Host and Qt5 build gates pass;
real-device timing/visual confirmation remains user-led.

Discover home now renders the server's Recently added and Recently played hubs as
card grids while preserving all other hub rows, section headings and Show all links.
The verified PMS identifiers use numeric section suffixes. Library alphabet taps
position the complete artist grid reliably, and the highlighted letter follows
forward/reverse scrolling. Host, SDK production and Qt5 fixture builds pass; phone
runtime/gesture confirmation remains user-led.

Library title-sorted artist browse fills all artists through bounded backend pages,
and alphabet shortcuts position it locally without a replacement request. Recently
added and recently played use the same card grid on both shells, with retained scroll
position during incremental paging. Full host checks pass; physical-phone visual/
gesture confirmation remains user-led.

Show all and Read more/less now use shared lightweight text links. Italian phone
launch corrected by reading the configured locale when SSH omits language variables;
actual fallback test and production startup pass. Latest app deployed/raised on phone
(PID/MPRIS 18911). Separate detail pixel check remains limited by window exposure.

Discovery header overlap corrected: title margin removed, title bounded before
the button, and shared action shortened to Show all / Mostra tutto. Desktop/SDK
checks and phone fixture/production startup checks pass with explicit exit statuses.
Update deployed and raised on phone, app/MPRIS PID 14813. Interactive confirmation
of long real group headings remains user-led.

Phone retry completed: shared-policy and native navigation/caption/geometry checks
pass, production QML smoke passes, and latest shared-navigation bundle is deployed
and raised with sole app/MPRIS PID 10743. Saved session and downloads retained;
the connectivity/deployment blocker recorded below is resolved.

Latest shared-catalogue phone launch requested; initial policy fixture passed and
native caption-parent binding corrected. Final native fixture and deployment are
blocked by repeated No route to host for 192.168.1.120. Corrected bundle is built;
resume transfer, geometry check and launch when developer SSH is reachable.

## Shared navigation/main-menu definitions — 2026-10-05

Completed: primary destinations and both main-menu groups share one QtQuick2.6
catalogue, including labels, ordering, capabilities, selection and dispatch. Desktop
and Sailfish keep native rendering/window/page-stack adapters. Shared policy and
actual desktop adapter regressions pass, as do host and Sailfish build gates. Local
desktop update is running; Sailfish runtime verification of the new repeaters remains
distinct from the successful package/fixture build.

Discovery's shared Show all navigation now appears once per group header, rather
than in every child item's context menu. Desktop checks pass and local app is
running for review; matching Sailfish source uses retained presentation DTOs.

Desktop now mirrors mobile's three primary destinations (Discover, Library,
Playlists) and contextual/secondary menu split. Queue is a plain menu destination,
Back is separate, controls align equally. Desktop build/smoke gates pass and the
updated local app is open for interactive review; replaces standalone Home layout.

Desktop navigation cleanup completed locally: direct Discovery Home, single Back,
Queue in menu, non-duplicated detail/category headings and Italian Scopri. Desktop
checks pass; local app launched for user review. Home/Library return-history is
covered by the shared controller regression.

## Discovery default / Library layout — 2026-10-05

Implemented and deployed: Discovery Home startup, inline header scroll plane,
single-row expandable search/browse/sort and in-viewport alphabet shortcuts with
full-width aligned destination tabs. Host/SDK gates and phone startup/navigation/
geometry checks pass. Current app/MPRIS PID 43912. Interactive scroll/expansion
ergonomics and visual confirmation remain user-led.

## Discovery access / menu organization — 2026-10-05

Discovery Home, Library and Playlists now have visible root-header destinations;
contextual actions and secondary navigation/settings are split between pull-down
and pull-up. Queue sits beside Now playing. Retained-view completion/idle handling
and library-only browse controls address stale/misleading discovery presentation.
Host/SDK gates, native phone destination/back checks and read-only live discovery
pass. Rootless update deployed (app/MPRIS PID 39668); user confirmation of the
reported interaction and the revised menu ergonomics is next.

## Next recommendations — 2026-10-05

Implemented: opt-in scheduled/reconnect refresh on an independent worker; normalized
offline search index; canonical server filter choices and validated browsing; basic
smart-playlist create/rule editing; scoped discovery visibility/order and hub paging;
local qualified listening insights; 2–8-waypoint Sonic Adventure; MP3/AAC trim/seek
checks and actual transcode playback/seek validation. Car Bluetooth controls are
user-confirmed. Disconnect auto-pause is verified by an isolated actual signal/audio
fixture; physical disconnect confirmation remains after deployment. Guest DJ/Sonic
Sage, complex Boolean rule editing, full server-history insights/full-library sync,
and sustained power/call/routing coverage remain distinct follow-ups.

Final gates pass (122 Rust tests, desktop CTest 2/2, SDK package/fixture builds),
along with live read-only filter choices/browse and phone navigation/management/
MPRIS checks. Expansion deployed to the existing rootless directory, sole process
and MPRIS owner PID 36423. Next user check: disconnect the car while playing and
confirm pause, then reconnect and confirm playback remains paused. Live smart
playlist mutations and normally installed sandbox validation remain pending.

## Navigation presentation fix — 2026-10-05

Outgoing Sailfish pages now retain their own heading/detail/list presentation
before artist/album pushes and during back/peek transitions. Shared state/model
regressions and the phone's native forward-stack fixture pass. Final native
pop/reload fixture transfer and deployment are pending restored developer SSH;
the latest connection attempt timed out. See validation.md for exact check scope.

Retry completed: final native push/pop/reload checks and production QML smoke pass.
The rootless update is deployed and raised, with sole process/MPRIS owner PID 24916;
saved session and downloads retained. Interactive visual confirmation remains user-led.

## Before calling this a complete Plexamp clone

- Actual Plex/device validation: owner/shared resources, large libraries,
  compilations, FLAC/MP3/AAC, connection failure, expired tokens and reconnect.
- Broader live timeline/scrobble/offline-history coverage and delivery edge cases.
- Interactive media-key/lock-screen behavior, background/call/Bluetooth checks.
- Wider lossy-codec gapless trims, advanced transition/limiting policy and
  sample-rate matching; existing gapless/crossfade/gain/EQ foundations are implemented.
- Throughput-adaptive streaming and broader transcoder compatibility; progressive
  MP3 quality selection, codec fallback and session cleanup are implemented.
- Automatic complete offline library synchronization, richer download management,
  metered/Wi-Fi policies and offline play-history sync.
- Broader sonic-radio live coverage/weighting, Guest DJ and Sonic Sage; server
  discovery stations and Sonic Adventure browsing are implemented.
- Smart-playlist filter authoring and richer personalized discovery hubs/mixes.
- Plex Home managed users/PIN switching, multi-server queue identity, secure
  platform credential store adapters and explicit token revocation.
- Responsive now-playing/lyrics polish, accessibility/native translation review,
  release signing and license distribution (Harbour checks and Linux packaging/CI
  are done; see Open items).

## Testing policy

Implement deterministic local tests before live server/device attempts. Separate
protocol/state tests, Qt integration and hardware-only behavior. Record sources,
commands and observed results in research/validation docs. Phone access is for
the remaining runtime questions, not repeated compile-and-see iterations.

## Device baseline completed (2026-10-02)

Rootless main-QML launch and the shared Qt native-audio integration fixture pass
on aarch64 SailfishOS 5.2.0.18. User confirmed direct server/token connection.
Browser account authorization and fixed mixed-resource server discovery are now
verified against the real account; user confirmed the interactive selection flow.
Server-selection visibility was improved in both UIs following that feedback.
Remaining device work covers supported music codecs, sustained audio,
call/Bluetooth behavior and an installed sandboxed launch.

## Radio baseline completed (2026-10-02)

Artist station radio, sonic album/track fallback, metadata-window continuation,
bounded history and visible radio controls are implemented. Artist queue creation
and continuation are verified on the saved server; sampled album/track seeds lack
sonic analysis. Remaining work includes actual radio listening/long sessions,
album/track live tests on analyzed seeds, station timeline edge cases and future
prefetch/gapless design. Offline audio caching remains separate roadmap work.

## Artist/album browsing baseline completed (2026-10-02)

Artist-first navigation, detail photos/biography/album listing, album cover/
description/tracks, expandable text and icon radio actions are implemented on
both platforms. Shared history and typed metadata are locally tested and phone
metadata/navigation checks pass. Remaining UI validation includes native back
gestures, layout/large-library responsiveness and visual accessibility review.
The saved account's lifetime Pass is confirmed; the selected library has Sonic
Analysis disabled, so analyzed-seed album/track radio live coverage remains pending.

The reported detail-page overlap has a reproduced/fixed Loader sizing regression,
now checked with rendered fixture geometry on desktop and the phone. Continue
visual review with real long biographies, multiple photos and font scaling.

## Mobile cache foundation implemented (2026-10-02)

Persistent original-audio caching, validated retry/resume, bounded eviction,
known-queue prefetch, explicit loaded-track downloads and a downloaded catalogue
are implemented and locally tested. Native local playback passes after the
fixture HTTP server is closed. The radio glyph uses native scalable geometry
with pixel regression coverage instead of Canvas.

SSH was restored and the update deployed. Qt5 offline decoding and one bounded
real Plex download pass; HTTP framing now handles observed DB size differences.
Phone icon capture still needs an unlocked/on display. Next: validate mobile
signal-loss sessions, storage pressure and settings interaction. Later: persist
full explicit job plans and radio
queues, pin albums, cache artwork/catalogue, add metered/Wi-Fi network policies,
measure long intermittent-network sessions and improve first-track buffering/
gapless transitions.

## Stable/incremental artist browsing prepared (2026-10-03)

Passive polling no longer toggles foreground busy or rebinds unchanged state;
list DTO adapters insert/reconcile rows without resets. Artist search is debounced,
75% threshold pagination is transparent, a server-indexed alphabet rail supports
direct page jumps, and main-list radio controls are removed. Album action layout
is bounded and separate from photo hints.

Local and Qt5 phone fixtures/index queries pass. Deployment is currently blocked
by developer SSH No route to host; deploy the prepared bundle once connectivity
returns, then confirm full UI scrolling, touch alphabet and layout on real data.

Connectivity returned and this prepared update was deployed/relaunched on
2026-10-03. The temporary-state phone QML load check passed. Next is interactive
real-data scrolling, alphabet touch/search and pulse/layout confirmation.

Artist browsing now uses responsive photo/title grids on desktop and Sailfish.
Alphabet jumps retain all preceding artists and position the selected letter, so
users can scroll in either direction; host verification is recorded in validation.md.
Interactive phone grid sizing, touch and large-library latency remain user-led.

## Similar artists implemented/deployed (2026-10-03)

Artist pages on both platforms show optional server-ranked similar library artists
with artwork and selectable navigation. Local filtering/navigation regressions,
SDK builds and Qt5 phone metadata/navigation checks pass; the sampled live artist
returned one recommendation. The rootless update is deployed and main-QML smoke
passes. Next: interactive native card/touch/back-history confirmation and broader
coverage of artist metadata/server versions.

## Selected everyday-use priorities implemented (2026-10-03)

1. Queue/current occurrence/shuffle/radio/resume persistence; play-next/add/move/
   remove actions and complete paged album play-all.
2. Standard session-bus MPRIS controls and local-artwork metadata; Qt5/Qt6 protocol
   and generated-audio controls pass. Physical headset/lock-screen behavior and
   a normally installed sandboxed app still need interactive checks.
3. Dedicated now-playing views with artwork, transport/seek/favorites and plain/
   timed lyrics. Local timed fixtures pass; the sampled real track has no lyrics.
4. Global music search, five-star favorite tracks and paged recently added albums/
   server-reported played tracks; live metadata counts pass.
5. Persistent bounded track/album/playlist pins, protected audio, saved library
   metadata/artwork/lyrics and explicit offline browsing/search. This stores visited
   pages; automatic complete-library synchronization remains future work.

The user chose to retain the current audio engine. Gapless, fades and DSP remain
deferred rather than part of this iteration. Updated rootless deployment and
validation results are recorded in validation.md.

JSON lyric documents and blank stanza lines are now normalized rather than shown
literally; live decoding of 37 current-track lines passes. Playlist list/opened
views show total counts/time, and the Sailfish app cover now includes the album
image. Updated bundle is deployed. Next: interactive review of cover/layout and
timed JSON lyric variants on other server/provider versions.

## Requested i18n follow-up — implemented (2026-10-04)

- Implement Qt translation loading/catalogue packaging using the patterns from
  `../ElectricEel` and `../sailfish-proton`; generate the translations ourselves.
- Match their 39 locale catalogues: bg, bn, cs, da, de, el, es, et, fi, fr, gu,
  hi, hu, it, kn, lt, lv, ml, mr, nb, nl, pa, pl, pt, pt_BR, ro, ru, sk, sl,
  sv, ta, te, tr, tt, uk, vi, zh_CN, zh_HK, zh_TW, plus the English source template.
- Follow full-locale → base-language → English fallback and install translators
  before loading QML. Support installed Sailfish, rootless bundle and desktop paths.
- Reuse the lupdate/lrelease + generated dictionary workflow, preserve placeholders,
  plural forms and markup, and verify compiled QM loading on Qt5.6 and Qt6.
- Translate Rust-origin user messages through stable message keys/parameters at the
  presentation boundary; do not translate protocol fields or store localized data.

Audio-engine replacement is implemented; see `audio-engine-plan.md` for the
Rust worker/PCM/HTTP/output boundary and actual verification limits.

## Next everyday-use expansion implemented/deployed — 2026-10-03

- Regular playlist create/add/rename/delete and occurrence-level move/remove.
- General streaming timelines plus durable heard-time-based offline history/sync.
- Artist/album/track views and sorting, optional album-type grouping.
- Download manager with progress, pause/resume/retry/cancel/remove and Wi-Fi-only
  gating/cancellation independent of slow metadata requests.
- Automatic real-neighbor/artist-station continuation and balanced multi-seed mixes.

Host/SDK and selected phone runtime/metadata checks pass. The bundle is deployed;
normal UI/server mutation and long-session tests remain interactive follow-ups.
The i18n/audio follow-up above was implemented in the next iteration (2026-10-04).

## Rust runtime / audio / i18n — 2026-10-04

- Rust-owned workers replace the C++ QThread/player, maintenance/listening sampler,
  MPRIS and network observation. C++ is Qt models/properties/events, translation and
  window activation only; QML does not manage Rust audio lifetime.
- Rust header-authenticated, no-redirect HTTP media transport; installed GStreamer
  decoders feed Rust PCM/DSP and a persistent appsrc/PulseAudio music sink.
- Exact decoded-PCM gapless joins, equal-power crossfade (0–12s, off by default,
  suppressed within the same identified album), ten-band EQ and optional Plex gain.
- 39 TS/QM locale catalogues, full/base/English pre-QML fallback, checked resources
  and placeholders, Qt5.6/Qt6 load tests. New technical translations use compact
  localized clauses, with expanded Italian UI prose; wording polish can continue.
- Host/SDK and phone fake/real-PulseAudio generated-media regressions pass.
- Remaining: wider compressed-codec trim/mixed-rate traces, long sessions/power,
  Bluetooth/headset/call routing, installed sandbox validation, adaptive sample-rate
  matching and advanced DSP/transition policy.

### Automated CI/release — implemented

- Branch pushes/PRs: Rust, Qt integration/smoke, all-QML syntax, 39-locale freshness
  and actual-SDK GNU aarch64 compilation.
- Tags/manual: gated SDK RPM/device fixture and Linux amd64 desktop tar.gz artifacts; only tags publish.
- Pinned clean-runner Linguist bootstrap and committed release-version preflight.
- Remaining validation: first GitHub run after committing/pushing these workflows.
  Initial shuffled-history review failure was observed; final working-tree Rust
  recheck passes all 65 tests, retaining the complete gate.

### Production review fixes — implemented 2026-10-04

- Queue append cap, cache schedule 1000-track consistency, discovery skipping
  malformed servers, artwork reschedule-after-cancel, non-blocking audio
  checkpoints, poison-safe plans locking, primary-route wifi, EntryModel
  identity separators, version-guard packaging, bounded cache index writes,
  shared mutex helper, corrected output-timing comments, resilient release
  tooling with bootstrap locking.

### First everyday-use regressions

- Restore native Sailfish media-volume recognition using its x-maemo stream role.
- Preserve optional-request correlation keys on failed replies to finish lyric loading.
- Resynchronize seek sliders after dragging and track changes on both UIs.
Local regressions pass; phone verification results are recorded in validation.md.
- Numeric-string gain tolerance also restores full track/lyric metadata retrieval;
  current live sample returns 28 decoded lines. All fixes are deployed/tested on the
   phone; physical key/visual interaction confirmation remains everyday-use validation.

### Review follow-ups — 2026-10-04

The six supplied queue/offline/cache/album regressions pass, with extra coverage
for natural-end occurrence identity, temporary quota accounting, streaming seek
representations and stale reply delivery. Local checks and Qt5.6 SDK packaging
pass; details are in validation.md.

Remaining review cleanup: indexed/cached offline-search text, common bounded
pagination and storage utilities, request-admission/UI busy-race stress coverage,
queue/playback reply-order stress coverage, complete optional-data guards across
both shells, translated sentence/plural composition and timer policy, centralized
SDK/Linguist version configuration, a hash-locked Linguist dependency bootstrap,
and structured Rust translation-message extraction. Reduced polling is still
polling; power/latency measurements and broader phone runtime checks remain open.
- Optional numeric-string gain parsing restored real lyric metadata retrieval;
  current live probe decodes 28 lines. All fixes are phone-tested/deployed; physical
   key/visual interaction confirmation remains part of everyday-use validation.

### Daily-use priorities 1–4 — implemented baseline, 2026-10-04

1. Independent Wi-Fi/mobile/download quality controls, progressive MP3 transcoding,
   local-file preference, one-shot codec fallback, fresh seek sessions and bounded
   cleanup. A real-server 160 kbps/256 KiB transfer and cleanup pass.
2. Duration-bounded station/radio/playlist downloads, persistent ordered plans,
   refreshable smart/regular playlist membership, completed-only group playback,
   and paged offline catalogue/search including inferred parents.
3. Server-provided discovery home with grouped music hubs/mixes/stations,
   station activation/download, sonic neighbors and Sonic Adventure. Actual server
   discovery returns 72 entries/4 stations after string-radio compatibility correction.
4. Track/album/auto gain and headroom, safe crossfade gains, reduced idle wakeups
   and MPRIS queue copies, plus FLAC/mixed-rate/pause/gain PCM regressions. Native
   Qt5.6 navigation/settings/MPRIS and real PulseAudio generated transport pass.

Next within these priorities: measured-throughput adaptation, automatic refresh
policy and full-library sync, richer server hubs/advanced sonic modes, compressed
encoder-delay/padding traces and sustained power measurements. Physical Bluetooth
buttons, calls/routing and installed Sailjail remain hardware/user-session checks.
Remote-player control is deferred per the user's priority decision.
