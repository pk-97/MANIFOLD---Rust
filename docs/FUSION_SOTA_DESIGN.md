# Fusion SOTA — closing the freeze compiler's structural gaps

**Status:** IN PROGRESS · 2026-07-14 · Fable 5 (with Peter in the room) · Sonnet 5 executing
P1–P3 SHIPPED (markers module, segment worker robustness, refusal census committed as
`docs/fusion_census.md` — no D4 default flipped, all four stand). P4a SHIPPED. **P5 SHIPPED**
(Vec3 lift + D4 scope-expansion Vec4/Color lift, landed BEFORE P4b per the reordering below —
`classify_node`'s param gate narrowed, fused codegen + install-time param seeding extended,
`fusion_coverage_baseline` widened to effect+generator/flattened and its floor raised). P4b,
P6–P7 remain.
**Prerequisites:** none for P1–P4; P5–P6 read P3's census numbers. The companion Sonnet sweep
(BUG-135/141 includes fix, the 13-atom `CONVERSION_DEBT_LEDGER` conversion sweep, BUG-146 prewarm,
BUG-115 spike, content-key normalization, tolerance/comment hygiene) is SEPARATE work with existing
specs — it does not depend on this doc and this doc does not depend on it, except where a phase
below names it.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 before starting any phase.

Peter, 2026-07-14, on the whole backlog of compiler gaps: *"I would like ALL of this work to be
implemented and fixed"*, *"I never made a call about post release. No need to defer this fusion
work"*, and the goal: *"I would like all nodes where possible to fuse and for this system to be
'future proof' as possible."* This doc is the contract for the five items that needed design
judgment rather than transcription: the marker ABI's type-level enforcement, the segment worker's
hang mode, the `BufferIndex` read path (BUG-114), the deliberate under-fusing boundaries, and the
fused-cache leak model.

**For the instrument:** the compiler's failure modes today are all *silent slowness*, never wrong
pixels — every refusal renders unfused, which is correct but costs N dispatches where a fused run
costs ~1. On a heavy show graph that's the difference between headroom and dropped frames. This
design removes the silent-slowness classes (hung worker, unfusable overlay HUDs, boundary families
that never got a second look) and hardens the one silent-WRONG-output class that exists (marker
drift) so it cannot recur.

Companion docs: `FREEZE_COMPILER_MAP.md` (authoritative current-state map — §4 cut rules, §5
marker ABI, §11 honest edges are this doc's inputs); `ADDING_PRIMITIVES.md` (the conversion recipe
P4 reuses); `docs/BUG_BACKLOG.md` BUG-114 (the `draw_*` gap this doc's P4 closes).

---

## 1. Audit — what exists (verified 2026-07-14)

| Piece | Where | State |
|---|---|---|
| Marker emit sites (7 markers) | `freeze/codegen.rs:1547,1571,1577,1898,1906,1918,2275` + `freeze/install.rs:1450` | Literal `format!`/`push_str` strings |
| Marker parse sites | `primitives/wgsl_compute.rs` (`introspect` + helpers, ~163–1000: `@sampler_address_mode` 163, `@reset_gated` 167, `@dispatch_count_param` 176, `@derived_uniform_member` 183/328/994, `@static_param` 211/637, `@fused_output` 295/699/914, `@camera_external` 711) | Literal string matches, no shared source with emit |
| Segment worker | `freeze/install.rs:418–447` (`segment_worker`), `:453–478` (`pump_segment_results`), `:483–525` (`fused_segment_view_for`) | Panic mid-job kills the thread; the in-flight key stays in `SEGMENT_PENDING` forever (later enqueues get `Refused` via the dead-sender check at `:512`, but the wedged key never resolves). No deadline anywhere. |
| Cache-cap insert skip | `freeze/install.rs:258,291,462` | `if m.len() < FUSED_CACHE_CAP { m.insert(…) }` — at cap, an EXISTING key's refresh is also skipped |
| `InputAccess` | `freeze/classify.rs:76–120` | `BufferIndex` named as planned-not-built at `:73`; `Coincident`/`CoincidentTexel`/`Gather`/`GatherTexel`/`BufferGather` shipped |
| `buf_<port>` global convention | `freeze/codegen.rs:728,802,823,963` | Buffer bodies reference input array globals by name; codegen emits the binding |
| Whole-fragment namespacing | `freeze/codegen.rs:1325` | Fused codegen namespaces a member's entire fragment (`n{i}_` fields; helpers too) |
| Multi-output struct-return wrapper | `freeze/codegen.rs:1001,1030` | Standalone BUFFER atoms already do `buf_<port>[idx] = result.<port>` from a struct-returning body — the precedent P6 extends to texture atoms |
| `draw_*` family | `primitives/draw_dots.rs` (+ markers/ticks/gauge/scanlines/connections, `blob_overlay`) | Plain-WGSL, `boundary_reason: Blocked`; one thread per output pixel, reads a `Channels[…]` detections array by index (`draw_dots.rs:89`) — a gather-shaped read, per-element in the mandate's sense |
| Boundary families (under-fusing by design) | Buffer fan-out `region.rs:1528–1532`; nested stencils / `MAX_VIRTUAL_CHAIN=1` `region.rs:368,435`; multi-output texture atoms (cut rule 6); Vec3/Table params (cut rule 4); resample (cut rule 7) | Each a deliberate v1 refusal; none re-examined since shipping |
| Census tools | `region.rs:2957` (`explain_presets`), `:2981` (`audit_all_presets`), both `#[ignore]`d; `graph_tool fusion` | Print per-node class + per-wire verdicts; do NOT bucket refusals by family or count dispatches saved |
| Leak model | `install.rs:163,167,276,288,621–622,761,1187,1674,1682` (`Box::leak`, `leak_params`, `leak_ports`); cap rationale comment `:217–229` | Values are `&'static`; past `FUSED_CACHE_CAP=512` the miss path recompiles AND re-leaks per rebuild — the comment itself names the Arc/LRU fix as "deferred with the background-compile step" (which shipped for segments; the deferral is stale) |

Extend, don't redesign: every mechanism below names its in-repo precedent. Nothing in this design
adds a thread, a lock, or shared state; nothing changes the fuse decision model
(structural — settled, `fusion-decision-is-structural`, do not re-propose measurement).

## 2. Decisions

**D1 — One markers module owns the marker ABI; both ends compile against it.**
New `freeze/markers.rs`: a `Marker` enum with one variant per marker (§5 of the map), each
carrying its typed payload (e.g. `DispatchCountParam { field: String }`,
`DerivedUniformMember { first_field: String, words: u32, type_id: String, camera_port: Option<String> }`),
with `emit(&self) -> String` and `parse(line: &str) -> Option<Marker>` as the ONLY implementations
of the wire format. Codegen/install emit through it; `wgsl_compute::introspect` parses through it.
Precedent: `fusion_classification_string` (`classify.rs:209`) — one implementation shared by
`catalog_gen` and `graph_tool` for exactly this reason.
*Rejected: a typed sidecar struct carried next to the WGSL (serde field on the fused node), because
the WGSL text is the cross-session pipeline-cache key and the single deterministic artifact — a
sidecar splits the contract into two artifacts that can drift independently. Markers stay in the
text; the module makes the text's grammar single-sourced.*
Consequences, stated honestly: the wire format is still strings-in-comments — drift between
`markers.rs` and OLD cached WGSL text is impossible (the text is regenerated by the same binary
that parses it), but hand-authored kernels (`@pure` on BlackHole, user `@fusion:` fragments) write
markers by hand and the module only validates them at parse time. Byte-identical emission is a hard
gate (the text is the pipeline-cache key): P1 must prove no output change.

**D2 — The segment worker gets panic containment and a pump-side deadline; Pending can no longer
be forever.** (a) `catch_unwind(AssertUnwindSafe(…))` around `compile_segment_view` per job; a
panic sends `SegmentResult { key, view: None }` — the key negative-caches as `Refused` and the
thread survives. (b) `SEGMENT_PENDING` becomes `AHashMap<u64, std::time::Instant>` (enqueue time);
`pump_segment_results` — which already runs every chain dispatch — expires entries older than
`SEGMENT_COMPILE_DEADLINE` (60s; codegen is pure CPU and measured in ms, so 60s means something is
truly wrong) into the negative cache with one
`eprintln!("[freeze] chain segment compile timed out — rendering per-card …")`. A late result
landing after expiry still inserts (the insert path overwrites), so a slow-but-alive worker
self-heals back to fused. (c) The expiry check runs only when the pending map is non-empty — zero
cost in the steady state.
*Rejected: a watchdog thread — new thread + shared state for a check the existing per-dispatch pump
does for free.*
Consequences, stated honestly: a genuinely hung worker thread is not killed (Rust can't), so a hang
still costs one OS thread; what the deadline buys is that the CHAIN stops waiting and the negative
cache makes the state visible and stable instead of silently-pending. On stage: a card that would
have quietly rendered per-card forever now logs once and renders per-card — same pixels, but the
operator can see it, and every other segment keeps compiling because the panic case no longer kills
the worker.

**D3 — `InputAccess::BufferIndex` is the array-into-texture read path (closes BUG-114).**
A texture-domain fusable atom may tag an `Array`/`Channels` input `BufferIndex`. Semantics: the
body reads elements of the input array global `buf_<port>` by indices it computes — exactly
`BufferGather`'s convention (`codegen.rs:728,802`), hosted in a texture-domain kernel.
- **Classify** (`classify_node`): a wire into a `BufferIndex`-tagged input does not cut (narrowing
  cut rule 9, same shape as the Camera exemption at map §4.9).
- **Region** (`partition_regions`): the array producer NEVER unions into the texture region
  (cross-domain, exactly like a gather-consumed wire) — it stays an external; `build_region`
  records it as a buffer external.
- **Codegen**: standalone kernel binds `var<storage, read> buf_<port>: array<ExtK>` with `ExtK`
  synthesized from the port's `Channels[…]` layout (draw_dots' `Detection` = X/Y/WIDTH/HEIGHT →
  4×f32 — derivable, no hand-written struct); the fused path namespaces the global per member and
  rewrites the body's `buf_<port>` token, riding the existing whole-fragment namespacing
  (`codegen.rs:1325`). Bodies guard with `arrayLength` as the `draw_*` shaders already do.
- **Then convert** the six `draw_*` atoms + `blob_overlay` per the `ADDING_PRIMITIVES` recipe
  (wgsl_body + `standalone_for_spec` + generated-vs-hand parity oracle), removing their `Blocked`
  reason.
*Rejected: re-encoding detections as a 1-px-row texture so the existing texture path could read
them — a per-frame encode dance that changes channel-type semantics, and the compiler gap would
simply wait for the next array-consuming texture atom.*
Consequences, stated honestly: one new codegen read path is real compiler surface (~binding
emission + external bookkeeping + namespacing); it is additive (the classify comment at
`classify.rs:66` designed for exactly this: "one more codegen read-path + one region-grow rule,
never a re-tag of shipped atoms"). v1 scope: BufferIndex atoms fuse as members of texture regions;
fusing the array PRODUCER into anything is out of scope.

**D4 — The boundary families get a census, two lifts by default, and honest verdicts on the rest.**
The census (P3) extends `audit_all_presets` to bucket every refusal by family and report
dispatches-saved-if-lifted, run over the bundled presets + the Liveschool fixture. The default
decisions, revisable only by census numbers:
- **Vec3 params (cut rule 4, Vec3 half): LIFT.** Pack a Vec3 param as a vec4-padded uniform field
  (the wgsl-vec3-alignment rule; the uniform packer already lays out vec4). Table/String params
  stay boundary — Table is storage-shaped data, String has no GPU representation; both get
  `Enforcement: boundary_reason` and are not debt.
- **Scope expansion, decided with Peter 2026-07-14 (P4a escalation):** Vec4/Color params ALSO
  LIFT in P5, not just Vec3. Found during P4a: `classify_node`'s param gate (`region.rs:954–961`)
  rejects Vec3/Vec4/Color/Table/String uniformly for the fused path — Vec4/Color already gained
  STANDALONE codegen support in the companion Sonnet sweep (`codegen.rs`, "P3 wave 2", reassembled
  as `vec4<f32>`) but `param_wgsl_type` still boundary-cuts them for fusion on purpose, pending
  this design. All six `draw_*` atoms (BUG-114/P4) carry a `Color` param — without this expansion,
  P4b's conversion would leave them mechanism-correct but still Boundary, and BUG-114 would not
  actually achieve its promised dispatch reduction. Peter's call: expand P5 to cover Vec4/Color
  using the same vec4-padded-uniform mechanism as Vec3 (Color/Vec4 are already 4 words — no
  padding needed, simpler than Vec3's case). **Phase order changed:** P5 now lands BEFORE P4b (was
  after), so the remaining `draw_*` conversions in P4b land against a param gate that already
  accepts their Color param and actually fuse.
- **Multi-output texture atoms (cut rule 6): LIFT, census-gated.** Body returns a struct; the
  standalone wrapper writes each output — the buffer path already ships exactly this
  (`codegen.rs:1001,1030`); extend it to texture kernels. If the census shows zero shipped-preset
  cuts from this family, it lands anyway as capability (palette vocabulary: voronoi's cell+distance
  outputs) but LAST in the wave order.
- **Buffer fan-out regions (`region.rs:1532`): DEFER.** Lifting means N fresh-dst arrays per fused
  buffer kernel with per-output alias reasoning against §9.7's in-place-loop model — the riskiest
  interior in the compiler for a family the census must first show actually cuts anything. Trigger:
  census ≥3 refusals across shipped content, or a user graph demonstrably paying it.
- **Nested stencils / `MAX_VIRTUAL_CHAIN=1` (`region.rs:368`): CORRECT AS-IS.** The cap is a cost
  cliff, not a gap — absorption recomputes the chain at every tap's 4 bilinear corners, so depth
  compounds 4^d; lifting it would fuse things into being SLOWER. Not debt. Revisit only with a
  measured preset where the round-trip loses to the recompute.
- **Resample-into-region (cut rule 7): DEFER.** A mid-region scale change breaks the one-grid-per-
  region model; the sampled-external mechanism covers the common cases already. Trigger: census
  evidence of hot resample-sandwich chains in real content.
*Rejected: lifting everything uniformly because "SOTA" — the conversion-sweep precedent (uniform,
don't gate per-atom) covers ATOMS, where each conversion is cheap and identical; boundary families
are each a distinct compiler capability with distinct risk, and building the two that pay while
naming honest triggers for the rest IS the root fix for the class ("never re-examined" was the
defect, not "not all lifted").*

**D5 — Fused-cache values become `Arc`; the cap becomes LRU eviction; the leak model dies.**
`FUSED_EFFECT_CACHE` / `FUSED_GENERATOR_CACHE` / `SEGMENT_CACHE` values change from
`Option<&'static T>` to `Option<Arc<T>>` (`Arc`, not `Rc`: `SegmentView` is built on the
chain-fusion worker and sent to the content thread). Inside the views, today's leaked interiors
(`leak_params`/`leak_ports`/`Box::leak` str at `install.rs:761,1187,1674,1682`) become owned
`Vec`/`String` fields. At cap, evict least-recently-hit instead of refusing to insert (precedent:
the chain state-cache eviction, `EFFECT_CHAIN_LIFECYCLE.md`). Fix the at-cap refresh nit
(`install.rs:258,291,462`) in the same phase: `m.len() < CAP || m.contains_key(&key)`.
Migration is compiler-driven: change the cache value type and the struct fields, follow the
errors; the canonical bundled-preset views (loaded once, genuinely session-lived) may stay
`&'static` — the seam is the FUSED artifacts only.
*Rejected: raising the cap — doesn't remove the class. Rejected: evicting leaked values — eviction
can't reclaim a leak; ownership has to change first, which is the whole point.*
Consequences, stated honestly: this touches the type that threads through chain building
(`LoadedPresetView` consumers in `preset_runtime.rs` and the generator registry), so it is the
widest mechanical diff in this design — but every step is compiler-guided, the failure mode of a
missed site is a compile error (not a runtime bug), and the end state deletes an entire
"unbounded growth under edit-spam" class plus the `#[allow]`-adjacent leak helpers. Arc clone cost
is editing-time (cache hit at rebuild), never per-frame.

## 3. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| Every marker byte on the wire is produced/consumed by `freeze/markers.rs` | Negative gate: `rg '"// @' crates/manifold-renderer/src --type rust` returns hits ONLY in `markers.rs` (test `marker_literals_live_in_one_module`, a `std::process`-free source scan like `every_boundary_atom_declares_its_reason`) |
| `Marker::parse(m.emit()) == Some(m)` for every variant | Round-trip property test in `markers.rs` (`marker_roundtrip_every_variant`) |
| Marker refactor changes zero emitted bytes | P1 gate: WGSL snapshot equality over every bundled preset's fused defs, before/after (`fused_wgsl_snapshot_unchanged`) |
| A Pending segment key resolves within the deadline or negative-caches visibly | `segment_pending_expires_to_refused` (unit test on the expiry fn with an injected `now`) |
| A worker panic refuses the in-flight key and the worker survives | `segment_worker_panic_refuses_key` (feed a def whose compile is made to panic via a test hook) |
| A `BufferIndex` external never unions into a texture region | Classify/region unit test + `explain_presets` verdict assertion (`buffer_index_external_stays_external`) |
| Converted `draw_*` atoms match their hand shaders | Per-atom generated-vs-hand parity oracle (existing `gpu_tests` pattern), value-level |
| Fused caches are bounded and owned (no leak past cap) | `rg 'Box::leak' crates/manifold-renderer/src/node_graph/freeze/` returns zero hits after P7 (negative gate `freeze_has_no_leaks`); LRU eviction unit test |
| Boundary-family verdicts stay visible | Census output committed as `docs/fusion_census.md` (P3 deliverable), regenerated by the census test — stale-check rides the docs-index freshness pattern |

## 4. Phasing

Forbidden across all phases: any change to the fuse DECISION model (structural, no measurement —
settled) · any new thread/lock/`Arc<Mutex>` · fuse-for-parity (converting an atom by bundling
neighbors) · silent fallback beyond the existing render-unfused contract · trusting `classify.rs`
doc comments over code (known drift, map header) · landing with a red freeze suite.
GPU-touching phases (P4–P6) run their gates via plain `cargo test -p manifold-renderer --features
gpu-proofs <module>` (never nextest); every phase runs the freeze suite
`cargo test -p manifold-renderer --lib node_graph::freeze` + scoped clippy; full sweep at landing
per GIT_TREE_DISCIPLINE.

- **P1 — `freeze/markers.rs` (D1). SHIPPED `3dac02c7`.** Deliverables: the module (enum + emit/parse + roundtrip
  test), all emit/parse sites rewritten through it (inventory in §1 — re-derive with
  `rg '"// @' crates/manifold-renderer/src` at execution; if counts differ from §1, stop and list),
  `marker_literals_live_in_one_module`, `fused_wgsl_snapshot_unchanged`. Gate: freeze suite green;
  snapshot test proves byte-identical emission; negative gate zero stray literals. Demo: none — L1
  (pure refactor proven by snapshot).
- **P2 — segment worker robustness (D2). SHIPPED `6297946b`.** Deliverables: catch_unwind wrap, timestamped
  `SEGMENT_PENDING`, `SEGMENT_COMPILE_DEADLINE=60s` expiry in `pump_segment_results`, the two unit
  tests, the at-cap refresh nit fix (all three caches). Gate: freeze suite; new tests green.
  Demo: none — L1 (the observable surface is a log line on a fault path).
- **P3 — refusal census (D4 instrument). SHIPPED `c7ff8ebc`, `docs/fusion_census.md` committed.
  No trigger crossed (buffer-fan-out measured 0 refusals vs. trigger ≥3; resample's trigger is
  runtime hot-chain evidence, not a static count) — all four D4 defaults stand unchanged.**
  Deliverables: `audit_all_presets` extended to bucket
  refusals by family (fan-out / stencil-depth / multi-output / param-type / resample / arity /
  BufferIndex-shaped / other) and count dispatches-saved-per-lift; run over bundled presets + the
  Liveschool fixture; committed `docs/fusion_census.md` with the numbers + one paragraph reading
  them against D4's defaults. Gate: the census runs in CI as an #[ignore]d-but-invocable test;
  numbers in the doc. Demo: the census file itself — L2. **This phase's numbers may flip D4's
  census-gated defaults; flipping a DEFER to LIFT is an escalation to Peter with the number
  attached, not a silent scope change.**
- **P4 — `BufferIndex` + the draw-family conversion (D3, closes BUG-114).** Split: **P4a —
  SHIPPED `ae9ab74c`.** The read path (classify variant, region rule, standalone+fused codegen,
  synthesized `Channels` struct) + `draw_dots` converted as the proving atom, with parity oracle.
  Escalation found and resolved with Peter (see D4's scope-expansion note): the literal
  region-formation demo (`draw_dots_fuses_into_texture_region`) is NOT reachable yet — draw_dots'
  `Color` param independently boundary-cuts it until P5 lands; P4a instead proved the mechanism
  at the classify/region layer directly (wire never unions, producer stays external) plus the
  before/after `graph_tool fusion` dispatch count on `BlobTracking.json` (unchanged, as expected,
  pending P5). **Phase order changed: P5 now runs BEFORE P4b.** **P4b — the remaining five
  `draw_*` + `blob_overlay`**, parity oracle each, `Blocked` reasons removed, BUG-114 Status →
  FIXED, `docs/node_catalog.json` regenerated. Gate: gpu-proofs on touched modules; freeze suite;
  `explain_presets` shows the HUD preset ACTUALLY fusing (region forms, dispatch count drops —
  this is now reachable because P5 lands first and lifts the Color param that was blocking it).
  Demo: before/after dispatch count on the Blob Track HUD preset (the census tool prints it) — L2.
- **P5 — Vec3 + Vec4/Color param lift (D4, expanded scope). SHIPPED.** `classify_node`'s param
  gate (`region.rs`, was `:954–961`) narrowed via a new shared predicate
  `codegen::param_is_fusable` (Vec3/Vec4/Color pass, Table/String still cut); `classify_refusal`'s
  mirror updated in lockstep (`refusal_census_matches_classify_node` invariant), including a
  second drift found there — the D3 `buffer_index_ports` wire exemption existed in `classify_node`
  but was missing from its census mirror, invisible until the param gate stopped short-circuiting
  before that wire loop for `draw_dots`. Both `generate_fused` and `generate_fused_buffer` extended
  with the same Vec3 (3 namespaced `_x/_y/_z` fields) / Vec4+Color (4 `_x/_y/_z/_w` fields, no
  padding) struct + arg emission the standalone path already proved. A second layer this phase
  found necessary beyond codegen text: `install.rs`'s param-seeding loop only ever seeded ONE
  scalar field per param — extended with `effective_param_vec3`/`effective_param_vec4` to seed the
  3/4 sub-fields from the atom's default/override, with a fail-closed refusal if a control wire
  ever targets a Vec3/Vec4/Color param (structurally unreachable today — `ParamConvert` has no
  non-scalar variant, so no outer-card binding can target one either). Re-classification (census,
  real-preset probe): `node.shininess`/`node.rim_light`/`node.matcap_two_tone` (OilyFluid),
  `node.brightness` (MetallicGlass), `node.channel_mixer` (StarField), and `node.gradient_map`
  (synthetic pair, no bundled preset) all flip Boundary → Eligible; `draw_dots` flips to
  `[pointwise]`/Eligible in `graph_tool fusion` output on `BlobTracking.json`, though that preset
  still shows 0 regions — its neighbors are the five still-unconverted `draw_*` atoms, P4b's job,
  not a new gap. `fusion_coverage_baseline` had two of its own blind spots found and fixed at the
  root (Effect-only + no group-flatten, both silently excluding every atom this phase's real proof
  lives in — OilyFluid/MetallicGlass/StarField are grouped GENERATOR presets); floor raised on the
  widened, isolated-measured numbers (32/52/203 pre-P5 → 32/54/216 post-P5 on the same walk).
  Gate: gpu-proofs full suite + `--lib` full suite, both clean modulo 8 pre-existing failures
  verified unchanged at P4a HEAD (21794f5c) — 6 synthetic `codegen::gpu_tests` cases and 2
  prewarm-cache tests that fail only under full-suite contention, not in isolation; clippy clean.
  Demo: census refusal count for the param-type family 19→10 (the 9 real Vec3/Vec4/Color flips);
  `wave2_color_param_atoms_now_fuse_in_shipped_presets` proves all 5 real-preset atoms now fuse;
  `buffer_index_external_stays_external` proves `draw_dots` forms a real 2-member region with a
  synthetic neighbor via `partition_regions` — L2.
- **P6 — multi-output texture atoms (D4).** Deliverables: struct-return texture wrapper (precedent
  `codegen.rs:1001`), voronoi converted, cut rule 6 narrowed to "multi-output without struct-return
  body". Gate: parity oracle; freeze suite; coverage ratchet. Demo: census delta — L2. Ordered
  last of the lifts per D4 unless P3's numbers promote it.
- **P7 — cache ownership (D5).** Deliverables: Arc-valued caches, owned view interiors, LRU
  eviction, the `freeze_has_no_leaks` negative gate, eviction unit test. Seam brief applies
  (standard §6): re-derive the consumer inventory with
  `rg "&'static LoadedPresetView|&'static SegmentView|&'static EffectGraphDef" crates/manifold-renderer/src`
  at execution time; compiler-driven migration (change the type, follow the errors); misfit sites
  escalate, never adapt. Gate: freeze suite + full `-p manifold-renderer --lib`; negative gate.
  Demo: none — L1 (behavior-identical by construction; the observable is the negative gate).

Phase-completeness: every §2 decision lands in exactly one phase (D1→P1, D2→P2, D4→P3+P5+P6 with
DEFER verdicts recorded in Deferred below, D3→P4, D5→P7). No design-body affordance exists outside
this list.

## 5. Decided — do not reopen

1. Fuse decision stays structural; no measurement, no perf gate (pre-existing, reaffirmed).
2. Markers stay in the WGSL text; the module single-sources the grammar (D1) — no sidecar.
3. Segment deadline is pump-side, 60s, expiry-to-Refused with self-healing late insert (D2) — no
   watchdog thread.
4. `BufferIndex` externals never union across domain (D3).
5. Table/String params are boundary by nature, not debt (D4).
6. `MAX_VIRTUAL_CHAIN=1` is correct — the 4^depth recompute cliff is real (D4).
7. Canonical bundled-preset views stay `&'static`; only fused artifacts move to Arc (D5).
8. The companion Sonnet sweep's items are out of this doc's scope and vice versa.

## 6. Deferred

- **Buffer fan-out regions** — trigger: census ≥3 refusals in shipped content, or a real user
  graph paying it (P3 measures; escalate with the number).
- **Resample-into-region** — trigger: census evidence of hot resample-sandwich chains.
- **Fusing the array producer into/across regions (BufferIndex v2)** — trigger: a converted
  BufferIndex atom whose producer chain shows up as the remaining dispatch cost in a real HUD graph.
- **Hand-authored marker validation at authoring time** (a `graph_tool` lint for user `@fusion:`
  fragments) — trigger: first user-authored fragment bug traced to a malformed marker.
- **Hot-toggle for kill switches** (restart-scoped today) — unchanged from the map; not this doc's
  scope.
