Name: harbour-plexfreq
Version: 0.1.0
Release: 1
Summary: Plex music player for Sailfish OS
License: GPLv3+
Source0: %{name}-%{version}.tar.bz2
BuildRequires: pkgconfig(sailfishapp) >= 1.0.2
BuildRequires: pkgconfig(Qt5Core)
BuildRequires: pkgconfig(Qt5Qml)
BuildRequires: pkgconfig(Qt5Quick)
BuildRequires: pkgconfig(gstreamer-1.0)
BuildRequires: pkgconfig(gstreamer-app-1.0)
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
%{_datadir}/icons/hicolor/172x172/apps/%{name}.png

%changelog
* Fri Oct 02 2026 PlexFreq contributors - 0.1.0-1
- Initial Rust backend and native Sailfish/desktop music player
