Name: harbour-plexfreq
Version: 0.1.4
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
