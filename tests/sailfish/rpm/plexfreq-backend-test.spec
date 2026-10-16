Name: plexfreq-backend-test
Version: 0.1.4
Release: 1
Summary: Local PlexFreq native audio integration fixture
License: GPLv3+
Source0: %{name}-%{version}.tar.bz2
BuildRequires: pkgconfig(Qt5Core)
BuildRequires: pkgconfig(Qt5Gui)
BuildRequires: pkgconfig(Qt5Network)
BuildRequires: pkgconfig(libpulse)
BuildRequires: pkgconfig(Qt5DBus)
BuildRequires: pkgconfig(Qt5Test)
BuildRequires: pkgconfig(Qt5Qml)
BuildRequires: pkgconfig(Qt5Quick)

%description
Self-contained local fixture exercising Rust and Qt5 playback. No Plex account
or network service is required. Tests generate media in memory and use private
temporary application state.

%prep
%setup -q -n %{name}-%{version}

%build
%qmake5
make %{?_smp_mflags}
strip --strip-unneeded plexfreq-backend-test

%install
%qmake5_install

%files
%{_bindir}/%{name}

%changelog
* Thu Oct 15 2026 PlexFreq contributors - 0.1.4-1
- Production review regression coverage
* Tue Oct 13 2026 PlexFreq contributors - 0.1.3-1
- Playback recovery and reliability regression coverage
* Mon Oct 12 2026 PlexFreq contributors - 0.1.2-1
- Rolling queues
* Mon Oct 12 2026 PlexFreq contributors - 0.1.1-1
- Reliability improvements
* Fri Oct 02 2026 PlexFreq contributors - 0.1.0-1
- Native Qt5 device integration fixture
