# PlexFreq

PlexFreq is a music player for your own Plex Media Server or Jellyfin server
(10.9 or newer). It brings artist-first browsing, radio, discovery, lyrics and
offline listening to **SailfishOS** phones and to the **Linux desktop**.

On SailfishOS it uses the native Silica interface, the app cover and the system
media controls. On the desktop it is a regular Qt 6 window. Both versions share
the same playback engine, so they look and behave alike.

PlexFreq is free software, licensed under GPL-3.0-or-later.

## Install

Download the latest release from
[github.com/nappa85/plexfreq/releases](https://github.com/nappa85/plexfreq/releases).

### SailfishOS

The release includes a `harbour-plexfreq-…aarch64.rpm` package for
64-bit ARM phones running SailfishOS 5.

1. In **Settings → Untrusted software**, allow installing software from outside
   the Jolla Store (if you have not already done so).
2. Open the downloaded RPM from your browser's downloads or the file manager and
   confirm the installation.
3. Start **PlexFreq** from the app grid.

PlexFreq is installed like any other app and runs inside the standard Sailjail
sandbox, with only the Internet, Audio and Bluetooth permissions. It needs no root
access or extra system services.

### Linux desktop

The release includes `plexfreq-<version>-linux-amd64.tar.gz` for 64-bit PCs.
You don't need to install it. Extract it anywhere and run it:

```sh
tar -xzf plexfreq-<version>-linux-amd64.tar.gz
./plexfreq-<version>-linux-amd64/bin/plexfreq
```

The archive uses libraries already on your system, so you need:

- glibc 2.39 or newer, libstdc++ and libpulse;
- Qt 6 (Core, Gui, Qml, Quick) with the QtQuick Controls 2, Layouts, Window,
  Templates and WorkerScript QML modules, plus an X11 or Wayland platform plugin;
- PulseAudio, or PipeWire with its PulseAudio service, for sound output.

PlexFreq decodes MP3, FLAC, AAC/ALAC, Ogg Vorbis and WAV itself. The server converts
other formats on the fly. Package names differ between distributions;
the standard Qt 6 and PulseAudio packages are usually enough. Your settings and
cached music are stored in your user data folder, wherever you extract the archive.

## Getting started

### Plex

1. Open PlexFreq and choose **Sign in with Plex**. A browser page opens. Sign in
   there, then go back to the app.
2. Choose your server from the list, then pick a music library.
3. Browse artists, albums or tracks, or search. Select a track to start playing.

If you can't sign in through the browser, choose **Connect directly** and enter
the server address (for example `http://192.168.1.10:32400`) with a Plex token.

### Jellyfin

1. In the connection settings, set **Server type** to **Jellyfin**.
2. Enter the server address, including any path your server is published under,
   for example `http://192.168.1.10:8096` or `https://example.org/jellyfin`.
3. Enter your user name and password and choose **Sign in to Jellyfin**. Or choose
   **Use Quick Connect**: PlexFreq shows a code. In a Jellyfin app where you are
   already signed in, open **Quick Connect** in the user settings and enter the
   code. PlexFreq signs in when you approve it.
4. Pick a music library, then browse and play as with Plex.

The password is used only to sign in; PlexFreq does not keep it. Jellyfin lacks
some Plex features, listed in [PlexFreq and Plexamp](#plexfreq-and-plexamp). If a
track is in a Jellyfin playlist more than once and the server gives the copies
the same entry id, PlexFreq refuses to move or remove it rather than risk
changing the wrong copy.

**Sign out** removes your saved login, together with the cached music.

## Features

**Browsing**
- Artist pages with photos, biography, albums grouped by type and similar artists.
- Album pages with cover art, description and track list.
- Search across artists, albums and tracks, an alphabet bar for quick jumps, and
  sorting.
- Favorites (five-star tracks on Plex, favorite items on Jellyfin), recently added
  and recently played music.
- Library filters by genre, mood, style, year, rating and unplayed content (genre,
  year and unplayed on Jellyfin).

**Playback**
- Play, pause, seek, previous/next, shuffle, and repeat one or all.
- An editable queue: play next, add to queue, move and remove entries.
- The queue and position are restored when you reopen the app.
- Lyrics, including synced lyrics that scroll with the song, when the server has
  them.
- Gapless playback, crossfade, loudness normalization and a 10-band equalizer.

**Radio and discovery**
- Artist radio, plus album and track radio based on Plex sonic analysis. On
  Jellyfin, artist, album and track radio use the server's instant mix, which
  picks random songs sharing a genre with the seed. For radio based on how the
  music sounds, install an instant mix plugin on the Jellyfin server; PlexFreq
  uses it automatically. [BetterMix](https://github.com/StergiosBinopoulos/jellyfin-plugin-bettermix)
  runs inside Jellyfin with no other service (Linux or Windows x64, Jellyfin
  10.11 or newer). [Harmonie](https://github.com/mxschll/jellyfin-harmonie) needs
  one extra container and [AudioMuse-AI](https://github.com/NeptuneHub/audiomuse-ai-plugin)
  several, including a database; both have releases for Jellyfin 10.10 and newer.
- A discovery home with the mixes and stations your server offers (on Jellyfin:
  recently added, recently played, favorites and most played). You can reorder or
  hide its sections.
- **Sonically similar** music for any track, album or artist.
- **Sonic Adventure**: a musical path from one track to another, or a journey
  through several waypoints.
- Mixes built from up to ten artists or albums you choose.
- Optional autoplay of related music when the queue ends.

**Playlists**
- Create, rename and delete playlists. Add, move and remove tracks.
- Create a playlist from an album, the current tracks or the queue.
- Create smart playlists from library filters and edit their basic rules.

**Offline listening**
- Upcoming queue tracks download automatically, so playback continues through
  short connection drops.
- Keep albums, playlists or individual tracks on the device for offline listening.
- Download a set length of radio, a station or a playlist (30 minutes to 8 hours),
  and refresh it later for new music.
- Browse and search your saved library without a connection.
- Plays made offline are sent to the server when you reconnect.

**Phone and desktop integration**
- System media controls (MPRIS) on the lock screen, in the desktop panel and on
  Bluetooth devices, which show the correct track and position.
- Play/pause and next on the SailfishOS app cover.
- Playback pauses when Bluetooth headphones disconnect. It does not resume on its
  own when they reconnect.
- Playback pauses while another app records from the microphone, for example
  during a call, and resumes afterwards if it was playing.
- **Listening insights**: your top artists, albums and tracks over 7, 30 or 90 days,
  based on what you listen to in PlexFreq.
- Available in 39 languages, following your system language. Translations other than
  English are machine-written; corrections are welcome.

Sonic radio, Sonic Adventure and sonically similar music need a Plex server with
Sonic Analysis enabled. That requires Plex Pass on the server owner's account.
Jellyfin has no sonic analysis. Lyrics,
artwork and biographies depend on the metadata your server provides.

## Settings that matter

### Streaming and download quality

**Audio settings → Streaming quality** has three separate choices: Wi‑Fi,
mobile data and downloads. Each one can be **Original** (the untouched file) or a
limit from 64 to 320 kbps.

- Wi‑Fi and downloads default to Original.
- New installations limit mobile data to 320 kbps. The mobile limit applies only
  when PlexFreq knows the phone is on a cellular connection.
- Changes apply from the next track or download. Music already on the device always
  plays from local storage first.

When a limit is set, the server converts the music to MP3 while it plays. If the server
stops a converted stream early, PlexFreq continues it from where it stopped.

### Downloads and offline

- **Offline audio cache** (in the connection settings) sets the storage limit
  (512 MiB by default) and whether upcoming queue tracks download automatically.
  Music you choose to keep offline is never removed to make space.
- **Download only on Wi‑Fi** holds downloads until Wi‑Fi is confirmed. Streaming
  still works on any connection.
- The **Download manager** shows progress for each album, playlist or radio download.
  You can pause, resume, retry, cancel or remove downloads there.
- **Automatically refresh download plans** updates downloaded playlists and radio
  in the background at an interval you choose.
- **Browse saved library offline** switches to the music already on your device.
  **Go online** reconnects to the server.

### Audio

- **Crossfade**: off, or 1 to 12 seconds. Tracks from the same album always play
  gaplessly, without crossfade.
- **Normalize loudness**: evens out volume using the loudness data of your server, by track, by
  album or automatically. **Headroom** lowers the overall level to avoid clipping.
- **Equalizer**: ten bands from −12 to +12 dB, with a reset button.

### Language

PlexFreq follows your system language. On the desktop you can choose another one
when starting the app:

```sh
PLEXFREQ_LANGUAGE=it_IT ./bin/plexfreq
```

## PlexFreq and Plexamp

PlexFreq is inspired by Plexamp, Plex's own music player, which is not available
for SailfishOS. The table compares their main features. The PlexFreq column is for
a Plex server; the last column shows where a Jellyfin server differs.

| Feature | PlexFreq (Plex) | Plexamp | PlexFreq with Jellyfin |
|---|:---:|:---:|:---:|
| SailfishOS app | ✓ | ✗ | ✓ |
| Linux desktop app | ✓ | ✓ | ✓ |
| Android / iOS apps | ✗ | ✓ | ✗ |
| Browse, search, library filters | ✓ | ✓ | ✓ (genre, year, unplayed only) |
| Favorites | ✓ (five-star tracks) | ✓ | ✓ (favorite items) |
| Star ratings | ✓ | ✓ | ✗ |
| Alphabet bar for artists | ✓ | ✓ | ✗ |
| Gapless playback, crossfade, loudness leveling, EQ | ✓ | ✓ | ✓ |
| Synced lyrics | ✓ | ✓ | ✓ |
| Artist, album and track radio | ✓ | ✓ | ✓ (server instant mix) |
| Sonically similar music, Sonic Adventure | ✓ | ✓ | ✗ |
| Server mixes and stations | partial (those the server lists) | ✓ | ✗ (home lists instead) |
| Playlist creation and editing | ✓ | ✓ | ✓ (a track listed twice under one entry id can't be moved or removed) |
| Smart playlist rule editing | partial (basic rules) | ✓ | ✗ |
| Offline downloads of albums, playlists and radio | ✓ | ✓ | ✓ (not stations) |
| Offline play history sent to the server later | ✓ | ✓ | ✓ |
| Streaming quality per network type | ✓ | ✓ | ✓ |
| Guest DJ | ✗ | ✓ | ✗ |
| Sonic Sage (AI-generated playlists) | ✗ | ✓ | ✗ |
| Plex Home managed users and PIN switching | ✗ | ✓ | ✗ |
| Casting to other players and speakers | ✗ | ✓ | ✗ |
| Android Auto / CarPlay | ✗ | ✓ | ✗ |

## Troubleshooting

**No sound.** Check that the system volume is up and the right output is selected.
On the desktop, make sure PulseAudio or PipeWire's PulseAudio service is running.
If playback paused by itself, Bluetooth headphones may have disconnected, or
another app may have been recording. Press Play to continue.

**"Audio output stopped responding".** The headphones, speaker or car stayed
connected but stopped accepting sound, so PlexFreq paused instead of waiting
silently. Your place in the song is kept. Turn the device off and on again, or
reconnect it in the Bluetooth settings, then press Play.

**Downloads don't start.** Open the Download manager and check whether:
- downloads are paused (choose **Resume downloads**);
- **Download only on Wi‑Fi** is on and the device is not on Wi‑Fi;
- it shows *Downloads wait while the stream catches up*. Downloads step aside while
  a slow connection struggles to keep the current song playing, and continue
  afterwards;
- an item failed. Failed downloads and offline saves are listed with the reason,
  and you can retry them.

**A track is dimmed with "No longer on the server".** It was removed from your
server's library. Any copy already on your device stays playable.

**Sonic radio or Sonic Adventure is unavailable.** The server needs Sonic Analysis
and Plex Pass, and the music must have been analyzed. Jellyfin servers do not
offer these.

**Jellyfin: the sign-in fails.** Check that the address is reachable from the
device and includes the path if your server sits behind a reverse proxy at one
(`https://example.org/jellyfin`, not `https://example.org`). PlexFreq needs
Jellyfin 10.9 or newer. A "not a Jellyfin server" message usually means the
address, port or path is wrong.

**Jellyfin: Quick Connect says it is not enabled.** An administrator has to turn
on **Quick Connect** in the Jellyfin dashboard. Use your password
otherwise. A code that is not approved within a few minutes expires; start again.

**Jellyfin: the app asks me to sign in again.** The server rejected the saved
session (error 401), for example because it expired or was revoked on the
server. Sign in again. Downloads and play history stay.

**Radio repeats the same songs.** If the connection drops or the server cannot
refresh the radio, PlexFreq keeps playing downloaded tracks and may cycle through
them. This can happen with repeat switched off. Check the connection and try
starting the radio again when the server is reachable.

**Errors while the app is in the background.** PlexFreq also sends errors and
radio-refresh warnings to system notifications, requesting that they stay until
dismissed so you can read them later. The latest application error and radio
warning are kept separately; newer messages replace the previous message in
their category. Identical messages are throttled for five minutes. Notifications
are silent, and their display and retention depend on the system. Errors also
remain visible in the app when the notification service is unavailable.

**Logs.** On SailfishOS, PlexFreq keeps a daily log for the last seven days in
`~/Documents/PlexFreq`. If that folder cannot be created, it uses
`~/.local/share/org.plexfreq/harbour-plexfreq/logs` instead. On the desktop it
writes its log to the terminal it was started from. PlexFreq tries to remove
tokens from the logs, but this is best effort, and logs may also contain server
addresses, media URLs and track names. Check them before sharing.

**Reporting problems.** Open an issue at
[github.com/nappa85/plexfreq/issues](https://github.com/nappa85/plexfreq/issues).
Describe what you did, what you expected and what happened. Include your PlexFreq
version, your platform (phone model and SailfishOS version, or Linux distribution)
and the relevant part of the log.

## For developers

PlexFreq has a Rust core for Plex and Jellyfin access, playback, audio processing and offline
storage, with thin Qt/QML interfaces: Silica on SailfishOS and Qt Quick Controls 2
on the desktop. See [docs/development.md](docs/development.md) to build, test and
package it, and [docs/architecture.md](docs/architecture.md) to learn how it is
put together.
