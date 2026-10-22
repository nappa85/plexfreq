# Development

How to build, test, translate, validate on a phone and release PlexFreq. The
component split and runtime design are in [architecture.md](architecture.md);
the binding rules for contributors (human or agent) are in `AGENTS.md`.

## Repository layout

| Path | Contents |
|------|----------|
| `src/` | Rust core (`plexfreq_core`, built as `rlib` and `staticlib`): Plex/media HTTP, credentials, models and queue policy, worker lifetime, audio decode/DSP/PulseAudio output, MPRIS, network facts, C ABI (`ffi.rs`). In-crate test modules live beside the code (`*_tests.rs`, `src/store/tests.rs`, `#[cfg(test)]` modules). |
| `tests/*.rs` | Rust integration tests against the public crate API, with local HTTP fixtures and temporary state. |
| `tests/qt_backend.cpp` | Qt Test integration fixture for the C++ `Backend` adapter and shared QML; built for Qt6 by CMake and for Qt5.6 by `tests/sailfish/`. |
| `tests/*.qml`, `tests/qt-backend-test.qrc` | QML fixtures embedded into the Qt test executable. |
| `app/src/` | Thin C++ adapter: `Backend` (Qt model/C ABI bridge), translations, artwork provider, `main.cpp`. |
| `app/qml/pages/`, `app/qml/harbour-plexfreq.qml` | Sailfish Silica UI (Qt5.6). |
| `app/qml/desktop/` | Desktop UI (Qt6, QtQuick Controls 2). |
| `app/qml/shared/` | QML shared by both UIs; must stay QtQuick 2.6 compatible and import neither Silica nor Controls 2. |
| `app/harbour-plexfreq.pro`, `app/rpm/` | qmake project and RPM spec for the Sailfish package. |
| `CMakeLists.txt`, `packaging/` | Desktop build, CTest registration and CPack archive. |
| `app/translations/`, `tools/translations.json` | Generated TS/QM catalogues and the authored translation source. |
| `examples/` | Standalone PulseAudio probes (`pulse-diagnostics`, `pulse-play`). |
| `tools/` | Check, build, packaging, translation, review and read-only phone inspection scripts. |
| `review-notes/` | Gitignored local review records (see [Review process](#review-process)). |

## Prerequisites

Desktop development (Linux):

- Rust stable with `rustfmt` and `clippy`; `Cargo.lock` is part of the source and
  every build uses `--locked`.
- CMake 3.16+, a C++17 compiler, `pkg-config`.
- Qt 6.2+: Core, Gui, Qml, Quick, QuickControls2; for tests also Test, Network
  and DBus.
- libpulse development files.
- `dbus-run-session` (dbus-daemon) for isolated tests.
- `ffmpeg` and `lame`, used by `tests/audio.rs` to generate codec fixtures
  (FLAC/ALAC/AAC/Vorbis/Opus/PCM, MP3).
- Python 3.11+ (`tomllib`) with `pip`. Qt Linguist `lupdate`/`lrelease` 6.11.2 are
  bootstrapped by `tools/qt_tools.py` from hash-pinned PySide6-Essentials wheels
  into `${XDG_CACHE_HOME:-~/.cache}/qt-tools/`; a matching
  `qt-tools/PySide6/` cache is reused.

Server-backed audio tests additionally need `pactl`, `pacat`, `parec`, and either
a `pulseaudio` binary or Docker.

Sailfish packaging needs Docker and the Rust target:

```sh
rustup target add aarch64-unknown-linux-gnu
```

Use the GNU target, not musl: the static library is linked with the Sailfish Qt
toolchain against the real target libraries.

## Building

### Desktop

```sh
cmake -S . -B build/desktop -DCMAKE_BUILD_TYPE=Release
cmake --build build/desktop --parallel
./build/desktop/plexfreq
```

CMake builds the Rust static library with `cargo build --locked --release --lib`
into `build/desktop/cargo-target/`, rebuilding it when `src/**/*.rs`,
`Cargo.toml` or `Cargo.lock` change. The `translations` target runs
`tools/build-qm.sh --check`, so a stale catalogue fails the build. Desktop and
shared QML, translations and icons are compiled in with `qt6_add_resources`
(`app/desktop.qrc`, `app/translations.qrc`, `app/icons.qrc`); the generated rules
depend on each listed file, so editing QML rebuilds the binaries. A new desktop or
shared QML file must be added to `app/desktop.qrc`, and a new test QML file to
`tests/qt-backend-test.qrc`.

`PLEXFREQ_STATE_DIR=<dir>` replaces the application data directory (session,
caches) for a run; use a temporary directory for experiments. For a user-local
install: `cmake --install build/desktop --prefix "$HOME/.local"`.

### Sailfish RPM

```sh
./tools/build-sailfish.sh
```

The script:

1. Verifies the version with `tools/release-version.py` and the translations with
   `tools/build-qm.sh --check`.
2. Builds `plexfreq-audio-sdk:local` from `tools/audio-sdk.Dockerfile` if missing
   (the Sailfish platform SDK image pinned by digest, plus `pulseaudio-devel` in
   the `SailfishOS-5.2.0.15-aarch64` target).
3. Cross-compiles `libplexfreq_core.a` with host Rust for
   `aarch64-unknown-linux-gnu`. C dependencies are compiled by `tools/sdk-cc.sh`
   (the SDK GCC with the target sysroot, run in the container) and pkg-config
   queries go through `tools/sdk-pkg-config.sh` against the same sysroot.
4. Copies `app/` and the archive into the disposable `build/sailfish/` and runs
   `mb2 build` there, then restores file ownership. The source tree is not
   modified.

Output: `build/sailfish/RPMS/`. Overrides: `SDK_IMAGE`, `SDK_TARGET`.
`CARGO_TARGET_DIR` is honoured only inside the repository (the compiler container
mounts only the repository and a read-only `CARGO_HOME`). When the SDK image digest
changes, re-check the Qt, glibc and target versions. The qmake project lists the
Rust archive in `PRE_TARGETDEPS` so a Rust-only change relinks the executable.
Only `qml/harbour-plexfreq.qml`, `qml/pages` and `qml/shared` are installed;
desktop QML never ships in the RPM. Launcher icons (86/108/128/172 px, plus the
512 px desktop icon) are regenerated from `logo.png` by `tools/build-icons.py`
(Pillow). `sailfishapp.prf` reserves the qmake `icon` install group, so the
launcher icons are installed through a `launchericon` group. TLS is rustls, so
there is no OpenSSL dependency.

## Checks

Run before every change set:

```sh
./tools/check.sh
```

It runs, in order:

1. `tools/build-qm.sh --check`: every user-visible string has all 39 translations
   and the committed TS/QM/qrc files are current.
2. `cargo fmt --all --check`.
3. `cargo clippy --locked --all-targets --all-features -- -D warnings`.
4. `cargo test --locked --all-features` under `tools/isolated-test.sh`.
5. A Release desktop build in `build/desktop` and `ctest`:
   - `native_backend_integration`: `qt_backend_test` under
     `tools/isolated-test.sh`, offscreen, software Quick backend,
     `PLEXFREQ_AUDIO_SINK=fake` (a timed silent sink).
   - `desktop_qml_smoke`: `tools/smoke-test.sh` starts `plexfreq --smoke-test`
     with a temporary `PLEXFREQ_STATE_DIR`.

### Isolation from the host

```sh
tools/isolated-test.sh COMMAND [ARGS...]
```

The platform observers connect to `$XDG_RUNTIME_DIR/pulse/native` and to the
session and system buses; on a desktop they would react to real microphone
streams and Bluetooth state and register MPRIS names. The wrapper gives COMMAND a
fresh private `XDG_RUNTIME_DIR` with no audio server, points `PULSE_SERVER` into
it, and starts one private `dbus-run-session` bus that serves as both the session
and the system bus. It sets `PLEXFREQ_TEST_ISOLATED=1`; tests that assert the
isolation (for example that no PulseAudio server is reachable and both bus
addresses match) run only when it is set. Use it for any manual test run that
starts the runtime.

### Server-backed audio tests

`src/audio/pulse_server_tests.rs` drives the audio engine through real libpulse
output against a private headless daemon:

```sh
tools/pulse-test-server.sh cargo test --locked --all-features --lib audio::pulse -- --nocapture
```

The script creates a temporary directory with a daemon that has no hardware (null
sinks `plexfreq_a`, the default, and `plexfreq_b`, each kept running by a silent
`pacat`), no autospawn, no D-Bus and a private cookie. COMMAND gets
`PULSE_SERVER` and `PLEXFREQ_PULSE_TEST_SERVER` pointing at its socket and
`PLEXFREQ_PULSE_ISOLATED=1`, the only signal that lets these tests move streams and
load or unload sinks. The daemon and directory are removed on exit or INT/TERM,
and the command's exit status is returned. The developer's own audio server is
never contacted.

| Variable | Effect |
|----------|--------|
| `PLEXFREQ_PULSE_BACKEND` | `native` (`pulseaudio` from `PATH`), `docker` (same daemon from `tools/pulse-test.Dockerfile`, `--network none`, run as your uid) or `auto` (default; native if available). PipeWire hosts without a `pulseaudio` binary use Docker. |
| `PLEXFREQ_PULSE_SHM=0` | Disables shared memory on daemon and client, so all data uses the socket. |
| `PLEXFREQ_REQUIRE_PULSE=1` | Turns every skip into a failure. |
| `PLEXFREQ_PULSE_KEEP_PCM=<dir>` | Keeps the sink-monitor recordings (s16le mono 48 kHz). |

Without the script these tests print a skip message and pass. They serialize on
one lock and include a long soak, so a run takes minutes. To inspect the local
user-session daemon without creating playback, or to play a file through the
production output:

```sh
cargo run --locked --example pulse-diagnostics
cargo run --locked --example pulse-play -- <file> [seconds]
```

### CI

`.github/workflows/ci.yml` runs on every branch push, pull request and manual
dispatch, with six independent jobs on Ubuntu 24.04:

| Job | Runs |
|-----|------|
| `rust` | fmt, Clippy, `cargo test --locked --all-features` under `tools/isolated-test.sh` |
| `pulse` | `tools/pulse-test-server.sh` with the native daemon and `PLEXFREQ_REQUIRE_PULSE=1`, once with shared memory and once with `PLEXFREQ_PULSE_SHM=0` |
| `qt` | Qt6 Release build with `BUILD_TESTING=ON` and `ctest` |
| `qml` | Qt5 `qmllint` syntax check of all Silica, shared and desktop QML |
| `i18n` | `tools/build-qm.sh --check`; `python3 -m unittest test_external_review` |
| `harbour` | `tools/build-sailfish.sh`, then `tools/check-harbour.sh` |

CI does not exercise phone audio routing, hardware keys, Sailjail or a real Plex
account.

### Harbour validation

```sh
./tools/check-harbour.sh
```

Runs `mb2 check --suites harbour` in the SDK container on the RPM in
`build/sailfish/RPMS`; run `tools/build-sailfish.sh` first. Harbour forbids,
among others, linking GStreamer, which is why audio decoding is pure Rust and
output uses libpulse only.

## Testing guidelines

- Write deterministic local tests before any live server or phone experiment.
  Rust protocol, state and audio logic is tested with local HTTP servers,
  generated media and the fake sink; Qt adapter and QML behaviour with
  `tests/qt_backend.cpp`. Phone runs answer only what cannot be reproduced
  locally.
- Keep session and cache fixtures in temporary directories (`tempfile`,
  `QTemporaryDir`, `PLEXFREQ_STATE_DIR`). Fixtures are synthetic: never put real
  tokens in fixtures, command-line arguments, logs or source files.
- Framework and media URLs can appear in diagnostics and logs; treat such output
  as unredacted and do not paste it into reviews or issues.
- Mutation-check each new regression test: break or revert the fix and confirm
  the test fails, then restore it.
- Tests that start the runtime go through `tools/isolated-test.sh`; tests that
  need a real audio server go through `tools/pulse-test-server.sh`.
- QML used by both UIs must load on Qt5.6 and Qt6; the `qml` CI job only checks
  syntax, so runtime behaviour of shared QML is covered by the Qt fixture on both
  Qt generations (desktop CTest and the Sailfish device fixture).
- A successful package build is not a runtime test on hardware.

### Opt-in checks in the Qt fixture

`tests/qt_backend.cpp` skips these unless enabled. Select one QtTest function by
passing its name as an argument.

| Variable | Enables |
|----------|---------|
| `PLEXFREQ_ACCOUNT_CHECK_STATE_DIR=<state dir>` | The `saved*` live checks against the account saved in that directory (discovery, library details, radio planning, filters, lyrics/playlists, bounded transcode playback, `savedCacheDownload` with one real download of at most 8 MiB). They print counts and flags, not metadata, URLs or tokens. Run `savedCacheDownload` only while the app is closed, as the app holds the cache writer lock. |
| `PLEXFREQ_MEDIA_ROLE_CHECK=1` | During local playback, reads the Sailfish `com.Meego.MainVolume2` `MediaState` over the PulseAudio D-Bus peer to confirm the stream is classified as media. Phone only. |
| `PLEXFREQ_RECORDING_CHECK=1` | Records from the existing `source.null` with `parec` and checks that playback pauses while recording, resumes afterwards, and stays paused after an explicit pause. Phone only. |
| `PLEXFREQ_SYSTEM_LOCALE_CHECK=1` | Sailfish build only: loads translations from `/etc/locale.conf` with the shell locale variables cleared. |

`PLEXFREQ_LANGUAGE=<locale>` overrides the UI language of the app and fixture.

## Translations

There are 39 locales (`LANGUAGES` in `tools/translations.py`). `tools/build-qm.sh`
wraps `tools/translations.py`; `tools/translations.json` is the authored source for
every locale and the only input a normal build needs.

1. Extract source strings from `app/qml` and `app/src` (`lupdate`) plus the Rust
   user-facing errors (`Error::Input`/`Error::ProtocolAt` literals and the `Core`
   list in `translations.py`) into `app/translations/harbour-plexfreq.ts`:

   ```sh
   tools/build-qm.sh --extract
   ```

2. Author translations for new strings only:

   ```sh
   python3 tools/author-translations.py --missing-only
   ```

   Existing entries are kept. New strings are filled from exact reference matches,
   then from compact phrase recipes (`SIMPLE`, `PHRASES`, `ALIASES` with
   `tools/translation-vocabulary.json`); Italian uses the full sentences in
   `ITALIAN`. Without `--missing-only` every entry is regenerated, discarding
   hand edits. Review the result and edit `tools/translations.json` directly
   where a recipe is not good enough.

3. Generate the per-locale TS, QM and `app/translations.qrc`, then verify:

   ```sh
   tools/build-qm.sh
   tools/build-qm.sh --check
   ```

The check fails on a missing or empty translation, a placeholder mismatch
(`%1`…`%9`, `%n`), or any committed TS/QM/qrc file that differs from the
generated one. Identical translations are allowed (loanwords such as "Mix").

## Device testing on SailfishOS

Platform integration (audio routing, Bluetooth and MPRIS, permissions, storage)
is validated only with the RPM installed normally and the app started from its
launcher, so that it runs under Sailjail with the permissions declared in
`app/harbour-plexfreq.desktop` (`Internet;Audio;Bluetooth`). Running an unpacked
binary from a terminal is unsandboxed and does not establish sandbox
compatibility.

- Copy the RPM from `build/sailfish/RPMS/` to the phone and install it as the
  normal user with `pkcon install-local harbour-plexfreq-*.rpm` (or open it in
  the file manager). No root, `devel-su`, privileged helper or change to system
  media configuration is used.
- Developer-mode SSH as the normal user is for read-only inspection: logs,
  process state and the `tools/inspect-*.py` probes (run with
  `ssh <phone> python3 - < tools/inspect-....py`), which read the saved session on
  the phone and print only counts, types and flags. Run them only after local
  tests, and do not modify phone state unless explicitly requested.
- The app writes one diagnostic file per local day to
  `~/Documents/PlexFreq/plexfreq-YYYY-MM-DD.log`, falling back to
  `<AppDataLocation>/logs/` (`~/.local/share/org.plexfreq/harbour-plexfreq/logs/`)
  when Documents is not writable. The system journal is not readable by the
  unprivileged user. Logs may contain media URLs.
- The Qt5 device fixture builds the same `Backend`, Rust library and test QML for
  the phone; it needs `tools/build-sailfish.sh` first:

  ```sh
  ./tools/build-device-tests.sh
  ```

  Output: `build/device-tests/plexfreq-backend-test` (and an RPM in
  `build/device-tests/RPMS/`). It uses the phone's Qt5 runtime, generated media,
  local loopback HTTP and temporary state. Copy it to a user-owned directory and
  run it over SSH, adding the opt-in variables above where needed. It tests the
  adapter and runtime on real hardware, not the installed sandbox.
- A transient QML load check of an installed or unpacked binary:
  `PLEXFREQ_STATE_DIR=$(mktemp -d) harbour-plexfreq --smoke-test`.

## Release

The version lives in `Cargo.toml`, `app/harbour-plexfreq.pro`,
`app/rpm/harbour-plexfreq.spec` and `CMakeLists.txt`;
`tools/release-version.py` fails if they differ and, with `--tag`, if the tag is
not `v` plus that version. Binaries receive it as `PLEXFREQ_VERSION` from CMake or
qmake. The device fixture spec `tests/sailfish/rpm/plexfreq-backend-test.spec`
carries its own version.

`.github/workflows/release.yml` runs on `v*` tags and manual dispatch:

1. The full CI workflow.
2. `build`: version check, `build-sailfish.sh`, `check-harbour.sh`,
   `build-device-tests.sh`, and an RPM version/`aarch64` check.
3. `desktop`: Qt6 Release build without tests, then
   `cpack --config build/desktop/CPackConfig.cmake -B build/packages`, producing
   `plexfreq-<version>-linux-amd64.tar.gz` (one top-level directory with
   `bin/plexfreq` and `share/`). The archive is extracted to a temporary
   directory, checked for layout and x86-64 ELF, and smoke-tested offscreen under
   `dbus-run-session` with the fake sink.
4. `publish` (tag pushes only, the only job with `contents: write`): attaches the
   Sailfish aarch64 RPM and the Linux amd64 tar.gz to the GitHub release. Manual
   runs only upload workflow artifacts.

The desktop archive is not a bundled runtime: it uses the host's Qt6/QML, platform
plugins and libpulse (CI builds against Ubuntu 24.04's Qt 6.4 and glibc 2.39), and
runs unpacked from any directory. Runtime requirements ship in
`share/doc/plexfreq/README.md` (from `packaging/README-desktop.md`).

## Review process

`AGENTS.md` is authoritative; in summary:

- Every plan gets an independent adversarial review before execution, and every
  material change set before it is marked complete. Self-review does not count.
- Reviews run in parallel: OpenAI Sol through `tools/external_review.py --chain
  main` and a fresh Sonnet subagent. A third review uses `--chain third`, the
  first available of Space Bunny, DeepSeek v4.1 Flash and Muse Spark; if none
  works, record that and continue with two.

  ```sh
  python3 tools/external_review.py --source . --prompt-file <prompt.md> \
      --output review-notes/<name>.md --chain main [--diff]
  ```

  The tool runs `opencode` with a read-only reviewer agent on a sanitized snapshot
  (no Git data, build output, packages, logs, session/queue/listen/history files,
  credentials or compiled catalogues; `--diff` adds `git diff HEAD`). Exit code 0
  means a review was written (the first line names the model), 3 that every model
  in the chain was unavailable. Its sanitization and fallback order are tested by
  `tools/test_external_review.py`. Free tiers may log prompts: send only
  repository source and docs.
- Adjudicate blocking findings against the code before acting; resolve them before
  completion, or record the work as blocked.
- Records (scope, findings, severity, disposition, verification) go in the
  gitignored `review-notes/`, summarized in the commit message when committing,
  never in `docs/`.
- Prototype tier: disposable experiments under `tools/` or ignored
  `build/`/`target/` paths that never ship may use a self-check plus a short
  results note in `review-notes/`. Independent review is still required before
  their results drive a product decision or their code moves into the app.
  Security and data rules always apply in full.
