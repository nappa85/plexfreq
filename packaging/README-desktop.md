# PlexFreq for desktop Linux

This archive contains the PlexFreq executable for Linux on amd64 (`bin/plexfreq`)
and a `share/` folder with translations, an icon and an optional desktop
launcher. The user interface and translations are also built into the
executable, so nothing needs to be copied into system directories.

## Requirements

PlexFreq uses the shared libraries already installed on your system:

- glibc 2.39 or newer, libstdc++ and libpulse.
- Qt 6 (built against Qt 6.4.2): the Core, Gui, Qml and Quick libraries; the
  QtQuick, QtQuick.Controls, QtQuick.Layouts, QtQuick.Window,
  QtQuick.Templates and WorkerScript QML modules; and an X11 or Wayland platform
  plugin. Use a matching set of Qt libraries and plugins from your
  distribution.
- A running PulseAudio server, or PipeWire with its PulseAudio service.

MP3, FLAC, AAC/ALAC, Ogg Vorbis and WAV are decoded by PlexFreq itself. Other
formats are converted by the Plex server.

## Running

Extract the archive anywhere you can write to, then run:

```sh
./bin/plexfreq
```

No installation or root privileges are needed. To use a different language
than the system one:

```sh
PLEXFREQ_LANGUAGE=it_IT ./bin/plexfreq
```

## Where PlexFreq keeps its data

Settings, the sign-in, downloads and saved library data are stored in
`~/.local/share/org.plexfreq/harbour-plexfreq`, independent of where the archive
was extracted. Log messages are written to the terminal PlexFreq was started
from.
