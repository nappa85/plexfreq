# Development roadmap

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
  release signing/Harbour checks, Linux packaging/CI and license distribution.

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
- Tags/manual: gated SDK RPM/rootless/device fixture artifacts; only tags publish.
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
