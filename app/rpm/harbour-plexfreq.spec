Name: harbour-plexfreq
Version: 0.1.8
Release: 1
Summary: Plex music player for Sailfish OS
License: GPLv3+
Source0: %{name}-%{version}.tar.bz2
BuildRequires: pkgconfig(sailfishapp) >= 1.0.2
BuildRequires: pkgconfig(Qt5Core)
BuildRequires: pkgconfig(Qt5Qml)
BuildRequires: pkgconfig(Qt5Quick)
BuildRequires: pkgconfig(libpulse)
Requires: sailfishsilica-qt5 >= 0.10.9

%description
Native QML music player with a Rust Plex backend, browser authentication,
music libraries, search, playlists and direct-stream audio playback.

%prep
%setup -q -n %{name}-%{version}

%build
%qmake5
make %{?_smp_mflags}
strip --strip-unneeded harbour-plexfreq

%install
%qmake5_install

%files
%defattr(-,root,root,-)
%{_bindir}/%{name}
%{_datadir}/%{name}
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/*/apps/%{name}.png

%changelog
* Thu Oct 22 2026 PlexFreq contributors - 0.1.8-1
- Next during a radio no longer waits for the server when the next track is downloaded
- Send less metadata over Bluetooth, keeping the car's position in sync
- Resume transcodes the server cuts short; play received audio before reporting a broken transfer
- Show held downloads, failed offline saves and tracks no longer on the server
- Artwork no longer carries the Plex token in image addresses; offline library browsing is paged
* Mon Oct 19 2026 PlexFreq contributors - 0.1.7-1
- Buffer before resuming a stalled stream; defer the next track's download while the current one catches up
- Downloads yield to a starving stream; recover dropped streams in the engine (transcode restart)
- Mobile quality only on cellular, 320 kbps default for new installs; radio keeps playing cached tracks
* Fri Oct 16 2026 PlexFreq contributors - 0.1.6-1
- Fix stutter and stuck playback: use PulseAudio flow control, own 300 ms buffer
* Fri Oct 16 2026 PlexFreq contributors - 0.1.5-1
- Pure-Rust audio engine (Symphonia decoding, PulseAudio output); Harbour validation passes
- Fix Bluetooth disconnect auto-pause stalling after signal bursts
* Thu Oct 15 2026 PlexFreq contributors - 0.1.4-1
- Production review fixes across playback, cache, offline, runtime and UI
- Split session storage; shared async I/O runtime; event-driven MPRIS
- Harbour launcher icon sizes
* Thu Oct 15 2026 PlexFreq contributors - 0.1.3-3
- Preserve output position across sink clock resets and diagnose raw clock queries
* Tue Oct 13 2026 PlexFreq contributors - 0.1.3-2
- Preserve resume position across idle restart and show saved playback progress
* Tue Oct 13 2026 PlexFreq contributors - 0.1.3-1
- Playback recovery and reliability fixes
* Mon Oct 12 2026 PlexFreq contributors - 0.1.2-1
- Rolling queues
* Mon Oct 12 2026 PlexFreq contributors - 0.1.1-1
- Reliability improvements
* Fri Oct 02 2026 PlexFreq contributors - 0.1.0-1
- Initial Rust backend and native Sailfish/desktop music player
