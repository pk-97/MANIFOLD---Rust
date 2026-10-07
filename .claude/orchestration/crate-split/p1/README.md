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
  P1 inventory includes six seam files and 22 testkit files.
- `rewrites.tsv`: validated Rust path/macro substitutions. Move rows also derive
  module substitutions. Unmoved family references retain their renderer owner.
  File-path rewrites derive from moves and affect include/path syntax only.
- `manifests.json`: contextual replacements targeting only Cargo.toml or
  Cargo.lock, applied before relocation, including testkit feature wiring.
  Arbitrary source patch rows (declarations.json and finish.json) are forbidden.
- `declarations.tsv`: four nonempty tab-separated columns: file, `add|remove`,
  anchor, exact line. Applied in row order after relocation, path rewrites and
  templates; paths and anchors describe that replayed tree. An add inserts before
  a unique exact anchor line (`@start`/`@end` also work for empty files); a remove
  requires anchor = exact line and exactly one match. Only standalone module-level
  `mod ident;` and `use <use-tree>;` lines are accepted, with optional `pub`,
  `pub(crate)`, `pub(super)` or `pub(in path)` visibility. Adjacent attributes are
  limited to `cfg`, `cfg_attr` recursively containing only these attributes, and
  `doc(hidden)`. Remove all attached attributes when removing an item; additions
  cannot capture existing attributes. Bodies, comments, inline mod bodies and
  other attributes (including `path` and `macro_use`) are forbidden. Added modules
  need regular source files in the replayed tree; added use paths must exist in
  its lexical crate/source item inventory (external dependencies and re-export
  resolution are not inferred). Unedited bytes and line endings are preserved.
- `templates/`: new files at repository-relative destinations; overwrites fail.
  Engine build.rs hashes only engine-owned source rows. Renderer build.rs must
  already have its final family emission; replay cannot delete emission bodies.
  The former cross-crate tempo.rs source read is not reproduced.

Templates and manifests are code. Verify prints every template destination,
Git mode and SHA-256 of its exact bytes, and each manifest hunk with SHA-256 of
its canonical JSON (sorted keys, ASCII escapes, compact separators, UTF-8).
Verify also prints every declaration row, including its anchor and exact line.
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
grouped imports where they retain a common prefix. Reproducibility proves the
reviewed transformation, not Rust semantics. The lexical move gate, compiler,
census and phase tests remain required.

This plan is incomplete against the assembled pre-move tree: the base lacks six
seams and 22 testkit inputs. The removed 73 declaration patch rows need reviewed
typed declaration rows against that source. Prepare the renderer's
identity-emission seam separately. Review the engine identity source list and
coverage before the final replay; residual member widenings are not invented.
