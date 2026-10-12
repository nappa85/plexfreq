# PlexFreq desktop development build

Extract the archive anywhere you can write, then run `./bin/plexfreq` from the
extracted directory. No package installation or root privileges are needed to
run the application. QML and translations are embedded in the executable; the
adjacent `share/` tree also provides translations, an icon and an optional desktop
launcher. There is no need to copy these files into system directories.

The CI amd64 binary is built on Ubuntu 24.04 with Qt 6.4.2 and glibc 2.39.
It uses the host's shared libraries and plugins, which must already be available:

- glibc 2.39 or newer, libstdc++, GLib/GIO and libpulse.
- Compatible Qt6 Core, Gui, Qml and Quick libraries; QtQuick, Controls2,
  Layouts, Window, Templates and WorkerScript QML modules; an X11 or Wayland
  platform plugin. Use a matching set of Qt libraries and plugins from your OS.
- GStreamer core/base, Good plugins and libav for audio decoding and PulseAudio
  output. Audio requires a user PulseAudio server or PipeWire's PulseAudio service.

This is a development archive, not a self-contained runtime bundle. Distribution
package names vary; on Gentoo these dependencies come from the normal Qt6,
GStreamer and PulseAudio packages/USE flags. No Debian package manager is involved.
The build baseline and available host libraries determine compatibility.

`PLEXFREQ_LANGUAGE=it_IT ./bin/plexfreq` overrides the system language.
User state and audio cache remain in the normal per-user application data folder,
independent of where the archive was extracted.
