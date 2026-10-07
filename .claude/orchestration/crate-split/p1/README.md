# P1 replay plan

Reviewed input to `scripts/crate_move_replay.py`; no executable plan hooks.
Run from this repository:

```
scripts/crate_move_replay.py replay --plan .claude/orchestration/crate-split/p1 --dest target/p1-move
scripts/crate_move_replay.py verify --plan .claude/orchestration/crate-split/p1 <move-commit>
```

Replay reads HEAD by default; `--source <revision>` selects another pre-move
commit. It materializes Git blobs, not working-tree files, and requires a new
destination. It changes neither the source checkout nor its index. Verification
requires a single-parent commit and compares every file's bytes and Git mode,
including symlink targets. It requires the supplied plan to equal the plan in
that commit. A plan first added by the move is overlaid on the parent; no other
paths are excluded from comparison. Commit the tool before the move; verify
with the reviewed tool. Residual fixes belong in subsequent reviewed commits.

- `plan.json`: version, owning crates, Rust rewrite roots, explicit helper
  aliases, and the physics source-identity split contract. Later phases supply
  their own configuration, maps, wiring hunks and templates.
- `moves.tsv`: source and destination paths, separated by a tab; every source
  is required. Includes the six seam files and all 22 testkit rows available
  from the tests lane on 2026-10-07 (including runtime and impulses).
- `rewrites.tsv`: longest-prefix Rust path substitutions, including primitive
  and param_tooltips macro paths and primitive's exported helper macros.
  Move rows also derive module substitutions. Unmoved family references keep
  their renderer owner when the caller moves to the engine.
- `manifests.json`: exact contextual replacements applied before relocation.
- `declarations.json`: exact module/import wiring replacements applied after
  Rust path rewriting. Extracted from the stage-1 reference; no full-source
  snapshots or function-body repairs are embedded.
- `templates/`: new files at their repository-relative destinations. Existing
  destinations fail. Engine build.rs hashes only engine-owned source rows;
  renderer build.rs retains its family emission after removal of its integration
  emission. The pre-move identity seam must already exist. The old cross-crate
  tempo.rs source read is not reproduced.
- `finish.json`: exact testkit dependency/feature wiring, applied last.

Traversal and diagnostics are sorted. Conflicting move rows, missing sources,
changed wiring context and template collisions fail; nothing is silently skipped.
Rewriting is lexical and preserves grouped imports when they retain a common
prefix. Reproducibility proves the reviewed transformation, not its Rust semantics;
the lexical move gate, compiler, census and phase tests remain required.

This plan is not yet validated against the assembled pre-move tree. The base
34ca0d9a5 lacks the seam/testkit inputs. Reconcile declaration contexts with the
assembled source before the final replay. Stage-1 module visibility wiring is
retained for review; residual member widenings are not invented by the tool.
