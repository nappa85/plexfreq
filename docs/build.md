# Build and device workflow

The Rust audio runtime needs GStreamer/app development packages and GLib/GIO.
CMake queries `gstreamer-1.0`, `gstreamer-app-1.0`, `gio-2.0` and checks translations.
Qt Multimedia is no longer required.

`tools/build-sailfish.sh` derives `plexfreq-audio-sdk:local` using
`tools/audio-sdk.Dockerfile` when missing, installing development packages inside
the SDK image only. Rust pkg-config runs `tools/sdk-pkg-config.sh` against its
actual aarch64 sysroot; C dependencies retain `tools/sdk-cc.sh`. qmake links native
GStreamer/GIO and installs/embeds QMs. No phone root/packages are needed.

Qt Linguist tools follow the reference cache path
`${XDG_CACHE_HOME:-$HOME/.cache}/qt-tools/PySide6/{lupdate,lrelease}`.
`tools/build-qm.sh --check` consumes self-contained `tools/translations.json`.
`tools/author-translations.py` is the optional initial vocabulary/reference merge;
normal builds need no sibling repos. `PLEXFREQ_AUDIO_SINK=fake` selects a timed
silent fixture sink; production uses PulseAudio with media.role=music.

## Host checks

`tools/check.sh` runs formatting, Clippy, Rust tests, a release desktop build and
CTest QML loading. `cargo test --locked` can run without Qt. CMake regenerates the
Rust staticlib when sources/manifests change. Cargo.lock is part of the source.

## Sailfish build

`tools/build-sailfish.sh` uses host Rust (aarch64 GNU target) and SDK GCC for ring
C/assembly, then links the Qt adapter/packages using mb2 in a disposable source
copy. The original project files are not changed by SDK ownership adjustments.
The qmake project declares the Rust archive in `PRE_TARGETDEPS` as well as `LIBS`
so an incremental Rust-only change forces the Qt executable to relink.
Do not replace this with a modern Ubuntu cross-linker: the final binary must
link against the actual Sailfish target libraries.

Overrides: `SDK_IMAGE`, `SDK_TARGET`. If the image is updated, re-check Qt/glibc
versions and record the image digest. The compiler wrapper assumes Cargo's
sources reside under CARGO_HOME and outputs under the project root. Run the
build from the repository; custom external CARGO_TARGET_DIR is not supported.

## Rootless device validation

Application execution needs no root. RPM installation is an OS/package-manager
operation; where that is unavailable, use developer SSH and an extracted bundle:

The build also produces `build/plexfreq-0.1.0-aarch64-rootless.tar.gz`, containing
the executable and adjacent QML directory. Extract it in a user-owned phone
directory and run `./harbour-plexfreq`; this avoids the manual RPM extraction below.

1. On the host extract the RPM with `rpm2cpio <rpm> | cpio -id` in a clean staging
   directory (or use bsdtar). Copy `usr/bin/harbour-plexfreq` and the contents of
   `usr/share/harbour-plexfreq/` to a user-owned device directory, with `qml/`
   beside the executable. Our entry point explicitly checks this bundled layout
   before falling back to libsailfishapp's installed resource path.
2. Launch from that directory via the user's graphical session/developer SSH.
   Ensure DISPLAY/Wayland/session environment is inherited from the phone.
3. This unpacked terminal launch is unsandboxed. It tests app/runtime behavior,
   **not** Sailjail permissions. A sandboxed launcher requires normal installation.
4. Use `PLEXFREQ_STATE_DIR` to choose an isolated user-writable test state folder.
   No system directory, devel-su, setcap or system service is required.

Do not change system media configuration to make a test pass.

## Validated physical-device commands

Phone `defaultuser@192.168.1.120` is SailfishOS 5.2.0.18 aarch64; its SSH session
already has working Wayland and D-Bus environment variables. Bundle is in
`/home/defaultuser/plexfreq-test.mTmhFP` (fresh private mktemp directory).

```sh
# A transient QML load check; no login state survives it.
ssh defaultuser@192.168.1.120 'state=$(mktemp -d /tmp/plexfreq-smoke.XXXXXX); trap '\''rm -rf "$state"'\'' EXIT; PLEXFREQ_STATE_DIR="$state" /home/defaultuser/plexfreq-test.mTmhFP/harbour-plexfreq --smoke-test'

# Host-built standalone fixture uses the same Backend and Rust library.
./tools/build-device-tests.sh
scp build/device-tests/plexfreq-backend-test defaultuser@192.168.1.120:/home/defaultuser/plexfreq-test.mTmhFP/
ssh defaultuser@192.168.1.120 /home/defaultuser/plexfreq-test.mTmhFP/plexfreq-backend-test

# Interactive launch, when not already running. Normal user state; no log capture.
ssh defaultuser@192.168.1.120 'nohup /home/defaultuser/plexfreq-test.mTmhFP/harbour-plexfreq </dev/null >/dev/null 2>&1 &'
```

The fixture adds QtTest **build** dependencies in a disposable SDK container.
It uses the phone's existing QtTest runtime, local generated silence/loopback
HTTP and QTemporaryDir state; no phone installation is needed.

### Opt-in existing-account diagnosis

`tools/inspect-server-discovery.py` can run via `ssh ... python3 - < ...`.
It reads the normal phone state and outputs only resource counts and field types.
It never saves a payload or prints credential values.

After local regressions pass, the Qt native test can check the real account
through Rust without opening a browser or altering the saved connection:

```sh
ssh defaultuser@192.168.1.120 'PLEXFREQ_ACCOUNT_CHECK_STATE_DIR="$HOME/.local/share/org.plexfreq/harbour-plexfreq" /home/defaultuser/plexfreq-test.mTmhFP/plexfreq-backend-test savedAccountDiscovery'
```

Default local runs skip this explicitly opt-in case. It prints only a server
count; it does not select a server, play content or log URLs/tokens.

### Radio capability and planning checks

`tools/inspect-radio-capabilities.py` is a read-only phone-side probe of one seed
per music type. It prints Station structure/analysis flags/neighbor counts, never
names, identifiers, URLs or token values. Run only after local protocol tests.

The opt-in Rust planning check requests an artist station and its next window:

```sh
ssh defaultuser@192.168.1.120 'PLEXFREQ_ACCOUNT_CHECK_STATE_DIR="$HOME/.local/share/org.plexfreq/harbour-plexfreq" /home/defaultuser/plexfreq-test.mTmhFP/plexfreq-backend-test savedRadioPlanning'
```

It also checks availability for a sampled album/track. It creates an ephemeral
server play queue but does not download/play audio or fabricate timeline events.

### Detail/navigation checks

`artistAlbumNavigationAndRadioSeed` runs against the shipped shared Session QML
with a synthetic backend; it checks artist-first routing, child/back history and
radio seed type. On desktop it also instantiates the detail header and icon action.
This fixture is embedded into the Qt test executable for both Qt generations.

The opt-in `savedLibraryDetails` checks the selected server's artist biography/
gallery and album/track hierarchy through Rust. Run it with the same
`PLEXFREQ_ACCOUNT_CHECK_STATE_DIR` as other live cases. Only counts/presence flags
are printed, no private metadata or URLs. The capability script also reports
subscription flags and the musicAnalysis setting without modifying that setting.

### Cache and glyph checks

Local cache protocol tests run in temporary directories with fake tokens:
`cargo test --locked --test cache`. The Qt fixture includes
`radioIconRemainsCompleteAcrossResizeAndReuse` and `protocolAndNativeAudio` (the
latter closes its HTTP server after download and plays the next local file).

After connectivity is restored, use the rebuilt Qt5 fixture for those cases.
`savedCacheDownload`, enabled with PLEXFREQ_ACCOUNT_CHECK_STATE_DIR, is a separate
opt-in **real download**, limited to one track ≤8 MiB. It must run before the new
app owns the cache lock, or with the app closed; metadata-only diagnostics use
pf_new_inspect and do not take the writer lock/start media jobs. Never collect
framework logs from real decoded media as though URLs were redacted.

Cache defaults/configuration are persisted in session.json. Audio resides in
`<state-dir>/audio-cache`, including a persistent index, partial recovery metadata
and an exclusive writer lock. Clearing the cache keeps that lock file in place.

Phone pixel-capture cases require the display to be on/unlocked; SSH connectivity
alone does not expose a QQuickWindow. The local cached-audio test does not require
an exposed rendering window. `savedCacheDownload` now reports static cache errors
immediately and uses a strict 60-second wait instead of extended QTRY deadlines.
`tools/inspect-cache-source.py` compares one bounded candidate's metadata size and
HTTP range framing, reads one byte and prints counts/headers only, never URLs or
credentials. It diagnosed the observed database-vs-representation size mismatch.
