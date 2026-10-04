# PlexFreq development rules

- Read `docs/research.md`, `docs/architecture.md`, `docs/validation.md` and
  `docs/roadmap.md` before changing platform integration. Record new findings,
  consulted sources and actual check results in these documents.
- Research documented APIs/other implementations before choosing integration
  approaches. Develop local tests before live Plex or phone experiments.
- Rust owns Plex/media HTTP, credentials, models/queue policy, worker lifetime,
  audio/DSP/transport, MPRIS and network facts. C++ is a thin Qt model/C-ABI/
  translation/window adapter; QML is presentation/navigation.
- Sailfish target is Qt5.6 + Silica, desktop Qt6 + Controls2. Shared QML must
  remain QtQuick2.6 compatible. Never introduce desktop imports into Silica.
- Use aarch64-unknown-linux-gnu, not musl, when linking Rust with Sailfish Qt.
  Compile C dependencies through `tools/sdk-cc.sh` against the actual sysroot.
- No phone root, privileged helper or modifications to system media services.
  Rootless bundle runs from a user-owned directory; sandbox validation requires
  a normally installed app and is distinct from terminal execution.
- Checks: `./tools/check.sh`; SDK packaging: `./tools/build-sailfish.sh`.
  A successful package build is not a hardware runtime test.
- Keep session fixtures in temporary directories. Never put actual tokens in
  fixtures, command-line arguments, logs or source files. Framework media URLs
  may appear in diagnostics; don't treat their output as redacted.
- The repository may have no Git metadata. Do not initialize, commit or publish
  unless requested.
