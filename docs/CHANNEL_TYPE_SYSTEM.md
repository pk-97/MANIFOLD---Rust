# Channel Type System — Foundational Design

<!-- index: The named-Channels array-wire type system: Channels<T> identity, port typing, and coercion rules. Consult before extending the graph type system. -->

**Status:** Phases 0–4 + 6 shipped, including the §14.9 explicit-marker preprocessor (commits `72fd83af` extractor + `dd6889e3` integration), the editor-snapshot Channels extension (`dd6889e3`), the runtime debug-name registry (`dd6889e3`), and the doc sweep across companion files (BUFFER_PORT_PLAN, NODE_CATALOG, ADDING_PRIMITIVES, DECOMPOSING_GENERATORS, PRIMITIVE_LIBRARY_DESIGN). Phase 5 (workspace test gate + canonical-fixture visual sanity check) is the last remaining phase; Peter did informal eyeball verification on the affected presets but the formal `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` pass + GPU frame-time baseline check still wants doing. This document is the source of truth for the named-channel type system migration. Agents executing any remaining phase should read this document end-to-end first.

**Established:** 2026-05-27. Sign-off pass on 2026-05-27 added a Phase 0 (end-to-end smoke test on one typed family before Phase 1 hardens the foundation) and baked in five revisions to the original design: sample count rides on the wire's runtime interface; pad fields use an explicit per-field marker instead of a `_pad*` name-prefix heuristic; one canonical macro form per typed family enforced by lint; the `well_known` registry + collision test are emitted from a single source list via macro; the Permissive allow-list is a `pub const` in the validator that the test enumerates against. §13 resolutions and the relevant sections (§3.1, §7.5, §8.2, §8.6, §9.5, §10, §11.4, §12.1) reflect these decisions. The pad-marker mechanism — originally walked-back to a `_pad*` heuristic during Phase 4a — landed as designed in Phase 4b.7 via the `// @channel_skip` preprocessor (§8.2, §14.9).

**Implementation log (2026-05-27 → 2026-05-28):**

- **Phase 0 shipped** — commit `c59427e4`. Throwaway smoke test on `EdgePair` validated the design end-to-end before Phase 1 hardened anything.
- **Phase 1 shipped** — commit `6a7a469c`. Core types (`ChannelName`, `ChannelElementType`, `ChannelSpec`, reshaped `ArrayType`, `MatchMode`, std430 calculators, `well_known_channels!` macro + registry, `channels_compatible` predicate, `GraphError::ChannelMismatch(Box<ChannelMismatchInfo>)`). Channel-name registry + collision test emit from a single source list per §7.5 resolution.
- **Phase 2 shipped** — commit `05463952`. `primitive!` macro extended with `Channels[name: Type, ...]` inline syntax and `Channels[permissive]` modifier. TT-muncher `__channels_specs!` handles mixed `well_known::*` ident and inline string literal names. Four smoke primitives + six tests exercise the syntax end-to-end through the validator.
- **Phase 3 shipped** — commit `e6357705`. `KnownItem` trait gains `const SPECS: &'static [ChannelSpec] = &[]` default; `ArrayType::of_known<T>()` folds `T::SPECS` into wire specs. Every typed family (`Particle`, `MeshVertex`, `Vec4Vertex`, `InstanceTransform`, `CurvePoint`, `EdgePair`, `Blob`) defines its `_SPECS` constant and wires it through `KnownItem`. §13(3) resolution applied (paired scalars at 4-byte align for CurvePoint and Vec4Vertex). Seven drift-assertion tests confirm `std430_stride(SPECS) == size_of::<Struct>()` for each family.
- **Phase 4a shipped** — commit `40af5d37`. `wgsl_compute`'s naga walk extracts ChannelSpec lists from `var<storage>` struct fields (§8.2). The existing `_pad*` name-prefix heuristic ships as v1; the explicit-marker preprocessor originally signed off is slotted as scheduled follow-up (rationale in §8.2). `port_types_compatible` gains an Anonymous-pair compatibility rule preserving the wgsl_compute → cast atom bridge during the transition.
- **Phase 4b.1 shipped** — commit `b57b4e87`. JSON surgery on three generator presets (`BlackHole`, `ComputeStrangeAttractor`, `ParticleText`) splicing out four `node.cast_as_*` nodes; wgsl_compute's atomic-accumulator path also updated to emit `[value: U32]` specs matching what `u32`'s `KnownItem` produces downstream. Visually verified on all three presets before commit.
- **Phase 4b.2 shipped** — commit `ab8f53ec`. Cast atom family deletion (6 atoms + 5 stub `Blob[N]` Pod types via `cast_array.rs` removal) and legacy `wgsl_compute_0in_1tex` / `_1tex_1tex` / `_2tex_1tex` variant deletion (3 files). Tests in `persistence.rs` and `json_graph_generator.rs` migrate to the generic `node.wgsl_compute`.
- **Phase 4b.3 shipped** — commit `6a1d15b8`. `pub struct Blob` + `BLOB_SPECS` + drift assertion removed from `mesh_common.rs`. `blob_detect_ffi` and `blob_overlay_render` each carry their own module-private `BlobRect` Pod struct; the port declarations switch to inline `Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32]`. The wire is the public contract.
- **Phase 4b.4 shipped** — commit `52b7f732`. `pub enum ItemKind`, `KnownItem::ITEM_KIND` const, `ArrayType::item_kind` field all deleted. `port_types_compatible` simplifies to "exact equality OR matching empty-specs Array sizes." Nine primitive test files drop their `assert_eq!(layout.item_kind, ItemKind::X)` assertions; the `every_conventional_array_port_declares_a_kind` invariant renames + tightens to require non-empty `specs` on every non-carve-out Array port.
- **Phase 4b.6 shipped** — commit `72fd83af`. `extract_channel_skip(source)` preprocessor lands as a pure transformation with 17 unit tests covering every edge case from the original design: marker variations, multi-line struct decls, mixed `//` / `/* */` comments, per-struct isolation, attribute prefixes, stacked-markers idempotence, same-line markers rejected, orphan markers warn-only. No integration yet — function lands isolated so the integration commit reads cleanly.
- **Phase 4b.7 + Phase 6.1 shipped** — commit `dd6889e3`. Skip set threads through `introspect` → `element_to_array_type` → `struct_members_to_specs`; the storage-struct `_pad[0-9]*` name-prefix heuristic retires entirely (no current core-dev shader exercised it — every `_pad*` field in the five wgsl_compute presets lives in *uniform* structs, which keep their separate `parse_uniform` `_pad*` filter). Three integration tests exercise the marker end-to-end + the post-heuristic behaviour + per-struct isolation through naga. **Bundled with the runtime debug-name registry** (`channel_names::register_runtime_name` + a `OnceLock<RwLock<AHashMap>>` overflow map; `wgsl_compute::struct_members_to_specs` registers each WGSL field name) so editor tooltips and validator errors recover "position" / "_pad0" / etc. instead of the raw hex hash. **And the snapshot extension:** `PortKindSnapshot::Array` reshapes to a struct variant carrying `channels: Vec<ChannelSnapshot>`, `match_mode`, `item_size`, `item_align`; `ChannelSnapshot { name: String, ty: String }` is the editor-facing shape. `graph_canvas::PortView::from_kind` migrates to borrow `&PortKindSnapshot`.
- **Phase 6.2 shipped** — commit `<this>`. Companion doc sweep: NODE_CATALOG.md adds §1.1 with the `well_known` overview + a one-line note that `Array(T)` macro syntax expands to a Channels-typed wire; ADDING_PRIMITIVES.md's macro reference enumerates all four Array/Channels port-type forms; BUFFER_PORT_PLAN.md, DECOMPOSING_GENERATORS.md, PRIMITIVE_LIBRARY_DESIGN.md each get a top-of-doc note declaring their Array-port type-system sections superseded by this doc (topology references stay accurate; just read `Array<T>` as `Channels<T>`). Memory entry `feedback_named_channels_canonical.md` written for future sessions.

**Phase 5 remains — workspace test gate + canonical-fixture visual sanity check.** Peter did informal eyeball verification on the three migration-affected presets during Phase 4b.1 sign-off; the formal `cargo test --workspace` + workspace clippy + GPU frame-time baseline check on `Liveschool Live Show V6 LEDS.manifold` still wants doing. Saved for whenever the next end-to-end testing session happens.

**§17 (Texture2D channel signatures) — Phase 17.A shipped 2026-05-28.** Extends the Channel type system to decorate Texture2D ports with a four-slot RGBA channel signature. Same well_known registry, same FNV-1a-64 const-hash interning, same compile-time decidable match. Untyped Texture2D stays the back-compat default. Validator surfaces a structured `TextureChannelMismatch` carrying the first diverging slot index. Macro: `Texture2D[R: Name, G: Name, B: Name, A: Name]`. Migrated `node.optical_flow_estimate` to declare the Watercolor `(R: FLOW_X, G: CONFIDENCE, B: FLOW_Y, A: VALID)` convention; downstream consumer migrations (the bug-fix that motivated this) live in a follow-up commit. See §17 for the full surface.

**Scheduled follow-up:** ~~Build the explicit-marker (`// @channel_skip`) preprocessor for `wgsl_compute` pad-field handling.~~ Shipped 2026-05-28 in commits 4b.6 + dd6889e3. See §8.2 and the §14.9 historical note for the final form.

**Acceptance criteria after Phase 6:** 862/862 manifold-renderer lib tests passing; clippy clean; `check-presets` reports 49/49 OK; three affected presets (BlackHole, ComputeStrangeAttractor, ParticleText) visually verified; manifold-app binary builds; companion docs reference CHANNEL_TYPE_SYSTEM.md as the type-system source of truth.

**Companion docs:**

- [BUFFER_PORT_PLAN.md](BUFFER_PORT_PLAN.md) — the prior plan that established `Array<T>` wires as the fourth port-type family. This document supersedes its Array-port type system; the producer/consumer architecture and the buffer-pool / `MTLBuffer` infrastructure it established remain unchanged.
- [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) — primitive authoring model, port-shadow conventions, state model.
- [NODE_GRAPH_SYSTEM.md](NODE_GRAPH_SYSTEM.md) — graph runtime, executor, chain build.
- [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) — `primitive!` macro, parity test pattern.
- [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) — WGSL std430 layout rules, uniform alignment.
- [NODE_CATALOG.md](NODE_CATALOG.md) — registry of currently shipped primitives. Updates during migration.
- [PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md](PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md) — the active 2nd-pass decomposition. Several future decompositions become substantially easier after this migration lands.
- [GRAPH_COMPILER.md](GRAPH_COMPILER.md) — the planned WGSL fusion compiler + `for_each_n` per-pixel loops initiative. The Channel type system is designed to compose with the future fusion compiler; see §16 for the design constraints this imposes on the channel system.

---

## 0. Quick orientation

If you're picking this up cold:

- **What's being built:** a named-channel type system to replace the existing typed-array-byte-bucket model on graph wires. Data flowing between nodes carries a list of named typed channels — `Channels[x: F32, y: F32, width: F32, height: F32]` — instead of an opaque byte-layout tag like `Array<Blob>`.
- **Why:** unifies "typed arrays" and "untyped Array<f32>" into one self-describing model. Unlocks AI graph authoring, variable-channel data (audio bins, sensor streams), user-introduced wire shapes without Rust code, better editor visibility, catching naming/layout mismatches at graph compile time.
- **Scope:** 7 phases (Phase 0 → Phase 6), ~7½ chats of work. Migrates every typed array (Particle, MeshVertex, CurvePoint, etc.) and the `wgsl_compute` escape hatch onto Channels. Deletes `ItemKind` enum + `Array<Anonymous>` + the `cast_as_*` atom family.
- **Performance impact:** zero runtime cost. Same byte layouts, same buffer allocations, same WGSL shaders. The migration is type-system-only on the data path.
- **Risk surface:** primarily *naming consistency* across migrated primitives. Mitigated by a code-level `well_known` channel-name registry that every migrated primitive references.
- **Where to start:** Phase 0 in §10 — a ~½-chat end-to-end smoke test on one typed family (EdgePair recommended) before Phase 1 hardens the type infrastructure. Read §3-§6 first for the model; §11-§12 give worked examples; §13 records the closed sign-off decisions; §16 covers fusion-compatibility constraints any future amendment must respect.

---

## 1. Why this exists

### 1.1 The problem with the current model

Today's graph wires carry a `PortType` whose `Array(ArrayType)` variant identifies a `(item_size, item_align, item_kind)` triple where `ItemKind` is a small enum (`Particle`, `MeshVertex`, `CurvePoint`, `EdgePair`, `InstanceTransform`, `Vec4Vertex`, `Blob`, `U32Slot`, `F32Slot`, `Vec2Slot`, `Anonymous`).

This works fine for the handful of typed shapes the graph runtime knows about. It breaks down in four places:

**(1) New data shapes require Rust code.** Adding a new wire shape — say "face detection regions with confidence" — means writing a new Rust struct, defining a WGSL counterpart, adding an `ItemKind` variant, and recompiling. Users can't introduce data shapes from the graph editor. AI agents authoring graphs can't either.

**(2) Generic flows lose all type discipline.** Anything that doesn't fit a named type (audio FFT bins, MIDI velocity arrays, OSC sensor streams, multi-band envelope outputs, depth-DNN scalar streams) flows as `Array<f32>` with stride-N conventions documented in `composition_notes` comments. The validator sees `Array<f32>` and nods through. A typo in the convention silently corrupts data downstream. There is no compile-time check that two `Array<f32>` consumers expect the same stride.

**(3) One-off types proliferate.** `Blob` exists as a whole named type just to label `(x, y, width, height)` rectangles. It's structurally identical to a face box, hand region, sprite rect, viewport. Naming it "Blob" attaches the type to one specific source (the OpenCV detector) when the data itself is generic. Every "small typed shape" in the catalog is this same trap.

**(4) Math across channels is awkward.** To compute "x + width / 2" on an `Array<Blob>`, the standard pattern is to unpack into separate `Array<f32>`s per channel, run scalar math, repack. The pattern works but the graph blooms with `pack`/`unpack` plumbing and the channel meaning lives in stride conventions, not in the graph itself.

**(5) The graph editor can't read the data flowing through it.** A wire labelled `Array<Blob>` tells a graph reader nothing about what's actually flowing. To understand `Array<f32>` they'd have to trace back to the producer's `composition_notes` doc string. AI agents authoring graphs face the same gap, amplified — they can't introspect a wire and know its semantic.

### 1.2 What changes after this migration

Wires self-describe. A wire labelled `Channels[x: F32, y: F32, width: F32, height: F32]` says exactly what's flowing on it. The graph editor renders the channel names on the wire / port. AI agents reading a graph see the data shape directly. Users authoring presets see what each node produces and consumes without reading Rust code.

The validator enforces channel name and type matching. Naming inconsistency between producer and consumer fails at graph compile time with a clear error, not at runtime as silent visual corruption.

Generic flows (audio bins, sensor streams) get first-class type discipline. The "Channels of named typed values per sample" model is one unified vocabulary across every wire in the graph, not two parallel models (typed arrays + flat float buffers).

New data shapes are graph-authoring concerns, not Rust changes. A user wanting "detections with confidence and dominant-color" wires a graph that emits `Channels[x, y, w, h, confidence, r, g, b]`. No new Rust struct. No recompile.

### 1.3 What does NOT change

- Same runtime cost. Same MTLBuffer allocations. Same buffer pool. Same GPU shaders.
- Same byte layouts on every wire. The Rust `#[repr(C)]` structs (Particle, MeshVertex, etc.) stay in place as the in-memory representation. The WGSL struct definitions stay in place. The bytes flowing through the wire are unchanged.
- Same primitive code on most primitives. A primitive that reads `Array<Particle>` today writes through the `Particle` struct's field accesses; same code after migration, just the wire type label changes.
- Same preset JSON files. Wires reference ports by name only; no port-type info is serialized in saved presets. Existing presets continue working unchanged, *assuming* naming conventions are kept consistent through the migration.

---

## 2. Naming and vocabulary

The thing flowing on a wire is a **`Channels` array**: a list of *samples*, where each sample carries a set of named typed *channels*.

- **Sample** — one item in the array. A particle is a sample. A detection is a sample. An audio FFT frame's bin array is one sample carrying many bin channels (or N samples each with one bin channel, depending on framing — see §4.5).
- **Channel** — one named typed slot on every sample. A particle has channels named `position`, `velocity`, `life`, `age`, `color`. A detection has channels named `x`, `y`, `width`, `height`.
- **Channel element type** — the type of values in one channel (F32, I32, U32, Vec2F, Vec3F, Vec4F).
- **Channel spec** — the `(name, element type)` pair describing one channel.
- **Channels signature** — the ordered list of channel specs describing a Channels array. The wire's type identity.

The name *Channel* was chosen for MANIFOLD because: (a) the graph is fundamentally a signal-flow surface, and data on wires is structured signal; (b) MANIFOLD's audience (live performers, VJs, signal-processing folks) is channel-native through audio, MIDI, OSC, and pixel-channel terminology; (c) the metaphor generalizes cleanly to non-time-series data (per-frame detection lists are one-sample-per-frame streams; each sample carries named channels); (d) error messages read naturally ("missing channel `confidence`"). See [conversation history 2026-05-27 — Channel naming decision] for the design discussion.

---

## 3. The conceptual model

### 3.1 Data shape

Every Channels array carries N samples × M channels, where M is fixed per wire type (set at graph compile time) and N is the runtime sample count (bounded by `max_capacity`). The active sample count is exposed *through the wire's runtime interface* — a consumer that declares a Channels port receives both the buffer handle and the sample count cohesively, without separately wiring an `active_count` scalar port. The underlying runtime mechanism is unchanged from BUFFER_PORT_PLAN.md §"Active-count slider": the producer sets the count per dispatch and the runtime passes it via uniform. The wire-level API just bundles the uniform with the buffer so consumers (and AI agents reading the graph) see one handle, not two parallel signals. This preserves fusion-compatibility (§16.6 — the fusion compiler still emits a shader looping over a runtime uniform).

Channel layout is **interleaved sample-major**:

```
[sample_0_ch_0, sample_0_ch_1, ..., sample_0_ch_M-1,
 sample_1_ch_0, sample_1_ch_1, ..., sample_1_ch_M-1,
 ...
 sample_N-1_ch_0, ..., sample_N-1_ch_M-1]
```

Byte offset of channel `i` in sample `j` is `(j × sample_stride_bytes + channel_offset[i])`. The `sample_stride_bytes` and per-channel `channel_offset[i]` are derived from the Channels signature via WGSL std430 layout rules (see §4.4). They are compile-time constants per wire.

One `MTLBuffer` per Channels wire. Same as today's `Array<T>` allocation. Buffer pool / allocation lifecycle inherited from the existing typed-array infrastructure unchanged.

### 3.2 The wire type carries names

The wire's `PortType::Array` carries the full Channels signature, including channel names. The validator uses the signature to check producer→consumer compatibility (§5). The graph editor uses it to render channel names on ports and wires.

Names are **interned at compile time** as `ChannelName` values (u64 hash of the name string — see §4.2). Lookup, comparison, and storage are cheap; the original string is retained in a parallel debug registry for display and error messages.

### 3.3 The byte layout matches what the existing typed structs use

Every existing typed array (Particle, MeshVertex, etc.) is `#[repr(C)]` with explicit pad fields that match WGSL std430 layout (verified — see §6 migration map for per-type proofs). Replacing these with Channels signatures of equivalent specs produces byte-identical layouts: same offsets per field, same total stride, same alignment. The existing GPU shaders see the same bytes; the existing Rust producer code writes through the same struct field accesses.

The migration is therefore byte-preserving on the data path. The change is in the *type tag* the validator sees and the *names* the editor and AI agents can read.

### 3.4 Self-describing means: agents and users can introspect

A wire carrying `Channels[position: Vec3F, velocity: Vec3F, life: F32, age: F32, color: Vec4F]` answers four questions any reader (human or AI) might ask:

- How many channels are there? (5)
- What are they named? (position, velocity, life, age, color)
- What types are they? (vec3, vec3, scalar, scalar, vec4)
- How do I read channel `velocity` of sample 100? (offset = 100 × 64 + 16 = 6416; size = 12 bytes; type vec3<f32>)

The current `Array<Particle>` only answers "how many bytes per item": 64. Everything else is documentation.

### 3.5 Designed to compose with the fusion compiler

The Channel type system is designed forward-compatible with the planned WGSL fusion compiler (see [GRAPH_COMPILER.md](GRAPH_COMPILER.md)). Decisions in §4 (compile-time-known specs, deterministic std430 layout, const-foldable channel names, closed element-type set, simple match modes) are all in service of letting a future compiler pass walk a sub-graph of Channels-typed atoms and emit one fused shader without runtime introspection. **§16 enumerates the specific constraints; future amendments to §4 must check against §16 before changing the type system shape.**

---

## 4. The type system contract

### 4.1 Concrete types

All types live in `crates/manifold-renderer/src/node_graph/ports.rs` alongside the existing port types.

```rust
/// A channel's name. Interned at compile time via const FNV-1a hash of the
/// name string. Comparison is u64 equality; the original string is retained
/// in a runtime debug registry for display and error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelName(u64);

/// The closed set of element types a channel can carry. Closed by design —
/// adding a new variant requires updating the std430 layout calculator,
/// the validator, the macro, and the test matrix. Each new type is a
/// deliberate decision, not a generic-over-T extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelElementType {
    F32,
    I32,
    U32,
    Vec2F,
    Vec3F,
    Vec4F,
}

/// One named typed slot on a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelSpec {
    pub name: ChannelName,
    pub ty:   ChannelElementType,
}

/// Wire-type identity for a Channels array. Replaces today's
/// `ArrayType { item_size, item_align, item_kind }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayType {
    /// Channel layout. Identifies the wire's semantic and byte layout.
    pub specs: &'static [ChannelSpec],

    /// Bytes per sample, std430-aligned. Derived from `specs` at compile
    /// time. Stored explicitly to avoid recomputation on every validator
    /// check.
    pub item_size: u32,

    /// Sample alignment, std430. Derived from `specs`. Equals the max
    /// alignment of any channel's element type, rounded up to 4 (the
    /// minimum stride alignment for storage buffers).
    pub item_align: u32,

    /// Wire-matching policy this wire's port declared. Default: Exact.
    /// Permissive is the opt-in for generic transform operators.
    pub match_mode: MatchMode,
}

/// Wire-validator matching policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchMode {
    /// Producer and consumer Channels signatures must be identical
    /// (same specs in same order). Default for all consumers except
    /// the generic transform operators.
    Exact,

    /// Consumer accepts any Channels signature. Used by transform
    /// operators (rename, reorder, concat, channel_math, select_*)
    /// that act structurally on whatever flows in.
    Permissive,
}
```

`PortType::Array(ArrayType)` survives — it's the same wrapper enum variant. What's deleted is `ItemKind` (the typed-or-Anonymous tag enum). `ArrayType` is reshaped from carrying a kind tag to carrying a channel-spec slice.

### 4.2 `ChannelName` — interned at compile time via const FNV

A `ChannelName` is a 64-bit FNV-1a hash of the channel's name string. Hashing happens at compile time via a `const fn`:

```rust
impl ChannelName {
    pub const fn from_str(s: &'static str) -> Self {
        Self(const_fnv1a_64(s.as_bytes()))
    }
}

const fn const_fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = 14695981039346656037u64; // FNV-1a 64-bit offset basis
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(1099511628211); // FNV prime
        i += 1;
    }
    hash
}
```

Compile-time hashing means port type declarations stay `const`. The `primitive!` macro can declare `Channels[x: F32, y: F32]` in a `static` port array; no runtime allocation, no lazy initialization.

Storage cost: 8 bytes per channel name. Comparison: u64 equality. Hash for AHashMap lookup: identity.

**Collision risk:** 64-bit hashing across the expected name set (a few hundred well-known names plus a handful of user-introduced names per session, total bound under 1000) gives collision probability of approximately 2.7e-14. Treat as zero. If a collision ever appears (it won't), the failure mode is a validator accepting an incompatible wire silently; a CI test (§9.5) walks every distinct channel name in the registry and asserts pairwise distinct hashes.

**Original name retention.** Hash-only would make error messages unreadable. A parallel runtime registry maps `ChannelName → &'static str` so debug formatting, error messages, and editor display can resolve hashes back to strings. The registry is populated as channel names are constructed; entries are never removed.

### 4.3 `ChannelElementType` — closed set, six variants

The v1 element type set is **F32, I32, U32, Vec2F, Vec3F, Vec4F**. Closed by design.

- **F32** — single-precision float. Most channels. Particle life, audio bin magnitude, MIDI velocity, any scalar field.
- **I32** — signed 32-bit integer. Future use; no current consumer.
- **U32** — unsigned 32-bit integer. Edge indices, scatter accumulator slots, particle hash seeds.
- **Vec2F** — two-vector. UVs, 2D positions, 2D velocities.
- **Vec3F** — three-vector. 3D positions, normals, RGB colors (alternative to four-channel R/G/B).
- **Vec4F** — four-vector. RGBA colors, 4D positions, transform components.

Each element type has fixed WGSL std430 size and alignment:

| Type | Size (bytes) | Alignment (bytes) |
|---|---|---|
| F32 | 4 | 4 |
| I32 | 4 | 4 |
| U32 | 4 | 4 |
| Vec2F | 8 | 8 |
| Vec3F | 12 | 16 |
| Vec4F | 16 | 16 |

The std430 rule for Vec3F (12-byte payload, 16-byte alignment) is the same trap that produced `_pad0` fields in today's `Particle` / `MeshVertex` structs. The Channels layout calculator (§4.4) inserts the same padding automatically; the resulting byte layout matches the hand-coded structs exactly.

**Out of v1:**
- Bool / packed-bool channels. No current typed array carries bools; defer.
- Mat3F / Mat4F channels. The existing `InstanceTransform` is two Vec4F slots, not a Mat4F; no current typed array carries matrices. Add when a real consumer needs them.
- Half-precision (F16). Not used anywhere in the catalog. Add when needed.

**Why closed:** every new element type requires updating the std430 layout calculator, the validator's per-type compatibility check, the macro's type-keyword recognizer, and the GPU buffer binding path. Closing the set forces those updates to be deliberate; opening it (e.g., to "any Pod type") would push complexity into every operator.

### 4.4 Std430 layout, derived

Given a `&[ChannelSpec]`, the layout calculator produces `(per_channel_offsets, sample_stride_bytes, sample_alignment)` following WGSL std430 rules:

```text
algorithm std430_layout(specs):
    offset = 0
    max_align = 4   # minimum stride alignment for storage buffers
    offsets = []
    for spec in specs:
        align = spec.ty.alignment()
        size  = spec.ty.size()
        offset = round_up(offset, align)
        offsets.push(offset)
        offset += size
        max_align = max(max_align, align)
    sample_stride = round_up(offset, max_align)
    return (offsets, sample_stride, max_align)
```

**Implementation note for Phase 1:** the calculator runs at graph compile time, not in `const` context. Per-type `_SPECS` constants get their `item_size` / `item_align` cached via either (a) lazy `OnceLock<ArrayType>` initialization (preferred — keeps the calculator regular Rust), or (b) a `const fn` calculator if stable Rust permits the required `while`/`for` constructs at the time of writing. Layout drift is caught by a Phase 1 test (§9.4) that round-trips every `_SPECS` constant through the calculator and asserts the stride matches the existing `#[repr(C)]` struct's `size_of`. The runtime-vs-const-fn choice is an implementation detail; the test is the real safety net.

**Why std430 specifically:** the bytes flowing through the wire end up bound as storage buffers in WGSL compute kernels and storage / vertex inputs in render pipelines. WGSL std430 is the layout rule the shader compiler uses to read those bytes back. Matching std430 at the Channels layer means the producer's bytes and the shader's interpretation agree by construction.

### 4.5 What "sample" means in context

The "sample" axis is the variable-length axis on the wire. Sample count is bounded by `max_capacity` (allocated at chain build) and runtime-set via the producer's `active_count` mechanism (BUFFER_PORT_PLAN.md §"Active-count slider").

Different data shapes use the sample axis differently:

| Data | Sample axis | Channel axis |
|---|---|---|
| Detection regions per frame | One sample per detection | x, y, width, height per detection |
| Particles | One sample per particle | position, velocity, life, age, color per particle |
| Mesh vertices | One sample per vertex | position, normal, uv per vertex |
| Curve points | One sample per point | x, y per point |
| Audio FFT bands (one frame) | One sample per bin | magnitude per bin |
| MIDI velocity (one frame, 128 notes) | One sample per note | velocity per note |
| Per-channel scalar control (LFO sweep) | One sample per evaluation step | value per step |

The wire carries one frame's worth of samples. Frame-to-frame state lives in `node.array_feedback` (renamed in this migration — see §6) and the wider StateStore system.

---

## 5. Validator semantics

### 5.1 Match modes

Each consumer port declares its match mode. Two modes are valid:

- **`MatchMode::Exact`** — default for every primitive's port unless explicitly opted into Permissive. The producer's Channels signature must equal the consumer's signature in every respect: same number of channels, same order, same channel names, same element types.
- **`MatchMode::Permissive`** — opt-in for *generic transform operators only*. The consumer accepts any Channels signature on this port; the operator acts structurally on whatever channels flow in. Operators using Permissive: `node.rename_channel`, `node.rename_channels`, `node.reorder_channels`, `node.concat_channels_by_channel`, `node.concat_channels_by_sample`, `node.channel_math` (its target-channel-name param drives validation at compile time; the input port itself is Permissive).

No third "required-set" mode (where the producer must have at least the channels the consumer wants, but may have extras). Subset selection happens through an explicit `node.select_channels` atom. The graph stays readable: anywhere a wider Channels array narrows into a consumer expecting a subset, the narrowing is visible in the graph topology, not hidden in the validator.

### 5.2 The compatibility predicate

Replaces the current `port_types_compatible` function at [validation.rs:264](crates/manifold-renderer/src/node_graph/validation.rs#L264):

```text
fn port_types_compatible(from: PortType, to: PortType) -> bool:
    if from == to:
        return true
    match (from, to):
        # Array compatibility — the migration's main case.
        case (Array(a), Array(b)):
            return channels_compatible(a, b)
        # Anonymous Array path is GONE. The old asymmetric coercion
        # (typed → Anonymous accepted) deletes with ItemKind.
        # Cast atoms are also deleted; nothing produces or consumes
        # ItemKind::Anonymous anymore.
        # Other PortType variants (Texture2D, Scalar, etc.) match
        # by exact equality unchanged.
        _:
            return false

fn channels_compatible(producer: ArrayType, consumer: ArrayType) -> bool:
    match consumer.match_mode:
        case Exact:
            # Strict identity on the entire signature.
            return producer.specs == consumer.specs
        case Permissive:
            # Accept anything that's also Channels.
            return true
```

Sample byte sizes (`item_size`) and alignments (`item_align`) are derived from `specs`. Two `ArrayType` values with the same `specs` always have the same `item_size` and `item_align`. The validator does not need to compare them separately.

### 5.3 Error messages

The validator emits structured errors that surface the channel mismatch. Replace the current `PortTypeMismatch` variant with a richer `ChannelMismatch`:

```text
GraphError::ChannelMismatch {
    from_node, from_port, to_node, to_port,
    producer_specs: &'static [ChannelSpec],
    consumer_specs: &'static [ChannelSpec],
    reason: ChannelMismatchReason,
}

enum ChannelMismatchReason {
    /// Channel count differs.
    DifferentCount { producer_count: u32, consumer_count: u32 },
    /// Channel names differ at a specific index.
    NameMismatch { index: u32, producer_name: &'static str, consumer_name: &'static str },
    /// Channel element types differ at a specific index.
    TypeMismatch { index: u32, producer_type: ChannelElementType, consumer_type: ChannelElementType },
}
```

The original channel name strings (resolved from the debug registry, see §4.2) appear in the error message. A user sees:

```
ChannelMismatch in wire blob_detect_ffi.blobs → renderer.in:
  Producer: Channels[x: F32, y: F32, w: F32, h: F32]
  Consumer: Channels[x: F32, y: F32, width: F32, height: F32]
  Mismatch at index 2: producer channel 'w' != consumer channel 'width'

  Fix: rename one of the channels to match (use `node.rename_channel`),
  or update the producer / consumer to use the canonical name from
  `well_known::WIDTH`.
```

The error message points at the `well_known` registry as the resolution path. This is intentional: the registry IS the convention enforcement (§7).

### 5.4 Port matching at the graph level

The connect-time validator (`validate_connection`) and the compile-time sweep (`validate`) both use the new compatibility predicate. No other behavioural change at the validator level. State-capture port semantics, conditional-input requirements, format contracts (texture-format compatibility), and cycle detection are unchanged.

---

## 6. Migration map — every typed array → its Channels signature

Each existing typed array has a one-to-one byte-equivalent Channels signature. The migration is mechanical per type.

The pattern for each type:

1. Define a `pub const X_SPECS: &'static [ChannelSpec]` in the same module as the existing `#[repr(C)]` struct.
2. Replace `impl KnownItem for X { const ITEM_KIND: ItemKind = ItemKind::X; }` with the macro's new mechanism for emitting `ArrayType { specs: X_SPECS, ... }` when a primitive declares `Channels[...spec_pattern...]`.
3. The Rust `#[repr(C)]` struct STAYS in its current form — it remains the in-memory representation, the GPU shader's struct mirror, and the `bytemuck::Pod` source. The struct is no longer the wire-type identity; the specs constant is.
4. A drift-assertion test (§9.4) verifies `std430_stride(X_SPECS) == size_of::<X>()`. Caught at test time.

Per-type signatures and byte-layout proofs:

### 6.1 `Particle` → `PARTICLE_SPECS`

Existing struct (compute_common.rs:11):
```rust
#[repr(C)]
pub struct Particle {
    pub position: [f32; 3],   // offset 0,  size 12
    pub _pad0:    f32,         // offset 12, size 4  (vec3 → vec3 alignment)
    pub velocity: [f32; 3],   // offset 16, size 12
    pub life:     f32,         // offset 28, size 4
    pub age:      f32,         // offset 32, size 4
    pub _pad1:    [f32; 3],   // offset 36, size 12 (f32 → vec4 alignment)
    pub color:    [f32; 4],   // offset 48, size 16
}
// total: 64 bytes
```

Channels signature:
```rust
pub const PARTICLE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::VELOCITY, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::LIFE,     ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::AGE,      ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::COLOR,    ty: ChannelElementType::Vec4F },
];
```

Std430 layout:
- position at 0 (size 12, align 16 → next at 16)
- velocity at 16 (size 12)
- life at 28 (align 4 → starts at 28)
- age at 32 (size 4)
- color at 48 (align 16 → from 36 pad to 48)
- sample_stride = round_up(64, 16) = 64

Match with struct: position 0, velocity 16, life 28, age 32, color 48, total 64. ✓

### 6.2 `MeshVertex` → `MESH_VERTEX_SPECS`

Existing struct (mesh_common.rs:33), 48 bytes:
```rust
pub struct MeshVertex {
    pub position: [f32; 3],
    pub _pad0:    f32,
    pub normal:   [f32; 3],
    pub _pad1:    f32,
    pub uv:       [f32; 2],
    pub _pad2:    [f32; 2],
}
```

Channels signature:
```rust
pub const MESH_VERTEX_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::NORMAL,   ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::UV,       ty: ChannelElementType::Vec2F },
];
```

Std430 layout:
- position at 0 (align 16, size 12, pad to 16)
- normal at 16 (align 16, size 12, pad to 32 since next is align 8)
- uv at 32 (align 8, size 8)
- sample_stride = round_up(40, 16) = 48 ✓

### 6.3 `CurvePoint` → `CURVE_POINT_SPECS`

Existing struct (mesh_common.rs:104), 8 bytes, `xy: [f32; 2]`.

Channels signature — TWO valid options, decided by canonical-naming convention:

**Option A (two scalar channels):**
```rust
pub const CURVE_POINT_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::X, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::Y, ty: ChannelElementType::F32 },
];
```
Std430 layout: x at 0 (align 4), y at 4 (align 4). Stride 8, align 4. Match with struct. ✓

**Option B (one vec2 channel):**
```rust
pub const CURVE_POINT_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::XY, ty: ChannelElementType::Vec2F },
];
```
Std430 layout: xy at 0 (align 8, size 8). Stride 8, align 8. **NOT** byte-equivalent — alignment differs (8 vs 4).

**Resolution:** use **Option A** (two scalar channels at 4-byte alignment) to preserve current byte parity with `[f32; 2]`-style consumers. The existing `Array<CurvePoint>` is 4-byte aligned; Option B would break that. Consumers that want Vec2 semantics compose via downstream `pack_vec2_from_scalars` or treat the two scalars as Vec2 in shader code.

Documented as a convention in §7.

### 6.4 `Vec4Vertex` → `VEC4_VERTEX_SPECS`

Existing struct (mesh_common.rs:53), 16 bytes, `position: [f32; 4]`.

Channels signature options:

**Option A (one vec4):**
```rust
pub const VEC4_VERTEX_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec4F },
];
```
Std430: 16 bytes, align 16. **NOT** byte-equivalent to `[f32; 4]` which is align 4.

**Option B (four scalars):**
```rust
pub const VEC4_VERTEX_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::X, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::Y, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::Z, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::W, ty: ChannelElementType::F32 },
];
```
Std430: 16 bytes, align 4. Match with `[f32; 4]`. ✓

**Resolution:** Option B. Same pattern as CurvePoint: prefer scalar decomposition when the existing layout is scalar-aligned.

### 6.5 `EdgePair` → `EDGE_PAIR_SPECS`

Existing struct (mesh_common.rs:127), 8 bytes, `a: u32, b: u32`.

```rust
pub const EDGE_PAIR_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::A_INDEX, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::B_INDEX, ty: ChannelElementType::U32 },
];
```

Std430: 8 bytes, align 4. Match. ✓

The `SENTINEL` constant (`a == u32::MAX`) for unused slots survives as an associated constant on the Rust struct, which is still defined for in-memory use and for `bytemuck::Pod` source of the buffer.

### 6.6 `InstanceTransform` → `INSTANCE_TRANSFORM_SPECS`

Existing struct (mesh_common.rs:72), 32 bytes, two vec4 slots.

```rust
pub const INSTANCE_TRANSFORM_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POS_SCALE, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ROT,       ty: ChannelElementType::Vec4F },
];
```

Std430: 32 bytes, align 16. The struct's actual alignment (via Rust's `repr(C)` rules for `[f32; 4]`) is 4 bytes. **NOT directly byte-equivalent on alignment.**

**Resolution:** the struct's runtime alignment is determined by its largest field's alignment, which for `[f32; 4]` is 4. When the Channels migration declares `Vec4F` channels, the std430 alignment of 16 takes over at the wire level. Consumers binding the buffer in shaders use std430 layout (vec4 alignment 16), which matches what the existing shaders already do. The `align_of::<InstanceTransform>() == 4` is a Rust-side detail that doesn't affect GPU binding.

The actual byte stride matches (32 bytes). The alignment difference is consequence-free as long as `MTLBuffer` allocation honors the larger alignment, which it does for compute / storage buffer use. Drift assertion checks `size_of` (32 == 32) but not `align_of`; this is intentional.

### 6.7 `Blob` → `BLOB_SPECS` and the rename

`Blob` is the only typed array whose name attaches to a specific source (the OpenCV detector). Migration drops the `Blob`-specific naming entirely. The wire type becomes `Channels[x, y, width, height]` — generic for any rectangle stream (face boxes, hand regions, sprite rects, etc.).

```rust
// Defined alongside the migrated `blob_detect_ffi`; no need for a global
// const since the only consumers (blob_detect_ffi output, blob_overlay_render
// input) declare the signature inline through the macro.
//
// Equivalent representation:
//   Channels[
//     ChannelSpec { name: well_known::X,      ty: ChannelElementType::F32 },
//     ChannelSpec { name: well_known::Y,      ty: ChannelElementType::F32 },
//     ChannelSpec { name: well_known::WIDTH,  ty: ChannelElementType::F32 },
//     ChannelSpec { name: well_known::HEIGHT, ty: ChannelElementType::F32 },
//   ]
```

Std430: 16 bytes, align 4. Match with existing `Blob` struct (16 bytes, four f32 fields). ✓

The `Blob` Rust struct deletes alongside `ItemKind::Blob`. `blob_detect_ffi` writes the buffer through any equivalent struct (a literal `[f32; 4]` per item, or a transient `BlobView { x, y, width, height }` defined locally to the primitive). The wire type carries the canonical names; the Rust producer's struct is implementation detail.

### 6.8 `U32Slot`, `F32Slot`, `Vec2Slot` → single-channel Channels

These are the "fragments of larger compositions" types — scatter accumulators, scalar control arrays, packed vec2 sets.

```rust
// Array<u32> consumers → Channels[value: U32]
// Array<f32> consumers → Channels[value: F32]
// Array<[f32; 2]> consumers → Channels[x: F32, y: F32] at 4-byte align (same convention as CurvePoint)
```

These do NOT get global `_SPECS` constants. The macro emits the inline signature per port declaration. Consumers that today wire generic `Array<f32>` (array_math, generate_range, lfo output, smoothing input, etc.) declare specific channel names per port — `value`, `t`, `gain`, `freq`, whatever is semantically correct for that port. **This is where naming consistency matters most** (§7).

### 6.9 The deletion list

After migration completes (end of Phase 4):

- `pub enum ItemKind` — deleted entirely.
- `pub trait KnownItem` and its `ITEM_KIND` constant — deleted. The trait still exists if any helper code wants it, but the migration removes its only purpose.
- `ItemKind::Anonymous` variant — deleted. `wgsl_compute` migrates to declared signatures (§8).
- The `Array(Anonymous)` macro path — deleted.
- The asymmetric typed→Anonymous coercion in `port_types_compatible` — deleted.
- The cast atom family (`cast_as_particle`, `cast_as_u32`, `cast_as_mesh_vertex`, `cast_as_curve_point`, `cast_as_edge_pair`, `cast_as_instance_transform`) — deleted. Their job was bridging Anonymous → typed at the wgsl_compute boundary; Anonymous is gone.
- The "stub Pod types" used by cast atoms (`Blob4`, `Blob8`, `Blob32`, `Blob48`, `Blob64`) in `cast_array.rs` — deleted.
- `pub struct Blob` — deleted (the only Rust struct that gets fully removed; the others stay).

---

## 7. The `well_known` channel-name registry

### 7.1 Why it exists

The validator is strict (Exact match by default). A producer emitting `Channels[pos: Vec3F, vel: Vec3F]` doesn't connect to a consumer expecting `Channels[position: Vec3F, velocity: Vec3F]` even though both sides describe the same data. Naming consistency between primitives is structural — get it wrong and presets break.

Two options to enforce consistency:

- **(a) A prose convention doc** describing canonical names. Authors are expected to read it before naming channels.
- **(b) A code-level registry** of canonical name constants. Authors use the registry; non-registry names require explicit inline string literals.

Option (a) drifts. New primitive authors don't read docs. AI agents authoring graphs definitely don't. Option (b) makes deviation visible — a `Channels[pos: Vec3F]` declaration with `"pos"` as an inline string literal stands out next to `Channels[well_known::POSITION: Vec3F]`.

The registry is the enforcement mechanism. The doc describes the registry.

### 7.2 The registry shape

`crates/manifold-renderer/src/node_graph/channel_names.rs` (new file):

```rust
//! Canonical channel-name registry. Every primitive that produces or
//! consumes a channel with a meaning shared across the catalog uses
//! the constant here. Primitives that need a name not in the registry
//! either (a) add it to the registry (if reusable), or (b) declare it
//! as an inline string literal (if genuinely local).
//!
//! Hard rule: a primitive declaring a `Channels[...]` port should
//! reach for `well_known::*` constants by default. Inline string
//! literals are the deliberate exception, not the rule.
//!
//! Adding a name: append one line inside the `well_known_channels!`
//! macro invocation in the appropriate category. The constant
//! declaration and the collision-check coverage are generated from
//! the same source list — see §7.5.

use crate::node_graph::ports::ChannelName;

pub mod well_known {
    use super::ChannelName;

    // ─── Spatial axes ───────────────────────────────────────────────
    pub const X: ChannelName = ChannelName::from_str("x");
    pub const Y: ChannelName = ChannelName::from_str("y");
    pub const Z: ChannelName = ChannelName::from_str("z");
    pub const W: ChannelName = ChannelName::from_str("w");

    // ─── Vector positions (when not decomposed into x/y/z) ──────────
    pub const POSITION: ChannelName = ChannelName::from_str("position");
    pub const VELOCITY: ChannelName = ChannelName::from_str("velocity");
    pub const NORMAL:   ChannelName = ChannelName::from_str("normal");
    pub const TANGENT:  ChannelName = ChannelName::from_str("tangent");
    pub const UV:       ChannelName = ChannelName::from_str("uv");

    // ─── Rectangle / box geometry ───────────────────────────────────
    pub const WIDTH:  ChannelName = ChannelName::from_str("width");
    pub const HEIGHT: ChannelName = ChannelName::from_str("height");

    // ─── Color ──────────────────────────────────────────────────────
    pub const R:     ChannelName = ChannelName::from_str("r");
    pub const G:     ChannelName = ChannelName::from_str("g");
    pub const B:     ChannelName = ChannelName::from_str("b");
    pub const A:     ChannelName = ChannelName::from_str("a");
    pub const COLOR: ChannelName = ChannelName::from_str("color");

    // ─── Edge topology ──────────────────────────────────────────────
    pub const A_INDEX: ChannelName = ChannelName::from_str("a_index");
    pub const B_INDEX: ChannelName = ChannelName::from_str("b_index");

    // ─── Particle attributes ────────────────────────────────────────
    pub const LIFE: ChannelName = ChannelName::from_str("life");
    pub const AGE:  ChannelName = ChannelName::from_str("age");
    pub const SEED: ChannelName = ChannelName::from_str("seed");

    // ─── Instance transforms ────────────────────────────────────────
    pub const POS_SCALE: ChannelName = ChannelName::from_str("pos_scale");
    pub const ROT:       ChannelName = ChannelName::from_str("rot");

    // ─── Generic scalar / control ───────────────────────────────────
    pub const VALUE:     ChannelName = ChannelName::from_str("value");
    pub const T:         ChannelName = ChannelName::from_str("t");
    pub const INDEX:     ChannelName = ChannelName::from_str("index");
    pub const MAGNITUDE: ChannelName = ChannelName::from_str("magnitude");
    pub const PHASE:     ChannelName = ChannelName::from_str("phase");
    pub const FREQ:      ChannelName = ChannelName::from_str("freq");

    // ─── Confidence / probability / weight (DNN, FFI, classifiers) ─
    pub const CONFIDENCE: ChannelName = ChannelName::from_str("confidence");
    pub const WEIGHT:     ChannelName = ChannelName::from_str("weight");

    // ─── More to be added pre-1.0 ──────────────────────────────────
    // Registry grows as the catalog grows. Stays stable post-1.0.
}
```

### 7.3 Conventions for naming channels

These conventions guide additions to the registry. They are recommendations, enforced socially during code review until a primitive's signature is `pub const _SPECS`:

- **Lowercase, ASCII alphanumeric + underscore + dot.** No spaces. No special chars. Dots permitted for namespacing (e.g., `material.roughness`) but discouraged for general use.
- **Spell out short names.** `width` not `w` (except when `w` is the W axis — `x, y, z, w` is the established 4D-vector spelling). `position` not `pos`. `velocity` not `vel`.
- **Singular not plural.** `color` not `colors`. `position` not `positions`. The sample axis already implies multiplicity.
- **Domain-neutral.** `position` not `blob_position`. `confidence` not `face_confidence`. The wire's port name and the producer's identity convey the domain; the channel name conveys the semantic.
- **Match WGSL convention where it exists.** `r, g, b, a` for color components (WGSL spelling). `x, y, z, w` for axes. `uv` for texture coordinates.

### 7.4 Inline string literals — when permitted

A primitive may use an inline string literal for a channel name when:

- The channel is genuinely local to one primitive's role (e.g., a debug primitive emitting an internal-only counter).
- The primitive is experimental and the convention hasn't been decided yet.
- The primitive's first migration shipped before the registry entry existed; subsequent commits should pull the name into `well_known`.

Code review should flag inline string literals and ask "should this be in `well_known`?" by default.

### 7.5 Hash collision testing

The registry is declared via a `well_known_channels!` macro that takes a single source list of `(NAME, "string")` pairs and emits both:

1. The `pub const NAME: ChannelName = ChannelName::from_str("string");` declarations.
2. A CI test that walks the same list, computes the FNV-1a hash for each, and asserts pairwise distinct.

Adding a new well-known name is one edit (append a line inside the macro invocation); the constant and the collision-check coverage update together. This avoids the "two-place edit drifts" failure mode of hand-extended fixtures. The test catches the edge-case-not-going-to-happen FNV collision before it ships; it costs nothing once the macro is written.

---

## 8. `wgsl_compute` migration — typed signatures via naga

### 8.1 What changes

The current `wgsl_compute` family (`node.wgsl_compute_0in_1tex`, `node.wgsl_compute_1tex_1tex`, `node.wgsl_compute_2tex_1tex`, plus the generic `node.wgsl_compute` used by BlackHole and ComputeStrangeAttractor) parses its WGSL source via naga at JSON-load time, introspects its `var<storage>` bindings, and emits `Array(Anonymous)` for every storage-array port. Downstream consumers cast via `cast_as_particle` / `cast_as_u32` / etc.

After migration, naga's struct-field walk produces a typed `Channels` signature for each storage-array port. The cast atoms are deleted. The wire type carries the WGSL struct's field names directly.

### 8.2 The naga integration

When naga parses a WGSL struct like:

```wgsl
struct Particle {
    position: vec3<f32>,
    _pad:     f32,
    velocity: vec3<f32>,
    life:     f32,
    age:      f32,
    _pad2:    vec3<f32>,
    color:    vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
```

it has access to:
- The struct's field list, with names and types.
- The std430 byte layout (offsets + alignment) computed by naga internally.

The migration's wgsl_compute parser walks each `var<storage>` binding's element struct, maps each field to a `ChannelSpec`:

```text
for each field in struct.fields:
    name = ChannelName::from_str(field.name)
    ty = match field.ty:
        vec3<f32> → ChannelElementType::Vec3F
        vec2<f32> → ChannelElementType::Vec2F
        vec4<f32> → ChannelElementType::Vec4F
        f32       → ChannelElementType::F32
        i32       → ChannelElementType::I32
        u32       → ChannelElementType::U32
        _         → error: unsupported WGSL type in Channels signature
    append ChannelSpec { name, ty } to port_specs
```

The pad fields (`_pad`, `_pad2`) would naively become channels too — they're WGSL fields, so they're in the signature. Consumers connecting to this output would need to declare matching pad channels on their port type, which is awkward.

**v1.5 (shipped 2026-05-28): explicit `// @channel_skip` marker.** The naga struct walk's `struct_members_to_specs` helper honours a pre-naga preprocessor pass that extracts `// @channel_skip` markers from the WGSL source and builds a per-struct skip set. A field tagged with the marker on its preceding line is dropped from the emitted Channels signature; every other named field becomes a channel. The original `_pad[0-9]*` name-prefix heuristic that briefly shipped during Phase 4a (commit `40af5d37`) was retired in Phase 4b.7 — no core-dev shader exercised it (every `_pad*` field across the five wgsl_compute presets lives in a *uniform* struct, which keeps its separate `parse_uniform` `_pad*` filter), and the heuristic's failure mode (a non-padding field named `padding` silently lost from the wire) is exactly what the explicit marker eliminates.

The preprocessor lives in [primitives/wgsl_compute.rs](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs) as a pure function, `extract_channel_skip(source: &str) -> AHashMap<String, AHashSet<String>>`. The introspector calls it once per `reparse` and threads the resulting skip map through `element_to_array_type` → `struct_members_to_specs`. Naga itself never sees the marker (it's a `//` line comment, dropped by naga's lexer) but also never has to — the skip decision happens at signature-build time, not at parse time.

Marker conventions (every one covered by a unit test in `wgsl_compute::tests`):

- The marker is the bare token `@channel_skip` inside a line comment. Surrounding whitespace inside the comment is fine: `//@channel_skip`, `//   @channel_skip`, trailing whitespace, all match. Trailing prose (`// @channel_skip — reason`) does NOT match; use a separate comment line for prose.
- The marker must appear on its own line, immediately preceding (or separated only by blank/comment lines from) the field it applies to. Same-line markers (`x: f32, // @channel_skip`) are deliberately ignored.
- Multiple stacked markers are idempotent — they all apply to the single next field, not the next N fields.
- Block comments (`/* @channel_skip */`) are NOT honoured. Block comments are stripped before the line-comment scan.
- Multi-line struct declarations (with `{` on a separate line) are handled via a brace-depth walk; the per-struct skip set never leaks between adjacent structs.
- `@align(N)` / `@size(N)` attribute prefixes before a field name are parsed cleanly.
- Orphan markers (inside a struct but no following field; or outside any struct entirely) emit a `log::warn` but don't fail the parse.

### 8.3 The cast atom family deletes

Today's atoms in `cast_array.rs`:
- `node.cast_as_particle`
- `node.cast_as_u32`
- `node.cast_as_mesh_vertex`
- `node.cast_as_curve_point`
- `node.cast_as_edge_pair`
- `node.cast_as_instance_transform`

All delete in Phase 4. Their purpose was bridging `Array(Anonymous)` → typed; under the new model, the producer's `wgsl_compute` already emits a typed Channels signature. Consumers connect directly.

Preset migration: every preset currently using a cast atom (BlackHole inserts 2 × `cast_as_u32`; ComputeStrangeAttractor inserts 1 × `cast_as_particle`) gets its cast nodes deleted and the surrounding wires reconnected directly. The Channels signatures must align between the wgsl_compute output and the downstream consumer — see Phase 4 in §10 for the per-preset audit.

### 8.4 Reparse semantics

`wgsl_compute` reparses its port list when the user edits the shader source ([wgsl_compute.rs:766](crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L766)). Under Channels, the reparse rebuilds the typed Channels signatures from the new struct fields. Implications:

- The wire type carried by a wgsl_compute node's ports is *not* compile-time-static (it changes when the source changes). All other primitives have static port types.
- A previously-valid wire may become invalid after a reparse if the user renames a struct field, removes one, or changes a field's type. The validator surfaces this as a new `ChannelMismatch` error on the downstream wire. The editor should react by visually marking the now-invalid wire; the runtime executor refuses to run the affected node until the user fixes the type.
- Channel names introduced via WGSL field names get interned into the global debug registry on first parse. Bounded growth — only the distinct field-name set across all shaders in a session.

The reparse + ports-update mechanism already exists for the Anonymous case (wgsl_compute already reparses on source edit and rebuilds inputs/outputs). The migration extends it to carry channel signatures alongside the existing port-name updates.

### 8.5 Wgsl_compute output port matching

A subtle implication: a wgsl_compute node's downstream consumer expects a specific Channels signature. If the WGSL author renames their struct from `Particle { position: vec3<f32>, ... }` to `Particle { pos: vec3<f32>, ... }`, the wire's signature changes from `Channels[position: Vec3F, ...]` to `Channels[pos: Vec3F, ...]`. Downstream consumers expecting `position` no longer connect.

This is **desired behavior** — the wgsl_compute author has changed the data's semantic meaning by renaming. The validator catching it is the new model working correctly. The author either changes the WGSL field name back to match the consumer convention, or inserts a `node.rename_channel` atom in the graph to bridge.

### 8.6 The `wgsl_compute_*` variants

The three legacy variants (`wgsl_compute_0in_1tex`, `wgsl_compute_1tex_1tex`, `wgsl_compute_2tex_1tex`) are listed as deletion candidates in [PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md §1.5](PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md).

**Decision:** Phase 4 deletes them outright. They are texture-only variants that pre-date the generic `node.wgsl_compute` and don't touch the Channels migration's storage-array surface (no `var<storage>` bindings — only texture I/O). They are superseded by `node.wgsl_compute` for any consumer that needs the full surface. If the deletion audit during Phase 4 surfaces a shipping preset that still depends on one of the legacy variants, the preset migrates to `node.wgsl_compute` in the same change; no new naga-typed-signature work is needed because there are no storage-array ports on these variants to migrate.

---

## 9. Test strategy

### 9.1 Validator unit tests

In `crates/manifold-renderer/src/node_graph/validation.rs`. Build synthetic graphs and assert connect / reject:

- **Exact match — equal signatures connect.** Two ports both declaring `Channels[x: F32, y: F32]` wire cleanly.
- **Exact match — different channel count rejects.** `Channels[x, y]` → `Channels[x, y, z]` errors with `ChannelMismatch::DifferentCount`.
- **Exact match — different names rejects.** `Channels[x, y]` → `Channels[x, z]` errors with `ChannelMismatch::NameMismatch { index: 1, ... }`.
- **Exact match — different element types rejects.** `Channels[pos: Vec3F]` → `Channels[pos: Vec2F]` errors with `ChannelMismatch::TypeMismatch`.
- **Permissive match — anything connects.** A `node.rename_channel` consumer port accepts `Channels[a, b, c]`, `Channels[x]`, `Channels[pos: Vec3F, color: Vec4F]` — all fine.
- **Order matters for Exact.** `Channels[x, y]` → `Channels[y, x]` errors (same names, different order).
- **Error messages resolve names.** A mismatch produces a human-readable message with the original channel name strings.

Run via `cargo test -p manifold-renderer --lib node_graph::validation::tests::`.

### 9.2 Layout calculator tests

In `crates/manifold-renderer/src/node_graph/ports.rs` or a sibling module:

- Std430 layout for each shipping `_SPECS` constant matches the corresponding existing `#[repr(C)]` struct's `size_of::<T>()`.
- Per-channel byte offsets match the existing struct's field offsets (using `core::mem::offset_of!` where available).
- Composite cases: `Channels[vec3, f32, vec3, f32, f32, vec3, vec4]` → Particle's 64-byte layout, with each offset checked.
- Edge cases: zero-channel Channels (rejected; minimum 1 channel), single-channel Channels, all-scalar Channels, all-vec4 Channels.

### 9.3 Macro expansion tests

In `crates/manifold-renderer/src/node_graph/primitive.rs`'s test module:

- Macro syntax `Channels[x: F32, y: F32]` produces an `ArrayType` with the expected specs.
- Default match mode is Exact.
- Explicit `permissive` modifier produces Permissive.
- Inline string literal names are accepted alongside `well_known::*` constants.
- Compile-time drift assertion fires if `_SPECS` and matching struct drift apart.

### 9.4 Per-type drift assertion

In `mesh_common.rs` and `compute_common.rs` test modules:

```rust
#[test]
fn particle_specs_stride_matches_struct() {
    let stride = std430_stride(PARTICLE_SPECS);
    assert_eq!(
        stride as usize,
        std::mem::size_of::<Particle>(),
        "PARTICLE_SPECS std430 stride drifted from struct Particle size. \
         Update PARTICLE_SPECS or struct Particle so they describe the \
         same byte layout."
    );
}
```

One test per migrated type. Runs every build. Catches the drift class entirely.

### 9.5 Channel-name collision test

The collision test is emitted from the same source list as the `well_known::*` constants, via the `well_known_channels!` macro described in §7.5. The macro walks its `(NAME, "string")` pairs and generates:

- The `pub const` declarations.
- A test that computes the FNV-1a hash for each pair and asserts pairwise distinct, naming which two constants collide if one ever appears.

Single source of truth. Adding a name updates the constant and the test coverage in one edit — no parallel fixture to keep in sync.

### 9.6 Round-trip every shipped preset

The most important integration test. After Phase 3 (typed family migration) and Phase 4 (wgsl_compute migration), every preset in `assets/effect-presets/` and `assets/generator-presets/` must:

1. Load without `check-presets` errors.
2. Compile to an `ExecutionPlan` without validator errors.
3. Render its first frame on the canonical fixture (`Liveschool Live Show V6 LEDS.manifold`) without visual regression.

The first two are gated by `cargo run -p manifold-renderer --bin check-presets`. The third is the manual canonical-fixture sanity check pattern established in [PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md §3](PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md).

This is the test that catches naming-inconsistency landmines. If `pack_curve_xy` declares its output as `Channels[x, y]` but `render_lines` declares its input as `Channels[posx, posy]`, every Lissajous-shape preset breaks. The round-trip surfaces it immediately.

### 9.7 Parity tests for migrated primitives

The existing parity test pattern (`cargo test -p manifold-renderer --test parity <name>::`) covers visual equivalence for every shipping effect/generator. Run after Phase 3 + Phase 4 for the migrated primitives. Expected outcome: zero visual diff, because byte layouts are preserved.

### 9.8 Workspace test gate at Phase 5

One workspace run (`cargo test --workspace`) at the start of Phase 5 as the migration's overall gate. This is the only workspace run in the migration — per the project's test-discipline rule (see `feedback_prefer_focused_tests`), per-chat work uses focused tests.

---

## 10. Phase sequencing

Estimated chat counts per phase. Each phase's "Done when" is the explicit acceptance criterion.

### Phase 0 — End-to-end smoke test (~½ chat)

**Purpose:** de-risk the foundation before Phase 1 hardens the type infrastructure. Phase 1 as designed is scaffolding — no production code uses Channels at its end — so a bug in the macro→ArrayType→validator path wouldn't surface until Phase 2/3. A tiny throwaway end-to-end wire catches that class of bug before it sinks into the foundation.

**Deliverables:**
- Pick the smallest typed family (recommendation: `EdgePair`, 8 bytes, two U32 channels).
- Stub minimal versions of `ChannelName` / `ChannelElementType` / `ChannelSpec` / reshaped `ArrayType` and `channels_compatible` directly in a test module — *not* in the production `ports.rs` / `validation.rs` paths. Just enough to construct two `ArrayType` values, one for the producer port and one for the consumer port, and run the compatibility check.
- Declare a `Channels[a_index: U32, b_index: U32]` port signature on each side via direct struct construction (no macro syntax, no `_SPECS` constant, no registry).
- Wire one synthetic producer-shaped test fixture to one synthetic consumer-shaped test fixture through the throwaway predicate.
- Confirm the validator accepts the matching pair and rejects a mismatched pair (mismatch on name, on count, on element type).
- Confirm a `MTLBuffer` of `2 × size_of::<u32>() × max_capacity` bytes binds and reads back identically to the existing `Array(EdgePair)` allocation. Re-use the EdgePair Rust struct as the Pod source — only the wire-type tag changes for this smoke test.

**Out of scope:** anything in Phase 1 (the production type infrastructure, registry, macro). The throwaway stubs in Phase 0 get deleted when Phase 1 ships the real types.

**Done when:** a single integration test passes that exercises declare → validate → bind → read on a Channels-shaped wire end-to-end. If it doesn't, the foundation design in §4-§5 needs revision before Phase 1 ships.

### Phase 1 — Core type infrastructure (~2 chats)

**Deliverables:**
- `ChannelName(u64)` with `const fn from_str` using const FNV-1a-64.
- Debug-only runtime registry `ChannelName → &'static str` for error/display.
- `ChannelElementType` enum with `size()` and `alignment()` methods.
- `ChannelSpec` struct.
- `ArrayType` reshape: `specs: &'static [ChannelSpec]`, `item_size`, `item_align`, `match_mode`. `Copy + Hash + PartialEq + Eq`.
- `MatchMode` enum.
- `std430_stride` and `std430_offsets` calculators. Runtime fn for now.
- New `GraphError::ChannelMismatch` variant with structured reason. Old `PortTypeMismatch` replaced.
- Validator: new `channels_compatible` predicate inside `port_types_compatible`. Old `Array(Anonymous)` coercion path deleted.
- `well_known` channel-name registry — initial roster (§7.2).
- All unit tests from §9.1 + §9.2 + §9.5 passing.

**Out of scope for Phase 1:** macro changes, primitive migrations, wgsl_compute changes. The type system exists but no primitive uses it yet.

**Done when:** `cargo test -p manifold-renderer --lib node_graph::ports::` and `cargo test -p manifold-renderer --lib node_graph::validation::` are green. Workspace not affected — `ItemKind` still exists alongside the new types, no production code uses Channels yet.

### Phase 2 — Macro syntax + smoke-test primitive (~1 chat)

**Deliverables:**
- `__primitive_port_type!` macro arm for `Channels[name: Type, name: Type, ...]` syntax.
- Macro accepts both `well_known::POSITION` constants and inline string literals: `Channels[x: F32, "custom_field": Vec3F]`.
- Default match mode Exact; `Channels[permissive]` opt-in modifier for Permissive.
- One trivial primitive migrated to the new syntax end-to-end as proof-of-life. Recommendation: pick `node.value` or `node.constant_channels` (newly-added) — a no-input, single-channel-output primitive.
- Macro expansion tests from §9.3 passing.

**Out of scope:** the broader catalog migration.

**Done when:** the smoke-test primitive's Channels port wires into another smoke-test primitive's Channels port in a unit test. End-to-end through the validator + executor. `cargo test -p manifold-renderer --lib` green.

### Phase 3 — Typed-family migration (~1 chat)

**Deliverables:**
- `PARTICLE_SPECS`, `MESH_VERTEX_SPECS`, `CURVE_POINT_SPECS`, `EDGE_PAIR_SPECS`, `INSTANCE_TRANSFORM_SPECS`, `VEC4_VERTEX_SPECS`, `BLOB_SPECS` defined per §6.
- Drift assertions (§9.4) for each typed family.
- `impl KnownItem for X` for each typed array updated to emit `ArrayType { specs: X_SPECS, ... }`.
- For the primitive-types (`u32`, `f32`, `[f32; 2]`): `KnownItem` impls updated to single-channel Channels signatures with sensible default names (`value`, `xy`).
- Every primitive in the catalog that declares `Array(T)` ports continues to work — the macro emits the right Channels signature via the `ItemKind` → specs mapping.
- `ItemKind` enum + `KnownItem::ITEM_KIND` constant — the deletion path is queued but the enum still exists at the end of this phase. Phase 4 deletes it.

**Out of scope:** wgsl_compute. The `Array(Anonymous)` path still works for cast atoms and wgsl_compute, both of which get migrated in Phase 4.

**Done when:**
- `cargo run -p manifold-renderer --bin check-presets` runs every preset without errors.
- `cargo clippy -p manifold-renderer -- -D warnings` is clean.
- Focused parity tests for ~5 representative shipping presets (Lissajous, BasicShapes, MetallicGlass, FluidSim2D, NestedCubes) pass.

### Phase 4 — `wgsl_compute` migration + Anonymous deletion (~1 chat)

**Deliverables:**
- `wgsl_compute`'s naga walk extracts ChannelSpec lists from `var<storage>` struct fields (§8.2).
- Explicit pad-field marker mechanism chosen and implemented (§8.2 — preferred WGSL attribute, fallback doc-comment preprocessor). Existing core-dev shaders that use `_pad*` naming migrate to the marker in the same pass.
- The 6 cast atoms (`cast_as_particle`, etc.) deleted.
- Stub Pod types (`Blob4`, `Blob8`, `Blob32`, `Blob48`, `Blob64`) deleted.
- `Array(Anonymous)` port type — deleted from the macro and the validator.
- `pub enum ItemKind` — deleted.
- `pub trait KnownItem` — deleted (or stripped to deprecation if any non-port code uses it; otherwise deleted).
- `pub struct Blob` — deleted.
- BlackHole.json + ComputeStrangeAttractor.json (the two presets using cast atoms) — cast nodes deleted, wires reconnected directly.
- Any other primitive that uses `Array(Anonymous)` — audited and migrated to typed Channels signatures.
- Legacy `wgsl_compute_0in_1tex` / `wgsl_compute_1tex_1tex` / `wgsl_compute_2tex_1tex` variants deleted (§8.6). Any shipping preset still depending on one migrates to `node.wgsl_compute` in the same pass.

**Out of scope:** snapshot UI updates (Phase 6), docs (Phase 6).

**Done when:**
- No reference to `ItemKind`, `Anonymous`, or `cast_as_*` anywhere in production code (grep confirms zero).
- `cargo run -p manifold-renderer --bin check-presets` runs every preset without errors.
- All wgsl_compute presets parse cleanly with new typed signatures.

### Phase 5 — Workspace test gate + naming consistency sweep (~1 chat)

**Deliverables:**
- One `cargo test --workspace` run. Any failures triaged and fixed.
- One `cargo clippy --workspace -- -D warnings` run. Any warnings triaged and fixed.
- Comprehensive `check-presets` pass against every preset in both effect and generator preset directories.
- Manual canonical-fixture sanity check (`Liveschool Live Show V6 LEDS.manifold`). Visual diff: zero.
- Triage any naming-inconsistency landmines surfaced. Fix by either renaming primitive channels to canonical `well_known::*` or expanding the registry.
- Performance sanity check: open Performance Profiler, confirm GPU frame time unchanged from pre-migration baseline.

**Done when:**
- Workspace tests green.
- Workspace clippy green.
- Visual parity confirmed on the canonical fixture.
- GPU frame time within 0.1ms of pre-migration baseline.

### Phase 6 — Documentation + snapshot UI extension (~1 chat)

**Deliverables:**
- This document (CHANNEL_TYPE_SYSTEM.md) — status updated to "Phase 1-5 shipped".
- [BUFFER_PORT_PLAN.md](BUFFER_PORT_PLAN.md) — `Array<T>` references updated to `Channels` terminology; note that its Array-type-system section is superseded by this doc.
- [NODE_CATALOG.md](NODE_CATALOG.md) — every catalog entry's port types updated to Channels notation. New section: `well_known` channel registry overview.
- [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) — sections that reference `Array<f32>` stride conventions updated.
- [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) — Array-port type system section updated.
- [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) — macro syntax examples updated to Channels.
- Snapshot UI extension: `PortKindSnapshot::Array` gains a `channels: Vec<ChannelSnapshot>` field. Editor hover-tooltip displays channel name list per port. (Editor implementation work — out of scope for this doc; the snapshot data must carry the info.)
- Memory file `feedback_named_channels_canonical.md` — short notes on the migration that future agents should know without re-reading this doc.

**Done when:** documentation passes a read-through; new primitive authors have everything they need from `ADDING_PRIMITIVES.md` + `NODE_CATALOG.md` + this doc to write a Channels-aware primitive without referring to migration commit messages.

---

## 11. Risks and mitigations

### 11.1 Naming inconsistency between producer and consumer

**Risk:** the migration's largest landmine. A producer emits `Channels[pos: Vec3F]`; a consumer expects `Channels[position: Vec3F]`. The validator rejects what used to silently work. Every preset using both primitives breaks.

**Mitigation:**
- The `well_known` registry. Every migrated primitive uses canonical constants. Drift is mechanically detectable in code review (inline string literals stand out).
- §9.6 round-trip test runs every shipped preset through `check-presets` and the canonical-fixture sanity check after each migration phase. Naming inconsistencies surface immediately.
- The migration phase ordering (typed families first, then wgsl_compute) batches the naming decisions per phase. One pass through `mesh_common.rs` aligns CurvePoint / MeshVertex / EdgePair / Vec4Vertex / InstanceTransform / Blob; one pass through `compute_common.rs` aligns Particle. Each pass is a coherent reviewable change.

### 11.2 `bytemuck::Pod` and visible pad fields

**Risk:** `#[repr(C)]` structs require all fields to be `pub` for `bytemuck::Pod`. The pad fields (`_pad0`, `_pad1`, etc.) are public Rust fields but are *not* channels in the new model. The "real" channels (position, velocity, etc.) and the pad fields describe the same bytes through different lenses.

**Mitigation:** the model accommodates this by design. The `_SPECS` constant lists only the semantic channels; std430 layout reintroduces the padding automatically. The Rust struct's pad fields are an implementation detail of the in-memory representation; they don't appear in the wire type. Drift between specs and struct is caught by the per-type drift assertion (§9.4).

Subtle implication: a primitive author adding a new channel to a typed family must update BOTH `_SPECS` AND the matching `#[repr(C)]` struct. The drift assertion catches "I updated specs but forgot the struct" or vice versa. Adding a new channel is a deliberate two-place edit.

### 11.3 wgsl_compute reparse semantics

**Risk:** wgsl_compute is the only primitive whose port types change at runtime (when the user edits the shader source). A previously-valid wire can become invalid after a reparse.

**Mitigation:** the existing reparse mechanism already handles port-list changes (the Anonymous case). The migration extends it to carry channel signatures alongside port-name updates. The editor's existing handling of "wire becomes invalid" (visual flag + error in node detail) covers the new case. The validator surfaces a clear `ChannelMismatch` instead of a generic type error.

### 11.4 Per-port match-mode discipline

**Risk:** a primitive author declaring a Permissive port for a non-transform reason (e.g., "I'll handle channel name variation at runtime") undermines the Exact-by-default discipline. Permissive should only be used for true transforms (rename, reorder, etc.).

**Mitigation:** the allow-list is a `pub const PERMISSIVE_PRIMITIVE_ALLOWLIST: &[PrimitiveTypeId]` in `validation.rs`, and a CI test enumerates every primitive declaring Permissive on a port and asserts the primitive's `TYPE_ID` appears in that const. The const IS the canonical list — primitives can't sneak Permissive in without editing one specific, code-reviewed file. The test enforces the const, the const is human-readable next to the validator code that consumes it, and reviewers see Permissive additions as `pub const` diffs in the file that defines the rule.

### 11.5 The compile-time-vs-runtime layout calculator

**Risk:** I committed in §4.4 to allowing either const-fn or runtime layout calculation. If the implementer goes runtime, lazy `OnceLock<ArrayType>` initialization adds a synchronization point on every primitive's first port type access. Compounded across hundreds of primitives, this could measurably slow chain build time.

**Mitigation:** measure first. The runtime calculator runs once per primitive at first port access, then never again. Total cost ≈ (number of distinct `_SPECS` constants) × (calculator runtime). For ~10 typed families × ~5 channels each × ~100ns per calculation ≈ 5µs total per process lifetime. Negligible.

If the measurement shows non-negligible cost, the calculator can be promoted to `const fn` (stable Rust supports the required `while` loops). Treated as Phase 5 polish, not Phase 1 blocker.

### 11.6 Snapshot / serialization back-compat

**Risk:** the editor snapshot ([snapshot.rs:188](crates/manifold-renderer/src/node_graph/snapshot.rs#L188)) maps `PortType::Array(_)` to a coarse enum variant. Adding channel-name info to the snapshot is a serialization extension; older snapshots (cached graph editor state) might not know about the new field.

**Mitigation:** the snapshot is not persisted — it's runtime-only editor state. Adding a field is a non-breaking change. The runtime / editor restart picks up the new field; no migration concern for saved data.

Saved presets (the JSON files) carry no port-type info — wires reference ports by name only. No serialization compat issue.

### 11.7 Naming convention drift in fast-moving primitives

**Risk:** wgsl_compute reparse introduces user-typed channel names into the runtime registry. A user who consistently spells `pos` instead of `position` in their custom shaders creates a fork — their wgsl_compute outputs don't connect to standard consumers.

**Mitigation:** by design. The author's options are (a) match the canonical name in their WGSL, (b) insert `node.rename_channel` in their graph, or (c) opt into Permissive consumers if they're doing something genuinely generic. The "broken wire" is the right signal — the data semantically *is* different from what the consumer expects, and the validator catches it.

### 11.8 Design choices that would break fusion-compiler compatibility

**Risk:** the Channel type system is designed forward-compatible with the planned WGSL fusion compiler (see §16). A future amendment that introduces a feature incompatible with fusion would silently delete a major end-game capability. Examples of the kind of additions that would break fusion: a `ChannelElementType::Dynamic` that resolves at runtime; per-sample variable channel layouts; channel names that aren't const-foldable; a match mode whose compatibility check requires runtime introspection.

**Mitigation:** §16 lists the fusion-compatibility constraints explicitly. Any amendment to §4 (the type system contract) or §5 (validator semantics) that introduces new variants, modes, or runtime behaviour MUST check §16 before merging. CI can't catch this directly — the fusion compiler doesn't exist yet — so the check is a code-review discipline. The section is positioned so that an agent extending the type system finds the constraints before writing the code.

---

## 12. Worked examples

### 12.1 Migrating `Particle`

**Before:**

```rust
// compute_common.rs
#[repr(C)]
pub struct Particle {
    pub position: [f32; 3],
    pub _pad0:    f32,
    pub velocity: [f32; 3],
    pub life:     f32,
    pub age:      f32,
    pub _pad1:    [f32; 3],
    pub color:    [f32; 4],
}

impl KnownItem for Particle {
    const ITEM_KIND: ItemKind = ItemKind::Particle;
}
```

```rust
// seed_particles.rs (one of many particle primitives)
crate::primitive! {
    name: SeedParticles,
    inputs: { ... },
    outputs: {
        particles: Array(Particle),
    },
    ...
}
```

**After:**

```rust
// compute_common.rs
#[repr(C)]
pub struct Particle {
    pub position: [f32; 3],
    pub _pad0:    f32,
    pub velocity: [f32; 3],
    pub life:     f32,
    pub age:      f32,
    pub _pad1:    [f32; 3],
    pub color:    [f32; 4],
}
// struct stays — still the in-memory representation + bytemuck Pod source.

pub const PARTICLE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::VELOCITY, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::LIFE,     ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::AGE,      ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::COLOR,    ty: ChannelElementType::Vec4F },
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn particle_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(PARTICLE_SPECS) as usize,
            std::mem::size_of::<Particle>()
        );
    }
}
```

```rust
// seed_particles.rs
crate::primitive! {
    name: SeedParticles,
    inputs: { ... },
    outputs: {
        // For typed families with a `_SPECS` constant, `Channels<T>`
        // is the canonical form. The inline `Channels[...]` form is
        // reserved for ad-hoc signatures (no associated `_SPECS`).
        particles: Channels<Particle>,
    },
    ...
}
```

Primitive code reading the buffer continues to use the `Particle` struct's field accesses unchanged. The wire type's identity is now `PARTICLE_SPECS`-shaped, not `ItemKind::Particle`-shaped.

The two macro forms have non-overlapping scopes: `Channels<T>` is the only form used for the ~10 typed families that have a `_SPECS` constant (Particle, MeshVertex, EdgePair, CurvePoint, InstanceTransform, Vec4Vertex, Blob-equivalent, etc.); `Channels[...]` inline is for ad-hoc signatures (wgsl_compute outputs, primitive-types like single-channel scalars, experimental shapes). A lint enforces "if a typed family exists for this signature, use `Channels<T>`" so authors can't drift between forms. This keeps `rg "Channels<Particle>"` reliable for catching every site touching the family.

### 12.2 Migrating a wgsl_compute consumer (BlackHole)

**Before:**

```text
wgsl_compute (BlackHole — emits Array(Anonymous) for splat accumulators)
        │
        ▼
cast_as_u32 (Array(Anonymous) → Array<u32>)
        │
        ▼
resolve_accumulator (Array<u32> → Texture2D)
```

**After:**

Naga walks the BlackHole shader's `struct AccumulatorRGB { r: u32, g: u32, b: u32 }` and emits `Channels[r: U32, g: U32, b: U32]` per accumulator. The cast nodes delete. The wires connect wgsl_compute's output directly to `resolve_accumulator`'s input — which is now declared as `Channels[r: U32, g: U32, b: U32]`.

```text
wgsl_compute (BlackHole — emits Channels[r: U32, g: U32, b: U32])
        │
        ▼
resolve_accumulator (Channels[r: U32, g: U32, b: U32] → Texture2D)
```

JSON preset: the cast nodes' entries delete; the wires connecting through them get one endpoint rerouted to the wgsl_compute node directly. Channel naming alignment: wgsl_compute's WGSL struct names (`r, g, b`) and `resolve_accumulator`'s expected channel names (also `r, g, b` via `well_known::R, G, B`) match by canonical convention.

### 12.3 A new effect using audio FFT data (post-migration)

A future audio-reactive effect wants to take an FFT spectrum (1024 bins) and use it to drive particle colors.

**Wire shapes:**

- Producer (`node.audio_fft_analyzer` — future primitive): output `Channels[magnitude: F32]` of sample count 1024.
- Consumer (`node.particle_colorize_by_audio` — future primitive): input `Channels[magnitude: F32]` of sample count matching the producer.

The wire carries 1024 samples × 1 channel = 4096 bytes per frame. The consumer reads sample `j` for FFT bin `j`. Channel name `magnitude` is in `well_known::*` already.

**No new Rust struct needed.** No `Audio` ItemKind variant. The single-channel `Channels[magnitude: F32]` is the wire type; under the hood it's one MTLBuffer of 4096 bytes. The validator confirms producer and consumer agree on the channel.

If the analyzer later evolves to emit `Channels[magnitude: F32, phase: F32]` per bin, the wire type changes. Consumers requesting just magnitude wire through `node.select_channels[magnitude]` to narrow. The graph reads cleanly; the introspectability surfaces "this wire has magnitude and phase" without any documentation.

### 12.4 A user introducing a new data shape

A user authoring a custom shader in `wgsl_compute` for hand tracking emits:

```wgsl
struct HandJoint {
    position:   vec3<f32>,
    _pad:       f32,
    confidence: f32,
};
@group(0) @binding(0) var<storage> hand_joints: array<HandJoint>;
```

Naga's struct walk produces `Channels[position: Vec3F, confidence: F32]` (skipping `_pad` per §8.2). No Rust code change. No recompile. The wire is typed; downstream consumers wiring to it can introspect the signature and reach for `well_known::POSITION` and `well_known::CONFIDENCE`.

A future "particle field follows hand position" effect just wires `Channels[position: Vec3F, confidence: F32]` → its input. The validator confirms; the data flows; the effect runs.

This is what end-game flexibility looks like: new user data shapes are graph-authoring concerns, not Rust changes.

---

## 13. Resolved decisions (originally deferred to first review)

The three decisions originally parked for first-review pass are resolved below. Sign-off pass on 2026-05-27 closed all three; the resolutions are now load-bearing throughout the doc and Phase implementations should not re-litigate them.

1. **Sample-count exposed via the wire's runtime interface.** Consumers declaring a Channels port receive the buffer handle and the sample count cohesively through one interface — no separately-wired `active_count` scalar port. The runtime mechanism is unchanged from BUFFER_PORT_PLAN.md (the producer sets the count per dispatch, the runtime passes it via uniform, the fusion compiler still emits a shader looping over the runtime uniform per §16.6); only the API surface changes. Resolved in favour of cohesive wire-level introspection because the project's stated end goal is AI-authored graphs (per `project_primitive_library_for_ai_authoring.md`), and forcing an agent to wire two parallel signals (buffer + count) for every Channels consumer undercuts the "wires self-describe" promise. See §3.1 for the wire-interface description.

2. **One canonical macro form per case, not "ship both".** `Channels<T>` is the canonical form for the ~10 typed families with a `_SPECS` constant (Particle, MeshVertex, EdgePair, CurvePoint, InstanceTransform, Vec4Vertex, Blob-equivalent, etc.). `Channels[...]` inline is the canonical form for ad-hoc signatures (wgsl_compute outputs, primitive-types, experimental shapes). A lint enforces non-overlap. Resolved this way because "ship both" permits drift — readers mentally translate between forms and `rg` queries for a typed family miss the inline-form sites. See §12.1 for the worked example.

3. **2D positions emit as paired scalars, not Vec2F.** Codified as a §7.3 naming convention. `CurvePoint` and `Vec2Slot` migrate as `Channels[x: F32, y: F32]` (4-byte aligned, matching today's `[f32; 2]` consumers). New primitives authoring 2D position channels follow the same convention. Consumers needing Vec2 semantics either consume the two scalars directly in shader code or compose via a downstream `pack_vec2_from_scalars` atom.

---

## 14. Out of scope for v1; future extensions

Items deliberately not in the v1 migration. Sequenced for "when there's a real consumer."

### 14.1 Bool channels

No current typed array carries bool. Particle's `life` is `f32 ∈ [0, 1]`, not a bool. When a real consumer needs bool (e.g., per-particle "alive" flag separate from life), add `ChannelElementType::Bool` with std430 packing (1 byte natural, 4 bytes in storage-buffer std430).

### 14.2 Matrix channels (Mat3F, Mat4F)

No current typed array carries matrices. `InstanceTransform` is two Vec4F slots, not a Mat4F. When a real consumer needs Mat4F (e.g., a Niagara-style per-instance camera-space transform), add `ChannelElementType::Mat4F` with std430 storage (16 floats, 16-byte align).

### 14.3 Half-precision (F16) channels

Storage and bandwidth optimization for high-channel-count arrays. Not used anywhere currently. Add when a real consumer (audio spectrogram, large detection batches) shows performance benefit.

### 14.4 Variadic channel-name patterns

A producer emitting "N magnitude channels named `bin_0`, `bin_1`, ..." today requires declaring each name individually. A future extension: variadic / template channel names (`bin_{0..N}`) in the macro. Useful for FFT-style data. Skipped in v1 because the workaround (declare all 1024 names) is verbose but works.

### 14.5 Channel-domain attributes (Blender-style)

Channel data attached to a topological domain (per-vertex, per-edge, per-face mesh attributes). Maps onto our Channels via parallel arrays — one Channels per domain. No new type system primitives needed; just a convention for naming and pairing. Could be useful for future user-authored mesh attribute work but not necessary for v1.

### 14.6 Channel-name pattern matching (regex / glob)

A consumer wanting "any channel matching `bin_*`" — a TD-CHOP feature. Niche; defer until a use case appears. The current Exact / Permissive split covers everything we need today.

### 14.7 Per-channel metadata (rate, unit, range)

A `tx` channel might carry "this is normalized [0, 1]" or "this is in pixels" or "this is in beats per minute" as metadata. Useful for editor display and AI authoring. Defer to a future "channel annotations" extension that can ride on top of the v1 type system without breaking it.

### 14.8 Channel-channel arithmetic shortcut

A future `node.channel_math_pairwise` that takes two Channels arrays and applies an op channel-wise (Channels A's `x` + Channels B's `x`, A's `y` + B's `y`, etc.). Currently expressible via repeated `select_channel` + `array_math` + `pack_channels`. Worth a single-atom shortcut when the pattern shows up enough to be visible.

### 14.9 Explicit-marker preprocessor for `wgsl_compute` pad-field handling — **shipped 2026-05-28**

Historical note. The Phase 4a `wgsl_compute` naga walk shipped a `_pad[0-9]*` name-prefix heuristic for skipping padding fields, walked back from the sign-off's "explicit per-field marker" decision. Phase 4b.6 (`extract_channel_skip` preprocessor) and Phase 4b.7 (integration + heuristic deletion) finished the work on 2026-05-28. See §8.2 for the current contract.

The migration sweep collapsed to "drop the heuristic, no shader changes" — the read-only audit of all five wgsl_compute presets (BlackHole, ComputeStrangeAttractor, FluidSimulation, ParticleText, StarField) plus `DEFAULT_WGSL` found every `_pad*` field in *uniform* structs, none in storage structs. The uniform-side `_pad*` filter in `parse_uniform` was out of scope for this work and stays as the ergonomic shortcut for uniform layout, which has no Channels-signature implications.

Trigger note retained for future agents: if a future amendment wants to extend the marker syntax (multi-token markers, attribute-based markers, etc.), check the `wgsl_compute::tests::skip_marker_*` test set first — every supported variation has a named test, and any extension should add tests alongside the existing ones rather than replace them.

See [feedback_wgsl_compute_is_real_user_surface.md](../.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/memory/feedback_wgsl_compute_is_real_user_surface.md) for the correcting feedback that made the original "ship name-prefix permanently" plan untenable.

---

## 15. Document conventions

Rules for amendments and additions to this document:

- **Status section at top stays accurate.** Phase shipping updates "Phase X — not yet started" → "Phase X — shipped <date>".
- **Numbered sections are stable.** Don't renumber existing sections; append new ones with the next available number.
- **Each section has a one-line role.** Skim-readers should know what a section covers from its header.
- **Code examples use the current Rust/WGSL spelling.** If macro syntax or struct shape changes during implementation, update the examples to match in the same commit.
- **Open questions deferred go in §13.** Resolved questions move to the appropriate section.
- **Risks discovered during implementation go in §11.** Don't bury them in commit messages.
- **Migration map (§6) stays canonical.** New typed-family additions append entries with byte-layout proofs.
- **The `well_known` roster in §7.2 mirrors the code.** Update both when adding a name.
- **Naming decisions reference this doc.** Memory entries (`feedback_*.md`) for end-game decisions cite this document; the source of truth is here.

---

## 16. Forward compatibility — fusion compiler

### 16.1 Why this section exists

The Channel type system designed in §3-§5 is foundational infrastructure that will outlive everything built on top of it. The biggest single thing that gets built on top of it — by stated MANIFOLD architectural direction — is the **WGSL fusion compiler + `for_each_n` per-pixel loops** initiative documented in [GRAPH_COMPILER.md](GRAPH_COMPILER.md). Plasma decomposition is parked until the fusion compiler lands; performance optimization for atomized graphs depends on it; the no-fused-monolith rule survives long-term because of it.

The Channel design choices in this document were made *with fusion compatibility in mind*. This section makes that compatibility explicit so future agents extending the Channel type system don't accidentally close off a major end-game capability without realising it. **If you are amending §4 (type system contract), §5 (validator semantics), or §8 (wgsl_compute integration), read this section before merging your change.**

This section also explains the concrete fusion-compiler synergies the Channel system unlocks — useful context both for the fusion compiler's eventual implementer and for any agent reasoning about why the channel design landed where it did.

### 16.2 What the fusion compiler does (brief)

Full details in [GRAPH_COMPILER.md](GRAPH_COMPILER.md). Summary:

The fusion compiler walks a sub-graph of pointwise atoms at graph-compile time, identifies a maximal fusable boundary, and emits **one fused WGSL shader** that does the whole chain's math as local variables in registers — no intermediate texture or buffer roundtrips. The same machinery underlies `node.for_each_n`, which wraps a sub-graph body in a `for` loop and emits one shader with the body inlined as the loop body. Plasma's 75-atom decomposition collapses to ~8 dispatches; Plasma's Noise variant becomes a 4-atom body wrapped in `for_each_n`.

The fusion compiler is unstarted (Status section of GRAPH_COMPILER.md), so this isn't speculative future work — it's planned work that is being explicitly designed for.

### 16.3 Why Channels and fusion compose well

Four concrete synergies — these are the wins the fusion compiler picks up from the Channel system "for free":

**(1) Auto-generated intermediate struct definitions.** A fusion compiler chaining `producer → atom_a → atom_b → consumer` along Array wires must emit a WGSL struct for each intermediate buffer type so the inlined shader can declare local variables of the right shape. Today, every typed array (Particle, MeshVertex, etc.) has a hand-written WGSL struct in a `.wgsl` file matching the Rust `#[repr(C)]` definition; the fusion compiler would need a hardcoded mapping from `ItemKind` → struct source string. With Channels, the compiler reads the `_SPECS` constant and emits the WGSL struct declaration mechanically: `struct ParticleIntermediate { position: vec3<f32>, velocity: vec3<f32>, life: f32, age: f32, color: vec4<f32>, };`. The std430 layout calculator (§4.4) gives the field offsets directly. No hand-mapping table to maintain.

**(2) Dead-channel elimination.** A producer emitting `Channels[position, velocity, life, age, color]` whose downstream consumers in a fused sub-graph only ever read `position` lets the fusion compiler skip writing the other four channels (or, if the intermediate isn't observed at all in the fused output, skip allocating them entirely). Today, atom A writes the full Particle struct because the fusion compiler can't analyze across the buffer boundary to know what atom B will read. Channels make this cheap: walk the consumer's source for `channel_get("velocity")`-style calls, drop unused channels from the local intermediate. Bigger wins on richly-channeled types (Particle is 5 channels; a future audio FFT bucket might be hundreds).

**(3) Channel-level operator fusion.** Operators like `node.channel_math` declare their target channel name via a param. The fusion compiler reads this at graph-compile time and emits code that touches only the target channel inline, leaving the others as passthrough local variables. A chain like `select_channel[x] → array_math(Mul, scale=k) → pack_channels(x, y_unchanged)` compiles to one read + one multiply + one write on x; y never gets a load/store. Pre-Channels this required buffer-aliasing analysis the fusion compiler couldn't reliably perform.

**(4) Pure rename/reorder operators eliminate entirely.** `node.rename_channel`, `node.rename_channels`, `node.reorder_channels` are pure type-level transforms — they shuffle the producer's bytes into the consumer's expected channel order without computing anything new. The fusion compiler erases them completely: the next consumer downstream reads through the renamed/reordered channel mapping as if the rename node weren't there. Today the closest equivalent (unpack-into-Array<f32>, rename, repack) actually copies bytes through an intermediate buffer.

Result: a Channels-aware fusion compiler optimizes graphs the byte-bucket model could only optimize via expensive cross-buffer analysis, and emits cleaner IR doing it.

### 16.4 Forward-compatibility constraints — what the Channel system MUST maintain

These constraints are the "fusion is possible" surface of the Channel type system. **Amendments to the channel system that violate any of these are likely to break fusion. Don't make those amendments without explicitly considering this section.**

**C1. Channel specs are compile-time-known.** Every `ArrayType::specs` is a `&'static [ChannelSpec]` known at graph-compile time. The fusion compiler reads specs to emit WGSL struct definitions; specs that resolved only at runtime would force the fusion compiler to be dynamic, which is not a small change — it would require runtime shader generation per draw, which is the model the fusion initiative is *avoiding*.

**C2. Std430 layout is deterministic from specs alone.** Given `(specs)`, the std430 layout calculator (§4.4) produces `(per_channel_offset, sample_stride, sample_align)` deterministically with no runtime input. The fusion compiler uses these offsets to emit correct WGSL struct field accesses. Layout that depended on a runtime value (e.g., a `dynamic_padding` param) would break this.

**C3. Channel names are const-foldable.** `ChannelName` is a u64 FNV hash computed at compile time via `const fn from_str`. The fusion compiler compares hashes at compile time when deciding whether a `select_channel` operator's target channel exists on the producer. Channel names that required runtime hash computation, runtime string lookup, or runtime registry resolution would force fusion-time decisions into runtime — defeating the optimization.

**C4. The element type set is closed and concrete.** Every variant in `ChannelElementType` maps to exactly one concrete WGSL type (F32→f32, Vec3F→vec3<f32>, etc.). The fusion compiler emits WGSL declarations and field accesses using this mapping. A "wildcard" or "dynamic" element type that the WGSL emitter couldn't resolve concretely would break shader emission. Adding new element types is fine (e.g., F16, Bool — both flagged in §14); each must be added with a concrete WGSL mapping.

**C5. Match modes are statically decidable.** `MatchMode::Exact` and `MatchMode::Permissive` both decide compatibility at graph-compile time from the wire's static `specs`. The fusion compiler uses match decisions to determine which sub-graphs can fuse (compatible wires fuse; incompatible boundaries don't). A match mode whose compatibility check required runtime data would force fusion boundaries to be runtime-decided, defeating the compile-time fusion model.

**C6. Pure type-level operators are byte-preserving.** `rename_channel`, `reorder_channels`, `select_channels`, and similar operators describe themselves as pure data-shape transformations: they emit the same bytes the producer wrote, just in a (possibly reordered) channel layout. The fusion compiler erases them in the fused output. Adding a "rename" variant that actually mutated bytes would force the fusion compiler to detect and preserve the mutation — eliminating the optimization for that operator.

**C7. `wgsl_compute` Channels signatures are recovered from the WGSL source.** The naga parser walks the storage-array struct fields at graph-compile time and produces the Channels signature (§8.2). The fusion compiler must be able to read the same WGSL source and reconstruct equivalent inlinable expressions; the naga walk is the bridge. Any wgsl_compute feature that produced a Channels signature without a matching WGSL struct (e.g., synthetic channels added in Rust code) would break the equivalence.

**C8. Sample stride is identical at runtime and at fusion time.** The buffer the producer writes has stride `sample_stride_bytes`; the fusion compiler emits WGSL that reads at the same stride. A future "stride-variant" feature (e.g., compact packed sub-byte channels) would force the fusion compiler to emit per-channel offset arithmetic, which is fine in principle but adds complexity. Keep the rule "one channel-spec → one stride" simple.

### 16.5 Anti-patterns — what NOT to add

Concrete examples of additions that would break the constraints above. Listed so future agents recognize the pattern by sight:

- ❌ `ChannelElementType::Dynamic` — runtime-resolved element type.
- ❌ `ChannelSpec { name, ty, runtime_layout_hint }` — any field that needs runtime resolution.
- ❌ `ArrayType { specs, item_size, item_align, match_mode, dynamic_subset_pattern }` — adding a runtime channel-subset filter.
- ❌ `MatchMode::RuntimeSubset` — accept producer if it has at least the channels listed in this runtime-evaluated param. (Use the explicit `select_channels` atom instead; same outcome, compile-time.)
- ❌ `node.synthesize_channel(name, formula)` that adds a new named channel to a wire without writing to any underlying buffer. (Use a real `pack_channels` from a producer instead; same outcome, the channel exists in bytes.)
- ❌ Per-sample channel layouts (different samples in the same array have different channels). Variable layouts per sample would require runtime per-sample dispatch decisions.
- ❌ Channel names that aren't ASCII identifiers, since they'd need quoting in WGSL field-name emission. Use the §7.3 naming conventions.

### 16.6 Constraints that don't apply

To prevent over-constraining future work, these are NOT fusion-compatibility constraints:

- **New element types are fine** if each has a concrete WGSL mapping. Adding `F16` with `f16` mapping is straightforward; same for `Bool` (mapped to `u32` in std430 storage buffer per WGSL rules).
- **New operators are fine** as long as their compute model is consistent with C6 (pure type-level operators are byte-preserving) or they live outside the fusable subgraph (operators with side effects, multi-tap reads, etc., simply don't inline). The fusion compiler will skip them; that's correct behaviour.
- **Per-channel optimization metadata is fine** if it's compile-time-known and the fusion compiler can use it as a hint (e.g., "this channel is read-only, this one is write-only"). Hints that improve optimization without changing semantics are forward-compatible by definition.
- **Variable sample count is fine** — the underlying `active_count` uniform mechanism is unchanged. The fusion compiler emits a shader that loops over samples; the loop bound is a runtime uniform. (Per the §13(1) resolution, the wire's *API surface* exposes the sample count through the Channels handle rather than as a separately-wired scalar port. This is an API ergonomics change for consumers and AI agents; the runtime data path — uniform passed per dispatch, fusion-compiler loop over the uniform — is unchanged.)

### 16.7 Reference reading order for agents extending the type system

If you're an agent extending the Channel type system (adding a new element type, new match mode, new operator family, new validator rule, etc.):

1. Read §4 (current contract) and §5 (validator semantics) — understand what exists.
2. Read this section (§16) — understand what cannot change without breaking fusion.
3. Read [GRAPH_COMPILER.md §4](GRAPH_COMPILER.md) — understand the fusion compiler's implementation model.
4. Read [GRAPH_COMPILER.md §6](GRAPH_COMPILER.md) — understand the per-atom inline gates and what makes an atom fusable.
5. Draft your amendment, then check it against C1-C8 in §16.4 above.

If your amendment violates a constraint, the resolution is one of: (a) redesign the amendment to be compile-time, (b) confine the new feature to non-fusable atoms only (lives outside the fusable subgraph), or (c) escalate to a design discussion that updates §16 with the new constraint structure. Don't merge an amendment that silently breaks fusion compatibility.

---

## 17. Texture2D channel signatures

### 17.1 Why this exists

The Array channel work (Phases 0–6) typed every `Array<T>` wire so producer and consumer agree on per-sample channel layout. The Texture2D family was untouched: a `Rgba16Float` wire labelled `PortType::Texture2D` says "4 × half-float per pixel" and nothing about *what those four components mean*. Two primitives can pack different meanings into the same RGBA layout, the validator nods through because both endpoints say `Texture2D`, and the runtime renders garbage.

The bug case that motivated this section: the V2 WireframeDepth graph wired `node.optical_flow_estimate.out` — which packs `(R=flow_x, G=confidence, B=flow_y, A=valid)` (the Watercolor R/B-flow convention) — into a downstream `wgsl_compute` pass that read `(R=flow_x, G=flow_y, B=confidence, A=valid)` (the MiDaS convention). Validates fine. No log output. Visible-on-screen garbage.

Extending the Channel type system to Texture2D ports plugs the gap. Producers declare what each RGBA slot means; consumers declare what they expect; the validator enforces exact match per slot at graph compile time. Untyped Texture2D stays the default — the migration valve through which one primitive migrates at a time without breaking the catalog.

### 17.2 Shape of the extension

`PortType` gains a second texture variant carrying a four-slot named-channel signature. The untyped variant survives unchanged as the back-compat default.

```rust
pub enum PortType {
    /// Untyped Texture2D — the back-compat default. Four RGBA slots
    /// carry whatever the producer packs; consumers rely on prose
    /// `composition_notes` for the layout. Connects to anything.
    Texture2D,
    /// Texture2D with a four-slot named-channel signature. The
    /// validator enforces exact-match between two typed endpoints
    /// and surfaces a per-slot diff on mismatch.
    Texture2DTyped(TextureChannels),
    // … rest unchanged …
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureChannels {
    /// Channel names in R, G, B, A order.
    pub slots: [ChannelName; 4],
}
```

Slot element types are deliberately absent — they're implicit in the texture format (`Rgba16Float` → four F32 slots, `R8Unorm` → one F32 with the rest ignored, etc.). The texture-format contract (`output_format` / `accepted_input_formats`) already covers that axis. The channel signature adds the orthogonal "what does each slot *mean*?" axis.

The same `ChannelName` interning model as the Array path (FNV-1a-64 const hash, `well_known` registry, runtime debug-name lookup) — no parallel registry, no parallel hash mechanism. Names like `well_known::FLOW_X` and `well_known::CONFIDENCE` mean the same physical quantity whether they appear on a Channels array or a Texture2DTyped slot.

### 17.3 Macro syntax

The `primitive!` macro gains a new arm for `Texture2D[R: Name, G: Name, B: Name, A: Name]`. Each slot accepts the same dual form as Array Channels:

- `well_known::*` constant ident — canonical names from the shared registry.
- Inline string literal — escape hatch for genuinely local meanings (same `composition_notes` discipline as Array Channels: prefer well_known; literals stand out in review).

```rust
crate::primitive! {
    name: OpticalFlowEstimate,
    outputs: {
        out: Texture2D[R: FLOW_X, G: CONFIDENCE, B: FLOW_Y, A: VALID],
        // …
    },
    // …
}
```

Existing `outputs: { out: Texture2D, … }` declarations stay unchanged — the macro's plain `Texture2D` arm continues to emit the untyped variant. Migration is per-primitive and opt-in.

### 17.4 Validator semantics

The Channels-aware path in `validate_wire_endpoints` gains a Texture2DTyped branch alongside the Array branch:

| Producer | Consumer | Result |
|---|---|---|
| `Texture2D` (untyped) | `Texture2D` (untyped) | Connect — existing behaviour. |
| `Texture2D` (untyped) | `Texture2DTyped(_)` | Connect — back-compat valve. |
| `Texture2DTyped(_)` | `Texture2D` (untyped) | Connect — back-compat valve. |
| `Texture2DTyped(a)` | `Texture2DTyped(b)`, a == b | Connect. |
| `Texture2DTyped(a)` | `Texture2DTyped(b)`, a ≠ b | `TextureChannelMismatch` with the first diverging slot. |

The compatibility predicate:

```rust
pub fn texture_channels_compatible(
    producer: TextureChannels,
    consumer: TextureChannels,
) -> Result<(), TextureChannelMismatchReason> {
    for (i, (p, c)) in producer.slots.iter().zip(consumer.slots.iter()).enumerate() {
        if p != c {
            return Err(TextureChannelMismatchReason::SlotNameMismatch {
                slot: i as u32,
                producer_name: p.debug_name(),
                consumer_name: c.debug_name(),
            });
        }
    }
    Ok(())
}
```

No `MatchMode` distinction. Unlike Array ports, typed Texture2D ports always match exactly; there's no Permissive use case (a rename-channel-style operator would shuffle bytes through a memcpy, which doesn't make sense for raster textures the way it does for storage-buffer items). If a future use case appears, add the mode as an extension; v1 is exact-only.

The error variant mirrors the Array path:

```rust
GraphError::TextureChannelMismatch(Box<TextureChannelMismatchInfo>)

pub struct TextureChannelMismatchInfo {
    pub from_node: NodeInstanceId,
    pub from_port: String,
    pub to_node: NodeInstanceId,
    pub to_port: String,
    pub producer_slots: [ChannelName; 4],
    pub consumer_slots: [ChannelName; 4],
    pub reason: TextureChannelMismatchReason,
}

pub enum TextureChannelMismatchReason {
    /// Channel names differ at a specific slot index (0=R, 1=G, 2=B, 3=A).
    SlotNameMismatch {
        slot: u32,
        producer_name: Option<&'static str>,
        consumer_name: Option<&'static str>,
    },
}
```

A typed-vs-typed mismatch on optical-flow now reads:

```
Texture-channels mismatch in wire optical_flow_estimate.out -> wgsl_compute.in:
  Producer: Texture2D[R: flow_x, G: confidence, B: flow_y, A: valid]
  Consumer: Texture2D[R: flow_x, G: flow_y, B: confidence, A: valid]
  Mismatch at slot 1 (G): producer `confidence` != consumer `flow_y`.
  Align both sides to the same canonical name from `well_known::*`, or
  relabel the producer's WGSL to match.
```

### 17.5 Why a new variant instead of mutating `Texture2D`

Two designs were considered:

**(a) Mutate `PortType::Texture2D` into a tuple variant carrying an Option-shaped `TextureChannels`.** Every existing `PortType::Texture2D` reference in the codebase (~150 sites across primitive declarations, tests, snapshot conversions, pattern matches) becomes a syntax error overnight. Strictly cleaner final shape, but the migration touches every file in the catalog and conflicts with every other agent's in-flight work.

**(b) Add a new variant `PortType::Texture2DTyped(TextureChannels)` alongside the existing unit `Texture2D`.** Strictly additive. Zero existing references break. Untyped Texture2D becomes the back-compat default by virtue of existing. Validator handles cross-compatibility through one new branch.

The §17 implementation took (b). The "two variants for one conceptual thing" cost is real but contained — both variants share one pool key (the channel signature is a validator concern, not a GPU allocation one) and one snapshot bucket category. A future cleanup pass could collapse to (a) once every Texture2D-producing primitive has migrated to a typed signature; treat that as a Phase-6-style sweep after the per-primitive migration finishes.

### 17.6 Pool-key & runtime invariants

Both texture variants flow through the same `MTLBuffer` allocation path. `pool_key` collapses `Texture2DTyped(...)` to `Texture2D` so a typed producer's output recycles through the same pool entry as any untyped slot of matching format + dims. The channel signature has zero runtime impact — it's a validator-only tag.

Patterns that previously matched `PortType::Texture2D` for "is this a texture port?" purposes (canvas-scale propagation, output-dim inference, slot allocation in `effect_chain_graph`) widen to `PortType::is_texture_2d()` which covers both variants. No code path treats typed and untyped Texture2D differently at runtime.

### 17.7 Snapshot extension

The editor-facing `PortKindSnapshot` mirrors the runtime split with a new variant:

```rust
pub enum PortKindSnapshot {
    Texture2D,
    Texture2DTyped { slots: [String; 4] },
    // …
}
```

`From<PortType>` resolves each slot's `ChannelName` through `debug_name` (well_known + runtime registry from §6.1) so the hover-tooltip can render readable layouts like `Texture2D[R: flow_x, G: confidence, B: flow_y, A: valid]` directly. Unknown names fall back to hex hashes, same as the Array channel path.

### 17.8 `well_known` additions

Three texture-relevant names land in the registry with this section:

- `FLOW_X = "flow_x"` — horizontal component of an optical-flow vector (UV units; positive = right).
- `FLOW_Y = "flow_y"` — vertical component (positive = down per the Watercolor convention).
- `VALID = "valid"` — boolean-as-F32 validity mask for sparse / partial fields.

`CONFIDENCE` was already in the registry. The collision test in `channel_names::well_known::collision_tests` automatically extends to the new names — no parallel fixture to update.

The registry stays single-source-of-truth for any channel name shared across the catalog, regardless of whether it appears on an Array port or a Texture2DTyped port. If a future texture-only name (`AO`, `ROUGHNESS`, …) wants adding, it goes in the same `well_known_channels!` invocation under the appropriate category — there is deliberately no parallel `well_known_texture_channels!` registry.

### 17.9 Migration scope

Phase 17.A (this commit set): the type-system surface (types, macro, validator, snapshot) lands plus one primitive migrated to prove the surface — `node.optical_flow_estimate`'s `out` port. Every other Texture2D-producing primitive stays untyped; their existing wires continue connecting through the back-compat valve.

Phase 17.B (separate work): migrate the V2 WireframeDepth pipeline's typed-side consumers to declare matching signatures. That's the bug-fix migration this whole extension was built to unblock. Lives in a separate commit because Peter wants the type-system infrastructure verified standalone before depending on it for the fix.

Phase 17.C (eventual sweep): migrate every Texture2D-producing primitive in the catalog to declare its slot meanings. Catalog-wide; mechanical once the convention is settled; collapses the validator's two-variant Texture2D representation into one once nothing depends on the untyped fallback.

### 17.10 Forward-compatibility check (vs §16)

The fusion-compiler compatibility constraints in §16.4 stay satisfied:

- **C1 / C2 / C3 / C4 / C5:** `TextureChannels::slots` is a fixed-length `[ChannelName; 4]` known at compile time. Per-slot identity is the const-hashed `ChannelName` already in scope. Texture format determines per-slot element type at compile time (out of scope here; covered by the existing texture-format contract). The match check is exact equality on a fixed-length array — fully decidable at graph-compile time.
- **C6:** No new operators added. Future rename / reorder atoms for texture channels would be a separate decision; the type system doesn't presuppose them.
- **C7:** N/A — Texture2DTyped signatures come from the primitive's macro declaration, not from naga introspection. `wgsl_compute` outputs that are textures stay untyped Texture2D for now; typing them is a future concern that would extend the naga walk's texture-binding path.
- **C8:** Per-slot byte stride is fixed by the texture format (1, 2, 4 bytes per channel depending on format). No variant of the new types affects stride.

The §16 anti-pattern list also stays clean: no runtime-resolved slot count, no per-pixel variable layout, no introspection requirement for compatibility — all decisions are static at graph compile time. Same fusion-compatibility surface as the Array channel design.

---

## Appendix A — File checklist for Phase 1 implementation

Files added:
- `crates/manifold-renderer/src/node_graph/channel_names.rs` — `well_known_channels!` macro, generated `well_known::*` constants, debug-name lookup. The collision test is emitted by the same macro into this module's test mod — no separate test file.

Files modified:
- `crates/manifold-renderer/src/node_graph/ports.rs` — types reshape, `ItemKind` retained alongside (deletion in Phase 4), new types added.
- `crates/manifold-renderer/src/node_graph/validation.rs` — `channels_compatible` predicate, `ChannelMismatch` error variant, updated error display. Also adds `PERMISSIVE_PRIMITIVE_ALLOWLIST: &[PrimitiveTypeId]` const (see §11.4).
- `crates/manifold-renderer/src/node_graph/mod.rs` — re-export new types and `well_known`.

Tests added:
- `crates/manifold-renderer/src/node_graph/ports.rs` — std430 layout calculator unit tests.
- `crates/manifold-renderer/src/node_graph/validation.rs` — Channels compatibility unit tests; also a test enumerating every primitive with a Permissive port and asserting its `TYPE_ID` ∈ `PERMISSIVE_PRIMITIVE_ALLOWLIST`.
- `channel_names.rs` test mod — the macro-generated collision check.

Compile-time assertions:
- Each migrated `_SPECS` constant gets a drift assertion in its module's test mod (added in Phase 3, not Phase 1).

---

## Appendix B — Pre-Phase-0 readiness checklist

Before starting Phase 0 implementation (and by extension Phase 1, since the same pre-reading applies), an agent should:

- [ ] Read this document end-to-end.
- [ ] Read [BUFFER_PORT_PLAN.md](BUFFER_PORT_PLAN.md) to understand the existing Array port system this migration reshapes.
- [ ] Read [GRAPH_COMPILER.md](GRAPH_COMPILER.md) to understand the fusion-compatibility constraints in §16. Especially relevant if extending §4 (type contract) or §5 (validator semantics).
- [ ] Read [crates/manifold-renderer/src/node_graph/ports.rs](../crates/manifold-renderer/src/node_graph/ports.rs) — the current `PortType` / `ArrayType` / `ItemKind` shapes.
- [ ] Read [crates/manifold-renderer/src/node_graph/validation.rs:264](../crates/manifold-renderer/src/node_graph/validation.rs#L264) — the current `port_types_compatible` predicate.
- [ ] Read `crates/manifold-renderer/src/generators/mesh_common.rs` and `compute_common.rs` — the typed-array struct definitions and `KnownItem` impls this migration touches. Phase 0's smoke test starts from `EdgePair` here.
- [ ] Read [crates/manifold-renderer/src/node_graph/primitive.rs:671](../crates/manifold-renderer/src/node_graph/primitive.rs#L671) — the `__primitive_port_type!` macro arm Phase 2 extends. (Not required for Phase 0 — that phase bypasses the macro and constructs `ArrayType` directly — but worth glancing at to understand what Phase 2 will add.)
- [ ] Confirm understanding of std430 alignment rules: vec3 = 16-byte align with 12-byte size + 4-byte tail pad, vec4 = 16-byte align, vec2 = 8-byte align, scalars = 4-byte align.
- [ ] Confirm understanding of the `bytemuck::Pod` constraint: all fields visible / no private padding (which is why the existing structs have `pub _pad0` etc.).
- [ ] Confirm focused test discipline per [feedback_prefer_focused_tests.md](../.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/memory/feedback_prefer_focused_tests.md) — workspace tests run once in Phase 5, not per-chat.

---

End of document.
