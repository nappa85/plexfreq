# Development roadmap

## Before calling this a complete Plexamp clone

- Actual Plex/device validation: owner/shared resources, large libraries,
  compilations, FLAC/MP3/AAC, connection failure, expired tokens and reconnect.
- Broader live timeline/scrobble/offline-history coverage and delivery edge cases.
- Interactive media-key/lock-screen behavior, background/call/Bluetooth checks.
- Gapless engine, crossfade, ReplayGain/loudness, limiter, equalizer; measured
  audio tests and scheduling guarantees before claiming gapless/DSP support.
- Transcode negotiation for unsupported codecs/limited bandwidth; cancellation
  and session cleanup. Current adapter reports codec failures.
- Automatic complete offline library synchronization, richer download management,
  metered/Wi-Fi policies and offline play-history sync.
- Broader sonic-radio live coverage/weighting, library stations and sonic adventure.
- Smart-playlist filter authoring and richer personalized discovery hubs/mixes.
- Plex Home managed users/PIN switching, multi-server queue identity, secure
  platform credential store adapters and explicit token revocation.
- Responsive now-playing/lyrics polish, accessibility/translations,
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
