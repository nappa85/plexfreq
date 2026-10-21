# MPRIS delta emission and AVRCP position resync (2026-10-21)

Bug: a Bluetooth (AVRCP) display showed the track cursor at about half when
songs ended. BlueZ `mpris-proxy` forwards our PropertiesChanged. BlueZ
`media.c` resets the AVRCP position to 0 on every Metadata it receives, and
PlexFreq re-sent Metadata on every projection change. Evidence is in
docs/research.md ("MPRIS and the Bluetooth AVRCP bridge").

## Plan review (round 1)

Scope: the plan, against the repository snapshot.
Reviewers: Sol (`openai/gpt-6.1-sol`, main chain), a fresh Sonnet subagent, and
DeepSeek v4.1 Flash (third chain; Space Bunny failed its preflight with a
server error).

| ID | Severity | Finding | Adjudication / disposition |
|----|----------|---------|----------------------------|
| P1 (DeepSeek) | Blocking | mpris-proxy behavior and BlueZ 5.87 vs master unverified | Valid. Checked upstream tag 5.87 and the Sailfish fork. The relevant `media.c` functions and all of `mpris-proxy.c` match. The proxy forwards PropertiesChanged and Seeked verbatim and calls GetAll only at registration. Recorded in research.md. |
| P2 (all three) | Blocking/Major | trackid, Metadata and Seeked position come from separate reads; a Seeked could carry another occurrence's position | Valid. Metadata now takes its trackid from the gate snapshot. The resync re-reads the snapshot and requires the same occurrence, loaded and `presented.shown == id`. Otherwise it is dropped (id changed or unloaded) or deferred. |
| P3 (all three) | Major/Minor | Baselines advance even when `emit_signal` fails | Valid. `emitted`/`previous` advance only on Ok; the resync clears only on Ok. |
| P4 (Sol, DeepSeek) | Major | Signal-level tests required, both sides of the 1 s threshold | Valid. The session-bus fixture was extended (see validation.md). |
| P5 (Sonnet, DeepSeek) | Minor | Disappearing keys | Valid. They go in `invalidated`. |
| P6 (DeepSeek) | Minor | Stale `app/src/mpris.*` paragraph in architecture.md | Valid. Rewritten. |
| P7 (DeepSeek, Sonnet) | Note | A resync Seeked stretches the MPRIS spec | Accepted. Display clients only update position; a scrobbler may log a seek. Documented in research.md. |
| P8 (DeepSeek) | Minor | Duplicate Seeked after a user seek plus Metadata | Accepted. Harmless; the resync comes last. |

## Implementation review (round 1)

Scope: the uncommitted diff of `src/runtime/platform.rs` and the docs.
Reviewers: Sol, Sonnet (ran the tests 3x, all passed) and DeepSeek. No
Blocking findings.

| ID | Severity | Finding | Adjudication / disposition |
|----|----------|---------|----------------------------|
| I1 (DeepSeek) | Major | With `shown == id` but the model not yet merged (`start_plan`, `settle_resolution`), Metadata carries the new trackid with the old title; docs claimed "model caught up"; not tested | Partly valid. The Seeked position always belongs to the emitted trackid, so it is never wrong. Only the title is briefly stale, which was already the case before this change, and the merge re-sends Metadata plus the resync. Docs corrected (architecture, validation). The untested window is recorded as a gap. |
| I2 (Sonnet, Sol, DeepSeek) | Minor | Negative Seeked assertions used `try_recv`; resume not checked for Seeked | Fixed: bounded 200 ms / 300 ms windows, including after resume. |
| I3 (Sonnet) | Minor | The Volume wait could match an older queued signal | Fixed: queued signals are drained first. |
| I4 (Sonnet, DeepSeek) | Minor | "No Seeked at start" depends on timing | Rejected as a flake risk. The resync check runs in the same pass as the Metadata emission, right after it. |
| I5 (Sol, DeepSeek) | Minor | No tests for emission-failure retry, occurrence change/unload cancellation or `invalidated` on the wire | Accepted test gaps, recorded in validation.md. |
| I6 (Sol, DeepSeek) | Minor | validation.md referenced a check result that was missing | Fixed. |
| I7 (DeepSeek) | Note | Dict equality might be positional | Rejected. zvariant 4.2 `Dict` wraps a `BTreeMap` (`dict.rs:19-20`). |

Checks: see docs/validation.md. Remaining: validation on the phone with an AVRCP
display. Not production-validated on hardware.
