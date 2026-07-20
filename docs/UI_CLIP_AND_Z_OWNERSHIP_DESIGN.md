# UI Clip & Z Ownership — bounds become binding, stacking becomes declared

**Status:** P1 SHIPPED 2026-07-08 (region mechanism + main-window migration + D4 enforcement; BUG-060 stopgap removed — landing report: `docs/landings/2026-07-08-ui-clip-z-p1.md`). P2 (editor window + perform) and P3 (enforcement closure + sweep) OPEN. One carried gap: D2 tier-ordering is enforced on the `traverse()` render path (headless snapshots + editor window) but NOT on the live main-window cache path (`panel_cache_info`, array-ordered), where D1 containment alone carries BUG-060 — see VERIFICATION_DEBT VD-018, close in P2. · design 2026-07-07 · Fable
**Prerequisites:** none (UI_ARCHITECTURE_OVERHAUL phases 0–8 shipped 2026-06-23; this builds on that substrate)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The governing insight: the architecture overhaul gave panels a clean way to
**compose**, but nothing makes their **bounds** binding. Pixel clipping is
opt-in per panel (`CLIPS_CHILDREN`), and stacking is an accident of build order
(`draw_order = insertion index`). Every post-overhaul geometry bug is one of
those two facts wearing a different panel: the inspector paints over the footer
because it never opted into a clip and happens to build later (BUG-060); Audio
Setup content spills past the panel edge (BUG-047); node previews composited
over the wrong chrome in the editor window (BUG-027, fixed locally). This
design moves both properties to the substrate: **a top-level region's rect is a
GPU scissor its content cannot escape, and its stacking tier is declared, not
inferred** — for the main window, the graph-editor window, and every future
window, because the mechanism lives at the `UITree` level both windows share.

On stage this is containment: a panel can never obscure the transport bar or
bleed into the output preview, no matter what a future feature builds inside
it. The bug class dies by construction instead of panel-by-panel.

Companions: `UI_ARCHITECTURE_OVERHAUL.md` (the substrate this extends),
`archive/UI_ARCHITECTURE_AUDIT.md` (as-built findings), `DRAG_CAPTURE_DESIGN.md` (the
input-side sibling: same single-owner principle applied to pointer events),
`FOUNDATIONAL_GAPS.md` A2 (the inventory entry this design discharges).

---

## 1. Audit — what exists (verified 2026-07-07)

| Piece | Where | State |
|---|---|---|
| `CLIPS_CHILDREN` flag | `manifold-ui/src/node.rs:381` | Complete mechanism, **opt-in**: tree traversal tracks active clip ancestors (`tree.rs:723–821`), renderer batches rects by scissor (`manifold-renderer/src/ui_renderer.rs:248`) |
| Chrome API clip | `manifold-ui/src/chrome/view.rs:389`, `chrome/diff.rs:69,141` | `.clip()` sets the flag declaratively — for chrome-built subtrees only |
| ScrollContainer | `manifold-ui/src/scroll_container.rs:86` | Sets `VISIBLE \| CLIPS_CHILDREN` on its viewport node — the in-repo precedent for "a container that owns its clip" |
| Panels that opt in today | `viewport.rs:1470`, `layer_header.rs:1532`, `param_slider_shared.rs:2013`, `param_card.rs:1834` | Correct but scattered; the inspector's only `CLIPS_CHILDREN` is in a test (`inspector.rs:2711` comment) — BUG-060's root |
| Z / stacking | `manifold-ui/src/tree.rs:247` (`draw_order: self.count as i32`) | **Insertion order is the only z.** Whoever builds later paints later |
| Overlay z registry | `manifold-app/src/ui_root.rs:15–17` | Explicit bottom→top `Z_ORDER` for popups/modals — z exists for the overlay tier only; base panels are unmanaged |
| Panel build contract | `ui_root.rs:632–633` (`self.footer.build(&mut self.tree, &self.layout)`) | Panels self-root at tree root and read their rect from `ScreenLayout` — nothing intercepts the rect |
| Main-window layout | `manifold-ui/src/layout.rs:12` (`ScreenLayout`) | Computes region rects (`transport_bar()`, `content_area()`, `timeline_area()`, …); hands them out, enforces nothing |
| Editor-window layout | `manifold-ui/src/dock.rs:79,136` (`Dock::rects(area) -> DockRects`) | Same pattern, second window |
| Timeline lane scissor | `manifold-renderer/src/ui_renderer.rs:390,731–740` (`lane_content_scissor`, RAII) | Bespoke local containment for lane content — proof of need, deeper than region level; **stays** |
| Headless verification | `ui-snapshot` feature, `scripts/ui-flows/` (L3 driver) | Both windows renderable headless; flows can click/drag/assert |

Bug family this touches (from `docs/BUG_BACKLOG.md`): BUG-060 (inspector over
footer — dies here), BUG-047 (Audio Setup overflow — becomes visible clipping;
the missing scroll stays its own bug), BUG-027 (editor z — fixed locally;
pinned structurally here), BUG-025 (timeline row bleed — gains an enforced
invariant). **Out of scope, explicitly:** BUG-049 (row indent arithmetic — row
geometry, not bounds; deferred, see §8) and BUG-015 (stale content — state-sync
class, FOUNDATIONAL_GAPS A1).

Extend, don't redesign: every mechanism needed already exists — the flag, the
ancestor stack, the scissor batching, the overlay registry. This design adds
**one construction rule and one sort key**, then migrates ~10 call sites.

## 2. Decisions

- **D1 — Regions are the only way to root a top-level subtree.** A new
  `UITree::begin_region(rect, tier, label)` creates a container node carrying
  `CLIPS_CHILDREN` by construction and registers it in a per-tree region list.
  Panels build under the region node instead of the tree root. Rationale: the
  clip becomes unforgettable exactly where forgetting happened. Rejected:
  *adding `CLIPS_CHILDREN` to the inspector and footer by hand* — that is the
  current opt-in model and the observed leak (BUG-060 shipped *after* the
  overhaul); the next panel forgets again.
- **D2 — Stacking is a declared tier, ordered as `(tier, insertion)`.** Four
  tiers: `Base` (timeline, viewport, inspector, chrome), `Chrome` (transport,
  header, footer — the always-visible frame), `Overlay` (popups/modals — the
  existing `Z_ORDER` registry becomes ordering *within* this tier), `Ghost`
  (drag ghosts, tooltips, toasts). Render visits regions sorted stably by
  tier; insertion order still breaks ties, so today's intra-tier behavior is
  preserved byte-for-byte. Rationale: the footer can never lose to the
  inspector again regardless of who builds first. Rejected: *reordering the
  panel build calls* — fixes one overlap by creating the next one, and says
  nothing in the code about intent.
- **D3 — Overflow is an explicit, named opt-out.** `ALLOW_OVERFLOW` on a
  region (next free `UIFlags` bit — ⚠ VERIFY-AT-IMPL: read the bitflags block
  at `node.rs:374–390` and take the next free bit) suppresses the region clip.
  Legitimate users: the `Ghost` tier (drag ghosts follow the cursor across
  regions). Anything else using it must name why in a comment with a doc/bug
  reference — the same rule as `#[allow(dead_code)]` (CLAUDE.md).
- **D4 — Enforcement is structural, not disciplinary.** A `manifold-ui` unit
  test walks any built tree and asserts every child of the tree root is a
  region node; a debug assertion in `add_node` rejects direct children of the
  root outside `begin_region`. Rationale: the invariant holds for panels
  written in 2027 by a model that never read this doc. Rejected: *lint/review
  discipline* — that is what the invariant memories were, and the bugs
  shipped anyway.
- **D5 — The mechanism lives in `manifold-ui`; both windows adopt it.** Main
  window: `ui_root.rs` wraps each `ScreenLayout` rect in `begin_region`.
  Editor window: the dock host does the same with `DockRects`. Rejected:
  *deriving scissors renderer-side from layout rects* — the renderer has no
  layout knowledge by design since the Phase-5 layering inversion
  (`archive/UI_LAYERING_INVERSION.md`); re-coupling it would undo that work.
- **D6 — The timeline's `lane_content_scissor` RAII stays.** It is
  *within-region* containment (lane rows inside the timeline region), one
  level deeper than this design governs. Folding it into regions would turn
  every lane into a region and explode the region list. Consequences, stated
  honestly: two containment idioms coexist — region clips at the window
  level, the RAII scissor inside the timeline — and a reader must know which
  applies where; §1's table is that record.

**The plausible-wrong architecture, forbidden by name:** you will want to fix
BUG-060 by adding `CLIPS_CHILDREN` to the inspector panel and calling it done
— no. That patch closes one bug and preserves the class; this design exists
because that patch has already been applied four times (viewport,
layer_header, param_slider, param_card) and the fifth panel leaked anyway.
Equally forbidden: a `sort_by(z)` over all nodes per frame (only region roots
carry tiers; nodes keep insertion order), and any change to
`ScrollContainer`'s own clip (inner clips nest under region clips via the
existing ancestor stack — no interaction).

## 3. Committed shapes

```rust
// manifold-ui/src/node.rs — bitflags block (⚠ VERIFY-AT-IMPL: next free bit)
const ALLOW_OVERFLOW = 1 << N;

// manifold-ui/src/tree.rs (or a new region.rs if tree.rs crowds — executor's call)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ZTier { Base = 0, Chrome = 1, Overlay = 2, Ghost = 3 }

pub struct RegionToken { pub root: NodeId }

impl UITree {
    /// The ONLY way to root a top-level subtree. Creates a container node at
    /// `rect` carrying CLIPS_CHILDREN (unless ALLOW_OVERFLOW is passed in
    /// `extra_flags`), registers (tier, root) in the tree's region list,
    /// and returns the token panels build under.
    pub fn begin_region(&mut self, rect: Rect, tier: ZTier, label: &'static str) -> RegionToken;
}
```

Render traversal: where the renderer walks root children today, it walks the
region list sorted stably by tier (sort happens at tree build, not per frame —
region count is ~10; zero per-frame allocation, per hot-path discipline). The
overlay driver's `Z_ORDER` array keeps ordering overlays relative to each
other inside `Overlay`.

Thread residency, serialization, time model: none touched — this is entirely
UI-thread, nothing persists, no beats/seconds involved. The binding constraint
is the hot path only in the weak sense above (no per-frame work added).

## 4. What each existing bug becomes

- **BUG-060** — dies in P1: the inspector region clips at its rect; the footer
  lives in `Chrome`, above `Base`, so even an escaped pixel loses.
  **Evidence upgrade (2026-07-08, Fable, instrumented live repro):** this
  design's premise is now proven, not assumed. Traced runs on
  `fix/bug-060-footer-leak-trace` showed the footer draws all 8 nodes with
  correct bounds and no clipping on every rebuild — yet its right half comes
  out near-black in the atlas. So the damage is done *after* the footer's
  pass, by a write the node-level scissor trace cannot see (tree-node draws
  were all clamped at the footer line; immediate-mode draws are untraced and
  were never ruled out). Exactly the class P1's binding region scissor kills
  by construction. Full history: `docs/FOOTER_OVERPAINT_INVESTIGATION.md` on
  that branch; the worktree binary doubles as the verification rig — rerun the
  drawer-open + scroll repro after P1 and the footer must be unbreakable.
- **BUG-047** — the spill becomes clean clipping at the panel edge (correct
  rendering of a layout problem). The panel still needs a scroll container for
  ≥18 rows; the bug stays open, re-scoped to "add scroll", severity unchanged.
- **BUG-027-class** — pinned by D4's structural test in the editor window; the
  local fix stops being load-bearing.
- **BUG-025** — lane bleed gains a second fence (region clip around the whole
  timeline) on top of the RAII scissor; the repro hunt stays open.

## 5. Phasing

### P1 — Region mechanism + main window (vertical slice)

- **Entry state:** clean main; `cargo test -p manifold-ui --lib` green;
  re-verify anchors `node.rs:381`, `tree.rs:247`, `ui_root.rs:632`.
- **Read-back:** this doc §1–§3; restate D1–D6, the forbidden per-panel patch,
  and entry-check results, before any code.
- **Deliverables:** `ZTier`, `RegionToken`, `begin_region`, `ALLOW_OVERFLOW`;
  region-list sorted traversal; `ui_root.rs` builds every main-window panel
  under a region (`ScreenLayout` rects; tier assignments: transport/header/
  footer → `Chrome`, timeline/viewport/inspector/top-region panels → `Base`,
  existing overlay driver → `Overlay`, drag ghost/toast paths → `Ghost` +
  `ALLOW_OVERFLOW`); D4's debug assertion + structural unit test.
- **Seam brief:** `panel.build(&mut tree, &layout)` sites in `ui_root.rs`
  (~10; re-derive: `rg -n '\.build\(&mut self\.tree' crates/manifold-app/src/ui_root.rs`
  — if the count differs from what you find, stop and list before touching).
  All mechanical: wrap each call's subtree in `begin_region`; one worked
  example in the first commit. Compiler-driven where the build signature
  changes; no old path survives (negative gate below).
- **Gate:** `cargo test -p manifold-ui --lib` + `-p manifold-app` green;
  headless PNG set (`cargo xtask ui-snap` fixture scenes) — **pixel-identical
  everywhere except** the two scenes that reproduce BUG-060 (inspector
  trigger-drawer open, scrolled to bottom) and BUG-047 (Audio Setup ≥18 rows),
  whose diffs must show containment. Negative: `rg 'add_node\(UITree::ROOT'`
  (or the equivalent root-parent constructor — derive exact pattern from
  `add_node`'s signature) returns zero hits outside `begin_region`.
- **Acceptance demo (L3):** a `scripts/ui-flows/` flow that opens the
  trigger-gate drawer, scrolls the inspector to bottom, and asserts the
  footer's widget is hit-testable and unoccluded. Target L3 (flow driver
  reaches it); PNG L2 fallback only if the drawer proves undrivable — say so
  in the report.
- **Performer gesture:** scroll a long inspector hard during playback; the
  transport/footer stays readable and clickable throughout.
- **Forbidden moves:** the per-panel `CLIPS_CHILDREN` patch; keeping any
  panel rooted at tree root "temporarily"; fixing PNG diffs by reordering
  build calls; blanket `ALLOW_OVERFLOW` to silence a diff.
- **Test scope:** focused (`-p manifold-ui -p manifold-app --lib`); no GPU
  suite (no shader/kernel touched); workspace sweep deferred to P3.

### P2 — Editor window + perform surface

- **Entry state:** P1 landed; re-verify `dock.rs:136` and the editor host's
  build path (re-derive: `rg -n 'dock' crates/manifold-app/src/app.rs`).
- **Deliverables:** editor window panels (sidebar, inspector column, canvas
  chrome) under regions from `DockRects`; canvas itself `Base`, its node
  previews inside the canvas region (BUG-027's fix now structural); perform
  HUD surfaces under regions (HUD `Chrome`, output preview `Base`); D4 test
  extended to run against an editor-window and perform-mode tree.
- **Gate:** focused tests green; editor-window + perform ui-snap PNGs
  pixel-identical to pre-phase captures; the D4 structural test passes on all
  three tree builds.
- **Acceptance demo (L2):** editor-window PNG with a node preview overlapping
  a sidebar edge — preview clipped at the canvas region, chrome intact.
  (L3 if an editor-window flow exists by then; check `scripts/ui-flows/`.)
- **Forbidden moves:** special-casing the canvas ("it's big, skip the
  region"); `ALLOW_OVERFLOW` on the canvas to dodge a clip bug — a canvas
  clip failure is an escalation, not an opt-out.
- **Test scope:** focused, as P1.

### P3 — Enforcement closure + sweep

- **Deliverables:** the debug assertion promoted to the permanent contract
  (documented on `begin_region`); `docs/UI_ARCHITECTURE_OVERHAUL.md` §status
  and `FOUNDATIONAL_GAPS.md` A2 updated to point here; BUG-060 moved to
  Fixed / BUG-047 re-scoped in `BUG_BACKLOG.md`; subregion-scissor /
  compositor-ordering invariant memories updated to name the region contract.
- **Gate:** full workspace sweep (`cargo test --workspace` +
  `cargo clippy --workspace -- -D warnings`); full ui-snap fixture set
  re-captured as the new baseline; negative: `rg 'ALLOW_OVERFLOW'` hits only
  the `Ghost`-tier sites and each carries its justification comment.
- **Acceptance demo:** `Demo: the P1 flow re-run + full PNG set — L3`.
- **Test scope:** the one workspace sweep for the whole design, here.

Phasing-completeness check (§5 of the standard): every affordance the body
commits to — binding region clips (P1), declared tiers incl. overlay fold-in
(P1), editor window + perform (P2), enforcement + doc/status truth (P3),
`ALLOW_OVERFLOW` ghosts (P1) — appears in exactly one phase. Row-geometry
primitives and BUG-047's scroll are in Deferred, not silently absent.

## 6. Decided — do not reopen

1. Region root = the only top-level subtree root; clip by construction (D1).
2. Four tiers, stable insertion order within a tier (D2). No per-node z.
3. Overflow is a named opt-out with a justification comment (D3).
4. Enforcement is a structural test + debug assertion, not review discipline (D4).
5. Mechanism in `manifold-ui`; renderer stays layout-blind (D5).
6. `lane_content_scissor` RAII survives as within-region containment (D6).

## 7. Deferred

- **Row/indent geometry primitives** (BUG-049 class): different mechanism
  (arithmetic, not bounds). Revive when a third indent bug lands or when the
  chrome API grows row layout — whichever first.
- **BUG-047's scroll container** for Audio Setup: ordinary panel work, not
  substrate; revive on the bug's own priority.
- **Per-region dirty/damage tracking** (render only regions whose content
  changed): a real optimization the region list enables, but a separate
  performance design with measurement first (`graph-perf-campaign` pattern).
- **Region-aware hit-testing** (input rejected outside the owning region's
  clip): belongs to DRAG_CAPTURE's world; revive when its P1–P3 land, as the
  two designs' shared invariant ("pixels and pointers agree on bounds").

## 8. Honest costs

Wrapping every panel in a region adds one node per panel (~10 nodes — noise
against typical tree size). The tier enum is a new concept a panel author must
choose correctly; the wrong tier is at least *visible* (wrong stacking is
diagnosable in one PNG) where the old failure was invisible until it shipped.
BUG-047-style layout mistakes stop spilling and start truncating — arguably
uglier per-incident, but honest: clipped content says "this panel's layout is
wrong" instead of silently corrupting a neighbor. And D6 leaves two
containment idioms in the codebase, priced above.
