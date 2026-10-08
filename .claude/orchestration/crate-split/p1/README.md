# P1 replay plan

Reviewed input to `scripts/crate_move_replay.py`; no executable plan hooks.
Python 3.9+ on POSIX is supported. Run from this repository:

```
scripts/crate_move_replay.py replay --plan .claude/orchestration/crate-split/p1 --dest target/p1-move
scripts/crate_move_replay.py verify --plan .claude/orchestration/crate-split/p1 <move-commit>
```

Commit and review the complete plan and tool BEFORE the move. Verify reads the
plan from the immediate parent and rejects any plan addition, deletion, mode or
byte change in the move commit. The supplied local plan must match the parent.
No child plan is overlaid. Draft replay defaults to HEAD (`--source` selects a
different commit) and may use local plan bytes, but cannot verify until that plan
is committed before the move. Neither command changes the checkout or index.
Body changes and residual repairs belong in subsequent separately reviewed commits.

- `plan.json`: version, owning crates, Rust rewrite roots and explicit helper
  aliases. Unknown configuration and plan files fail closed.
- `moves.tsv`: required source and destination paths, separated by a tab. The
  P1 inventory includes the six existing seam files, root testkit fixtures and
  nested owner-local probes, with ordinary module mounts.
- `rewrites.tsv`: validated Rust path/macro substitutions. Move rows also derive
  module substitutions. Unmoved family references retain their renderer owner.
  File-path rewrites derive from moves and affect include_str!/include_bytes!
  syntax only; path-mounted modules are rejected.
- `manifests.json`: contextual replacements targeting only Cargo.toml or
  Cargo.lock, applied before relocation, including testkit feature wiring.
  Arbitrary source patch rows (declarations.json and finish.json) are forbidden.
- Module wiring is derived from `moves.tsv`. Replay finds exactly one old
  out-of-line `mod` item, removes it with its attached attributes, and mounts it
  in the new parent with identical visibility and attribute bytes. Only the
  module identifier may change, as determined by the destination filename.
  Co-moved parent/child mounts remain in place. Existing parents receive derived
  items; new parents must be reviewed templates containing the exact item.
  Missing, ambiguous, inline, `#[path]` and include-based mounts fail closed.
  The lexical reader also rejects comment-attached or non-whole-line mounts;
  prepare these in a separate reviewed fix before replay.
  `declarations.tsv` is not an accepted plan file. Use items are never added or
  removed: existing path rewrites preserve aliases, visibility, cfg and globs.
  Unresolved imports are compile errors for a separate reviewed fix commit.
- `templates/`: new files at repository-relative destinations; overwrites fail.
  Engine build.rs hashes only engine-owned source rows. Renderer build.rs must
  already have its final family emission; replay cannot delete emission bodies.
  The former cross-crate tempo.rs source read is not reproduced.

Templates and manifests are code. Verify prints every template destination,
Git mode and SHA-256 of its exact bytes, and each manifest hunk with SHA-256 of
its canonical JSON (sorted keys, ASCII escapes, compact separators, UTF-8).
Reviewers must sign off on exactly those bytes: template inventory;
module/import/visibility/cfg changes against prior owners; build.rs source list
and identity key; dependency versions, build dependencies, feature forwarding
and lockfile edges. A digest identifies bytes, not semantic purity. New bodies
and repairs need separate reviewed commits; do not hide them in these channels.

Materialization uses Git blobs and modes, including symlink targets, and must
round-trip exactly to the parent's Git tree inventory before replay. Verification
compares output against the commit's Git tree, never the checkout. Case-folded
and Unicode-normalized aliases anywhere in the tree, including directories,
fail before writes. Symlink components are checked before mkdir; regular files
are written without following symlinks. Failed replay removes its newly created
output; existing destinations are preserved. Text is explicit UTF-8 and preserves
newline bytes. Traversal and diagnostics are sorted.

Rewriting is lexical, preserves comments and ordinary strings, and preserves
grouped imports as one item, including when their owners diverge. Reproducibility proves the
reviewed transformation, not Rust semantics. The lexical move gate, compiler,
census and phase tests remain required.

The draft inventory has 598 moves against the prepared renderer layout. Mesh,
particles, nested tests and probes use ordinary mounts. The 27 retired test
rows, the family-only mesh-cut oracle, and the unmounted compile-contract source
are excluded. Eleven water shader rows include the shared adjacency source;
compositor shaders and fx_watercolor_compute stay with their family owners.
The engine identity uses emit_owned_source_identity and has no implicit
foundation source read. Templates preserve exact source mount attributes and
visibility and introduce no re-export facade.

Compilation of this draft is not verified. The requested target/p1-try-target
compile was refused by storage admission because it is not a registered
worktree's canonical target. In particular, post-move repair must remove the
renderer build script's engine emission, expose owner-local probes and fixtures
under testkit (their original cfg(test) mounts are preserved by INV-2), and
resolve compiler-demanded imports and visibility. No residual repairs are
encoded as template overwrites or source patches. The lead must compile and
review the residual inventory before treating this plan as complete.

A5 makes extent rules owner-submitted inventory entries. Its 65 engine-owned
rule files move with their node parents; 29 family rule files stay with theirs.
The ocean sizing helper remains inside the ocean family. Testkit graph imports
are explicit. Solver identity uses the equivalent relative visibility restriction
pub(in super::super), which replay preserves: node_graph before the move,
water afterwards.
