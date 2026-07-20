# Foundational Gaps — where a general system is missing

Status: inventory, 2026-07-07. Audience: design sessions choosing the next
fundamental work. Each entry names a place where point-patches stand in for a
system that was never built, with the evidence trail, a one-paragraph sketch of
what the real system is, and a kill-test (the honest reason it might not be
worth building). Read `docs/DESIGN_AUTHORING.md` before turning any entry into
a design doc.

Method: two lenses. **Part A** mines the patch trail — fix-commit churn,
`docs/BUG_BACKLOG.md` clusters, and the invariant-memory corpus; these are
bugs already paid for. **Part B** mines map coverage — subsystems no
authoritative current-state read has ever visited; these are the bugs not yet
met (CORE_ENGINE_MAP §13 → 43-item findings queue is the proof of yield).
Entries are ranked by stage risk — what it costs mid-set — not by churn count.

---

## Part A — patch-trail clusters

### A1. UI↔content state sync: projection layer with no enforcement (stage risk: HIGH) — **KILL-TEST RAN 2026-07-09: layer KILLED, enforcement SHIPPED**

**Outcome (Fable, 2026-07-09).** The kill-test below fired, one level deeper than the field
survey: the mirror fields' emit half was already compiler-enforced (exhaustive literal) and
their consume half is bespoke display logic no table can generate — the declarative layer
would have been escape hatches all the way down. The orphan-field rot was a *suppressed
compiler lint* (`#[allow(dead_code)]` on `ContentState`; manifold-app is a bin crate):
un-suppressed and purged as UI_PROJECTION_LAYER_DESIGN P0 (10 dead fields — the purge exposed
BUG-083/084, export-progress and recording-drop displays that never existed). Verdict record,
rejected shapes, and the reviving trigger: `UI_PROJECTION_LAYER_DESIGN.md`. Original entry
kept below for the evidence trail.

**Evidence.** `ui_bridge/state_sync.rs` is the highest-churn non-app file in
the repo (~79 commits matching fix-mining since March; log read 2026-07-07).
The churn is mostly *feature* work — every UI feature must hand-thread its
fields through the snapshot projection — plus a recurring stale-state bug
class: BUG-015 (stale inspector offsets), BUG-026 (frozen entrance fade),
BUG-036 (dead LFO on reload), BUG-060 (footer overpaint), `0327f20f` (stale
chain on layer switch). Memory family: `ui-state-sync-path`,
`per-window-resource-writes`, `effect-chain-state-caches`.

**The missing system.** Not knowledge — `docs/archive/UI_ARCHITECTURE_AUDIT.md`
(2026-06-18) already maps the mechanism and judges it well-built. What's
missing is *enforcement*: today every new snapshot field is a hand-written
pair (content-side emit, UI-side apply) plus drag-suppression rules re-derived
per feature. A real system makes the projection declarative — one place
declares a field's source, its drag behavior, and its dirty condition; the
emit/apply pair is generated or table-driven, and a field that skips the
declaration doesn't compile. The stale-state class dies at the layer where it
breeds, per `eliminate-bug-class-at-storage-layer`.

**Kill-test.** The audit called the current design sound, and a declarative
layer over ~dozens of heterogeneous fields can cost more indirection than it
saves. Kill it if a survey of the actual snapshot fields shows they're too
irregular to tabulate — the tell would be a "declarative" layer that's mostly
escape hatches.

### A2. Bitmap-UI bounds enforcement: clipping is opt-in, z is build order (stage risk: MED-HIGH) — **DESIGNED 2026-07-07**

**Corrected 2026-07-07 (same day, after running the kill-test).** The first
draft said "every panel hand-computes geometry" — under-credited:
`UI_ARCHITECTURE_OVERHAUL.md` shipped completely (phases 0–8, 2026-06-23) with
`ScreenLayout`, the declarative chrome API, and one input owner. What the
overhaul did NOT cover is precisely two properties: **pixel clipping is opt-in
per panel** (`CLIPS_CHILDREN`, node.rs:381 — the inspector never set it,
BUG-060) and **stacking is insertion order** (`draw_order = self.count`,
tree.rs:247 — overlays have a z registry, base panels don't).

**Evidence.** The post-overhaul bug family: BUG-060 (inspector over footer),
BUG-047 (panel overflow), BUG-027 (editor-window preview z), BUG-025 (row
bleed). The kill-test split the original six: BUG-049 is row *arithmetic* and
BUG-015 is *state-sync* (A1) — different mechanisms, removed from this entry.

**The system — designed, not built:** `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md`
(PROPOSED 2026-07-07): region roots are the only way to root a top-level
subtree, clip by construction, four declared z tiers, structural enforcement
test, both windows + perform surface. P1–P3, Sonnet-ready.

### A3. Input ownership beyond the pointer (stage risk: MED — pointer half already designed)

**Evidence.** BUG-058/059 (stuck drags, grabs leaking under modals) produced
`DRAG_CAPTURE_DESIGN.md` (approved 2026-07-07) — the proof this class exists.
Uncovered remainder: focus and keyboard routing (BUG-022: Escape leaves popup
open), modal ownership, and whatever an end-to-end read of the event path
finds (see B3). `archive/INPUT_IDENTITY_UNIFICATION.md` (shipped) covers widget
identity, not routing.

**The missing system.** The drag-capture design's single-owner principle,
extended to the other input modalities: one owner for focus, one for modal
scope, terminal events broadcast. Design after B3's map — don't guess the
inventory.

**Kill-test.** May be premature until drag capture ships and proves the
pattern. Kill (defer) if B3's map shows keyboard/modal routing has only one or
two consumers today.

### A4. Param binding/modulation: parallel mechanisms, one concept (stage risk: MED)

**Evidence.** Distinct "external thing drives a param" families that each ship
their own config, carry-rules, and UI: drivers/LFOs, envelopes, automation
lanes, `ableton_mappings`, `audio_mods` (now also trigger mode, per
LIVE_AUDIO_TRIGGERS §9), macros, control wires, param step-actions (proposed).
`PresetInstance::duplicated()` (`2e3dc4f3`) had to hand-write a carry-rule per
family — that's the tell. Bugs from the seams: BUG-004 (paste carries some
bindings, drops others), BUG-005 (macro addressing), BUG-036 (LFO dead on
reload), BUG-039 (angle wrap vs. modulation).

**The missing system.** Partially designed already —
`BINDING_UNIFICATION` and `CARD_TARGET_UNIFICATION` exist and shipped pieces
(B+/B++/C remain). The gap is finishing: one binding substrate with per-family
behavior, so carry-on-duplicate, serialization, reload-rebind, and UI listing
are written once. LIVE_AUDIO_TRIGGERS §9 (trigger = audio mod in fire mode,
Peter's call) is the precedent: unify onto an existing family rather than add
a parallel type — never re-propose a parallel config type.

**Kill-test.** The families genuinely differ (per-frame vs. event, project vs.
hardware scope); a forced substrate could be `param-storage`-grade surgery for
modest yield. Kill if the remaining B+/B++/C items already deliver the
carry/reload unification — check their scope first.

### A5. Identity on clone/duplicate: single home, no fence (stage risk: LOW-MED)

**Evidence.** BUG-001..004 fixed as a class by `duplicated()` (`2e3dc4f3`,
"one home for the fresh-copy carry-rule"); BUG-005 by EffectId addressing
(`9f43f183`). Remaining: the home is convention — a new clone path can bypass
it silently; BUG-031 (context-menu/rename still positional) shows positional
addressing survives; `pool-keyed-by-identity-not-position` memory guards the
GPU side of the same idea.

**The missing system.** An enforcement fence: id-bearing types get their
`Clone` gated or wrapped (e.g. clone-for-duplicate is a distinct trait that
mints ids; raw `Clone` for snapshots only), plus finishing the positional →
id-addressing migration (BUG-031, `graph-command-node-addressing` is the
graph-side precedent).

**Kill-test.** The class fix may already be good enough — one bug (BUG-031,
LOW) since June. Kill if a grep shows all clone paths route through
`duplicated()`/`clone_with_new_id` today; then this is a lint, not a system.

### A6. Device/resource lifecycle for embedded consumers (stage risk: LOW today, blocks headless growth)

**Evidence.** BUG-054: renderers cache raw `*const GpuDevice` that only
`ContentThread::run()` repoints — every new headless/embedded consumer hits
it. The headless proof harnesses (ui-snapshot, journey-proofs, gpu-proofs) are
exactly that growing consumer set.

**The missing system.** Ownership design for device + long-lived GPU resources
that doesn't assume the one-true-content-thread: likely `Arc<GpuDevice>` at
the seams (requires the shared-state conversation with Peter — CLAUDE.md hard
rule) or an explicit repoint contract all consumers must call.

**Kill-test.** If the embedded-consumer count stays at "test harnesses," a
documented repoint contract is enough; the system version only pays when a
second real runtime (plugin host, remote render) arrives.

### A7. Feature-matrix build rot (stage risk: LOW, cheap to kill)

**Evidence.** BUG-029 (`profiling` rotted), BUG-033 (`ui-snapshot` broken),
BUG-056/057 (clippy debt behind features). Non-default features rot because no
gate builds them.

**The missing system.** Not a system — a CI/gate matrix: the pre-push or
periodic gate builds `--features gpu-proofs,ui-snapshot,journey-proofs,profiling`
(build/clippy only, not the GPU-run suites). Half a day, kills the class.

---

## Part B — map-coverage gaps (latent bugs, not yet met)

Found bugs track what gets exercised; latent ones concentrate where usage is
low and stage-cost is high. The move that works: write the authoritative
current-state map and harvest its honest-edges section
(CORE_ENGINE_MAP → CORE_ENGINE_FINDINGS precedent).

| Subsystem | Map status | Stage risk of the dark | Next move |
|---|---|---|---|
| Project IO / migration chain (`manifold-io`) | **`docs/PROJECT_IO_MAP.md` — written 2026-07-07 (this pass)** | A migration bug eats a show file silently; BUG-040 proved the chain can drop data | Work the map's honest-edges list |
| Media/export pipeline as-built (`manifold-media`) | NO current-state map — `MEDIA_BACKEND_DESIGN.md` and the export designs are forward-looking contracts | Export failures surface days before a release deadline; recording seams covered by LIVE_RECORDING_PROOFS (proposed), decode/thumbnail/export paths are dark | Map next (Opus prompt pack candidate) |
| Input event path end-to-end (`manifold-app` input_host → panels) | Slices only (INPUT_IDENTITY shipped, DRAG_CAPTURE approved) | Stuck/leaked input mid-set = BUG-058/059 class | Map feeds A3's design |
| UI bridge / state sync | Mapped — `archive/UI_ARCHITECTURE_AUDIT.md` (2026-06-18) | n/a | Gap is enforcement (A1), not knowledge |
| Core engine, freeze compiler, GPU backend, audio stack | Mapped (CORE_ENGINE_MAP, FREEZE_COMPILER_MAP, MANIFOLD_GPU_ARCHITECTURE, AUDIO_INFRASTRUCTURE) | n/a | Work existing findings queues |

Third lens, standing: **adversarial soak where usage is thin** — project
migration across every version pair with the canonical Liveschool fixture,
device hot-plug and display reconfiguration mid-transport, two-hour soak at
real scale (53 layers / 2928 clips, `typical-project-scale`). The
LIVE_RECORDING_PROOFS design is the template for turning each of these into a
harness.

---

## Suggested order (stage risk × readiness)

1. **A7** — half a day, kills a class, no design needed.
2. **B1 follow-through** — work PROJECT_IO_MAP honest edges; show files are
   the one unrecoverable asset.
3. ~~**A1** — design the declarative projection (survey fields first; kill-test
   is cheap).~~ Done 2026-07-09: kill-test killed the layer, enforcement shipped (see A1 entry).
4. **B (media map)** — before the ~Aug release push leans hard on export.
5. **A2 → A3** — after drag capture ships; B3 map first, then extend the
   single-owner pattern.
6. **A4** — scope the remaining unification items before deciding.
7. **A5, A6** — lint-grade unless their kill-tests fail.
