# Review records

One file per reviewed plan or change set, named `YYYY-MM-DD-<topic>.md`, as required
by "Independent adversarial review" in `AGENTS.md`. Each record states:

- scope (plan, commit range or diff reviewed) and reviewers/models used, including
  any reviewer that was unavailable;
- findings with severity (Blocking / Major / Minor / Note), each adjudicated against
  the code (valid, wrong, duplicate);
- disposition (fixed, deferred with reason, rejected with evidence) and the
  regression tests or checks that verify each fix;
- remaining blockers, mirrored in the "Open items" section of `docs/roadmap.md`.
