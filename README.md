# PlexFreq

## Rust runtime, audio and languages

Playback runs in Rust-owned workers rather than C++ QThread/QMediaPlayer. Rust owns
Plex/media HTTP, queue policy, PCM timing, transport/heard time, ten-band EQ,
optional Plex loudness gain, prepared-track gapless joins and equal-power crossfade.
Installed GStreamer codecs/resampling and PulseAudio provide native decoding/output.
C++ is a Qt model/properties/translation bridge; Rust also owns MPRIS/network facts.

Open **Audio settings** from Connection/settings for crossfade (off by default),
normalization and EQ. Same-album tracks keep gapless transitions without crossfade.
Output PCM is fixed 48 kHz stereo; sample-rate matching and wider codec/routing/
long-session tests remain follow-ups.

39 locale catalogues match ElectricEel/sailfish-proton, with full locale → base
language → English fallback, packaged/embedded for installed and rootless builds.
`PLEXFREQ_LANGUAGE=it_IT` overrides the system locale for inspection.
`./tools/build-qm.sh` compiles the authored dictionary; `--check` verifies catalogue
coverage/placeholders and deterministic TS/QM/resources. New technical translations
use compact localized clauses; the Italian UI has expanded prose.
See `docs/audio-engine-plan.md` and `docs/validation.md` for boundaries/checks.

A native Plex music player for **SailfishOS** and **Linux desktop**, with QML
interfaces and a Rust application backend. This is the initial usable-player
implementation of a Plexamp-inspired client.

## Features

- Plex browser sign-in, server discovery and direct URL/token connection.
- Artist-first browsing: artist photos/biography/albums, then album cover,
  description and tracks, plus selectable similar artists.
- Global artist/album/track search, favorites and recently added/played views.
- Artist/album/track browsing, sorting and album-type grouping; regular playlist
  creation/editing and add-to-playlist actions.
- Paged browsing, audio playlists and editable queues with play-next/add/move/remove.
- Direct-stream playback, pause/resume, seek, previous/next, shuffle, repeat.
- Artist radio through Plex stations; sonic album/track radio when server analysis
  is available, with automatic queue continuation.
- Native Silica UI and playback cover on Sailfish; Controls2 desktop UI.
- Persistent queue/radio/resume state, now-playing artwork and plain/timed lyrics.
- Standard MPRIS session-bus media controls for desktop/Sailfish integration.
- Private per-user session storage; no root or background system service.
- Bounded audio cache: queue prefetch, resumable downloads, downloaded-track
  catalogue, persistent track/album/playlist pins and local-file playback.
- Offline browsing/search of saved library metadata, artwork and retrieved lyrics.
- Download manager with per-group progress, retry/cancel/remove and Wi-Fi-only
  policy; general streaming timelines and durable offline listening-history sync.
- Optional autoplay and balanced multi-artist/album mixes.

## Desktop

Requires Rust stable, CMake, a C++ compiler and Qt **6.2+** Core, Gui, Qml, Quick,
QuickControls2, Multimedia and DBus development/runtime modules. This host has Qt 6.11.
Local checks also use Qt Test, Network and `dbus-run-session` for an isolated bus.
Distribution package names vary. The same source builds on Linux distributions
providing these dependencies; it is not a single universally portable binary.

```sh
cmake -S . -B build/desktop -DCMAKE_BUILD_TYPE=Release
cmake --build build/desktop --parallel
./build/desktop/plexfreq
```

Click **Connect**, sign in in the browser, return and select your server. Or enter
the server origin (`http://192.168.1.10:32400`) and X-Plex-Token directly. Select a
library and browse artists. Open an artist to see its photos, biography and
albums; open an album for its cover, description and tracks. Click a track to
play the loaded track list. Album **Play** loads all its track pages. The token field is
never prefilled. Artwork/descriptions depend on metadata available from Plex.

Search finds artists, albums and tracks across the selected server after a short
debounce (up to 100 results per search hub). The side alphabet uses
the server's first-character index to jump directly to a letter's page. Letters
are hidden while a search is active. Lists load the next page automatically when
roughly 75% of the loaded content has been reached; a Retry action appears only
if that background fetch fails. Existing rows are appended rather than reset.
The main artist list has no radio buttons; start artist radio from its detail page.

Long-press an entry on Sailfish or use its **⋮** menu on desktop for queue actions,
favorites and offline pins. Queue entries offer **Move up**, **Move down** and
**Remove**. Explicit reordering exits shuffle; editing radio makes it an ordinary
queue. The saved queue and position are restored after restart; press Play to
resume. Position is checkpointed every five seconds and on pause/seek/normal exit.

Open **Now playing** for artwork, transport/seek, favorite status and lyrics.
Timed LRC lines highlight during playback; availability depends on Plex metadata.
Sailfish's pull-down menu and desktop's **Discover** menu open five-star favorite
tracks, recently added albums and server-reported recently played tracks.

Use the browser type/sort controls for albums/tracks, and **Group albums by type**
on an artist. Entry menus provide **Add to playlist**, **Create playlist from this**,
playlist rename/delete and occurrence-level moves/removal. Smart playlist contents
are controlled by Plex filters. **New playlist** can use the current loaded tracks
or queue. Add artists/albums as **mix seeds**, then choose **Play mix**; up to ten
seeds and 1000 unique tracks are supported, with at most 200 tracks sampled per artist.
Autoplay is enabled in settings and uses actual sonic neighbors or artist stations.

Use the **antenna-and-waves icon** beside an artist, album or track, or in the
artist/album detail header. Desktop hover text identifies the radio type;
accessible names are provided on both platforms. Artist radio uses a station
advertised by Plex. Album radio
plays a complete album, followed by sonically similar albums; track radio follows
sonically similar tracks. Album/track radio requires analyzed music and Plex Pass
on the server owner's account; owning Plex Pass does not itself enable library
analysis. Availability is checked by requesting recommendations, not by assuming
that an omitted analysis-version field means unlicensed. An unavailable mode reports a clear message and
keeps the previous queue. Stop radio from the Sailfish pull-down menu or the
desktop radio banner. Normal track playback also exits radio mode.

### Mobile cache

Caching is enabled by default with a **512 MiB** limit. The current track and the
next **five** known queue entries are downloaded in a Rust background worker.
Completed files are played locally; a newly selected uncached track starts
streaming while caching catches up. Interrupted transfers retry and resume with
HTTP range validation where the server supplies a validator.

Use **Download tracks** on an album page, or **Download displayed tracks** in the
Sailfish pull-down menu, to persistently pin the loaded tracks before travelling.
Use **Pin for offline listening** in an album/playlist entry's menu to fetch its
complete bounded plan, rather than only the displayed page. Up to 1000 distinct
tracks can be pinned. **Downloaded music** opens the
persistent catalogue and works without library/server requests. The app falls
back to it at startup if library loading fails and downloads are available.

Cache controls and storage usage are in **Connection → Offline audio cache**.
Pinned tracks are protected from eviction; unpinning makes them reclaimable.
If pins fill the budget, increase it or unpin downloads. Old unprotected audio is
evicted when space is needed. Automatic caching can be
disabled; existing completed files remain playable. Clearing the cache stops
playback/downloads; signing out clears cached audio too.

Only **completed** downloads are usable offline. Radio can play downloaded known
queue entries, but needs the server for new recommendations beyond that window.
**Browse saved library offline** / **Offline library** switches to saved metadata
without contacting Plex. Library pages are saved as you browse; this is not an
automatic full-library download. Saved albums and completed tracks remain
available, with previously cached images and lyrics. Artwork has a separate
128 MiB budget and metadata/lyrics 64 MiB. **Go online** resumes server access.
New radio recommendations and offline play-history synchronization require
connectivity. A saved server station can also expire between sessions.

The **Download manager** shows group counts, byte usage/current progress and errors,
with pause/resume/retry/cancel/remove controls. Wi-Fi-only background downloads wait
for a confirmed Wi-Fi connection; playback streaming is still allowed. Network
facts come from read-only ConnMan/NetworkManager observation and never alter system
settings. A separate Rust gate cancels transfers even while metadata work is busy.

Listening history uses measured heard time (half a track or four minutes), rather
than seeking or preparing a queue. Normal remote playback reports timelines;
offline/cached listens persist and synchronize after reconnection. The manager also
offers **Sync history**. Replay cannot promise original server timestamps or exactly
once delivery if a remote acknowledgement is lost.

For a user-local install:
`cmake --install build/desktop --prefix "$HOME/.local"`.

## SailfishOS

```sh
rustup target add aarch64-unknown-linux-gnu
./tools/build-sailfish.sh
```

Uses the available `coderus/sailfishos-platform-sdk-aarch64:latest` image and
`SailfishOS-5.2.0.15-aarch64` target. Rust C dependencies are compiled against the
SDK sysroot. Output RPMs: `build/sailfish/RPMS/`. See [build notes](docs/build.md)
for validation and rootless device execution.

## Checks and development notes

```sh
./tools/check.sh
```

Local protocol tests use a mock HTTP server and temporary state; they do not
contact Plex or require a phone/account. Desktop smoke test loads actual QML
offscreen. See [research](docs/research.md), [architecture](docs/architecture.md),
[validation](docs/validation.md) and [roadmap](docs/roadmap.md).

Direct streaming depends on native codec support. This version does not yet
implement gapless/DSP, transcoding, Plex Home switching,
or general play-history reporting. Native station playback reports timelines to
keep its server queue progressing. Sessions are stored as a private file,
not an encrypted keychain. License: GPL-3.0-or-later (SPDX).
