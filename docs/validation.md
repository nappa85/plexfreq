# Validation record

## Sailfish navigation flash — 2026-10-05

| Check | Actual result |
| --- | --- |
| Initial retained-page regression | Failed: root heading became Artist before navigation, expected Artists |
| Page-local presentation regression | Passed: outgoing root/artist headings, detail identity and rows retained; back/reload snapshot and stable model identity checked |
| Rust / formatting / Clippy | Passed: existing 107 Rust tests and all-feature warnings-as-errors gates |
| Final desktop rebuild + CTest | Passed 2/2, 13.82s; native Silica-only case intentionally runs on Qt5 |
| SDK production and expanded Qt5 fixture | Passed; existing package lint warnings only |
| Phone snapshot + native forward stack | Passed: 4 lifecycle/test entries, no skips/failures |
| Final fixture transfer (native pop/reload extension) | Blocked: SSH connection timeout; not run on hardware yet |

The phone case instantiated the shipped LibraryPage/ArtistPage/AlbumPage under a
real Silica ApplicationWindow/pageStack with synthetic metadata and an isolated
session bus. It checked outgoing page content during real pushes, not just a
standalone controller. The later fixture correction supplies the mock backend's
loadingMoreChanged notification and additionally tests native pop/reload.
No real Plex/media requests or credential fixtures were used. Deployment has not
occurred while developer SSH is unreachable.

### Retry completed — navigation fix deployed

| Check | Actual result |
| --- | --- |
| Final Qt5.6 phone snapshot + real Silica push/pop/reload fixture | Passed: 4 lifecycle/test entries, 0 failures/skips |
| Temporary-state production QML smoke | Exit 0; known vendor graphics/isolated-bus messages only |
| Rootless deployment/restart | Completed in existing directory; credentials, saved queue and downloads retained |
| Process/MPRIS ownership | Exactly one executable-path match: PID 24916; D-Bus owner PID matches |
| Raise / temporary fixture cleanup | Raise dispatched; temporary navigation directory removed |

The new presentation fix is now running on the phone. This resolves the SSH
transfer/deployment blocker recorded above. Fixtures used synthetic metadata and
isolated session buses; no live Plex mutations or fabricated account listens.

## Local test design

`tests/core.rs` exercises repeat/skip/end, shuffled traversal/current retention,
origin confinement, URL token encoding, real local HTTP request headers/paths,
music filtering, pagination, direct-stream planning/artwork fallback, private
session permissions, reload/logout, pending/approved/expired PINs, resource
filtering/redaction and corrupt state. `tests/rollback.rs` checks that failed
metadata resolution and authorization leave the current queue/session intact.
`src/ffi.rs` tests malformed/null requests and paired C-ABI memory ownership.
Fixtures are intentionally synthetic.

Desktop CTest loads the shipped QML offscreen with temporary application state.
`tests/qt_backend.cpp` runs a local Qt TCP server with JSON and generated PCM WAV,
including HTTP byte-range support for seeking. It exercises worker completion,
token headers, connection/library data, actual decoding/playback state,
pause/resume/seek, manual next, automatic EndOfMedia, and logout. Silence is
generated so no media files, account, external network or phone are required.
Sailfish compilation uses the exact Qt5.6 SDK rather than assuming modern Qt
compatibility. Compilation is not a phone-runtime verification.

## Results

Observed on 2026-10-02, Gentoo x86_64, Rust 1.95.0, Qt 6.11.2:

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked` | Passed: 8 Rust tests, 0 failures |
| CMake Release desktop build | Passed |
| `ctest --test-dir build/desktop --output-on-failure` | Passed: 2 tests, 0 failures; final run 3.09 seconds |
| SDK aarch64 GNU Rust staticlib | Passed using actual SDK C compiler/sysroot |
| `mb2 --target SailfishOS-5.2.0.15-aarch64 build` | Passed; Qt5.6 shim linked and RPM produced |
| SDK rpmlint | 0 errors; one `no-url-tag` warning (no public project URL assigned) |
| `bash -n tools/check.sh tools/sdk-cc.sh tools/build-sailfish.sh tools/smoke-test.sh` | Passed |

The complete `./tools/check.sh` finished successfully after the smoke-test
wrapper was updated to create/clean its session fixture in a temporary directory.

Artifacts:

- `build/desktop/plexfreq`: x86_64 desktop executable.
- `build/sailfish/RPMS/harbour-plexfreq-0.1.0-1.aarch64.rpm`.
- `build/plexfreq-0.1.0-aarch64-rootless.tar.gz`: stripped aarch64 executable + QML.

The desktop smoke test emits two Qt6 deprecation notices for shared QML's old
Connections/signal-parameter syntax. That syntax is intentionally compatible
with the Qt5.6 phone target; there are no QML load errors. Native audio test
passes without decoder errors after its fixture gained proper Range responses.
Qt/FFmpeg diagnostics can include the source URL and token query: application
errors are generic/redacted, but framework diagnostics are not guaranteed to be.
Tests use a synthetic `test-token`, never an actual account credential.

## Local corrections found before device testing

- Desktop Controls2 import needs 2.15 for Dialog/modern Slider signals; shared
  and Silica remain on QtQuick2.6.
- SDK ring compilation needed explicit GCC `-B` target assembler prefix.
- The image's default mersdk identity is UID/GID 100000; use a disposable build
  copy, container chown, direct mersdk process execution and ownership cleanup.
  `runuser` could not create a PAM session inside this image.
- `sailfishapp.prf` overrides qmake's `icon` group; use `launchericon` instead.
- RPM changelog/license/stripping corrected against SDK rpmlint. SDK license
  spelling is `GPLv3+`; Cargo uses SPDX `GPL-3.0-or-later` for the same license.
- Native test fixture accepts requests with an empty query suffix, supports
  HTTP ranges, and checks JSON null via QVariant::isNull (not isValid).
- Initial 120-second tool deadlines were too short for cold parallel builds;
  completed verification used longer deadlines. These were host checks only.

## Not yet validated

- Broader live Plex coverage: direct connection is user-confirmed, and account
  server discovery is verified below. Shared libraries, multiple server versions
  and account-discovered connection selection still need coverage.
- Interactive Silica connection-page/navigation behavior and visual quality,
  audio codecs beyond PCM WAV, call/Bluetooth routing, sustained/background
  playback and sandbox permissions on hardware. Main QML loading and short
  native playback have now been verified below.
- Other Linux distributions, other architectures and older Sailfish releases.
  This RPM targets Sailfish 5.2 aarch64; generated requirements include GLIBC_2.39,
  available in this SDK's glibc 2.41. Do not assume it runs on older phone images.

## Phone validation — 2026-10-02

Device: `defaultuser@192.168.1.120`, aarch64 SailfishOS 5.2.0.18. Ordinary
developer SSH only; no root, installed packages or system-service changes.

| Check | Actual result |
| --- | --- |
| SSH identity/version/session environment | Passed; matches relevant SDK ABI; graphical session environment inherited |
| Rootless executable `ldd` | All shared libraries resolved |
| Rootless `harbour-plexfreq --smoke-test` with mktemp state | Exit 0; no QML load errors; vendor graphics diagnostics noted in research |
| `./tools/build-device-tests.sh` | Passed; same fixture built against Qt5.6 in SDK |
| Physical `plexfreq-backend-test` | 3 QtTest entries passed (init, integration case, cleanup), 0 failures/skips |
| Interactive rootless app launch | Started PID 61850; still running after 2 seconds and SSH detach |

The native integration case verifies a local loopback Plex-shaped server,
synthetic token/header handling, generated PCM WAV decoding, duration and
advancing position, pause/resume, seeking, manual next, automatic queue end and
logout. Its state is in a temporary directory. It neither contacts real Plex nor
asserts audible output from the speaker. GStreamer emitted only
`GST_MESSAGE_STREAM_COLLECTION` debug messages during the passing test.

The complete host `./tools/check.sh` passed again after preparing the same fixture
for Qt5: 8 Rust tests and CTest 2/2 (3.15 seconds), formatting/Clippy passed.
`bash -n tools/build-device-tests.sh` also passed.
A temporary QtTest compatibility macro was removed after the
SDK header showed that the timeout macro is already available; this correction
was made at host compile time, before the phone fixture was run.

Deployment directory: `/home/defaultuser/plexfreq-test.mTmhFP`. Contains the app,
adjacent QML, and standalone test executable; there is no new application-menu
launcher or RPM installation. The interactive app uses the normal per-user state
path. Its shell stdout/stderr goes to `/dev/null` for subsequent real sign-in.

FLAC/MP3/AAC-specific coverage, browser return/user-flow confirmation, sustained
lock-screen playback, Bluetooth/call routing and an installed Sailjail launch
remain pending. Real-account findings follow.

## Browser sign-in regression fix — 2026-10-02

User confirmed direct URL/token connection worked, then reported an unexpected
response after browser login. Saved-account flags showed authorization succeeded.
A local fixture with missing/null player accessToken reproduced the discovery
failure **before** the fix. A value-free phone diagnostic confirmed the same
shape: one valid server and one player with a null token.

| Check | Actual result |
| --- | --- |
| Updated PIN → authorization → mixed-resource local regression | Failed with Protocol before fix; passed after fix |
| Strict malformed-server/context/redaction regression | Passed; non-server fields ignored, malformed server rejected with a static stage label |
| `./tools/check.sh` | Passed: 9 Rust tests, 2 CTest tests, formatting/Clippy; CTest 3.13 seconds |
| Production SDK build | Passed; explicitly relinked after adding Rust archive prerequisite; rpmlint 0 errors/1 no-url-tag warning |
| Native Qt5 test build | Passed; live account case added as opt-in |
| Phone `savedAccountDiscovery` using fixed Rust core | Passed: server count 1; init/test/cleanup passed, 0 failures/skips |
| Updated interactive application | Deployed/restarted PID 64356; still running after 2 seconds |

The host native test intentionally skips `savedAccountDiscovery` when no state
directory is explicitly supplied. Its synthetic audio fixture still runs. The
phone live check uses the existing private state in place; no token, account name,
server name, address or response payload is printed. Existing credentials and
direct-server settings were preserved during deployment.

The SDK initially packaged the old application despite a rebuilt Rust archive
(`make: Nothing to be done`). The `.pro` archive prerequisite was corrected, and
the production link command was observed before the executable was deployed.

Browser authorization itself was successful before the fix. The corrected
post-authorization discovery is now live-verified; final interactive confirmation
is Connection → Refresh servers → select server, or a new browser login.

## Server-list visibility update — 2026-10-02

User confirmed sign-in/server selection now works, but reported that the server
list was too inconspicuous. Both connection screens now distinguish discovery
from direct URL entry using a large selection title, explicit guidance and
highlighted cards with server names, action subtitles and chevrons. Desktop
lists scroll when several servers are present.

- `cmake --build build/desktop --parallel`: passed.
- `ctest --test-dir build/desktop -R desktop_qml_smoke --output-on-failure`:
  passed, 1/1, 2.01 seconds. This loads QML; it is not a screenshot/layout test.
- `./tools/build-sailfish.sh`: passed; updated RPM and rootless bundle produced.
  rpmlint: 0 errors, existing single `no-url-tag` warning.
- Phone `qml/pages/SettingsPage.qml` updated atomically and app restarted as PID
  354; process still running after 2 seconds. Existing saved state retained.

No backend change or additional live-account calls were needed. The new cards'
appearance and interaction still require visual user confirmation in Connection
  → Refresh servers; no automated screenshot/interaction was claimed.

## Radio implementation and validation — 2026-10-02

Local tests were developed before live capabilities/queue checks:

- Station child JSON variants, query preservation and origin/identity validation.
- Artist station creation with exact source URI and POST/audio parameters.
- Queue continuation after the loaded window, occurrence-ID deduplication (same
  song may occur again), and correct timeline method/state/queue IDs.
- Complete album order, including partial-page album expansion.
- Track sonic neighbors, explicit radio exit through normal Play/Stop radio.
- Unanalyzed/unavailable seeds and failed refill preserve previous playback;
  retry succeeds with a later valid recommendation.
- Bounded radio history retains current track and previous/next ordering.

| Check | Actual result |
| --- | --- |
| `./tools/check.sh` | Passed: 15 Rust tests, formatting/Clippy, 2 CTest entries |
| Final desktop rebuild/CTest after Qt5/Qt6 diagnostic-count type correction | Passed: 2/2, 3.18 seconds; no diagnostic format warning |
| Production SDK package and rootless bundle | Built; rpmlint 0 errors, existing no-url-tag warning |
| Qt5 device fixture | Built; opt-in radio planning available |
| Read-only capability probe on saved server | Artist Stations present; sampled album/track unanalyzed, nearest count 0 |
| Phone `savedRadioPlanning` | 3 lifecycle/test entries passed; artist window 5 → 10 tracks; album/track samples correctly unavailable |
| Updated phone app QML smoke with temporary state | Exit 0; existing vendor graphics messages, no QML load error |
| Phone generated-audio regression | 3 lifecycle/test entries passed, 0 failures/skips on Qt5/GStreamer |
| Interactive app after deployment | Started PID 5935 and stayed running after 2 seconds |

Live radio planning uses the saved account on the phone, creates an ephemeral
station queue and requests metadata continuation. It does not fetch media bytes,
play real music or submit pretend playback events. Actual artist-radio listening,
long-session continuation and native timeline behavior still need interactive
confirmation. Album/track radio has local protocol coverage but cannot be
live-validated on the sampled unanalyzed seeds. No server settings were changed.

The updated UI has Radio actions beside artists, albums and tracks, plus an active
radio banner and Stop radio action. Shuffle/repeat are disabled during radio;
failed requests show the requirement message and retain the prior queue.

## Artist-first UI and lifetime-Pass clarification — 2026-10-02

The account check confirmed active lifetime Plex Pass and PMS subscription=true.
The selected music library setting is `musicAnalysis=false`. A missing metadata
version is no longer treated as an entitlement/availability result: the new
regression failed before the fix and passed after using actual nearest results.

New local coverage:

- Valid sonic recommendations despite an omitted analysis-version field.
- Artist biography, artwork/gallery normalization, unsafe image-path rejection,
  album descriptions/year and paged artist/album children.
- Nullable optional metadata and signed unknown year/track/disc values.
- Shared QML root=artists, artist→album→back history, restored search query and
  correct artist radio seed. Desktop actual detail-header/icon instantiation.

| Check | Observed result |
| --- | --- |
| `./tools/check.sh` | Passed: 18 Rust tests, formatting/Clippy, 2 CTest entries, 3.22 seconds |
| SDK app/RPM/rootless build | Passed, production relink observed; 0 rpmlint errors/1 existing no-url-tag warning |
| Expanded Qt5 fixture build | Passed with QML/Quick and shared-controller resources |
| Phone navigation + live detail + radio planning cases | 5 entries passed (init + 3 cases + cleanup), 0 failures/skips |
| Sample artist detail | 4 albums, 1 photo, biography present |
| Sample album detail | 14 tracks, 1 cover, description absent |
| Artist radio planning | 5 initial queue entries → 10 after continuation |
| Updated app phone QML smoke (temporary state) | Exit 0; existing vendor graphics diagnostics only |
| Interactive rootless relaunch | PID 13885, running after 2 seconds |

The opt-in checks print only counts/flags and static errors; they fetch metadata,
not real audio/artwork. Default host tests skip the three live-account cases.
One prior combined run failed track browsing without a specific error; the final
instrumented/rebuilt run passed. No exact underlying cause was established by
the field-type-only probes. Native back gestures, gallery layout and icon visual
quality still require interactive user confirmation; no screenshot test is claimed.

The application now starts with artists, has separate artist/album detail views,
and uses antenna/waves radio actions instead of captioned buttons. The empty
recommendation message points to library analysis, not a missing Plex Pass.

## Detail header/list overlap regression — 2026-10-02

The new `detailHeaderReservesSpaceForRows` test reproduced the faulty sizing:
Loader height 0 vs content implicitHeight 1000. It passes after the shared loader
uses implicitHeight. The test runs an actual QQuickWindow and verifies wrapped
text resizing at narrow width, following-row placement, and unload/reactivation.
Only generated fixture content is shown; no media/user state is involved.

| Check | Observed result |
| --- | --- |
| Initial geometry regression with old expression | Failed: reserved height 0, intrinsic height 1000 |
| Fixed geometry case on desktop | Passed: init, geometry case, cleanup |
| `./tools/check.sh` | Passed: 18 Rust tests, formatting/Clippy, 2 CTest entries (3.60 seconds); geometry included in native suite |
| SDK production/RPM/rootless build | Passed; rpmlint 0 errors/1 existing no-url-tag warning |
| Expanded Qt5 test executable | Built successfully |
| Phone geometry case on Qt5.6/Wayland-EGL | 3 entries passed, 0 failures/skips; existing graphics diagnostics only |
| Rootless app update/relaunch | PID 15113, running after 2 seconds |

The fix reserves header space before albums/tracks, bounds image/control painting,
and retains Silica's native high-z pull-down menu. The phone regression verifies
the shared sizing mechanism with synthetic content, not a screenshot of the full
real-data artist/album pages. No new live Plex calls were needed.

## Radio pixel stability and persistent audio cache — 2026-10-02

- Shared icon no longer uses Canvas; native geometry is scaled/clipped as a unit.
  Its pixel test grabs only fixture content in memory and verifies full mast/wave
  coverage after 32/96/24/72/40/32-size changes and visibility reuse.
- New cache local tests exercise completed-file persistence/scoping/permissions,
  partial-not-playable behavior, validated Range + If-Range continuation, ignored
  Range restarting with 200, malformed/non-audio/oversized response rejection,
  restart recovery, clear/cancel races, total audio quota with active-track
  protection, offline catalogue/local URL selection, and shuffled prefetch order.
- The Qt native-audio fixture waits for two completed downloads, closes its local
  HTTP listener, advances to the second track and checks playbackSource=cache,
  actual decoding, seeking and automatic completion. No internet is needed.

| Check | Result |
| --- | --- |
| Final `./tools/check.sh` | Passed: 28 Rust tests, formatting/Clippy, 2 CTest entries; 4.93 seconds |
| SDK app build after final quota/config change | Passed; new RPM/rootless bundle available; 0 lint errors/1 existing no-url-tag warning |
| Qt5 test fixture build | Passed; new icon/offline cases compiled |
| Phone fixture transfer/launch | Blocked: No route to host, 192.168.1.120 |
| Phone connection recheck | Same failure; no deployment occurred |

`savedCacheDownload` is an opt-in check that will download at most one selected
track up to 8 MiB and verify a local-file playback plan without playing it. It
prints only byte count. This real-Plex check, phone pixel capture, phone offline
native decoding and the new cache-controls UI have not run yet. Existing phone
deployment remains the earlier layout-fix version until SSH is restored.

## Cache update resumed on phone — 2026-10-03

| Check | Actual result |
| --- | --- |
| Restored SSH | Available at the existing 192.168.1.120 address |
| Qt5 fixture rebuild/transfer | Completed |
| Phone local-file audio after fixture server close | Passed: init/test/cleanup, 0 failures/skips |
| Phone icon pixel capture | Blocked by unexposed window; MCE display status was off |
| Initial bounded real download | Failed size comparison; no partial file published |
| One-byte real media Range diagnosis | 206, HTTP total 1388080 vs metadata 1390392, audio/mpeg, ETag present |
| Local representation-size regression | Failed before fix; passed after HTTP framing became authoritative |
| Final local checks | Passed: 29 Rust tests + 2 CTest entries (4.84 seconds), formatting/Clippy |
| Final SDK app and Qt5 fixture builds | Passed, existing lint warnings only |
| Phone real cache check after fix | Passed: 1388080 completed bytes, local-file plan validated, no real playback |
| Updated phone QML smoke | Exit 0, existing vendor diagnostics, no load error |
| Rootless cache update/relaunch | PID 36494, running after 2 seconds |

The real-file case stores only user-owned audio/index metadata and prints byte
count; credentials remain on the phone. Completion now uses HTTP representation
framing, not possibly stale database Part.size. That correction does not relax
resume Content-Range/If-Range validation or permit partial-file playback.

Phone icon capture can be resumed with an unlocked/on display. Native decoding
of generated cached audio and a real authenticated media download are verified;
uncontrolled radio/signal-loss sessions, storage pressure and cache settings
interaction still need longer field validation.

## Pulse/layout and transparent artist browsing — 2026-10-03

Before changing busy accounting, the polling regression failed with busyChanged
count 2 for one cache_status call. After the fix it expects zero busy/state/cache
rebind signals for unchanged passive status. Page fetches use a separate loading
flag and stable-height footer, not a changing header spinner.

Added checks:
- Incremental model row insertion with no reset and retained entry identity.
- Pagination starts at 75%, deduplicates repeated threshold calls, stops automatic
  retry after failure and sends only the latest debounced search input.
- Narrow actual desktop album-action row confines both play/download controls.
- Rust firstCharacter count offsets, validated letter lookup and page replacement;
  UTF-8/special-character server search encoding.

| Check | Result |
| --- | --- |
| `./tools/check.sh` | Passed: 30 Rust tests, formatting/Clippy, 2 CTest entries; 5.62 seconds |
| SDK app/RPM/rootless + test build | Passed; existing lint warnings only |
| Server index shape probe | 28 groups, 503 artist entries; key/size/title schema |
| Phone passive/model/pagination/debounce/index-jump cases | 6 entries passed, 0 failures/skips |
| Live jump metadata | Offset 313, 100 rows fetched without walking earlier pages |
| Deployment + connection recheck | Both blocked: No route to host before any transfer |

The new UI/layout files have not been deployed. Rootless phone state/binary remain
the prior cache-update version. Native action-row appearance, alphabet touch and
real scrolling/pulse removal need interactive confirmation after deployment.
The checks used synthetic status/list fixtures and metadata-only live queries;
no token or library title/address was printed.

### Prepared browsing/layout update deployment completed — 2026-10-03

- SSH restored at `defaultuser@192.168.1.120`.
- Current prepared RPM/rootless build's executable and QML deployed to
  `/home/defaultuser/plexfreq-test.mTmhFP`.
- Phone `--smoke-test` with temporary state: exit 0, known vendor diagnostics only.
- Normal user-state relaunch: PID 58810, still running after 2 seconds.
- The new passive-poll handling, stable list models, bounded album controls,
  automatic paging, debounced search/alphabet rail and artist-list radio removal
  are now installed. No additional broad tests repeated after previously passing
  local/Qt5 checks; final visual and gesture confirmation is still user-led.

The first cache guarantees only completed-file availability. Initial unprepared
streaming, outage before download completion, recommendations beyond a cached
radio window, artwork/library browsing and offline play-history sync are not
covered by this implementation. Cache storage is user-owned and bounded; no
phone root, system-network change, installed helper or altered Plex preference
was used.

## Similar artists — 2026-10-03

| Check | Actual result |
| --- | --- |
| `./tools/check.sh` | Passed: 31 Rust tests, formatting/Clippy, CTest 2/2 in 6.22 seconds |
| `./tools/build-sailfish.sh` | Passed: aarch64 GNU Rust, Qt5.6 app, RPM/rootless bundle; existing no-url-tag warning |
| `./tools/build-device-tests.sh` | Passed; existing fixture lint warnings only |
| Phone `artistAlbumNavigationAndRadioSeed savedLibraryDetails` | 4 QtTest entries passed, 0 failures/skips |
| Metadata-only live similarity | Sampled artist returned 1 similar artist; endpoint available |
| Temporary-state phone QML smoke | Exit 0; known vendor diagnostics, no QML load errors |
| Rootless deployment/relaunch | Completed in existing directory; PID 60034 running after 2 seconds |

Rust fixtures verify endpoint/query, artist-only/self/duplicate/invalid-key
filtering, empty responses and separation from the album list. Shared QML tests
verify related-artist activation, back history and ignoring another artist's late
reply. Desktop checks instantiate the real header. Live inspection uses
`pf_new_inspect` and prints only counts/availability, without media/image downloads.
Full native card rendering and touch behavior still need interactive confirmation;
the package build and empty-state main-QML smoke do not establish those properties.

## Selected priorities 1–5 — 2026-10-03

| Check | Actual result |
| --- | --- |
| `./tools/check.sh` after final Rust/queue-generation changes | Passed: 38 Rust tests, formatting/Clippy, CTest 2/2 in 5.53 seconds |
| Final C++ source-reset correction: desktop rebuild + CTest | Passed: 2/2 in 5.58 seconds |
| SDK production/test builds | Passed; Qt5DBus included; existing package lint warnings only |
| Phone MPRIS/lyrics/navigation/debounce/generated-audio cases | All five selected cases passed; first combined live case failed separately as described below |
| Phone real Silica ApplicationWindow + now-playing fixture, MPRIS and native audio | 5 lifecycle/test entries passed, 0 failures/skips |
| Final phone native audio/control/logout-no-replay regression | 3 entries passed, 0 failures/skips |
| Isolated metadata-only live discovery/lyrics | 3 entries passed: search 7 music rows; favorites 2; added/played 100 each; sampled lyric request successful with 0 lines |
| Rootless main QML smoke with temporary state | Exit 0; known vendor graphics diagnostics only |
| Final process/service ownership | Exactly one executable-path match, PID 64399; D-Bus GetConnectionUnixProcessID returned 64399 |

Coverage added:
- Queue insertion under shuffle, current-occurrence retention during moves/removals,
  stop when removing the last active occurrence, serialization/restart and cached
  resume position; stale playback-generation checkpoints cannot overwrite it.
- Actual isolated-session-bus Properties.GetAll/method contract, typed metadata,
  decoder pause/play, microsecond seek, stale-track rejection and no replay after
  logout. CTest uses `dbus-run-session` to avoid interacting with the user's player.
- LRC multiple timestamps/offsets/plain text and current-track reply isolation;
  real platform now-playing components load with fixture lyric delegates.
- Synthetic global Hub.Metadata search with music filtering/query encoding,
  collection sort/filter requests and favorite rating PUT/cache update.
- Album pin plans survive restart/unpin, permanent protection survives job cancel
  and respects quota; saved artist/album pages, private artwork and retrieved lyrics
  remain available after the fixture server is destroyed.

The initial combined live discovery case failed at an empty-library assertion
without checking its error envelope. A value-free schema probe showed expected
types; an isolated rerun succeeded. The exact initial cause is unconfirmed. The
test now checks the library envelope before examining its array.

An initial bare Silica Page test emitted missing ApplicationWindow-context warnings.
The final fixture creates the real Silica ApplicationWindow and loads its initial
NowPlayingPage; the final phone run has no such QML-context warnings.

Final deployment found that `pkill -x harbour-plexfreq` did not match Sailfish's
truncated process name. Three executable-path matches existed (60034, 64049,
64314); merely checking a newly launched PID had not established sole cache/service
ownership. They were stopped with an anchored exact full-executable-path match.
The final relaunch has exactly one matching process and the MPRIS service PID is
that same process. Use this full-path stop/ownership check for future deployments.
Session credentials and existing audio files were retained.

Remaining hardware checks: real lock-screen/headset controls, native long-press
queue/pin interaction, long offline/storage-pressure sessions and installed
Sailjail behavior. The sampled server track has no lyrics; actual server-provided
timed lyrics remain unverified. Offline library mode covers saved pages/catalogue,
not an automatic complete-library crawl. Normal/offline playback history syncing
is still separate from the implemented PMS recently-played view.

## Lyrics decoding, playlist summaries and app cover — 2026-10-03

| Check | Actual result |
| --- | --- |
| Initial JSON-as-text regression | Reproduced literal JSON in lyric lines |
| Value-free lyric/playlist shape probe | PMS JSON spans, including empty Line objects; 6 playlist counts, 4 durations, 2 empty playlists without duration |
| `./tools/check.sh` | Passed: 43 Rust tests, formatting/Clippy, CTest 2/2 in 5.51 seconds |
| SDK app and test fixture builds | Passed; existing lint warnings only |
| Phone `playlistSummarySurvivesTrackNavigation savedCurrentLyricsAndPlaylistStats` | Final run 4 entries passed, 0 failures/skips; 6 count/duration summaries and 37 decoded current-track lyric lines |
| Updated native main-QML smoke, temporary state | Exit 0; known vendor messages only, no QML load errors |
| Deployment/process ownership | Single PID 2883; MPRIS service PID matches; raised to foreground |

The initial live lyric check rejected blank `{}` Line entries as missing spans.
A types-only probe confirmed these are stanza separators. Added a local regression
for absent/null Span and reran checks before the passing live run.

Regressions cover multi-span line extraction, empty lines, malformed/unrelated
JSON rejection, old literal-JSON cache repair, HTTP JSON-to-offline storage,
playlist totals independent of loaded-page size, null/empty/seconds fallback cases,
hour formatting/seed retention and album-thumbnail origin confinement.

Live checks use read-only Rust inspection and print counts only. No audio/images,
names or lyric text are exported. Deployment paused the old app before replacement,
retained credentials/audio, and used the anchored full-executable-path stop check.
The cover image source and main QML load are checked; no app-switcher screenshot
or automated pixel assertion of the native cover was performed.

## Playlist/history/library/download/mix expansion — 2026-10-03

| Check | Actual result |
| --- | --- |
| `./tools/check.sh` after final Rust/FFI changes | Passed: 53 Rust tests, formatting/Clippy, CTest 2/2 |
| Final C++/fixture desktop rebuild + CTest | Passed: 2/2 in 9.63 seconds |
| SDK app/RPM/rootless and test builds | Passed; existing lint warnings only after the indentation warning was corrected |
| Phone startup-hint/management/network/native-history/browsing cases | Five selected cases passed (7 lifecycle/test entries), 0 failures/skips |
| Read-only live sorted browsing | Album and track year-sort first pages: 100 rows each |
| Read-only live playlist chooser | 6 playlists, 5 smart; regular playlists have no nonempty sample, so regular occurrence-ID editing is protocol-tested locally |
| Final phone management-page/window fixture | 3 entries passed with known vendor graphics diagnostics, no DialogHeader/QML-context warnings |
| Temporary-state rootless main-QML smoke | Exit 0; known vendor diagnostics, no QML load errors |
| Deployment/ownership | Single PID 26781, matching MPRIS service PID; app raised to foreground |

New tests exercise encoded playlist create/add/rename/delete and occurrence-level
move/remove, smart-content restrictions, outbox persistence/deduplication,
ordinary timeline acknowledgement, Wi-Fi gating, actual file removal, sorted
metadata/grouping and balanced mix/autoplay neighbors. Cross-thread cancellation
interrupts a partial transfer without using Core's command queue; stale queued
network observations cannot reopen a blocked gate, and startup hints are retained.
C-ABI cancellation clones are verified independently owned after Core destruction.

The native generated-audio fixture listens long enough to qualify an offline play,
then seeks a second track to its end without creating another play. An initial
failure exposed state sampling being skipped while reporting was busy; sampling
now continues independently of report admission. Fixture sockets also tolerate
legitimate peer cancellation before full headers/while writing the response.

The first bare/initial-dialog fixture lacked a real page-stack indicator. The final
fixture pushes pages/dialogs through ApplicationWindow's stack attached to a real
QQuickWindow. A malformed test-only inline-QML semicolon was fixed before the
passing final run. Installed DialogHeader.qml was consulted to identify the missing
indicator; no system QML was modified.

Read-only `tools/inspect-playlist-entries.py` confirmed that the sampled nonempty
playlists are smart and omit playlistItemID; zero occurrence IDs in that sample
does not establish a parser failure. No live playlist mutation or synthetic real
listening/scrobble was performed. Live diagnostics print types/counts/facts only.

Remaining interactive checks include normal/sandboxed playlist editing on the
saved server, real Wi-Fi transitions/storage-pressure handling and longer
history/autoplay sessions. The audio engine was not replaced. i18n remains a
  recorded follow-up; its matching locale set and reference patterns were inspected.

## Rust runtime/audio/i18n — 2026-10-04

| Check | Actual result |
| --- | --- |
| `./tools/check.sh` | Passed: 59 Rust tests, fmt/Clippy, CTest 2/2 (10.31s) |
| Captured decoded PCM | Two mono WAVs: exact joined stereo sample count, no inserted/lost frames |
| Crossfade/DSP PCM | Exact configured overlap, equal-power midpoint/endpoints; gain/NaN/clamp/EQ tests pass |
| Translation generation/check | 39 locales, 409 context messages/299 distinct sources; no unfinished locale entries, placeholders checked |
| Qt6 QM loading | All 39 catalogues load; placeholder/HTTP-code and Italian lookup tests pass |
| SDK Rust/Qt5 app/RPM/rootless/test builds | Passed; explicit Sailfish linkage restored; final app compiler warnings corrected |
| Phone Qt5 catalogue/facts/passive polling/fake-sink transport | 6 entries passed, 0 failed/skipped |
| Phone real PulseAudio generated-media transport/history | 3 entries passed, 0 failed/skipped |

Phone fixtures use temporary state and a local protocol/silent-WAV server, not Plex
mutations or fabricated actual-account listens. The production sink test exercises
Rust HTTP, decoding, PCM/output, pause/seek/queue, completed-local playback and
heard-time qualification through PulseAudio. Connected service was non-Wi-Fi.

This does not establish all compressed-codec trims, mixed-rate joins, unlocked
appearance, power, long sessions, call/Bluetooth routing or installed sandboxing.
Only one successor is predecoded; late/unavailable data can still buffer. Translation
coverage and Qt compatibility are tested, not native-speaker review of all wording.
Deployment/smoke/ownership results are recorded after the final integration review.

### Final deployment

- Final host CTest 2/2 passed in 10.08s; SDK app/fixture builds passed.
- Restored Rust-side successful server/logout stream teardown and completed-cache
  fallback after streaming errors. Native fixture now logs out during active playback.
- Phone initially exposed an asynchronous seek-event assertion race: the audio
  snapshot can precede Core event delivery. The fixture now waits for the signal;
  final Qt5 catalogue + real PulseAudio transport/logout run: 4 passed, 0 failures.
- Rootless main-QML smoke with temporary state and it_IT override exited 0, with
  known vendor graphics diagnostics and no QML-load failure.
- Updated bundle deployed to `/home/defaultuser/plexfreq-test.mTmhFP`, restarted
  without autoplay, sole PID 59545. D-Bus reports the same MPRIS owner PID; Raise
  dispatched successfully. Session credentials and downloaded media were retained.

## CI setup validation — 2026-10-04

| Check | Actual result |
| --- | --- |
| actionlint 1.7.11 on both workflows | Passed (shellcheck unavailable, shellcheck integration disabled) |
| Qt5 qmllint in Ubuntu 24.04 container, all app QML | Passed |
| i18n --check with empty XDG cache | Pinned wheel bootstrap passed, all 39 TS/QM/resources match |
| Release version preflight | v0.1.0 passes; mismatched/shell-looking tags rejected |
| Bash syntax / git whitespace | Passed |
| Desktop build and CTest | Passed 2/2, 10.30s |
| CI SDK aarch64 command + versioned package script | Passed, app RPM/rootless bundle built |
| Full locked all-feature Rust run | Existing untracked production_review shuffled-history test fails (150 retained, expected <=101); other cases run before failure pass |
| Final full Rust recheck after concurrent working-tree updates | Passed: 65 tests, including all 4 production-review cases |
| GitHub-hosted workflow run / publication | Not performed; requires workflows to be committed/pushed |

CI configuration was inspected against both reference projects. Tests are not
skipped to hide the production-review failure; packaging waits for CI gates.
Ubuntu Qt5 syntax check was run in a disposable container; host desktop checks used
the installed Qt6. GitHub-specific artifact upload/publication is configuration-
linted, not claimed as a remote execution or device runtime check.

## Volume / lyrics / seek-thumb regressions — 2026-10-04

| Check | Actual result |
| --- | --- |
| Final `./tools/check.sh` | 61 Rust tests, fmt/Clippy, CTest 2/2 in 10.30s |
| SDK app/RPM/rootless/test builds | Passed |
| Phone optional-request failure/stale replies, Silica seek synchronization, native audio | 6 entries passed, 0 failed/skipped |
| PulseAudio peer media policy during generated playback | MediaState recognizes media after playback starts; no policy writes |
| Real read-only current lyric/playlist probe | 28 decoded lines; 6 playlist rows/counts/durations; 3 entries passed |
| Temporary-state rootless main-QML smoke | Exit 0, known vendor diagnostics |
| Deployment/MPRIS ownership | Sole PID 64815, matching D-Bus owner; Raise dispatched |

Fixed platform role (Sailfish x-maemo/desktop music), failed optional-request keys,
numeric-string gain parsing and idle slider resynchronization after native dragging.
First volume assertion ran before native playback/policy startup; final test waits
for both. Real lyric probe initially failed track parsing, then hit a transient
network/provider failure; final retry passed after gain tolerance. Error replies
now finish loading even when a provider is unavailable. Counts/types only, no texts.

Physical volume-key presses and visual touch interaction remain user confirmation;
actual phone policy recognition and QML value tracking were tested programmatically.
Credentials, queue settings and downloaded media were retained at deployment.

## Volume / lyrics / seek-thumb regressions — 2026-10-04

| Check | Actual result |
| --- | --- |
| Final `./tools/check.sh` | 61 Rust tests, formatting/Clippy; CTest 2/2, 10.30s |
| SDK app/RPM/rootless/test builds | Passed |
| Phone lyric correlation/stale failures + Silica slider tracking + native audio | 6 entries passed, 0 failed/skipped |
| PulseAudio peer policy observation during generated-media playback | MediaState recognized as media after startup (not inactive); no policy writes |
| Real account read-only current lyric/playlist probe | 28 decoded lyric lines; 6 playlist rows/counts/durations; 3 entries passed |
| Temporary-state rootless main-QML smoke | Exit 0, known vendor graphics diagnostics |
| Deployment/MPRIS owner | Sole app PID 64815; matching D-Bus PID; Raise dispatched |

Fixed: platform media role (x-maemo on Sailfish, music elsewhere), optional failed
reply keys, numeric-string gain parsing, idle slider resynchronization after native
dragging. A first volume-policy assertion ran before playback was active; the final
fixture waits for playing and policy updates. A provider network failure during the
live lyric probe was transient and passed on retry; request errors now finish lyric
loading rather than leave it spinning. Probe prints counts/types only, no lyric text.

Physical volume-key presses/visual drag interaction remain user confirmation; the
phone's actual media policy and QML state progression were exercised programmatically.
Existing credentials, queue settings and downloaded media were retained at deployment.

## Production review fixes — 2026-10-04

| Check | Actual result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | Passed |
| Full `cargo test --locked --all-features` | Passed: lib 14 (incl. new artwork pending test), production_review 7 (incl. queue-append, cache-limit, discovery-skip), all other suites green |
| Desktop `./tools/check.sh` (fmt, Clippy, Rust, release build, CTest) | Passed: CTest 2/2, 10.24s; translations match (no new user-facing strings) |
| `python3 tools/release-version.py --print-version` | Passed, version matches committed sources |
| `bash -n tools/build-sailfish.sh` / python syntax | Passed; version guard rejects empty/mismatched versions |

No live-account or phone runtime claims; device checks remain separate opt-in.

## Production hardening — 2026-10-04

Second review found 7 failing cases; all fixed without new user-facing strings.

| Check | Actual result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | Passed |
| Full `cargo test --locked --all-features --no-fail-fast` | Passed: lib 15 (incl. gain-preference), production_review 13 (6 old + 6 new hardening + discovery), all other suites green |
| Desktop `./tools/check.sh` | Passed: CTest 2/2, ~10s |
| `bash tools/build-qm.sh --check` via desktop build | Passed: 39 locales, 409 messages per catalogue |

Fixes: queue cursor validation + insert/presentation/advance panic guards,
SavePlayback/audio position preserve unknown duration, global track-gain
preference, next-track prepare failure keeps current playback, empty station
key falls back to sonic, MPRIS CanPause/CanGoNext corrections, GStreamer
unwrap guards, shared atomic-write/numeric helpers, artwork prune reuse,
documented 200-track mix sampling and shuffle-exit on move, check.sh
all-features parity, translations validate dead-code removal.

## Review follow-ups — 2026-10-04

| Check | Actual result |
| --- | --- |
| Initial `cargo test --locked --all-features --test review_followup` | All 6 supplied reproductions passed with pre-existing working-tree edits; unused prune warning found |
| Final fmt / locked all-target/all-feature Clippy | Passed, warnings denied |
| Final locked all-feature Rust tests | 89 passed; review_followup 10/10, library 18/18, existing suites green |
| Desktop release build / 39-locale freshness | Passed; 409 messages per catalogue |
| Final `ctest --test-dir build/desktop --output-on-failure` | Passed 2/2, 13.00s |
| `./tools/build-sailfish.sh` | Passed: aarch64 GNU Rust, Qt5.6, RPM/rootless bundle; rpmlint 0 errors, existing 1 warning |
| Bash syntax / `git diff --check` | Passed |

An earlier complete `./tools/check.sh` passed. After adding the last two
regressions, the final invocation passed Rust/build/translation stages but reached
its 120-second outer timeout during CTest after waiting for the concurrent SDK
Cargo lock. Only the interrupted CTest stage was rerun, successfully.

Additional regressions cover moving the last audible occurrence after natural end,
shuffle retaining that occurrence, removing its final slot without corrupting the
cursor, temporary bytes participating in audio quota, 200-response seek validator
matching, invalid/changed partial representations, and already-completed stale
list/failure replies waiting across navigation. The native pagination fixture now
uses the page-start correlation carried by actual runtime failures.

These are local protocol/state/Qt and SDK build results; no phone deployment or
hardware-runtime validation was performed in this follow-up.

## Daily-use priorities 1–4 — 2026-10-04

| Check | Actual result |
| --- | --- |
| Locked all-target/all-feature Clippy, fmt, Rust tests | Passed: 107 tests, parity 12/12, all existing suites green |
| Desktop release + CTest | Passed 2/2; final decoder-session isolation run 13.57s |
| Locale freshness/QM loading | Passed: 39 locales, 470 context messages; existing authored translations preserved |
| SDK production RPM/rootless build | Passed; 0 rpmlint errors, existing no-url-tag warning |
| SDK Qt5.6 fixture build | Passed; existing 2 fixture lint warnings |
| Phone shared navigation/settings/management/MPRIS/passive checks | Passed: 6 lifecycle/test entries, no skips/failures |
| Phone real PulseAudio generated-media transport/history/logout | Passed: 3 entries, no skips/failures |
| Initial real-server discovery | Failed: string-valued station radio flag; reproduced synthetically and fixed |
| Corrected real-server discovery | Passed: 72 entries, including 4 stations |
| Bounded 160 kbps real-server transcode probe | Passed: 256 KiB received, session cleanup acknowledged; no real playback/storage/history event |
| Phone temporary-state production QML smoke | Exit 0, known graphics/EGL diagnostics and isolated-bus startup messages only |

Local protocol tests cover quality persistence/validation/network selection,
representation-scoped downloads, original/transcode cleanup, progressive resume,
timed multi-window radio plans without queue mutation, ordered playlist refresh,
failed-refresh rollback, offline parent/search synthesis, grouped music-only hubs,
string radio flags, station playback/download and server Sonic Adventure queries.
Audio tests measure exact FLAC/WAV PCM joins, track/album/auto gain output,
crossfade/headroom math, pause retaining a prepared successor and mixed-rate joins
within one output frame (48 kHz stereo). These do not claim universal lossy-codec
encoder-delay trimming or bit-perfect output.

Phone tests used a fresh private test directory and isolated session buses, so
MPRIS calls could not control the running user player. Synthetic sessions used
temporary state. The live check used `pf_new_inspect` with existing credentials
in place, printing only counts/byte counts; `probe_quality` reads at most 256 KiB
and requests cleanup even on failed transfer. It does not save the sampled audio.

Remaining device work: physical Bluetooth/headset keys, call interruption and
routing recovery, installed Sailjail, long storage-pressure/offline/power sessions,
and real-server full transcoded playback/seek and recommendation variants. Live
playlist membership mutations and actual-account synthetic scrobbles were not used.

After the final decoder-session UUID isolation change, the complete host and both
SDK builds passed again. Phone checks above cover the preceding parser-corrected
build; the UUID race has local protocol regression coverage. The temporary phone
directory was removed. The existing player remains the normal-session MPRIS owner
(PID 64815); the new package/bundle is prepared, not installed over that player.
