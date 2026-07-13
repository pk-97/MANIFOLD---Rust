# Graph Tooling — validate/fusion CLI for agents + boundary-exemption enforcement

**Status:** SHIPPED — P1–P4 built, gated, and landed 2026-07-13 (same-day design→execution, Fable orchestrating Sonnet workers). Deferred items in §7 remain open.
**Prerequisites:** none. NODE_VOCABULARY_AUDIT apply pass is SHIPPED (2026-07-03), so the catalog speaks final names. This design deliberately pulls the *validate* slice of MCP_INTERFACE_DESIGN P2 forward as a CLI; the MCP server itself stays where DESIGN_BUILD_ORDER puts it (wave 3, after COMPONENT_LIBRARY).
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 before starting any phase.

Peter's directives, verbatim (2026-07-13): *"I want graph composition to be easy, effortless, and accurate."* · *"Surely our graphs and atoms should be opt-out and work with fusion by default?"* · *"This will be a fundamental part of the authoring process so it needs to be strong, fast, safe, efficient, and sensible."*

**The governing insight:** everything an agent needs to compose graphs mechanically already exists in the codebase — the catalog generator, the preset validator, the pure fusion classifier — but each is reachable only from one call site (a doc generator, a bundled-preset checker, the freeze pipeline). This design adds **no new validation or classification logic**. It extracts one seam (a shared validate function), exposes two CLI verbs over existing machinery, and adds one enforcement test that converts Peter's fusable-by-default rule from prose into a build failure. The instrument story: this is authoring infrastructure for the ~Aug content push — an agent (or later, Claude Desktop via MCP) authoring presets gets machine-checked feedback in milliseconds, and an imported .glb either loads clean or names the node and port that's wrong at drag-in time, never mid-rehearsal.

Companion docs: `MCP_INTERFACE_DESIGN.md` (the server this front-runs; its §6 validate contract is implemented here), `ADDING_PRIMITIVES.md` §"The codegen path is mandatory" (the exemption taxonomy §5 enforces), `FREEZE_COMPILER_MAP.md` (authoritative fusion pipeline), the `codegen-conversion-sweep` memory (the 2026-07-11 triage that seeds the declarations).

---

## 1. Audit — what exists (verified 2026-07-13)

| Piece | Where | State |
|---|---|---|
| Machine-readable catalog | `crates/manifold-renderer/src/bin/gen_node_catalog.rs` → `docs/node_catalog.json` + generated NODE_CATALOG.md block; logic in `node_graph/catalog_gen.rs` | **Exists.** Per node: type_id, label, purpose, VJ summary, category, role, aliases, examples, ports (name/type/required), params (type/default/range/enum values), stratum. Drift-guarded by `catalog_gen::tests::regenerates_in_sync`. **No fusion info** — that's the one-wire gap. |
| Graph validation | `crates/manifold-renderer/src/bin/check_presets.rs` `check_one()` | **Exists, bundled-dirs-only.** Full load+compile pipeline: `into_graph(registry)` + `compile(&graph)` + binding-resolution check + (generators) `PresetRuntime::from_def_with_device` for allocation errors. Catches UnknownTypeId, UnknownParam, ParamTypeMismatch, InvalidWire, RequiredInputUnwired, cycles, UnsizedArrayOutput, UnboundArrayResource. Cannot validate an arbitrary file path. Logic lives in the bin, not the library — that's the seam to extract. |
| Structured errors | `node_graph/graph_loader.rs:98` `GraphBuildError` | **Exists.** Variants carry `node_id`, `type_id`, `param`, `port` — the MCP §6 "errors name node + port" requirement is already satisfied at the type level. |
| Fusion classification | `node_graph/freeze/classify.rs` (`FusionKind`, default `Boundary`), `freeze/region.rs:205` `partition_regions(&EffectGraphDef, &PrimitiveRegistry) -> Vec<Region>` | **Exists, pure, GPU-free.** Region partition is callable outside the freeze pipeline. |
| Fusion declarations | `primitive!` macro `fusion_kind:`/`wgsl_body:` fields (`node_graph/primitive.rs:797`) | 213 macro invocations: 122 declare a fusable kind, 5 declare explicit `Boundary`, **~86 are silent** (default to Boundary with no stated reason). Plus hand-`impl Primitive` nodes (render_*, wgsl_compute) outside the macro. |
| Exemption taxonomy | `ADDING_PRIMITIVES.md` §"The codegen path is mandatory" (landed `ca145923`, 2026-07-11) | **Exists in prose only.** 4 exempt categories + BLOCKED≠exempt. No machine check. |
| Boundary triage | `codegen-conversion-sweep` memory (2026-07-11) | **Exists.** 223 files audited: 120 on codegen, ~54 legitimately exempt (non-GPU, reductions, state, IO/bridges, render_*, DNN, 4 fused bundles), remainder = 3 conversion waves. The declarations in P2 transcribe this triage; they do not re-litigate it. |
| Importer output | `node_graph/gltf_import.rs:267` `assemble_import_graph(path) -> Result<(EffectGraphDef, ImportReport), String>` | **Exists.** Emits an ordinary `EffectGraphDef` — the same type the validator takes. Called from `manifold-app/src/app_lifecycle.rs:528`. No validate pass on its output today. |
| Kind enum | `node_graph/bundled_presets.rs` `PresetKind` | **Exists** — reuse; do not invent a new effect/generator enum. |
| Card (performance-surface) schema | `manifold-core/src/effect_graph_def.rs:373` `PresetMetadata`, `:450` `ParamSpecDef` (min/max/default, `is_toggle`/`is_trigger`, `value_labels` modes, `curve`/`invert`, `is_angle`, `wraps`, `section`, OSC suffix), `:531` `BindingDef` (target, `scale`/`offset` remap, defaults) | **Exists, fully declarative** — the card is part of the same JSON artifact the validator sees. Card validation today is ONE check in one direction: `check_bindings_resolve` (binding id → outer param id). Everything else on the card is unvalidated. |
| Catalog vocabulary coverage | `docs/node_catalog.json`, measured 2026-07-13 | **237 nodes; 0 empty summaries / aliases / categories** (aliases include old names + TouchDesigner equivalents); 36 nodes lack `examples` (no shipping preset uses them). The vocabulary layer agents need already ships complete. |
| Look oracle | `crates/manifold-renderer/src/bin/render_generator_preset.rs` | **Exists** — headless preset → PNG. Agents can render-and-look today, pre-MCP. |
| Mux short-circuit | executor per-frame liveness (`node_graph/execution.rs`, see FREEZE_COMPILER_MAP §"execution") | **Exists** — a mux's unselected branch never executes. This is why discrete switching via `mix`-blend-at-the-rails is a performance bug, not just a style miss: both branches render every frame. |

Classification: the catalog and the validator **exist**; the CLI verbs and the fusion catalog field are **one wire away**; the only genuinely new pieces are the `BoundaryReason` declaration mechanism, its meta-test, and the importer hook.

*Extend, don't redesign.* The audit shrank this design from "build an agent graph-composition tool" to "extract one function, add two bin verbs, one macro field, one test, one hook, one lint pass, one authored guide."

## 2. Decisions

- **D1 — One validation implementation, extracted as a library seam.** New module `node_graph::validate` with `validate_def(...) -> ValidationReport` (§3), built by MOVING `check_presets::check_one`'s body (including `check_bindings_resolve`) into the library. `check_presets` becomes a thin directory-walker over `validate_def`. Every future consumer — CLI verb, importer hook, MCP `validate_graph` — calls this one function. **Rationale:** the validator's entire value is fidelity by construction (DESIGN_AUTHORING §4): a validator that is not the real loader will eventually approve graphs the loader rejects. **Rejected:** a standalone JSON-schema/xtask validator that parses graph JSON without the registry — that is a parallel reimplementation and will drift; forbidden by name.
- **D2 — CLI binary `graph_tool` in `manifold-renderer/src/bin/`** (precedent: `check_presets.rs`, `gen_node_catalog.rs` — registry-needing tools live as renderer bins). Verbs: `validate <file.json> --kind effect|generator [--json]` and `fusion <file.json> [--json]`. `--json` emits the structured report for agents; default output is human-readable. **Rejected:** a `compat <node> <port>` verb — port/channel types already ship in `node_catalog.json` and compatibility is derivable client-side; a verb would duplicate catalog data behind a second interface. **Rejected:** an xtask — xtask doesn't depend on manifold-renderer and shouldn't start to.
- **D3 — `catalog` is not a verb.** `docs/node_catalog.json` already IS the catalog artifact, regenerated by `gen_node_catalog` and drift-tested. Agents read the file. The only change: `catalog_gen` gains a `fusion` field per node (§3) so fusability is visible in the same artifact.
- **D4 — Boundary exemptions are declared in code via a closed enum.** New `BoundaryReason` enum (§3) mirroring the ADDING_PRIMITIVES taxonomy, declared via a new optional `boundary_reason:` field on `primitive!` and a `boundary_reason()` trait method (default `None`) for hand-impls. **Rationale (Peter's opt-out directive):** the compiler must stay conservative — `fusion_kind` describes an artifact (an authored `wgsl_body`), and defaulting it to fusable would promise codegen source that doesn't exist. The opt-out default lives at the *policy* layer: every atom is expected fusable; Boundary is the exception that must name its excuse. **Rejected:** flipping `FusionKind`'s default — miscompiles by construction. **Rejected:** an rg/text lint — hand-impl primitives make source text lie; the registry is the truth, so the check walks the registry.
- **D5 — The meta-test freezes conversion debt behind an explicit ledger.** `ConversionDebt` is a legal `BoundaryReason` ONLY for type_ids in an explicit const list inside the test, seeded verbatim from the 2026-07-11 sweep triage (wave 1–3 atoms). Converting an atom removes it from the list (test fails if a listed atom becomes fusable — stale ledger). Adding to the list requires editing the test file — deliberate, review-visible. A new atom cannot land as an undeclared boundary or claim ConversionDebt silently. This is the mechanism that makes "fusable by default" true for all future atoms while the sweep burns down the past.
- **D6 — The importer validates its own output.** `assemble_import_graph`'s result runs through `validate_def` before it reaches the project; a failure surfaces as the import error path with the report's messages (never a silent partial import — the forbidden move of load paths, DESIGN_DOC_STANDARD §5 round-trip corollary). **Rationale:** the assembler is code and has bugs; today its mistakes surface as wrong pixels or a load failure far from the cause.
- **D7 — MCP P2 becomes an adapter.** When MCP_INTERFACE executes, its `validate_graph` tool wraps `validate_def` and serializes `ValidationReport` — no new checks in the MCP layer. At this design's landing, add one line to MCP_INTERFACE_DESIGN §6 pointing here (landing-updates-the-doc rule).
- **D8 — Card lints join `validate_def` (P4), split by severity.** **Errors** (structural breakage — the card lies to the performer): binding target `node_id`/`param` absent from the graph; card param with no binding (dead slider); mode param whose `value_labels` count disagrees with its integer range; `is_trigger` bound to a non-trigger-typed inner param; duplicate OSC suffix on one card. **Warnings** (idiom/consistency — the agent corrects in-session; Peter 2026-07-13: agents don't habituate across sessions, "the likely thing is the warnings fire and they update the graph and cards"): discrete card control (toggle / labeled integer) bound to a continuous blend param on a mix-family node — the mux idiom message names the short-circuit cost; card/binding/inner default-value disagreement; card range that lands outside the inner param's range after `scale`/`offset`. **Rejected:** errors-only v1 (my initial lean — wrong, per Peter's habituation point). Warnings promote to errors only after a bundled-preset audit shows zero legitimate hits (Deferred).
- **D9 — Intent-recipes doc: `docs/CARD_AUTHORING.md`.** A working guide (standard §1) serving agents the semantic intent → idiom table: *toggle between two looks* → mux select + `is_toggle` (never blend-at-the-rails; blend is right only for a continuous morph — say so, so agents don't over-correct); *N-way mode* → `whole_numbers` + `value_labels` → mux select; *button that enables/fires* → `is_trigger` → trigger-typed param; *full-rotation knob* → `wraps`; *momentary vs latching* named; *"make it pulse/sync to the music"* → **expose, don't bake** — Peter's rule, verbatim: *"Beat sync'd stuff shouldn't be baked into the graph, the user has modulation tools to sync sliders."* The card is what makes a preset syncable; an internal oscillator baked into the graph steals the performer's control. New observed anti-patterns land here as rows (no code change); a row that proves mechanically detectable graduates to a D8 lint.
- **D10 — the `fusion` verb flattens first.** `partition_regions` returns empty for a def still carrying group nodes (`freeze/region.rs:208`); the loader flattens before fusion sees the def. The verb replicates loader order (flatten → partition) or every grouped preset falsely reports zero regions. Loader parity, same D1 principle.
## 3. Committed shapes

```rust
// node_graph/validate.rs (new module, manifold-renderer)
pub enum ValidateKind { Effect, Generator }  // From<PresetKind> — reuse, don't fork semantics

pub struct ValidationIssue {
    pub node_id: Option<u32>,      // doc id, as in GraphBuildError
    pub type_id: Option<String>,
    pub port: Option<String>,      // port or param name where applicable
    pub message: String,           // human/agent-readable, self-contained
}

pub struct ValidationReport {
    pub errors: Vec<ValidationIssue>,     // any → invalid
    pub warnings: Vec<ValidationIssue>,   // D8 idiom/consistency lints (P4); graph passes with warnings
}

/// THE validation entry point. Same pipeline the runtime loader takes:
/// parse is the caller's job; this takes the deserialized def.
/// Generators additionally run the PresetRuntime chain build (allocation
/// errors) — pass the device; effects ignore it.
pub fn validate_def(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    kind: ValidateKind,
    device: &GpuDevice,
) -> ValidationReport;
```

```rust
// node_graph/freeze/classify.rs — the exemption vocabulary (closed set)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryReason {
    NonGpu,               // CPU/control-rate op — no kernel to fuse
    BarrieredReduction,   // workgroup memory / barriers / multi-pass scan (peak, luminance, spawn_from_mesh)
    CrossFrameState,      // state must materialize to survive the frame (temporal)
    IoBridge,             // uploads, readbacks, DNN/FFI — data enters or leaves the GPU
    DrawCall,             // render_* rasterization passes
    FusedBundle,          // decomposition backlog (DigitalPlants, NestedCubes) — dies with the bundle
    Blocked,              // passes the scope test; codegen can't express an input — tracked BUG (114/115)
    ConversionDebt,       // owed a wgsl_body; legal ONLY for type_ids in the meta-test ledger
}
```

- `primitive!` gains optional `boundary_reason: <Ident>,` (same optional-field pattern as `fusion_kind:` at `primitive.rs:797`); the trait gains `fn boundary_reason(&self) -> Option<BoundaryReason> { None }`.
- Meta-test (name committed): `node_graph::freeze::classify::tests::every_boundary_atom_declares_its_reason` — walks `PrimitiveRegistry::with_builtin()`; for every registered primitive asserts `is_fusable() XOR boundary_reason().is_some()`, asserts `ConversionDebt` holders ⊆ the ledger const, and asserts every ledger entry still exists and is still Boundary.
- `catalog_gen` NodeRow gains `fusion: String` — `"pointwise" | "source" | "multi_input_coincident" | "boundary:<reason snake_case>"`.
- `graph_tool fusion` output: per-node kind/reason + `partition_regions` result — region count, members, and for each cut a one-line reason (from the classify path). Estimated dispatch count = regions + boundaries.

**Consequences, stated honestly:** `validate_def` takes a `GpuDevice` even for effects (uniform signature; device init ~50ms once per process — irrelevant for a CLI, and the importer already runs in the app where the device exists). The ConversionDebt ledger is a manually maintained list — "only ever shrinks" is enforced socially plus by the stale-entry assertion, not by history-aware machinery; a reviewer can still approve a bad addition, but never without seeing it. The catalog JSON churns once (every node gains a `fusion` field) — one regen commit.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| Exactly one validation implementation; the validator IS the loader's pipeline | By construction after P1 (check_presets and all consumers call `validate_def`); negative gate: `rg "into_graph|compile\(" crates/manifold-renderer/src/bin/graph_tool.rs crates/manifold-renderer/src/bin/check_presets.rs` → zero hits (bins reach the pipeline only via `validate::`) |
| Every registered primitive is fusable or names its boundary reason | `every_boundary_atom_declares_its_reason` (lib test, default sweep) |
| ConversionDebt list stays honest (converted atoms leave it; new atoms can't join silently) | Same test: ledger-membership + stale-entry assertions; additions require editing the test file |
| Catalog artifact never drifts from the registry (incl. the new fusion field) | Existing `catalog_gen::tests::regenerates_in_sync` (extends automatically) |
| Importer output is validated before it reaches the project | P3 unit test (named in the P3 brief): a deliberately corrupted assembler output fails the import with a named node |
| The card never lies to the performer (no dead sliders, no dangling bindings, no mislabeled modes, no trigger-type mismatches, no duplicate OSC addresses) | D8 error lints in `validate_def`, each with a named P4 unit test; bundled presets gate at zero card errors |

## 5. Phasing

**P1 — the validate seam + `graph_tool validate`.** *Entry:* anchors in §1 re-verified (`check_one` at check_presets.rs:100, `GraphBuildError` at graph_loader.rs:98). *Read-back:* this doc §2 D1–D2, DESIGN_DOC_STANDARD §5–§6, check_presets.rs whole. *Deliverables:* `node_graph/validate.rs` (`validate_def`, `ValidationReport`, `From<GraphBuildError> for ValidationIssue` etc.), check_presets rewired as a walker over it, new bin `graph_tool.rs` with the `validate` verb (`--json`), unit tests: every bundled preset validates clean via the new path; **held-out broken fixtures** (bad channel type, missing required port, unknown type_id, cycle, unresolved binding — five files under `crates/manifold-renderer/tests/fixtures/invalid-graphs/`, authored by the orchestrator, not the P1 worker) each produce an error naming node and, where applicable, port/param. *Gate (positive):* `cargo test -p manifold-renderer --lib validate` green; `cargo run -p manifold-renderer --bin check_presets` output unchanged vs pre-phase run (same pass count). *Gate (negative):* `rg "fn check_one" crates/manifold-renderer/src/bin/` → zero hits. *Demo:* `graph_tool validate` run on one bundled preset and one broken fixture, both outputs pasted — L2. *Forbidden moves:* reimplementing any check inside the bin; new Arc<Mutex>; changing loader behavior "while in there" (scope fence: extraction is a move, not a rewrite). *Test scope:* `-p manifold-renderer --lib` + clippy `-p manifold-renderer`.

**P2 — boundary reasons + meta-test + catalog fusion field.** *Entry:* P1 landed. *Read-back:* this doc D4–D5, ADDING_PRIMITIVES §"codegen path is mandatory", the `codegen-conversion-sweep` memory. *Deliverables:* `BoundaryReason` enum, macro field + trait method, declarations on every currently-Boundary primitive **transcribed from the sweep triage** (the triage is the verdict; the worker classifies nothing — an atom the triage doesn't cover is an escalation, not a guess), the ledger const seeded with the wave 1–3 atoms, `every_boundary_atom_declares_its_reason`, `catalog_gen` fusion field + regenerated `node_catalog.json`/NODE_CATALOG.md block. *Gate (positive):* meta-test green in the default sweep; `gen_node_catalog --check` clean; declared-reason counts reported and reconciled against the triage's ~54-exempt figure (a count mismatch >±5 is an escalation, not a shrug). *Gate (negative):* `rg "boundary_reason: ConversionDebt" crates/` count == ledger length. *Demo:* none — L1 (enforcement phase; the artifact is the failing-test proof: flip one declaration, show the test name the atom). *Forbidden moves:* classifying atoms by reading their kernels (the triage decided; transcribe), blanket-ConversionDebt to go green, touching any `fusion_kind` (this phase declares reasons, it converts nothing). *Test scope:* `-p manifold-renderer --lib`; no GPU runs (no kernel changes by construction).

**P3 — `graph_tool fusion` + importer hook + guidance.** *Entry:* P1+P2 landed. *Read-back:* this doc D6, FREEZE_COMPILER_MAP §3–§4, gltf_import.rs:267 + its app_lifecycle call site. *Deliverables:* the `fusion` verb over `partition_regions` + per-node classify output; `assemble_import_graph` output validated via `validate_def` with failures surfacing on the existing import-error path (never silently dropped); test: corrupted assembler output fails with a named node; docs — DECOMPOSING_GENERATORS and ADDING_PRIMITIVES each gain a short "machine-check your graph" pointer, CLAUDE.md's tooling section gains one line. *Gate (positive):* `graph_tool fusion` on `DepthOfField` (heaviest shipped fusion user) reports regions machine-compared equal (count + membership) to the real freeze pipeline's partition of the same def; importer test green. *Gate (negative):* no `unwrap`/`expect` on the new fallible importer-hook lines (`rg` scoped to the new hook function → zero hits). *Demo:* fusion report for one preset pasted; the import of a known-good .glb fixture still succeeds — L2. *Forbidden moves:* a second region-partition implementation inside the verb (call `partition_regions`); skipping the D10 flatten (a grouped preset reporting zero regions is the phase's known wrong answer — the gate preset must include one grouped def); making import failures warnings "to be safe" (silent fallback, forbidden by name). *Test scope:* `-p manifold-renderer --lib` + `-p manifold-app` build; partition is pure — if the ground-truth comparison seems to need a GPU run, escalate first.

**P4 — card lints + `docs/CARD_AUTHORING.md`.** *Entry:* P1 landed (P2/P3 not required, but the wave runs in order). *Read-back:* this doc D8–D9, `effect_graph_def.rs:373–560` (PresetMetadata/ParamSpecDef/BindingDef) whole, one bundled preset's JSON card block end-to-end. *Deliverables:* card lint pass inside `validate_def` — the five D8 error checks and three D8 warning checks, each with a named unit test; held-out invalid-card fixtures (one per error class, orchestrator-authored, same pattern as P1); `docs/CARD_AUTHORING.md` seeded with the D9 intent table verbatim (the doc's rows are decided here — the worker transcribes and formats, adds none); `graph_tool validate` output distinguishes errors from warnings. *Gate (positive):* all bundled presets produce **zero card errors** (a bundled-preset error is a real bug: fix it in the phase if it's a one-line metadata fix, else escalate with the list); warning counts on bundled presets reported verbatim for Peter's triage — not fixed, not suppressed; each fixture produces its named issue. *Gate (negative):* `rg "value_labels|is_trigger|osc_suffix"` in graph_tool.rs → zero hits (lints live in `validate::`, the bin only prints). *Demo:* validate output for one broken-card fixture and one warning-tripping graph pasted — L2. *Forbidden moves:* inventing new intent-table rows (D9 decided them; new rows are Peter's); auto-fixing bundled-preset warnings; weakening an error to a warning to make the gate pass. *Test scope:* `-p manifold-renderer --lib` + clippy `-p manifold-renderer`; no GPU.

Landing (per GIT_TREE_DISCIPLINE): batch P1–P4 as one workstream; full clippy+nextest sweep in the warm main checkout at landing; landing updates this Status line and adds the D7 one-liner to MCP_INTERFACE_DESIGN §6.

## 6. Decided — do not reopen

1. One `validate_def`; every consumer (CLI, check_presets, importer, future MCP) calls it. No parallel validators.
2. `FusionKind` default stays `Boundary`; opt-out lives in the meta-test policy layer, not the compiler.
3. `ConversionDebt` is ledger-gated inside the test; the ledger is seeded from the 2026-07-11 triage and only edited deliberately.
4. No `compat` verb; port/channel compatibility is served by `node_catalog.json`.
5. `graph_tool` is a manifold-renderer bin; not an xtask, not a new crate.
6. Import failures from validation are errors on the import path, never warnings.
7. This design does not convert any atom to codegen — the conversion sweep (its own plan) does; nor does it touch the MCP server's phases.
8. Card lints ship with warnings in v1 (D8 severity split); the intent table's rows are decided in D9 — executors add none.
9. Beat-sync / musical motion is delivered by exposing modulatable card params, never baked into the graph (Peter, 2026-07-13).
10. The `fusion` verb flattens groups before partitioning (D10 — loader parity).

## 7. Deferred

- **Composite binding-target lint** — `BindingTarget::Composite` resolves through a runtime `CompositeHandle`, not derivable from the def alone; zero bundled presets use one (verified P4). Trigger: the first preset that does.
- **`patch_graph`/compat/query verbs** — revive with MCP_INTERFACE execution if the stress-test shows agents need them.
- **Component tier in the catalog/validator** — rides in with COMPONENT_LIBRARY (its design already amends the surface).
- **Error-message quality pass** (messages tuned on real agent transcripts) — MCP_INTERFACE P5 owns it; the structured `ValidationIssue` shape here is what makes it cheap.
- **Fusion *advice*** ("this Boundary chain would fuse if X converted") — nice-to-have; revive if the content push shows agents authoring dispatch-heavy graphs in practice.
- **Warning → error promotion** — a D8 warning class whose bundled-preset audit (P4 gate output) shows zero legitimate hits may be promoted; Peter's call per class.
- **Craft doc (taste floor)** — the "what reads well on a big dark stage" guide in Peter's voice. Deliberately NOT a phase deliverable: Sonnet-authoring it generically produces filler agents would faithfully reproduce. Trigger: one session where Peter + a strong model harvest the rows from his corrections and taste corpus, then it joins CARD_AUTHORING/the catalog as served context.
- **Descriptor motion-character line** — a per-node "what this looks like moving / when it reads on stage" field extending `NodeDescriptor`. Trigger: the craft-doc session, which decides the vocabulary; coverage is a campaign like the summary/alias fill was.
