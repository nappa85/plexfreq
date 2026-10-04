# Research log

Findings recorded 2026-10-02. Sources distinguish upstream evidence from local
observations. Consult this file before changing the integration or build route.

## Reference projects examined

- `../ElectricEel/app/harbour-electric-eel.pro`: `CONFIG += sailfishapp`, Rust
  **glibc** static library, `pthread/dl/m`, QML installed by sailfishapp, explicit
  icon installation. `app/src/teslaclient.h`: Rust calls live on a dedicated
  QThread; queued signals cross to the GUI. `helper/make-app-bundle.sh`: GNU Rust
  target, never link a musl staticlib into a glibc Qt process.
- `../ElectricEel/build.sh`: use `mb2 --target SailfishOS-5.2.0.15-aarch64 build`
  inside the coderus image, not generic distribution packaging.
- `../sailfish-proton/Cargo.toml`: reqwest with rustls, avoiding OpenSSL headers
  in cross builds. `make-pkg-bundle.sh`: actual cross compiler and sysroot are
  `/opt/cross/bin/aarch64-meego-linux-gnu-gcc` and
  `/srv/mer/targets/SailfishOS-5.2.0.15-aarch64`.

## Verified toolchain facts

- Host: x86_64 Linux, Rust/Cargo 1.95.0, Qt 6.11.2 Core and Multimedia via
  pkg-config, CMake present. Plain `qmake` is absent; use CMake for desktop.
- SDK image inspected without installing anything on a phone:
  `coderus/sailfishos-platform-sdk-aarch64:latest`, image ID
  `sha256:e5f7596d14502746b308c3dde36f65fc25d7f76c45a01acaed56a1f55b76f592`.
  Target 5.2.0.15 aarch64, QtCore 5.6.3, QtMultimedia-devel 5.6.2, GCC 13.4.0,
  glibc 2.41. Rust aarch64 GNU standard library is already installed on host.
- Qt 5.6 is an important ceiling: no Controls 2, modern JS syntax, modern
  QML Connections syntax or newer Qt convenience functions in shared code.
- The image's default mersdk UID/GID is 100000, not host UID 1000. Docker root
  may chown a **disposable build copy**, then the normal image user invokes mb2
  directly. `runuser` cannot open a PAM session here. Ownership is restored with
  an EXIT trap. No user source directories or phone directories are chowned.

## Upstream documentation / implementations consulted

1. https://docs.sailfishos.org/Develop/Apps/ — Qt5/Quick2/Silica and
   libsailfishapp are the supported native application foundation.
2. https://docs.sailfishos.org/Develop/Apps/Application_Permissions/ — request
   permissions in the `[X-Sailjail]` desktop group; launching from a terminal
   bypasses the sandbox unless `sailjail` is used. App needs Internet and Audio.
3. https://python-plexapi.readthedocs.io/en/latest/modules/audio.html — music
   hierarchy artist → album → track, `ratingKey`, album/artist title fields,
   duration in milliseconds, media parts. Advanced sonic features exist but
   require server analysis/capability checks.
4. https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/myplex.py
   and https://python-plexapi.readthedocs.io/en/latest/modules/myplex.html —
   PIN/browser sign-in, resources, resource-specific access tokens, connection
   candidates. This is a community implementation, not a Plex stability promise.
5. https://doc.qt.io/qt-6/qmediaplayer.html — Qt6 `setSource`, QAudioOutput,
   asynchronous loading/status, EndOfMedia and errors.
6. https://raw.githubusercontent.com/qt/qtmultimedia/5.6/src/multimedia/playback/qmediaplayer.h
   — exact Qt5.6 surface: `setMedia(QMediaContent)`, `stateChanged`, overloaded
   `error`, `setVolume`, `setAudioRole(QAudio::MusicRole)`.
7. https://gcc.gnu.org/onlinedocs/gcc/Directory-Options.html — `-Bprefix`
   explicitly selects compiler subprograms. The SDK GCC otherwise finds the
   native assembler (`as: unrecognized option -EL`). The wrapper supplies
   `-B/opt/cross/bin/aarch64-meego-linux-gnu-` to select the target assembler
   without changing symlinks inside the image (the Proton build changes one).
8. https://raw.githubusercontent.com/sailfishos/libsailfishapp/master/data/sailfishapp.prf
   — reserves/overrides the qmake `icon` install group for a default PNG. Use
   a distinct `launchericon` group for this application's scalable SVG.
9. Actual SDK QML inspected: `Sailfish/Silica/Slider.qml` and
   `private/SliderBase.qml` confirm minimumValue/maximumValue/value/valueText and
   the inherited released signal. `DockedPanel.qml` confirms dock/open properties.
   The old web Slider documentation URL returned missing content; installed SDK
   source was used instead. This inspection is not a Sailfish runtime test.
10. https://raw.githubusercontent.com/sailfishos/libsailfishapp/master/src/sailfishapp.cpp
    — application/view ownership and data path helpers; packaged resources live
    under `/usr/share/<target>`. Rootless launch must explicitly select its
    bundled QML path when outside the installed package layout.

## Decisions

- Separate Silica and Controls2 shells, shared QtQuick2.6 session controller.
- Stable opaque C ABI rather than coupling a Rust QObject generator to two very
  different Qt versions. Tiny C++ shim only marshals JSON and adapts OS audio.
- Blocking Rust HTTP on one dedicated worker, never on GUI thread. Request and
  connection timeouts are bounded. Rust owns credentials and all queue policy.
- Rustls TLS + system-independent root bundle, no native OpenSSL dependency.
  `ring` still has C/assembly; compile that through the real SDK compiler using
  `tools/sdk-cc.sh`, rather than silently using the host libc.
- Plex JSON selected using Accept header. Music types: artist=8, album=9,
  track=10; section endpoint `/library/sections/N/all`, title filter for search,
  100-item pagination with Plex container start/size query parameters.
- Direct play uses the Part key, not the server's filesystem path. Part/artwork
  paths must stay on the selected server origin. Media and image URLs need an
  encoded X-Plex-Token query since native decoders/QML Image do not share the
  Rust HTTP client's headers. Do not log these URLs.
- Resource discovery sends includeHttps/includeRelay. Prefer non-relay HTTPS,
  then other advertised connections. Certificate validation is never disabled.
- Session uses an atomic private 0600 file within a 0700 per-user app directory.
  This is private-file storage, **not encrypted keychain storage**. No passwords
  are collected; sign-out clears locally stored tokens, not Plex-wide sessions.
- Qt6's decoder diagnostics were observed to print source URLs. Rust/network
  errors do not include tokens, but framework debug output is a separate channel
  to account for before adding log export/support diagnostics.
- Phone root access is unnecessary for execution and validation. Build on host;
  use user-session developer access for an unpacked bundle if RPM installation
  is unavailable. Actual codec/routing/sandbox behavior still needs device tests.

## Known unanswered questions

- Validate JSON response shapes against the user's actual Plex version, shared
  libraries, compilations and mixed media versions. Local fixtures test protocol
  mechanics, not server-version coverage.
- Device codec availability, background playback, call interruption and Bluetooth
  routing; Qt's MusicRole is enabled but must be tested on hardware.
- Full Plexamp gapless playback needs a dedicated scheduling/decoder design;
  sequential QMediaPlayer tracks do not guarantee gapless.
- Plex Home switching, server-side play queues, timeline/scrobble reporting,
  transcode negotiation, offline cache, DSP and MPRIS remain roadmap work.
- Harbour validation, entitlement-gated features and release signing are separate
  from producing an SDK RPM. No Harbour approval is claimed.

## Physical phone findings — 2026-10-02

- Authorized developer SSH: `defaultuser@192.168.1.120`, UID/GID 100000,
  home `/home/defaultuser`. Device is aarch64, SailfishOS **5.2.0.18**.
- Phone RPM versions match the SDK's relevant ABI: glibc
  `2.41+git5-1.11.3.jolla`, QtCore `5.6.3+git60-1.19.6.jolla`, QtMultimedia
  `5.6.2+git34-1.13.1.jolla`. Silica is `1.2.157-1.39.1.jolla`.
- SSH already supplies `XDG_RUNTIME_DIR=/run/user/100000`,
  `WAYLAND_DISPLAY=../../display/wayland-0` and
  `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/100000/dbus/user_bus_socket`.
  No environment harvesting from another process or system changes are needed.
- Rootless bundle deployed to a fresh private directory
  `/home/defaultuser/plexfreq-test.mTmhFP`. `ldd` resolves all shared libraries.
- Actual `--smoke-test` loaded the main Silica QML and returned **0**. Graphics
  initialization emitted vendor libhybris/Mali namespace/module diagnostics and
  an EGL teardown warning; there was no QML error. Do not try to repair vendor
  namespaces/system drivers from the app. Visible UI quality needs user review.
- The phone already has `libQt5Test.so.5.6.3`. The SDK lacked its development
  headers; declaring `pkgconfig(Qt5Test)` in the separate test spec makes mb2
  install `qt5-qttest-devel` **inside its disposable container only**.
- The installed Qt5.6 test header supplies `QTRY_VERIFY_WITH_TIMEOUT` directly.
  No compatibility macro is required. Reuse the exact desktop fixture source.
- Qt5's media plugins are GStreamer-based (SDK `libgstmediaplayer.so` etc.). The
  same native fixture passed on the physical device: PCM WAV/HTTP byte ranges,
  Rust requests/worker, playback position, pause/resume, seeking, manual next,
  EndOfMedia and logout. Synthetic silence proves pipeline progression, not
  audible output/routing quality or FLAC/MP3/AAC support.
- Interactive rootless app was started with `nohup` and stdout/stderr redirected
  to `/dev/null`; it remained running after SSH returned. No package was installed
  on the phone. Additional real-account results are recorded below; sandbox
  validation is still pending.

## Browser login / resource parsing correction — 2026-10-02

- User reported direct server URL + X-Plex-Token connection worked flawlessly.
  Browser authorization completed, followed by `Unexpected Plex response`.
- https://www.plexopedia.com/plex-media-server/api-plextv/resources/ documents
  a mixed resource list: media servers advertise `provides="server"` and have
  access tokens, while Plexamp players advertise client/player capabilities and
  omit that field. This page describes the older XML endpoint, so its example
  was evidence for resource semantics, not the exact v2 JSON response shape.
  The attempted `/api-plextv/pin/` documentation URL returned the site homepage;
  it was not used as PIN API evidence.
- Phone-side inspection printed only saved-authorization booleans: both account
  authorization and direct connection were present. PIN polling/authorization had
  succeeded; the failure was the subsequent `servers` operation.
- Before changing code, the local login fixture was updated with a player whose
  token is absent and another whose token is null. It reproduced the reported
  `Error::Protocol` at server discovery (previous fixture wrongly supplied every
  player with a token).
- `tools/inspect-server-discovery.py` runs **on the phone**; it reads existing
  private state, requests v2 resources with the token in an HTTPS header and
  prints only counts/field types. No payload or token is copied to the host.
  Actual response: one server (`accessToken` string), one non-server
  (`accessToken` null); all other fields expected by our server model were present.
- Fix: parse the resource array, filter `provides` for the exact `server`
  capability, then deserialize only those entries into the strict server model.
  Unrelated player fields no longer invalidate discovery; malformed server data
  still fails. Do not use an account token as an implicit shared-server fallback.
- JSON decode errors now report a static operation context (creation/polling,
  discovery, library request), never serde's raw error text or response values.
- A second build issue was found in the SDK output: qmake did not relink the app
  after the Rust archive changed because `LIBS` alone was not a prerequisite.
  Production and test `.pro` files now declare the archive in `PRE_TARGETDEPS`.
  The subsequent SDK build visibly relinked the production executable before
  packaging. This matters for all future Rust-only fixes.
- After local checks, an opt-in native `savedAccountDiscovery` case queried the
  real account through the **fixed Rust backend on the phone**: 1 server found,
  all 3 QtTest lifecycle entries passed. The check printed only a count.
- Updated executable deployed atomically into the existing rootless directory;
  restarted as PID 64356, normal saved account/direct-connection state retained.
  Because a saved direct connection is restored first on startup, use Connection
  → Refresh servers to display the already-authorized account's server list.

## Server-selection discoverability — 2026-10-02

- User confirmed the browser/discovery fix works, but the server list looked like
  anonymous ordinary buttons and was initially overlooked.
- Existing SDK `Sailfish/Silica/BackgroundItem.qml` was consulted: `highlighted`
  follows its native down/press state, and children live in its content item.
  `SectionHeader.qml` uses small text, so the selection title uses an explicit
  large, bold themed Label rather than relying on a small section heading alone.
- Sailfish connection page now has a "Choose a Plex server" heading and guidance,
  full-width tinted selection cards, large highlighted server names, an action
  subtitle and chevrons. Long names wrap and determine card height.
- Desktop uses the same hierarchy plus an outlined server-list panel and a
  bounded scrolling list, so several choices do not expand the entire dialog.
- Both retain the existing index-based `select_server` command. This is a QML
  presentation change; no API or credentials model change was needed.
- Desktop QML smoke check and SDK packaging passed. The updated SettingsPage
  was deployed atomically to the phone's rootless QML directory and the app
  restarted (PID 354), with saved state retained. Visual feedback is user-led;
  no screenshot or automated interaction of the new cards was performed.

## Streaming and radio — 2026-10-02

### Sources consulted

- https://developer.plex.tv/pms/ — official PMS API. Play Queue introduction:
  lightweight ephemeral queue/window, not downloads; advancement uses timelines.
  `playQueuePostSlash`: POST `/playQueues` with audio type and source URI.
  `playQueueQueueGetSlash`: GET queue with center/item ID, window and before/after
  flags. `timelinePostSlash`: POST `/:/timeline`, actual state/time/duration and
  queue-item ID, on state transitions and periodically (normally 10 seconds).
  `libraryMetadataGetNearest`: GET `/library/metadata/{ids}/nearest`, sonic
  neighbor limit/distance parameters. Use JSON Accept header; new clients should
  prefer it. Existing legacy request defaults are retained for includeStations.
- https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/audio.py
  — Artist.station requests metadata with `includeStations=1`, reads Stations;
  Audio.sonicallySimilar uses `{item.key}/nearest` for the same audio type.
- https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/playqueue.py
  — fromStationKey builds `server://{machineIdentifier}/com.plexapp.plugins.library{key}`
  then POSTs an audio queue. Queue occurrence identity is playQueueItemID, not
  ratingKey, since a radio can repeat the same track in different occurrences.
- https://raw.githubusercontent.com/plexinc/plex-for-kodi/master/lib/_included_packages/plexnet/playqueue.py
  — sliding windows and queue refresh; queue metadata is distinct from audio.
- https://support.plex.tv/articles/sonic-analysis-music/ — Track Radio is a real
  Plexamp feature. Album Radio plays complete albums then sonically similar albums.
  Sonic analysis requires server-admin Plex Pass, PMS >=1.24.0 and supported
  server platforms. This is a server requirement, not a phone CPU requirement.
- https://support.plex.tv/articles/202188298-play-queues/ — queues are ephemeral,
  distinct from saved playlists; client windowing is expected.
- https://plexapi.dev/llms.txt supplied its documented index after the guessed
  playqueue URL returned 404; current community index lacks that endpoint, so the
  official PMS docs were used. A Feishin Plex directory URL returned 404. The
  kernelkaribou/plex-radio-client example was inspected but uses its own radio
  relay service and GPIO, so it was not used as Plex station API evidence.

### Decisions and actual findings

- Existing playback passes a Part URL to Qt; defining a playlist only caches its
  metadata in process memory. Rust writes session state, not media. Qt may buffer
  audio internally; no durable offline cache/download is implemented.
- Station keys are discovered from the actual seed metadata, never synthesized.
  All keys are origin-confined before building a source URI; station query
  parameters are preserved. Optional Stations JSON may be an array or a child
  container (actual phone server uses `Stations: {size, Metadata:[...]}`).
- Artist radio uses a genuine server station/play queue. At the end of a loaded
  window, fetch another window centered after its final queue item. Preserve
  repeated rating keys if their occurrence IDs differ. Local history is trimmed
  to 100 prior tracks to prevent indefinite metadata growth.
- Prefer an advertised station for any seed type. Otherwise album/track radio
  requires `musicAnalysisVersion == 1` and nonempty same-type sonic neighbors.
  Our sonic fallback walks server-ranked neighbors, avoiding recent seeds where
  possible; it is not a reproduction of Plexamp's proprietary mix weighting.
  Albums are expanded in server track order across pages, up to 1000 tracks per
  album, and appended intact. Never silently substitute whole-library shuffle.
- Native station state/time is reported through Rust on playing/paused changes,
  every 10 seconds when idle, and at automatic completion (continuing=1).
  Shuffle/repeat are disabled for radio; normal Play exits radio. Stop radio clears
  the local queue. Sonic radio does not yet report general play history.
- Local tests passed before live probes/queue creation. The value-free capability
  probe found artist Stations, but sampled album/track analysis versions were not
  1 and their nearest queries returned zero items. This samples one item per type;
  it does not claim that no other item in the library is analyzed.
- Opt-in `savedRadioPlanning` called the new Rust radio commands on the phone:
  artist queue initially 5 tracks, then 10 after moving beyond the initial window.
  Album/track sampled seeds gave the expected analysis requirement message.
  The check obtained stream plans only: no audio bytes fetched, decoded or played;
  no synthetic timelines or played-history claims were sent to Plex.
- Updated rootless app passed phone QML smoke (exit 0) and was started as PID
  5935. Native generated-audio integration still passes on Qt5/GStreamer. One
  transient SSH banner timeout occurred before deployment; reconnect succeeded.
  Existing vendor graphics diagnostics remain; no system service was changed.

## Lifetime Pass, sonic availability and artist-first detail UI — 2026-10-02

### Additional evidence/sources

- User reported the radio requirement message despite lifetime Plex Pass, and
  requested Artists → Artist detail → Album detail instead of a browse-kind picker.
- The existing UI defaulted to `kind=album` on launch. Its generic Radio caption
  did not make the seed type obvious. Artist radio uses a different server feature
  from sonic album/track radio; the new navigation makes the artist seed explicit.
- A read-only value-free check of `https://plex.tv/api/v2/user` confirmed
  `subscription.active=true`, `subscription.plan=lifetime`. The PMS root confirmed
  `myPlexSubscription=true`. The selected section's `/prefs` returned the setting
  `musicAnalysis=false`. Recommendations for sampled album/track seeds were empty.
  No library preference was changed. Only flags/counters were printed.
- Metadata omitted `musicAnalysisVersion` rather than explicitly returning 0.
  A local regression reproduced false rejection when an omitted flag accompanied
  valid `/nearest` results. The hard gate was removed: station/nearest responses
  are authoritative. Empty results now mention the library's Sonic Analysis
  setting/completion, not a supposed missing subscription.
- https://python-plexapi.readthedocs.io/en/latest/modules/audio.html was consulted
  for artist/album hierarchy, summary, thumb/art/Image, album year and track/disc
  indexes. Full details use `/library/metadata/N`, children use `/children`.
- https://doc.qt.io/archives/qt-5.6/qml-qtquick-canvas.html confirms Canvas/Context2D
  support on the actual phone API ceiling. The shared radio icon draws an antenna
  and two waves on each side, without SVG plugins or platform-specific imports.

### Implementation choices

- Shared Session always browses artists. A `detail` backend command returns full
  seed metadata plus paged children. Artist and Album pages use this data; no Plex
  HTTP logic moved into QML. History restores the prior route and search query.
- Sailfish uses separate `ArtistPage.qml`/`AlbumPage.qml` stack pages, reusing the
  listing/player shell. Native back activation restores the parent backend route;
  resetting a connection returns to the root. Desktop uses a detail-header view
  inside its scrollable listing and explicit back navigation.
- Detail metadata includes plain-text description, artwork/gallery, year and
  track/disc numbers. Artist photo and background/extra Image URLs are deduplicated,
  origin-confined and limited to eight. Missing descriptions get an honest
  placeholder; missing photos are not invented. Large text can expand/collapse.
- Radio actions are icon-only, with accessible names and desktop type-specific
  tooltips. Both list rows and detail headers use the same shared drawing.
- Timeline acknowledgements with empty data no longer emit a full state change,
  avoiding unnecessary detail/gallery refresh while browsing during playback.
  Description expansion resets on item key changes, not every metadata update.
- Optional new detail fields tolerate nulls, and year/track/disc indexes use signed
  integers because they are metadata, not counts. Local regression tests cover
  null and unknown-number cases. Live field-type probes sampled 100 entries per
  type without revealing values, names, keys, URLs or credentials.

### Actual validation

- Local checks: 18 Rust tests and 2 CTest entries passed, including actual shared
  QML route/seed/history assertions and desktop detail-header instantiation.
- Qt5 phone fixture passed navigation assertions and live detail metadata:
  sampled artist: 4 albums, 1 photo, biography present; first album: 14 tracks,
  1 cover, no description. Those numbers are observations, not a library-wide
  claim. Artwork URLs were not printed and the fixture did not download images.
- Updated live radio planning passed: artist station 5 → 10 queue entries;
  sampled album/track unavailable because no recommendations were returned.
- An earlier combined live run failed the track-browse assertion without a
  detailed error. The assertion was instrumented, nullable/signed detail-schema
  regressions added, and the final combined run passed. The type probes did not
  establish the precise cause of that first failure; do not claim that it proved
  a particular null/negative field or an entitlement failure.
- SDK package build passed; updated rootless UI smoke returned 0 and the app was
  relaunched as PID 13885. Main-page load and process health were checked, not
  screenshots or automated native page-stack gesture interaction.

## Detail-page overlap / Loader geometry correction — 2026-10-02

- User reported artist/album pictures covering the pull-down cue and rows
  overlapping the entire detail header. The Sailfish header Loader used
  `height: item ? item.height : 0`. Qt resizes a loaded visual item to an explicitly
  sized Loader, so this is a height feedback loop rather than an intrinsic size.
- https://doc.qt.io/archives/qt-5.6/qml-qtquick-loader.html was consulted: explicit
  Loader geometry is applied to the loaded item. The size input must therefore
  be `item.implicitHeight` for this content-driven header, not `item.height`.
- Installed SDK `SilicaListView.qml`, `PageHeader.qml` and
  `private/PulleyMenuBase.qml` were inspected. The native menu already uses
  `z:10000` specifically to keep its indicator/dimmer above content. Preserve
  that behavior; do not compensate for bad header geometry with arbitrary z hacks.
- Before fixing the sizing expression, the local geometry regression reproduced
  a loaded header with implicitHeight=1000 and Loader height=0. Thus the list
  reserved no space while descendants still painted over following rows.
- `IntrinsicLoader.qml` now uses the intrinsic height and clips to its reserved
  region. Sailfish and desktop use the same helper. The mobile header Column
  explicitly follows its implicit height; thumbnails are constrained to row
  bounds and row height accommodates text and radio controls.
- The regression uses the actual shared loader with synthetic photo/text/control
  content, asserts that rows start below it, then narrows the width and unloads/
  reloads the header. A QQuickWindow is attached so text wrapping and positioner
  polish genuinely run; the first windowless resize check could not observe that
  rendering/layout phase. Default host checks use software Quick rendering.
- Final host checks passed. The same geometry regression passed on the physical
  phone's Qt5.6/Wayland-EGL runtime (3 lifecycle/test entries, 0 failures/skips).
  It used fixture content only, with no Plex credentials/network requests.
- Updated RPM/rootless build succeeded; rootless update deployed and relaunched
  PID 15113. Existing vendor GL diagnostics remain. Complete real-data page
  screenshots were not captured; user visual confirmation is still useful.

## Stable radio glyph and mobile cache foundation — 2026-10-02

### Sources/approach

- User reported intermittent oversized/cropped radio Canvas glyphs and requested
  audio caching for low/no mobile coverage. The new glyph uses QtQuick2.6 native
  rectangles/rounded outlines in a fixed 32-unit coordinate group, scaled as a
  whole to the allocated button area; no Canvas context/backing texture remains.
- https://doc.qt.io/archives/qt-5.6/qml-qtquick-item.html — Item scale/transform
  origin/clip and in-memory grabToImage. Pixel regression repeatedly resizes,
  hides/reuses the real glyph and checks full wave/mast extent, not only its corner.
- https://www.rfc-editor.org/rfc/rfc9110.html — sections 13.1.5, 14.2/14.4 and
  15.3.7: If-Range, byte ranges, Content-Range, full 200 responses when a validator
  no longer matches, and incomplete-message/length requirements.
- https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html — per-read timeout
  resets on successful reads, unlike an overall request timeout. The generic
  latest docs resolved to 0.13.5 first; implementation uses the pinned 0.12 API.
- Reuse Plex's documented Part endpoint, not the server-side download/sync queues:
  these cache the same original representation already used by direct playback.

### Implementation/findings

- New Rust cache worker on its own Tokio runtime, cancellable independently from
  the UI/Core worker. Default enabled: current track + next five queued tracks,
  512 MiB storage budget. Explicit loaded-track/album download requests merge with
  the priority queue and remain requested for the current Core session.
- Quota/LRU includes completed and partial audio. Current/requested entries are
  protected; abandoned partials can be reclaimed. Shrinking a configured budget
  prunes unprotected entries; clearing cache stops downloads/playback.
- Private 0700 folder/0600 files; cache namespace is SHA-256 of origin/token,
  media key additionally includes ratingKey/Part key. Index stores raw typed
  metadata, not generated authenticated framework URLs or request-token headers.
  Different origins/tokens are conservatively separate; alternate URLs to the
  same PMS can therefore duplicate cache storage in this first version.
- Completed files are published only after response framing/size validation,
  sync and atomic rename. Part.size is retained and checked when present. Invalid
  ranges, encoded/error bodies, changed sizes and truncated files are not played.
- Resume uses ETag (not weak ETag) or Last-Modified with If-Range. A full 200
  restarts the partial file rather than appending. Without validators, retry
  restarts safely. Retry occurs after 30 seconds; connection/read timeouts are
  bounded at 5/10 seconds while successful large transfers can continue.
- Partial-job metadata survives restart. A process file lock prevents competing
  writers; cancellation closes the job channel and joins the worker before lock
  release. Metadata-only diagnostics now use pf_new_inspect/read-only cache state
  so radio planning cannot accidentally start prefetch or alter LRU metadata.
- Playback first uses completed local files, even before resolving remote media
  metadata. Downloaded catalogue persists independently from library loading.
  Startup library failures can fall back to it. A failed remote decoder can try
  a completed local copy with saved playback position.
- Cache progress is a separate QObject property; polling does not rebind detail
  pages or clear unrelated UI errors. Cache-aware local station playback avoids
  HTTP timelines while disconnected; offline history synchronization is deferred.
- Normal first play may stream and download the same uncached current track in
  parallel. Ahead caching improves subsequent tracks; it cannot prevent a drop
  before the current track has fully downloaded. No partial-file streaming engine
  is claimed. Existing completed files remain usable with automatic caching off.
- Offline radio is limited to already-known downloaded entries; recommendations
  beyond that window still require a server. Complete radio/queue persistence,
  artwork/full-library cache, permanent album pins and Wi-Fi-only controls remain
  roadmap work. Explicit not-yet-started download lists are not yet persisted as
  full job plans; interrupted active jobs are recoverable.

### Validation status

- Local 28 Rust tests and both CTest entries pass, including HTTP interruption/
  range validation/restart, private storage/scoping, quota, cancellation, catalogue
  restart and offline file selection. The native decoder test now closes its HTTP
  server and plays the second track from cache, verifying actual local decoding.
- Icon pixel regression passes locally across six sizes/reuse transitions.
- SDK app/RPM/rootless builds succeed; production package lint has 0 errors and
  its existing no-url-tag warning. Qt5 fixture built with cache and icon cases.
- Phone validation/deployment attempted only after those local checks. SSH twice
  returned `No route to host` for 192.168.1.120. No new files were transferred and
  this update has NOT been deployed. A bounded real-media test (one track ≤8 MiB)
  is prepared but has not run. Mobile signal loss and actual PMS resume behavior
  are still pending once developer SSH is restored.

## Cache update phone validation/deployment — 2026-10-03

- Developer SSH became reachable again. Rebuilt the current Qt5 fixture before
  transfer. Generated-audio cache test passed on Qt5/GStreamer with its HTTP
  listener closed before the next track: the next decoder source was a local
  cached file. No phone network setting/service was changed to simulate that.
- The icon capture test could not expose its QQuickWindow. Read-only MCE
  `get_display_status` returned `off`; this was a test/display precondition,
  not evidence that the new primitive glyph failed its pixel bounds check.
  Phone icon capture remains pending an unlocked/on display.
- The first real cache check exceeded the outer tool timeout because QtTest's
  QTRY timeout extension hid repeated download failures. Replaced that wait with
  a strict 60-second loop and immediate static cache-error reporting.
- The surfaced error was Part.size mismatch. A phone-side, token-value-free
  one-byte Range probe showed metadataBytes=1390392, Content-Range total=1388080,
  status 206, contentLength=1, audio/mpeg and ETag present. No URL, item name,
  identifier or token was printed; the probe read only one media byte.
- Local regression reproduced rejection of valid HTTP framing when persistent
  Plex Part.size differs. Fixed completion policy: Content-Length/Content-Range
  and validators are authoritative, not the DB hint. Store actual received size
  in committed metadata. Strict partial-range/length checks remain intact.
- Final `./tools/check.sh`: 29 Rust tests, 2 CTest entries passed (4.84 seconds),
  formatting/Clippy passed. Both app and Qt5 fixture rebuilt successfully.
- Bounded real-server download then passed: 1388080 bytes completed, indexed and
  verified as a local-file playback plan; the check did not play that real track.
- Updated rootless bundle passed phone QML smoke (exit 0), deployed and relaunched
  PID 36494. One completed real audio entry is already available in the cache.
  Existing vendor graphics messages remain; no new QML load error was observed.
  Actual uncontrolled coverage loss and long-session cache behavior need further
  use; fixture offline decoding and a real PMS transfer are now verified.

## Stable polling/layout, incremental browsing and alphabet — 2026-10-03

- User reported an app-wide periodic layout pulse. Code review found every
  cache_status request incrementing the user-visible busy counter, despite passive
  status semantics. A local regression reproduced two busyChanged signals for
  one poll. The header's BusyIndicator was therefore repeatedly inserted/removed
  from a Column and content repositioned. This is unrelated to media downloading.
- Passive cache/timeline/index work now has no global busy signal. Cache maps are
  compared before notifications. Background page fetches have loadingMore, not
  global busy. Empty/unchanged state does not cause unrelated model rebinding.
- A thin QAbstractListModel adapter exposes Rust DTOs with the entry role. Inserts
  append existing pages; snapshot reconciliation preserves matching rows without
  modelReset. Rust retains metadata/queue/API policy. Request generations discard
  stale background list replies when navigation changes.
- Shared detail data is updated only when a detail reply contains new metadata,
  not re-derived from a generic state map on every items/queue update. This keeps
  gallery/description instances stable while pages append or background data moves.
- Installed SDK SearchField.qml was inspected: native search has a clear control
  and inherited text-change behavior. New 350 ms debounce searches the server's
  complete artist catalogue; Enter can submit immediately. Busy queries retain
  the latest input, old page loads cannot replace a newer route.
- https://doc.qt.io/archives/qt-5.6/qml-qtquick-listview.html documents custom list
  models, insertion notifications, visibleArea inheritance and PullBackHeader.
  The native header can reappear on backward scrolling; search is not limited to
  an obscure Enter-only action. Pagination starts at visible fraction >=0.75,
  deduplicates in-flight starts and offers retry after failure without tight loops.
- https://plexapi.dev/api-reference/library/get-first-character-groups.md documents
  `/library/sections/{sectionKey}/firstCharacter`. The read-only phone shape probe
  returned Directory groups with key/size/title: 28 groups, 503 artist entries.
  Rust requests artist type and titleSort ascending, sums group counts for page
  offsets and performs validated letter jumps. No artist names/IDs/tokens printed.
- Shared alphabet rail selects on touch release (dragging previews a letter), so
  a finger sweep does not issue dozens of server fetches. It uses returned groups,
  supports non-English/special labels and is hidden for filtered search results.
- Main artist-row radio controls were removed. Detail radio remains. Native album
  play/download controls have bounded equal-width buttons; the photo swipe hint
  is an independent wrapping line, and metadata/radio have a bounded caption area.
  Desktop uses matching separate control rows; duplicate album download toolbar
  action is hidden.
- Local checks: 30 Rust tests + 2 CTest entries passed (5.62 seconds), including
  polling, insertion/no-reset, pagination duplicate/failure guards, debounce and
  narrow desktop action bounds. SDK app/test builds pass.
- Phone Qt5 checks: 6 lifecycle/test entries passed for passive polling, model
  insertion, pagination/debounce and live index jumping. Live jump offset 313
  fetched 100 rows directly. No media or artwork downloaded by these checks.
- SSH failed with No route to host just before update deployment and on recheck.
  No bundle bytes/files transferred; the phone still runs the prior cache update.
  Prepared browsing update needs deployment after developer connectivity returns.

### Browsing update deployed — 2026-10-03

Developer SSH returned at the existing address. The prepared bundle was deployed
to the existing rootless directory. Temporary-state phone QML load check returned
0 with only the known vendor graphics diagnostics, no QML load error. PlexFreq
was relaunched as PID 58810 and remained running after 2 seconds. No credentials,
cached audio, Plex settings or system services were modified by deployment.
Full real-data pulse/layout/scrolling confirmation remains interactive user work.

## Similar artists — 2026-10-03

- Consulted https://plexapi.dev/api-reference/metadata/get-similar-items.md:
  GET `/library/metadata/{ratingKey}/similar` returns `MediaContainer.Metadata`.
  Official PMS documentation at https://developer.plex.tv/pms/ describes
  `libraryMetadataGetSimilar` and its `count` parameter (default maximum 200).
  The python-plexapi mixins package index was also inspected at
  https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/mixins/__init__.py;
  its SimilarArtistMixin is an editing mixin, so it was not used as evidence for
  the retrieval endpoint. Old guessed standalone mixins.py URLs returned 404.
- Rust requests up to 12 similar entries, retains numeric library artist IDs,
  removes self/duplicates/non-artists and normalizes artwork with existing
  origin confinement. HTTP 400/404 yields optional unavailable state; other errors
  are handled by the similarity section without replacing the artist's albums.
- Both artist headers present horizontally scrollable selectable artist cards.
  Activation reuses existing artist navigation/history. Loading, empty and
  unavailable states are explicit. Similarity is independent of sonic radio.
- Similarity commands are passive, keep their DTOs out of global state and carry
  list-view generations so late success/failure replies cannot affect a new route.
- Local checks passed before live requests: 31 Rust tests, formatting/Clippy,
  CTest 2/2. SDK production and Qt5 fixture builds passed. Phone QtTest navigation
  and saved-library metadata cases passed (4 entries); the sampled artist returned
  1 similar artist, with endpoint available. Diagnostics printed counts only;
  this live check did not download artwork or audio.
- Rootless update deployed to the existing directory; temporary-state QML smoke
  exited 0 with known vendor graphics diagnostics. Relaunched PID 60034 remained
  running after 2 seconds. Native card appearance/touch confirmation remains
  interactive validation.

## Everyday-use priorities 1–5 — 2026-10-03

The user selected queue persistence/editing, system controls, now-playing/lyrics,
search/favorites/recent views and pinned/offline browsing. The current Qt audio
engine remains the playback adapter.

### Sources consulted

- https://specifications.freedesktop.org/mpris-spec/latest/Player_Interface.html:
  Player methods, microsecond position/length, typed object-path track identity,
  stale SetPosition handling, PropertiesChanged and Seeked. Next/Previous preserve
  paused state. Root and Player interfaces are exported together.
- https://raw.githubusercontent.com/sailfishos/qtmpris/master/src/mprisplayer.cpp:
  Jolla's implementation uses the session bus by default, the standard service
  prefix/object path, typed metadata and explicit property-change notifications.
  Earlier guessed nemo-qml-plugin-mpris and multimedia documentation URLs were
  unavailable; qtmpris source supplies the relevant implementation evidence.
- https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/media.py:
  lyric streams are Part.Stream entries with streamType 4, advertised key and
  format (including lrc). No lyric provider URL or stream ID is fabricated.
- https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/mixins/rating.py:
  PUT `/:/rate`, numeric key, library identifier and 0–10 rating.
- https://python-plexapi.readthedocs.io/en/latest/modules/library.html:
  type-scoped section search, sort `field:asc/desc`, rating and play-count filters.
- https://plexapi.dev/api-reference/hub/global-search-across-hubs.md (located via
  llms.txt after a guessed URL failed): `/hubs/search`, query, limit per hub,
  MediaContainer.Hub.Metadata. Global search keeps numeric local music entries.

### Implementation decisions

- Raw queue, shuffle traversal, current occurrence, repeat, radio context and
  position persist atomically in private session state. Framework URLs are not
  persisted. Startup is stopped until Play; position is checkpointed every five
  seconds and on pause/seek/normal shutdown. Late checkpoints identify their track.
- Play-next inserts into playback order even under shuffle. Explicit move uses
  displayed order, preserves the playing occurrence and exits shuffle. Edits leave
  server radio mode; ordinary playback can then follow the edited queue. Removing
  the current entry resolves its replacement transactionally.
- C++ provides a thin QtDBus MPRIS adapter. Rust retains queue policy. D-Bus exposes
  title/artist/album and only local artwork, never authenticated playback/image URLs.
  Hardware headset behavior and a normally installed Sailjail app remain distinct
  checks from session-bus protocol support.
- Separate Silica/Controls2 now-playing views show artwork, transport/seek,
  favorites and plain/timed lyrics. Rust parses LRC timestamps/offsets, stores
  previously retrieved lyrics offline and confines fetches to the selected origin.
- Global music search requests up to 100 results per hub. Discovery views paginate:
  five-star favorite tracks, recently added albums and recently played tracks.
  Ratings change Plex state only after user action; validation uses synthetic PUTs.
- Track/album/playlist pins persist complete bounded plans (up to 1000 distinct
  tracks), survive restart and protect completed/partial audio from quota eviction.
  Overlapping groups retain shared tracks when one group is unpinned. Unpinning
  permits eviction rather than deleting an actively used file.
- Credential-scoped raw library snapshots and artwork are private. Offline mode
  cancels audio downloads and searches/browses saved metadata without contacting
  Plex. This saves visited pages, not a full automatic library crawl. Artwork uses
  a bounded Rust worker and 128 MiB cache (4 MiB/image); metadata/lyrics use 64 MiB,
  separately from the configured audio quota. Missing saved data remains explicit.

### Checks so far

- Local formatting/Clippy, 38 Rust tests and CTest 2/2 pass. MPRIS protocol tests
  run under an isolated dbus-run-session; native generated-audio tests exercise
  pause/play, stale-track seek rejection and actual local playback after closure
  of the fixture server. SDK production build passes with its existing lint warning.
- A combined 200-second build invocation timed out during the test packaging
  stage; rerunning `./tools/build-device-tests.sh` alone completed successfully.
- Phone Qt5 checks passed for D-Bus contract, lyric reply isolation, navigation,
  debounce and generated audio. The first combined live discovery case failed its
  library-list assertion without an underlying error; a read-only schema probe
  found expected field types and an isolated rerun passed. The precise initial
  cause is unconfirmed; the test now checks the envelope before the array.
- Live read-only results: global query 7 music entries, favorite tracks 2 rows,
  recently added and played first pages 100 rows each. Sample lyric request
  succeeded but returned zero lines, so actual server-supplied timed lyrics are
  not yet verified. No real ratings changed or audio/artwork downloaded by that
  diagnostic.

### Final runtime/deployment findings

- Final host checks retain 38 passing Rust tests and CTest 2/2. SDK production
  and device-fixture builds pass after adding QtDBus. Native Qt5 now-playing
  creation is tested inside a real Silica ApplicationWindow; a first bare-Page
  fixture exposed missing window-context warnings and was corrected.
- Playback generation is persisted and changes with new decoder plans, preventing
  old checkpoints/seeks from affecting a repeated occurrence of the same track.
  The native source is cleared on connection/logout, with a no-replay regression.
- Full rootless bundle was deployed; subsequent final C++ executable update passed
  temporary-state QML smoke and generated-audio/control/logout checks on the phone.
- Process audit exposed Sailfish's truncated comm-name behavior: exact-name pkill
  had left three old/new executable instances. Replaced the stop command with an
  anchored full executable-path match, then verified exactly one process. Final
  PID 64399 owns `org.mpris.MediaPlayer2.plexfreq`, confirmed by D-Bus PID query.
  Future deployments must verify sole executable/cache/service ownership, not just
  that a newly launched PID exists. No root/system-service changes were needed.

## JSON lyrics, playlist totals and Sailfish cover artwork — 2026-10-03

- User reported raw JSON instead of lyrics and requested playlist counts/duration
  and album artwork in the Sailfish running-app cover.
- A local regression first reproduced literal JSON rendering. The bounded,
  read-only `tools/inspect-lyrics-playlists.py` then inspected the current saved
  track's advertised lyric stream using header credentials and no redirects.
  It outputs only field names/types/counts, never lyric text, track names/IDs,
  authenticated URLs or tokens.
- Actual stream Content-Type is application/json. Its shape is
  `MediaContainer.Lyrics[].Line[].Span[].text`; the sampled document has 37 lines.
  A first live decoder check exposed legitimate blank stanza lines represented
  by `{}` rather than a Span array. The regression/decoder now preserve those
  blank lines, concatenate textual spans and reject unrelated/malformed JSON.
  Plain/LRC remains supported; previously cached literal JSON is normalized on
  offline retrieval without deleting downloads/session state.
- Consulted https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/playlist.py:
  leafCount is total item count, duration is milliseconds, durationInSeconds is
  a seconds fallback. Actual audio playlist list: 6 playlists, counts for all 6,
  duration present for 4; the two lacking duration are empty. No per-playlist
  child-list fetch is needed to display these totals. Missing nonempty duration
  remains explicitly unavailable instead of being invented.
- Both UI lists and the opened playlist view show a summary, formatting long
  totals as hours:minutes:seconds. Summary stays tied to the playlist seed while
  its tracks page in. Nullable/missing fields are tolerated.
- The existing native CoverBackground/CoverActionList presentation now includes
  a bounded square album image, track and artist captions and a missing-image
  glyph. Rust normalizes parentThumb separately as albumArtwork with existing
  origin confinement; passive polling replaces it with a cached local image when
  ready. Play/pause/next cover actions remain available. A guessed Sailfish cover
  tutorial URL returned 404; existing native cover structure was reused.
- Final local checks: 43 Rust tests, formatting/Clippy, CTest 2/2 (5.51 seconds).
  SDK production and fixture builds pass. Final phone Qt5 summary/current-lyric
  check: 4 entries passed, 6 playlist counts/durations, 37 decoded lyric lines.
- Rootless bundle deployed and main-QML temporary-state smoke exited 0 with known
  vendor diagnostics only. Single PID 2883 owns the MPRIS service; app raised to
  foreground. Native app-switcher cover appearance remains interactive review.

## Playlist editing, history, browsing, downloads and mixes — 2026-10-03

### Sources and choices

- https://plexapi.dev/api-reference/playlists/create-playlist.md and
  https://plexapi.dev/api-reference/playlists/update-playlist.md document POST/PUT
  playlist operations. The previously consulted python-plexapi playlist.py supplies
  server:// library metadata source URIs, playlistItemID removal/move and smart
  playlist restrictions. New regular playlists are seeded from selected tracks/
  albums or the queue; smart filter authoring is separate work.
- General timeline uses the already researched PMS `/:/timeline` API. Offline
  scrobbling follows python-plexapi's documented GET `/:/scrobble` with key and
  library identifier:
  https://raw.githubusercontent.com/pkkid/python-plexapi/master/plexapi/mixins/played_unplayed.py
  The community OpenAPI describes POST/ratingKey instead; GET/key matches the
  established implementation. Live diagnostics will not fabricate listening events.
- Library sort/type and album Format/Subformat metadata follow the previously
  consulted python-plexapi library/audio/media modules. Grouping uses advertised
  EP/Single/Live/Compilation tags, with unclassified entries under Albums.
- Read-only network observation uses ConnMan GetServices (sorted services,
  State ready/online and Type wifi) and desktop NetworkManager GetAll fallback:
  https://raw.githubusercontent.com/jku/connman/master/doc/manager-api.txt
  https://raw.githubusercontent.com/jku/connman/master/doc/service-api.txt
  https://networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.html
  Earlier guessed sailfishos/connman URLs were unavailable. No network properties,
  system media services or phone privileges are modified.
- Mixes balance shuffled track pools from up to 10 artist/album seeds, deduplicate
  track IDs and cap the result at 1000 tracks (up to 200 sampled tracks per artist).
  This is our transparent mix policy, not a claim to reproduce Plexamp weighting.
  Autoplay prefers actual sonic neighbors and otherwise an advertised artist
  station; it does not synthesize unsupported recommendations.

### Implementation/verification

- Playlist mutations use occurrence IDs; DTOs normalize the server's uppercase
  playlistItemID field for QML without emitting duplicate serde alias fields.
  Smart contents are protected, names/source URLs encoded, changed playlist
  snapshots invalidated and dialogs/choices separated from the main list model.
- Native elapsed/progress sampling records heard time, not seek position. Rust
  qualifies a listen at half the duration or four minutes, persists occurrence/
  credential-scoped history, acknowledges normal streaming timelines and syncs
  remaining plays through scrobble after reconnect. Original played time is retained
  locally; the replay API does not promise original server-history timestamps.
  Delivery retry is at-least-once if a server acknowledgement is lost, not a claim
  of exactly-once remote increments. Read-only inspection cannot mutate playlists
  or synchronize history.
- Artist/album/track browsing and title/added/year sorts are exposed on both UIs;
  optional artist-album grouping is bounded to 1000 albums. Grouping uses a thin
  extra presentation role, while Rust retains metadata ordering policy.
- Download manager reports per-group ready/total/bytes, active transfer and errors;
  provides pause/resume/retry/cancel/remove plus a Wi-Fi-only option. Current local
  playback files and overlapping pin groups are retained when appropriate.
- Wi-Fi-only starts fail closed when connection type is unknown. A separate
  Arc-owned Rust cancellation C-ABI handle carries only facts/policy hints and can
  cancel an in-flight transfer without waiting behind Plex HTTP. Core remains
  thread-confined. Live atomic hints cannot be overwritten by older queued network
  observations; hints arriving before worker startup are retained.
- Local fixes caught duplicate playlistItemID alias serialization, legitimate TCP
  cancellation in fixtures, and loss of the first listening interval when busy
  reporting skipped native state sampling. The sampler now runs independently of
  network-report admission.
- Current local check: 53 Rust tests, formatting/Clippy and CTest 2/2 pass
  (9.48 seconds). Fixtures exercise all new protocol/policy paths, real offline
  decoding/heard-time recording, seek non-counting, management page creation,
  cross-thread cancellation and startup/stale-network hints. SDK/device results
  are to be recorded after the build/runtime stage.

### Requested follow-ups

Inspected both reference translation trees and loaders/build scripts. They share
39 locales plus an English template. The i18n task and exact language set are
recorded in roadmap.md; catalogues/translations have not yet been implemented.
The audio replacement discussion is recorded in audio-engine-plan.md, consulting
GStreamer appsrc/appsink and Symphonia 0.6.1 support tables. No audio engine was
replaced in this iteration.

### Final device results and follow-up inspection

- SDK production and test builds pass. Final desktop CTest is 2/2 (9.63 seconds).
  Phone startup hints, management forms, read-only network facts, generated-audio
  offline history and sorted/playlist browsing cases pass. The connected default
  service was observed as non-Wi-Fi; strict Wi-Fi-only correctly treats that as
  ineligible without modifying any network settings.
- Live sorted album/track pages return 100 rows each. The chooser has 6 playlists,
  5 smart. A types/counts-only probe confirmed that nonempty samples are smart
  playlists and omit occurrence IDs; the regular sample is empty. Mutations remain
  locally protocol-tested rather than claiming a live regular-playlist edit.
- A first management fixture exposed missing native page-stack indicators; the
  installed DialogHeader.qml (read-only copy) showed the dependency. The corrected
  fixture uses ApplicationWindow/pageStack attached to a QQuickWindow. Final
  creation has only the known vendor graphics diagnostics, not QML indicator errors.
- The updated rootless bundle passed temporary-state smoke, was deployed and
  reopened as sole PID 26781. D-Bus confirmed the same MPRIS owner PID. Credentials,
  audio files, system network/media settings and drivers were retained.
- Reference i18n loaders/build scripts show full locale/base-language fallback and
  generated TS/QM dictionaries; the requested same-language translation work stays
  on the roadmap. Audio-engine planning documents Rust-owned orchestration over
  existing GStreamer infrastructure versus a pure-Rust PCM alternative, including
  codec/gapless/output limitations from the consulted sources.

## i18n and Rust audio runtime — 2026-10-04

- User committed the baseline (ae8eafb), then requested actual i18n/audio-engine
  implementation and a minimal C++ bridge, allowing architecture changes.
- Host GStreamer is 1.26.11, GLib 2.88.3, PulseAudio 17.0. SDK RPM inspection
  confirms GStreamer 1.26.11, GLib 2.86.4 and PulseAudio 17.0. Native development
  packages were absent initially; tools/audio-sdk.Dockerfile installs them only
  in a derived disposable SDK image. pkg-config queries the actual SDK sysroot.
- Consulted gstreamer-rs 0.23.7 installation/API docs (minimum 1.14), GStreamer
  playbin/URI-source, appsrc and pulsesink references:
  https://docs.rs/gstreamer/0.23.7/gstreamer/
  https://gstreamer.freedesktop.org/documentation/playback/playbin.html
  https://gstreamer.freedesktop.org/documentation/app/appsrc.html
  https://gstreamer.freedesktop.org/documentation/pulseaudio/pulsesink.html
  pulsesink stream-properties supports media.role=music without privileged changes.
- Rust now owns runtime threads, decoding orchestration/PCM, transport/timing,
  frame-stamped continuous output, successor preparation, equal-power overlap,
  ten-band EQ, metadata gain, MPRIS and network facts. The Qt bridge contains only
  QObject/list model/events, translation and window activation. Qt Multimedia and
  the C++ QThread/player/MPRIS implementation were removed.
- A local decoded-PCM regression initially exposed EOS being discarded by a bus
  error-only pop. Full bus handling fixes it. Exact two-track frame count and
  crossfade overlap/midpoint are now verified. HTTP tests exposed the host missing
  a GStreamer HTTP URI plugin; authenticated media transport was moved entirely
  to Rust appsrc readers instead of depending on that plugin or passing token URLs.
- Translation workflow follows reference full-locale/base-language/English loading,
  pre-QML translator install, TS/QM generation and packaged/rootless/resource paths.
  Generated catalogues cover 39 locales × 409 context messages (299 distinct sources),
  with placeholder checks and no unfinished entries. Exact shared reference strings
  are reused; new technical messages use authored vocabulary and compact localized
  clauses, with expanded Italian prose. Native-speaker wording polish remains useful;
  catalogue completeness is not a linguistic-review claim. Build/check needs no
  sibling projects because the expanded translations.json is self-contained.
- Host checks currently pass: 59 Rust tests, formatting/Clippy, 2 CTest entries
  (10.31 seconds), all 39 compiled QM load/placeholder tests. SDK Rust/Qt5 package
  and fixture builds succeed. Initial explicit Sailfish linkage was restored after
  link_pkgconfig stopped supplying it; no phone packages were installed.
- Phone codec/output/Qt5 catalogue checks will be recorded after local/SDK results.
- Final phone Qt5 locale/real-PulseAudio transport/logout checks pass (4 entries);
  temporary-state Italian rootless smoke exits 0. Native seek signal delivery is
  asynchronous to snapshot polling; a fixture race was corrected to wait for it.
  Final CTest 2/2 is 10.08s. Bundle deployed/reopened as sole PID 59545 with matching
  Rust MPRIS owner. Wider codec/routing/background/sandbox evidence remains pending.

## First Rust runtime regressions — 2026-10-04

- User reported ringtone volume keys during music, lyrics stuck loading and a stale
  seek thumb on later songs. Failed Rust replies lacked lyricsKey/similarKey;
  request correlation is now preserved on success and failure. Silica SliderBase
  assigns value imperatively while dragging/releasing, removing the original QML
  binding; playbackChanged now updates idle seek sliders on both platform UIs.
- Read-only phone inspection: `/etc/pulse/mainvolume-listening-time-notifier.conf`
  has `role-list = x-maemo`; stream-restore has sink-input-by-media-role:x-maemo.
  `/etc/os-release` identifies Sailfish. Rust selects x-maemo on Sailfish and music
  on other systems. No service/config/routing settings are changed.
- Consulted sources (initial guessed src/modules/config paths returned 404):
  https://github.com/sailfishos/pulseaudio-modules-nemo/blob/master/src/mainvolume/module-meego-mainvolume.c
  Its notifier watches configured roles and publishes background MediaState for
  running matching sink inputs. Peer API: /com/meego/mainvolume2,
  com.Meego.MainVolume2.MediaState, discovered using PulseAudio ServerLookup1.Address.
  https://github.com/sailfishos/pulseaudio-modules-nemo/blob/master/src/common/include/meego/proplist-nemo.h
  https://github.com/sailfishos/pulseaudio-policy-enforcement/blob/master/examples/xpolicy.conf
- Installed Silica Slider.qml/private/SliderBase.qml consulted read-only; down
  indicates active interaction. Local tests verify a broken value binding follows
  the next song/progress and stale lyric errors are discarded while current errors
  leave loading. Rust role-selection and actual failed request key tests pass.
- Local check: 60 Rust tests, fmt/Clippy, desktop CTest 2/2 (10.30s).
- Real current-track inspection exposed gain/albumGain parsing: optional analysis
  metadata can contain numeric strings. Finite optional gain now accepts strings/
  numbers/null and does not reject track/lyric streams for unusable analysis. New
  local schema tests pass. Live probe now decodes 28 lyric lines; a transient
  provider network error passed on retry. Diagnostics printed counts/types only.
- Final check: 61 Rust tests, fmt/Clippy, CTest 2/2 (10.30s), SDK app/test builds pass.
  Phone optional-reply/slider/native policy tests pass (6 entries); real lyric probe
  passes (3 entries). Policy fixture waits for playback startup before observing
  asynchronous MediaState. Bundle reopened as sole PID 64815, matching MPRIS owner.
- Live read-only inspection found a second lyric failure: newly introduced gain/
  albumGain fields rejected numeric-string analysis values, invalidating full track
  metadata and its lyric streams. Optional finite-gain decoding now accepts numbers,
  strings and null; unusable analysis values become absent without rejecting music.
  Local schema regression covers these cases. The live current-track probe changed
  from malformed metadata to successful decoding of 28 lyric lines (one transient
  provider network failure was followed by a passing retry).
- Final check: 61 Rust tests, fmt/Clippy, CTest 2/2 in 10.30s; SDK app/test builds pass.
  Phone tests pass: correlation/stale lyric replies, damaged-slider binding across
  track change, native transport and PulseAudio MediaState recognition. The volume
  fixture waits until playback starts before reading its asynchronous policy state.
  Bundle deployed/reopened as sole PID 64815 with matching Rust MPRIS owner.

## Reference CI replication — 2026-10-04

- Consulted ElectricEel and sailfish-proton `.github/workflows/ci.yml` and
  `release.yml`, plus ElectricEel's Qt wheel bootstrap. Both separate branch push/PR
  gates from v-tag/manual SDK packages; Proton also cross-compiles Rust.
- PlexFreq CI adds Rust, desktop Qt, all-QML syntax, translation freshness and
  actual-SDK GNU aarch64 jobs. Release reuses quality gates, verifies committed
  versions/RPM architecture, uploads artifacts and publishes only on tag events.
- Translation checks previously required a populated reference cache. Pinned
  6.11.2 bootstrap works from a clean cache and matches committed TS/QM/resources.
- Checks: actionlint 1.7.11, Ubuntu 24.04 Qt5 qmllint, clean-cache i18n, desktop
  CTest 2/2 (10.30s), SDK cross command/app packaging and version preflight pass.
  Mismatched and shell-looking tags are rejected. GitHub execution remains pending.
- Full Rust run finds one pre-existing untracked production-review failure:
  trim_history_bounds_shuffled_radio_growth keeps 150 items, expected <=101.
  Other review cases pass; the complete CI test gate is retained.
  Final recheck of the updated working tree passes all 65 Rust tests, including
  the four production-review cases; no test files were changed by the CI setup.

## Production review fixes — 2026-10-04

- Consulted std `Mutex` poisoning docs (recover via `into_inner`), ConnMan
  `GetServices` favourite-first ordering, reqwest redirect/header behaviour, and
  Python `fcntl.flock` for cross-process bootstrap locking.
- Queue `append` now enforces the 10000-track cap like `replace`/`insert`;
  cache `schedule` rejects over 1000 tracks to match its message and the
  `CacheTracks`/`Pin` entry points (was 1021).
- Plex `resources` skips malformed server entries (trims `provides` caps) and
  only errors when server entries existed but none decoded; mixed valid+broken
  discovery returns the valid servers.
- Artwork `pending` maps file->epoch, `cancel` clears it, and stale worker jobs
  only remove their own epoch entry, so reschedule-after-cancel is not lost.
- Audio checkpoints use non-blocking `try_send` with a bounded worker-drained
  overflow instead of blocking the GUI thread; `plans` locking is single-shot
  and poison-safe; ConnMan wifi reflects the primary (first ready/online)
  route so `wifi_only` cannot run over cellular while a dormant wifi service
  is listed.
- Checks: `cargo fmt --check`, `cargo clippy --locked --all-targets
  --all-features -- -D warnings`, full `cargo test --locked --all-features`
   pass (lib 14, production_review 7, all other suites green).

## Review follow-ups — 2026-10-04

- Inspected the existing working-tree fixes and six `review_followup` reproductions
  before extending them. All six passed on the first local run; that run also
  exposed an unused prune wrapper that would fail the warnings-as-errors gate.
- Consulted RFC 9110 again: https://www.rfc-editor.org/rfc/rfc9110.html,
  sections 13.1.5, 14.4 and 15.3.7. Streaming now sends If-Range when a strong
  ETag/Last-Modified is known, shares the cache range/validator helpers, checks
  ranges/lengths/known representation changes, and only skips a full-response
  prefix when its validator matches the original. Validator-less 206 responses
  remain supported with range/size checks; they cannot prove representation identity.
- Queue edits retain the last audible occurrence after natural end, including
  moving that occurrence itself and toggling shuffle; removing the final slot
  also clamps the persisted cursor. Offline detail fallback slices 100-item pages.
  Snapshot/lyric/artwork overwrites exclude the replaced file from quota accounting
  and use actual serialized sizes. Audio-cache startup reclaims old/new temporary
  filenames, and unreclaimed temporary bytes are visible to quota accounting.
- Album/chooser/grouping caps now check before accepting a final page: exactly
  1000 is allowed, including an unknown-total album's empty terminal page.
- Runtime view generations already existed at execution time. The uncovered gap
  was delivery: completed replies could wait in the event channel across a new
  navigation. Poll now discards those replies and stale failures too. Page replies
  carry their requested start; Session resets page admission when routes change.
- Inspected `src/ffi.rs`: directory/request C strings are consumed synchronously.
  Inline QByteArray temporaries survive the complete C++ call expression, so the
  reported dangling pointers are not reproduced here. Named byte arrays make that
  contract explicit; null returned strings have a defined unavailable envelope.
- Reduced Qt JSON polling from 40 to 10 Hz, reused one validated cache-status
  snapshot for download groups, cached offline sort keys, removed checkpoint's
  successful-send clone, and removed redundant successor queue copies/scheduling
  on the ordinary preparation path. No measured battery/performance claim.
- SDK packaging starts from a fresh disposable app copy and explicitly propagates
  build/ownership-cleanup failures. Actual SDK compilation produced the RPM and
  rootless bundle with 0 rpmlint errors and the existing no-url-tag warning.
- Local final gates: fmt/Clippy, 89 Rust tests, CTest 2/2 (13.00s), 39-locale
  freshness, Bash syntax and whitespace pass. The final combined check timed out
  during CTest after concurrent SDK/Cargo-lock contention; standalone CTest passed.
  No phone or live-account experiment was performed.
