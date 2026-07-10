# UI Layout Invariant Lints — the tree dump becomes a gate

**Status:** PROPOSED · 2026-07-10 · Fable, from Peter's ask after BUG-108's escape
**Prerequisites:** none — UI_HARNESS_UNIFICATION P0–P3 SHIPPED, UI_CLIP_AND_Z_OWNERSHIP shipped the flags this leans on
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The harness renders the app faithfully and dumps every node's real rect — and then the
only thing standing between a layout defect and main is someone looking at a PNG and
being honest about what they see. BUG-108 proved the failure mode in one sentence (the
executing agent's own words): *"I looked right at it and dismissed it … I rationalized
it away instead of flagging it."* The "+ Add Effect" button floated mid-card over the
Sun rows in that session's own verification PNGs, and the anomaly was explained away as
fixture noise. Peter's ask, verbatim: **"Is there a way we can improve our headless UI
test system to prevent these types of failures?"**

The answer this doc commits to: **machine-checkable layout invariants over the built
`UITree`, run as ordinary `cargo test` on every harness scene.** Assertions cover
geometry; the PNG look remains the net for appearance (color, glyphs — BUG-107's
mojibake is a font-coverage bug, not a geometry bug, and stays out of scope here).

Sibling doc: [HARNESS_FIDELITY_INVARIANT_PROPOSAL.md](HARNESS_FIDELITY_INVARIANT_PROPOSAL.md)
guarantees the harness renders *what the app renders* (the path); this doc guarantees
what's rendered is *geometrically legal* (the result). Together they close the
"faithful render, nobody checks it" gap. Like that doc, this is a focused post-wave
proposal — it does not reopen the shipped UI_HARNESS_UNIFICATION_DESIGN.md.

## 1. Audit — what exists (verified 2026-07-10)

Extend, don't redesign. Everything below was read in-tree today; the design is mostly
wiring.

| Piece | Where | State |
|---|---|---|
| Clip-aware geometric assertion over a real scene | `crates/manifold-app/src/ui_snapshot/mod.rs:1021` (`cache_path_inspector_does_not_paint_below_footer_top`) | **EXISTS — the pattern to generalize.** Builds the real `bug060` scene via `fixtures::build` + `UIRoot::build()`, drives the live scroll gesture, walks `traverse_flat_range` reconstructing the effective clip per node (`TraversalEvent::PushClip/PopClip/Node`), replicates the GPU cull, reports every leaking node with id/rect/clip/text. Pure CPU, ordinary `cargo test -p manifold-app`. |
| Scene registry | `crates/manifold-app/src/ui_snapshot/fixtures.rs:28` (`build`) | EXISTS — 15 named `UIRoot` scenes (`timeline`, `inspector`, `bug060`, `audiosends`, `gltfscene`, …), all buildable headless in tests. |
| Per-node geometry + style in the dump | `crates/manifold-app/src/ui_snapshot/dump.rs:28` (`dump_tree_ex`) | EXISTS — rect, type, `flags`, bg/border/text colors *with alpha*, `draw_order`, live `parent`, registered `name`, `widget` id. `custom_surfaces` is the precedent for additive top-level keys. |
| Overlay strata | `crates/manifold-app/src/ui_root.rs:379` (`overlay_draw: Vec<(usize, usize)>`) | EXISTS — z-ordered node-index ranges at the tree tail; each renders in its own depth push (`ui_frame.rs:662-698`). **Not represented in the dump today.** |
| Declared-overflow escape hatch | `crates/manifold-ui/src/node.rs:381-388` (`CLIPS_CHILDREN`, `ALLOW_OVERFLOW`) | EXISTS — `ALLOW_OVERFLOW` is UI_CLIP_AND_Z_OWNERSHIP D3's opt-out, legitimate use is the Ghost tier (drag ghosts), any other use must name why in a comment. This is already the declaration mechanism the lint respects — no new flag needed. |
| Named-node addressing | `dump.rs:62` (`name`), UI_AUTOMATION D8 registration | EXISTS — anchored assertions address nodes by registered name, same resolution the flow driver uses. |
| Immediate-mode surfaces (timeline clips, lanes, graph canvas) | `ui_snapshot/render.rs`, `custom_surfaces` | EXIST but are **not tree nodes** — out of this doc's scope (Deferred, with trigger). |

Genuinely new: the lint functions, the scene-iterating test, the report-mode rollout.
That is the whole build.

## 2. Decisions

**D1 — Lints run on the in-memory `UITree` in `cargo test -p manifold-app`, not on the
JSON dump.** The tree carries typed flags and the real traversal with clip events; the
walk replicates exactly what the renderer culls and scissors (the `mod.rs:1021`
pattern). The JSON dump stays the human/agent-facing artifact. *Rejected: linting the
JSON (e.g. a Python hook)* — a second, parallel representation of legality that can
drift from the renderer's actual clip/cull behavior, and it would live outside the
workspace gate. The fidelity proposal just spent a whole doc killing parallel
representations; we don't add one back.

**D2 — The legality model is structural; no new flags, no per-site mute lists.** A
node "paints" iff it is `VISIBLE`, has positive area, survives the clip-stack cull, and
has visible ink: bg alpha ≥ 8/255, or non-empty `text`, or `border_width > 0` with
border alpha ≥ 8/255. Overlap between two painted nodes is legal when ANY of:
1. one is an ancestor of the other (a label on its panel — normal composition);
2. they are in different paint strata — a stratum is the main range (nodes outside
   every `overlay_draw` range) or one `overlay_draw` range; strata are z-separated by
   construction (`Depth::OVERLAY.above(i)`), so cross-stratum overlap is the *point*
   of overlays;
3. either node sits under an `ALLOW_OVERFLOW` subtree (the Ghost tier — already the
   declared escape hatch, already comment-gated by UI_CLIP_AND_Z D3).

Everything else — two painted non-ancestor nodes in the same stratum whose clipped
rects intersect — is a finding. BUG-108's floating button over the Sun rows is exactly
this shape. *Rejected: a per-node "overlap is fine here" flag* — that is the noisy-gate
death in slow motion: every false positive gets a mute flag instead of a legality-model
fix, and in a month the lint means nothing. If the structural rules above are wrong,
the fix is in the rules, once, not at call sites.

**D3 — Report first, enforce second.** P1 runs the lint in report mode across all 15
scenes and produces the violation inventory; each finding is triaged to exactly one of:
a real bug (gets a BUG entry), a legality-rule gap (D2 is amended in this doc), or a
legitimate declared overflow (gets `ALLOW_OVERFLOW` + the mandatory comment). Only
then does P2 flip to enforcing per-scene tests. *Rejected: enforce from day one* — an
untuned gate that cries wolf on a popover gets muted within a week, which is worse
than no gate (this is why the legality model is the one real design decision here).
**Escalation trigger:** if report mode finds intra-stratum overlaps that are neither
bugs nor Ghost-tier — i.e. the codebase legitimately layers opaque siblings within a
stratum in some pattern D2 doesn't foresee — stop, bring Peter the inventory, and amend
D2 before any enforcement lands.

**D4 — Two nets, not five.** v1 ships (a) the **generic intra-stratum overlap lint**
(the class-killer, runs on every scene unchanged) and (b) an **anchored-assertion
helper** for per-surface contracts, addressing nodes by registered name (D6's
mechanism), of which `mod.rs:1021` is the hand-rolled precedent. Containment stays
per-surface anchored (the bug060 test is one); a *generic* containment lint is mostly
redundant with the renderer's own scissoring — content escaping a `CLIPS_CHILDREN`
ancestor is clipped, invisible, and therefore not a visual defect. Text-overflow
lints need text measurement the dump doesn't carry — Deferred.

**D5 — The dump gains a sibling `overlay_ranges` key** (`[[start, end], …]`, additive,
shaped like `custom_surfaces` at `dump.rs:67-85`) so agents reading a dump can see the
strata the lint reasons about. The lints themselves don't need it (they run on `UIRoot`
directly, which owns `overlay_draw`); this is for the humans and agents downstream.

**D6 — Anchored assertions address nodes by registered `name`/`widget`, never by index
or text-matching.** Same resolution the UI_AUTOMATION flow driver uses
(`select-and-inspect.json` precedent). An assertion that can't find its named node
FAILS (a renamed or unregistered node must break the test, not silently skip it).

**The plausible-wrong architectures, forbidden by name.** You will want to (1) write a
Python script over the JSON in `.claude/hooks/` — no: D1, it's outside the workspace
gate and drifts from the renderer's clip behavior. (2) Reach for golden-image diffing —
no: explicitly deferred by UI_HARNESS_UNIFICATION, and it answers "did pixels change",
not "is the layout legal". (3) Silence findings with per-scene skip lists or a new
per-node flag — no: D2/D3, legality is structural and tuning happens in the model.
(4) Widen into linting the immediate-mode surfaces via `custom_surfaces` rects — no:
Deferred with trigger; tree scope ships first.

## 3. Committed shapes

New module `crates/manifold-app/src/ui_snapshot/layout_lints.rs` (test-support code in
the lib, like the existing `mod.rs` tests' helpers):

```rust
/// One painted node's effective geometry after clip reconstruction.
pub struct PaintedNode {
    pub id: usize,              // tree index
    pub name: Option<String>,   // registered name, when present
    pub rect: Rect,             // clipped (painted) rect, not raw bounds
    pub stratum: Stratum,
    pub allow_overflow: bool,   // true if self-or-ancestor carries ALLOW_OVERFLOW
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Stratum { Main, Overlay(usize) }

/// Walk one built UIRoot the way the renderer paints it (traverse_flat_range +
/// clip stack + GPU-cull replication — shape this like mod.rs:1021) and return
/// every node that paints, per D2's ink predicate.
pub fn painted_nodes(ui: &UIRoot) -> Vec<PaintedNode>;

/// D2's generic lint. Empty vec = clean.
pub struct OverlapFinding { pub a: usize, pub b: usize, pub a_name: Option<String>,
    pub b_name: Option<String>, pub overlap: Rect }
pub fn intra_stratum_overlaps(nodes: &[PaintedNode], tree: &UITree) -> Vec<OverlapFinding>;

/// D6's anchored-assertion helper: painted rect of a named node, or panic
/// with the names that DO exist (a missing name must fail loudly).
pub fn painted_rect_of(nodes: &[PaintedNode], name: &str) -> Rect;
```

Interior (pair-pruning strategy, how ancestorhood is computed, report formatting) is
the executor's business. The ink-predicate constants (alpha ≥ 8) live as named
`const`s in this module so a tuning change is one line.

## 4. Invariants & enforcement

- **I1 — Within a paint stratum, two painted non-ancestor nodes never overlap unless
  declared (`ALLOW_OVERFLOW`).** Enforcement: `layout_lints::tests::scene_<name>_has_no_intra_stratum_overlaps`,
  one per registry scene, enforcing after P2. (P1: same lint, report mode.)
- **I2 — Every scene in `fixtures::build` is covered.** Enforcement: the per-scene
  tests are generated from the same match arms (a test iterating `SCENES`, a slice the
  registry exposes); adding a scene without lint coverage fails
  `layout_lints::tests::registry_is_fully_linted`.
- **I3 — BUG-108's contract ("+ Add Effect" sits below the last card row).**
  Enforcement: an anchored assertion via `painted_rect_of` — **delivered by the
  BUG-108 fix, not by this doc's phases** (the class fix IS the check, and a red test
  can't land ahead of the fix; see Phasing).

## 5. Phasing

**P1 — the walk, the lint, the inventory (one session).**
- Entry state: `cargo test -p manifold-app --lib` green; anchors re-verified
  (`mod.rs:1021` test exists, `fixtures.rs:28` registry, `ui_root.rs:379`).
- Read-back: this doc whole; `mod.rs:1021` end-to-end; `ui_frame.rs:654-699`.
- Deliverables: `layout_lints.rs` (shapes per §3); `SCENES` slice on the fixtures
  registry; report-mode test printing the full finding inventory for all 15 scenes;
  the `overlay_ranges` dump key (D5); the triage table appended to THIS doc (§7) —
  every finding classified bug / rule-gap / declared, per D3.
- Gate (positive): `cargo test -p manifold-app --lib layout_lints` runs all scenes and
  prints the inventory; the triage table is committed with zero unclassified rows.
  Gate (negative): `rg 'skip_scene|lint_allowlist' crates/manifold-app` → zero hits
  (no mute machinery landed).
- Forbidden moves: enforcing anything this phase; adding mute flags; touching scene
  fixtures to make findings disappear.
- Demo: the printed inventory itself — `none beyond L1` is acceptable here (the
  artifact is the report). Test scope: `-p manifold-app --lib` focused.

**P2 — enforce (one session; entry-gated on P1 triage complete).**
- Entry state: P1's triage table has zero rows classified "bug" still unfixed in a
  linted scene — an enforcing test may not land red (a finding that can't be fixed
  yet keeps its scene in report mode, listed by name in the test with a BUG-NNN
  comment, and that list must shrink to empty as the bugs close).
- Deliverables: per-scene enforcing tests (I1), `registry_is_fully_linted` (I2),
  report mode deleted or reduced to the named-exceptions list.
- Gate: full `cargo test -p manifold-app` green; negative gate from P1 re-run.
- Forbidden moves: the per-node mute flag (D2); silently dropping a scene from
  coverage; fixing a finding by making the offending node transparent.

The BUG-108 anchored regression (I3) rides the BUG-108 fix, whichever session takes
it, using P1's `painted_rect_of` — that fix's phase brief should name it a deliverable.

## 6. Decided — do not reopen / Deferred

Decided: lint the tree, not the JSON (D1) · structural legality, no mute flags (D2) ·
report-then-enforce (D3) · two nets in v1 (D4) · names, not indices (D6).

Deferred, each with its revival trigger:
- **Immediate-mode surface lints** (timeline clips, lanes, graph canvas via
  `custom_surfaces` rects) — trigger: a layout escape on one of those surfaces.
- **Text-overflow lint** — trigger: a text-clipping escape; needs measured text
  extents in the dump first.
- **Golden-image diffing** — owned by UI_HARNESS_UNIFICATION's deferral; not revived
  here.
- **Cross-stratum sanity** (an overlay fully off-screen, a stratum with zero painted
  nodes) — trigger: an overlay-positioning escape.

## 7. P1 triage table

*(Appended by P1. Every row: scene · finding · classification (bug BUG-NNN /
rule-gap → D2 amendment / declared ALLOW_OVERFLOW) · resolution.)*
