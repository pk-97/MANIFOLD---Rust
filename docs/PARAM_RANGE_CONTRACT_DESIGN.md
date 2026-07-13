# Param Range Contract — real boundaries are contracts, everything else is a hint

**Status:** PROPOSED · 2026-07-13 · Fable (design session with Peter; direction ruled by Peter same day)
**Prerequisites:** GRAPH_TOOLING_DESIGN (SHIPPED 2026-07-13) — the card lints, `validate_def`, and the `boundary_reason` declared-excuse pattern this design reuses.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 before starting any phase.

Peter's rulings, verbatim (2026-07-13): *"Sliders that 'over drive' isn't a thing, a slider shouldn't be able to go past a value that makes no sense."* · *"We should make this a design and real fundamental invariant of the nodes and graph systems. Bloom sounds correct, it lets you blow out the image if you want. Inner nodes that don't have a real physical range or boundary shouldn't have a boundary — that's what the card mappings and ranges are for."* · *"As long as the node ranges and sliders themselves are not restrictive to users."*

**The governing insight:** today a node param's `min`/`max` conflates two unrelated facts — "beyond this the math is undefined" (a contract) and "this is a sensible slider span" (a display hint) — and nothing in the codebase can tell them apart, so GRAPH_TOOLING's range lint (h) cannot be an error: it flags disagreements between two declarations without knowing which one lies. This design splits them. **A contract is declared only where a real physical/mathematical boundary exists, must name its reason (the `boundary_reason` pattern applied to ranges), and is what validation enforces. The existing min/max become explicitly display hints: the default slider travel, never a restriction** — cards own the creative envelope (curve, invert, remap, range), and a user can always type, modulate, or card-map past a hint. Bloom's [0,5] drive into `mix.amount` becomes legitimate by policy: a lerp beyond 1 extrapolates, "it lets you blow out the image if you want."

Instrument story: the performer's slider does exactly what it claims across its whole travel — no dead stretches, no silent clamps — and the boundaries that DO exist (an index that must address a real input, a radius that divides by zero) are enforced at authoring time with a named reason instead of discovered as a black frame mid-set.

Companion docs: `GRAPH_TOOLING_DESIGN.md` (lint h, `validate_def`, the declared-excuse precedent), `docs/CARD_AUTHORING.md` (cards as the creative-range owner — gains a §on this split at P2).

---

## 1. Audit — what exists (verified 2026-07-13)

| Piece | Where | State |
|---|---|---|
| Node param ranges | `manifold-core/src/effects.rs:36` `ParamDef` — `min: f32, max: f32`, bare, mandatory, `Serialize/Deserialize` (reaches project files) | The conflated fact. No way to express "no boundary" or one-sided bounds. |
| Registry param specs | `primitive!` macro param rows → `PrimitiveSpec` params; catalog rows already render `range: Option<(f32,f32)>` (`catalog_gen.rs:90` ParamRow) | Catalog can already say "rangeless"; the source model can't. |
| Write boundary | `node_graph/param_binding.rs:264` | **Deliberately unclamped** ("must not clamp" — folded deg→rad remaps would break). Nothing enforces node ranges at runtime. A "contract" today is prose. |
| UI knob rendering | `manifold-app/src/ui_translate.rs:349,396` | `value_norm` clamp is **display-only** — the drawn knob pins at the rail; the stored value is untouched. Slider *drags* write within the span by construction (inherent to sliders). ⚠ VERIFY-AT-IMPL: whether the text-entry path clamps to min/max — `rg "clamp" <the param text-input commit path>`; if it clamps, that's a P1 behavior change (hints must not clamp). |
| Range lint | `node_graph/validate.rs:566–598` lint (h), a WARNING | Compares card-mapped bounds against the conflated range; cannot be an error until ranges mean one thing. GRAPH_TOOLING §7 defers promotion to this design. |
| The declared-excuse precedent | `freeze/classify.rs` `BoundaryReason` + `every_boundary_atom_declares_its_reason` + ledger | Shipped this morning. The exact enforcement shape to reuse. |
| Seed audit | the 10-preset diagnostic table (§2 below), worker-produced 2026-07-13 | Every currently-known card-vs-range conflict, each with a physical-vs-arbitrary read. |

## 2. Seed audit — the ten known conflicts, re-read under the ruling

| Inner param | Declared today | Verdict under the ruling |
|---|---|---|
| `node.mix` `amount` [0,1] (Bloom) | fake | **Rangeless.** Lerp extrapolation is legitimate vocabulary (Peter: Bloom is correct). |
| `node.math` `a`/`b` [-1000,1000] (FluidSim2D) | fake | **Rangeless.** A generic arithmetic node with a range is absurd — this fake bound made FluidSim2D's count-in-millions card *unfixable* under the old reading (mapped minimum already 100× past the "limit"). Dissolves entirely. |
| `edge_gain.gain` [0,4] · `text_force_gain.gain` [0,4] · `grade_contrast.contrast` [0,2] · `growth_mask.phase` [0,1] | fake | **Rangeless.** Creative amounts; hints at most. Phase additionally wraps. |
| `content_window.width` [0.1,0.9] (VoronoiPrism/Glitch) | suspect | ⚠ VERIFY-AT-IMPL: read the window shader — if a full-width window is well-defined, rangeless; if geometry degenerates, keep as contract `DegenerateGeometry`. |
| `connect_nearest.max_distance` min 0.01 | likely real | **One-sided contract** `DegenerateFloor` if the kernel divides/degenerates at 0 — read the kernel to confirm; else rangeless. |
| `render.window` min 0.001 (BlossomWire) | likely real | Same check, same shape. |
| `split.amount` ±32 (MetallicGlass) | suspect | ⚠ VERIFY-AT-IMPL: if the shader clamps sample offsets at ±32 texels, contract `ShaderClamp`; if it just samples further, rangeless. |
| ApricotWeather (lint g, not h) | — | Defaults disagreement (card 9.0 vs binding 26.205) — fixed at P2 with the card's value as truth (GRAPH_TOOLING D8 rule). |

The pattern the table proves: **most declared ranges are fake.** Expect the contract sweep to KEEP ranges on a small minority of params.

## 3. Decisions

- **D1 — Additive contract, not a range migration.** `ParamDef` keeps `min`/`max` exactly as-is, redefined as **display hints** (default slider travel). A new optional field carries the contract: `#[serde(default, skip_serializing_if = "Option::is_none")] pub contract: Option<RangeContract>` — additive, so every saved project and bundled preset loads byte-identically with zero migration. **Rejected:** making `min`/`max` themselves `Option` — breaks every slider consumer and forces a project-file migration for zero expressiveness gain (a slider always needs SOME travel to draw).
- **D2 — `RangeContract` shape (manifold-core, next to ParamDef):**
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
  #[serde(rename_all = "camelCase")]
  pub struct RangeContract {
      pub min: Option<f32>,            // one-sided bounds are first-class
      pub max: Option<f32>,
      pub reason: RangeReason,         // no contract without a named excuse
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  pub enum RangeReason {
      Index,              // addresses a discrete resource (mux select, array slot)
      Count,              // sizes an allocation (num_inputs, particle caps)
      DegenerateFloor,    // kernel divides/degenerates at or below the bound
      DegenerateGeometry, // geometry collapses outside the bound
      ShaderClamp,        // the shader physically clamps; beyond = dead input
      NormalizedDomain,   // the math is ONLY defined on the interval (true domains, not lerps — lerps extrapolate legitimately)
  }
  ```
- **D3 — Hints never restrict (Peter's constraint, verbatim above).** Slider drag writes within the hint span (inherent). Everything else — text entry, card remaps, modulation, OSC — may exceed hints freely; only contracts clamp-or-reject, and rejection happens at VALIDATION time (authoring), not silently at the write boundary. The write boundary stays unclamped (param_binding.rs:264 stands). ⚠ If the P1 text-entry check finds a clamp-to-hint, removing it is a P1 deliverable.
- **D4 — Lint (h) validates against contracts only, and becomes an ERROR in the same phase.** A card range escaping a *contract* is always a bug (the contract names why). Card-vs-hint disagreement is not a finding at all — that's the design working. **Rejected:** keeping a card-vs-hint warning — it re-creates today's noise and trains nothing.
- **D5 — Enforcement is the boundary_reason pattern verbatim:** a meta-test `every_range_contract_names_a_real_boundary` walks the registry and asserts (a) every `RangeContract` carries a reason (type-guaranteed) and (b) — the real teeth — a curated assertion table in the test pins each contracted param to its reason, so adding a contract means editing the test (review-visible, exactly like the ConversionDebt ledger). Fake ranges can't creep back in silently.
- **D6 — The sweep direction is remove-by-default.** P2 seeds contracts ONLY where the kernel/shader read proves the boundary (Index/Count are mechanical: every mux select and num_inputs; the floors and clamps in §2 get their reads). Everything else keeps its min/max as a hint and gains no contract. No per-param agonizing: no proof, no contract.
- **D7 — `node.math` and FluidSim2D dissolve, not fix.** Generic arithmetic params get no contract; the [-1000,1000] hint may stay as a display span or be widened freely — either way the card stops being "in violation." ApricotWeather's lint-g fix rides in P2.

**Consequences, stated honestly:** contracts are enforced at authoring/validation time only — a runtime write (modulation sweeping an Index param out of range) still lands unclamped, and the node's own defensive behavior (existing clamps in kernels) is what catches it; if that proves insufficient for Index/Count params, a targeted write-boundary clamp FOR CONTRACted params ONLY is the follow-up (deferred, trigger: an observed runtime failure). The curated test table is a second ledger to maintain — same cost as the fusion one, same payoff. The catalog will show `contract` alongside the hint; agents reading the old `range` field see no change until P2 regenerates.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| No range contract without a named physical reason | Type shape (`RangeContract.reason` mandatory) + the curated meta-test table (D5) |
| Hints never clamp a stored value | P1 test: write an out-of-hint value through the real param write path, read it back intact; plus the VERIFY-AT-IMPL text-entry check |
| Card ranges never escape a contract | lint (h) as ERROR in `validate_def` (P1), all bundled presets green (P2) |
| Saved projects load unchanged | additive serde (`Option` + skip_serializing_if) + the Liveschool fixture round-trip in the default sweep |

## 5. Phasing

**P1 — the contract model + validation switch.** *Entry:* GRAPH_TOOLING on main (`7781ba94`); anchors §1 re-verified. *Read-back:* this doc whole; ParamDef (effects.rs:36); lint (h) in validate.rs; `BoundaryReason` + its meta-test as the pattern. *Deliverables:* `RangeContract`/`RangeReason` in manifold-core; `contract` field on ParamDef + the macro's param row syntax (optional, additive); lint (h) rewritten to compare against `contract` only and moved to errors (its unit tests updated: hint-disagreement fixtures now expect NO finding); meta-test `every_range_contract_names_a_real_boundary` with an EMPTY curated table (no contracts exist yet — the test proves the mechanism); the hint-never-clamps test (write out-of-hint value through the real write path, read back intact); resolve the ⚠ text-entry check, removing any clamp found. *Gate:* `-p manifold-core -p manifold-renderer --lib` green; bundled presets: zero errors (no contracts exist, so lint h finds nothing — expected); clippy scoped; Liveschool fixture loads (round-trip gate). *Forbidden moves:* migrating min/max to Option (D1 rejected it); adding any contract in this phase; write-boundary clamping. *Demo:* none — L1. *Test scope:* focused libs.

**P2 — seed the real contracts + fix the residue.** *Entry:* P1 landed. *Read-back:* this doc §2 table + D6; the kernels named in §2. *Deliverables:* mechanical contracts on every mux select / num_inputs / index-count param (`rg`-derived list, re-derive at execution); the §2 VERIFY reads performed (connect_nearest, render.window, content_window, split.amount) with contracts added ONLY where the kernel proves it, each with the file:line evidence in the curated test table; catalog gains the contract column (regen); rerun lint h over all presets — fix any card that now genuinely escapes a contract (expected: at most the two floors — card min 0→0.01 and 0→0.001, sub-pixel travel nobody will miss); ApricotWeather lint-g fix (binding default := card default 9.0); CARD_AUTHORING.md gains the hint-vs-contract paragraph. *Gate:* all bundled presets zero errors AND zero lint-h warnings (the class is now empty by construction); meta-test green with the populated table; full `-p manifold-renderer --lib`; clippy. *Forbidden moves:* contracts without a kernel read cited; widening the sweep into a hint-tidying campaign (hints are not this design's business); touching binding scale/offset. *Demo:* `graph-tool validate` on Bloom and FluidSim2D pasted — both clean, no warnings — L2.

Landing: batch P1–P2, full sweep in warm main checkout, status flip, supersession sweep (GRAPH_TOOLING §7's lint-h deferral row updated to point here).

## 6. Decided — do not reopen

1. Node min/max = display hints, never restrictions (Peter, verbatim). Contracts are a separate, optional, reasoned declaration.
2. Additive serde; no project-file migration; min/max stay mandatory f32.
3. Lerp/blend factors get NO NormalizedDomain contract — extrapolation is legitimate vocabulary (Bloom ruling).
4. Lint (h) is an error against contracts, silent against hints.
5. Remove-by-default sweep: no kernel proof, no contract.
6. Write boundary stays unclamped; contract enforcement is authoring-time.

## 7. Deferred

- **Runtime clamp for contracted params** — only if an observed runtime failure (modulated Index param crashing a kernel) proves authoring-time enforcement insufficient.
- **Slider span zoom / user-adjustable hint travel in the editor UI** — UX work, own scope; this design only guarantees hints don't restrict.
- **`suggest` verb** (carried from GRAPH_TOOLING discussion) — Peter 2026-07-13: preset-co-occurrence ranking "will funnel agents to make the same things over and over again"; revive only if cold-authoring stress-tests show agents struggling at "what comes next."
