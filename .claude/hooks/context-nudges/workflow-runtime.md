# Workflow runtime

Driving or editing a workflow program: read `docs/WORKFLOW_RUNTIME_GUIDE.md` first — exit
codes (0 is NOT success by itself; check `parked.jsonl`), expected failure classes
(parse-park, D-54 truncation, misquoted `find`, POOL FULL), check-in discipline (poll the
run dir, a rerun is a new sample), and the program TOML reference. Never steer a run
mid-flight; never hand-edit run state except escalation answers. The design contract is
`docs/WORKFLOW_RUNTIME_DESIGN.md`.
