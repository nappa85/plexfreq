# PlexFreq development rules

- Read `docs/architecture.md` and `docs/development.md` before changing platform
  integration.
- Documentation describes the current behaviour and the verified external facts
  it relies on, with their sources. It is not a log: no dates, incidents,
  outcomes, review rounds or TODO lists. Update the relevant section when
  behaviour changes and delete what no longer applies.
- `README.md` is for users: what PlexFreq does, installation, settings,
  troubleshooting and a feature comparison with Plexamp. Keep technical detail
  in `docs/`.
- Unfinished work is fixed, not documented. The only known gaps are missing
  Plexamp features, listed in the README comparison table.
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
  Sailfish integration/permission validation requires a normally installed app
  launched under Sailjail; unpacked terminal execution is not a substitute.
  Release assets are the Sailfish aarch64 RPM and Linux amd64 desktop tar.gz.
  The desktop archive runs unpacked using compatible host Qt6/audio libraries.
- Checks: `./tools/check.sh`; SDK packaging: `./tools/build-sailfish.sh`.
  A successful package build is not a hardware runtime test.
- Keep session fixtures in temporary directories. Never put actual tokens in
  fixtures, command-line arguments, logs or source files. Framework media URLs
  may appear in diagnostics; don't treat their output as redacted.
- The repository may have no Git metadata. Do not initialize, commit or publish
  unless requested.

## Independent adversarial review

- Every plan must receive an independent adversarial review before execution, and
  every material implementation step/change set must receive one before being marked
  complete, except under the prototype tier below. Use a separate reviewer/subagent
  with a fresh context; self-review is supplemental, not independent review.
- Review correctness, missing assumptions, failure modes, security, source/rule
  fidelity, deployability (Sailfish Qt5.6/Silica, Harbour, desktop Qt6), suitable
  existing libraries, duplicate logic, maintainability and production practices.
  Prefer a read-only reviewer; the implementer owns fixes.
- Record the reviewed scope, findings, severity, disposition and verification in
  the gitignored `review-notes/` folder (and summarize them in the commit message
  when committing), never in `docs/`. Review findings are not proof of
  correctness.
- Reviewer setup (adopted from ../mimik, user decision 2026-10-16):
  - Main reviewers: OpenAI Sol (`openai/gpt-6.1-sol` via
    `tools/external_review.py --chain main`) and a fresh Sonnet subagent, in parallel.
  - Third reviewer: the first that works of Space Bunny, DeepSeek v4.1 Flash, then
    Muse Spark (`--chain third`). If none works, proceed with the two main reviews
    and record that.
  - `tools/external_review.py` reviews a sanitized read-only snapshot: no Git data,
    build output, packages, logs, Plex session/queue/listen/history files or
    credentials (`tools/test_external_review.py`). Free tiers may log prompts, so
    send only repository source and docs.
  - Adjudicate Blocking findings against the code before acting on them.
- Resolve blocking findings before completion. If independence is unavailable or a
  blocker cannot be fixed, explicitly record it and keep the work blocked; do not
  call it production-ready or silently waive it. Review of review records themselves
  does not recurse.

### Prototype tier

- Disposable experiments (inspection scripts, benchmarks and prototypes under
  `tools/` or ignored `build/`/`target/` paths that never ship in the RPM or desktop
  archive) may use a lighter process: a self-check plus a short results note in
  `review-notes/` with exact scope, denominators and limitations.
- Independent review is still required before experiment results drive a product or
  architecture decision, and before any prototype code moves into the shipped app.
- Security and data rules still apply in full: no tokens read back or sent to
  reviewers, phone access stays read-only unless requested, and checks are never
  reported as passed when not run. Shipped code keeps every gate.
