# Adding a primitive

Authoring guide for the `primitive!` macro. Companion to [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) (design rationale + decomposition recipes) and [NODE_CATALOG.md](NODE_CATALOG.md) (the catalog of what's shipping today).

Primitives auto-register via `inventory::submit!` from inside the macro — dropping a file under `crates/manifold-renderer/src/node_graph/primitives/` is the only step required. Nothing else has to be edited to register a new primitive; `cargo build` picks it up and the palette + bundled-preset loader see it on next startup.

## Audit precondition (mandatory)

Before authoring any new primitive, complete the read-only audit per [DECOMPOSING_GENERATORS.md §2.5](DECOMPOSING_GENERATORS.md):

1. **Survey existing primitives** — `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g '*.rs'`. One line per node telling you what it does.
2. **Check the registered-but-unused atoms** — `mip_chain`, `uv_displace_by_flow`, `centered_uv`, `polar_field`, `distance_to_point`, `noise`, `depth_estimate_midas`, `blob_detect_ffi`, `blob_overlay_render`, `optical_flow_estimate`, `envelope_follower_ar`, `peak`, `render_3d_mesh`, `render_instanced_3d_mesh`, `generate_cube_mesh`, `generate_platonic_solid`, `generate_instance_transforms`, `integrate_particles`, and the unused noise/coordinate atoms. Many of these *exactly* cover what a new primitive proposal is reaching for; activate them by wiring them into your graph rather than building a new one. (Photoreal PBR is *not* an atom to wire up — it lives inside `node.render_3d_mesh`'s `node.pbr_material`; the standalone `cook_torrance_specular` / `equirect_envmap_sample` were removed 2026-05-30.)
3. **Read the nearest reference preset end-to-end** ([NODE_CATALOG.md §5 / §6.1](NODE_CATALOG.md), [DECOMPOSING_GENERATORS.md §2.5](DECOMPOSING_GENERATORS.md)).
4. **Reconcile your sketch** — state explicitly which existing primitives you'll reuse, which you'll extend, and which are genuinely new. State the audit findings in the PR description before any new-primitive code.

Skipping the audit produces the recurring "argue from snippets" anti-pattern and the bundle-as-primitive shortcut. The §2.5 precondition is a hard rule in `CLAUDE.md`.

## When to add one

The `≥2-use` filter applies (design doc §1.2):

- **Yes**: the math appears in 2+ existing effects/generators, OR the primitive replaces an existing effect 1:1 *and* it's at primitive granularity per the bundle-vs-atom criterion below.
- **No**: speculative future need; only one caller and not a 1:1 effect replacement; or — most commonly — a fused bundle of operations that should be expressed as a graph of atoms.

## What counts as "one primitive" (bundle-vs-atom criterion)

A primitive does **one composable thing**:

- **GPU compute / fragment** — one dispatch with one well-defined operation. Multiple operations in one shader is the bundle anti-pattern; build them as separate primitives and wire them in the graph.
- **DNN inference** — one inference call (e.g. `depth_estimate_midas`, `optical_flow_estimate`). The pre/post processing and the consuming effect are separate primitives.
- **FFI / native plugin** — one call (e.g. `blob_detect_ffi`). The filter and the render are separate.
- **CPU operation** — one operation (`envelope_follower_ar`, `peak`, `render_text`). Not a chain of CPU steps fused together.

What's **not** allowed:

- A "this is the whole effect" or "this is the whole generator" kernel that bundles multiple distinct dispatches behind a single primitive. The no-fused-monolith rule (`CLAUDE.md` hard rules, `DECOMPOSING_GENERATORS.md` §1.1) prohibits this.
- A primitive that wears primitive clothing but internally calls `dispatch_compute` multiple times for distinct operations. Each dispatch should be its own primitive.
- A primitive named after one consumer effect or generator (`my_effect_pipeline`, `digital_plants_render`, `fluid_simulate` bundling Euler + noise + diffusion). Those are bundles, not primitives.

What's **fine** when it's the right granularity:

- A single compute dispatch that does irreducible math — Cook-Torrance specular evaluation, a Lorenz ODE RK2 step, a Schwarzschild geodesic update — these are below the dispatch level (per-arithmetic-op decomposition would pay launch overhead for what should be inlined math). They stay as primitives.
- Curated families with N enum-selected variants where the variants are real user-facing aesthetic choices (e.g. attractor type, polytope shape). When the family is genuinely user-math (not just bundled operations), implement the backend as `wgsl_compute` with N shader strings + a "Custom" option per [DECOMPOSING_GENERATORS.md §5.6](DECOMPOSING_GENERATORS.md) — that's the curated-as-doorway pattern, not curated-as-wall.

## Files you touch per primitive

| File | Why |
|---|---|
| `crates/manifold-renderer/src/node_graph/primitives/<name>.rs` | The `primitive!` declaration + `Primitive::run` body |
| `crates/manifold-renderer/src/node_graph/primitives/shaders/<name>.wgsl` | Compute shader (only if your primitive runs GPU work — control-rate primitives like `value`/`math`/`lfo` don't need shaders) |
| `crates/manifold-renderer/src/node_graph/primitives/mod.rs` | `pub mod <name>;` to include the file |
| `crates/manifold-renderer/tests/parity_<effect>.rs` | Parity test vs the legacy effect this replaces (only when replacing a legacy fused shader) |

That's it. The macro generates the `EffectNode` impl, type-id constants, `PrimitiveSpec` metadata, the AI-surface `PrimitiveDescription`, and the `inventory::submit!` registration for the auto-populated palette.

## The codegen path is mandatory (per-element GPU atoms)

**Every barrier-free per-element GPU atom MUST ship on the freeze/graph-compiler codegen
path — never plain hand-WGSL that opts out of fusion.** (The scope test below defines
"barrier-free per-element" precisely — it replaces the earlier "single-dispatch" phrasing,
which wrongly admitted workgroup-barrier reductions like `peak`.) Peter's standing rule
(2026-07-11): all nodes, new and existing, must work perfectly with the graph compiler
in full. A plain-WGSL atom (one that builds its pipeline from
`create_compute_pipeline(include_str!("shaders/foo.wgsl"), …)`) is a hard **fusion
boundary**: it forces a VRAM round-trip and blocks the whole run of per-element atoms
around it from merging. A chain of them costs N GPU dispatches where a fused run costs
~1 — on the live rig with heavy meshes (scanned geometry, 10⁵–10⁶ verts) that is dropped
frames, i.e. a broken show. This is hot-path discipline at the instrument level, not an
optimization nicety.

The codegen authoring shape (reference: [`contrast.rs`](../crates/manifold-renderer/src/node_graph/primitives/contrast.rs)
texture-domain, [`displace_mesh.rs`](../crates/manifold-renderer/src/node_graph/primitives/displace_mesh.rs)
buffer + texture, [`neighbor_smooth.rs`](../crates/manifold-renderer/src/node_graph/primitives/neighbor_smooth.rs)
buffer gather):

1. **Author a `wgsl_body` fragment**, not a whole kernel. `shaders/<name>_body.wgsl`
   holds a single `fn body(idx, count, e_in: Element, <textures/samplers>, <params…>) -> Element`
   (texture atoms take/return the pixel; buffer atoms the struct element). The codegen
   wraps it with the read-once → math-in-registers → write-once boilerplate.
2. **Declare the freeze markers in the `primitive!`** — `fusion_kind:` (`Pointwise` for
   per-element ops; `Source`/`MultiInputCoincident`/`Boundary` per `freeze/classify.rs`)
   and `input_access:` (`[Coincident]` default — omit it; `[CoincidentTexel]` for exact
   texel loads; `[BufferGather]` when the body reads neighbours/other indices from an
   `Array` input).
3. **Build the runtime pipeline from `standalone_for_spec::<Self>()`**, not a hand WGSL
   string:
   ```rust
   let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
       .expect("node.<name> standalone codegen");
   gpu.device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, "node.<name>")
   ```
4. **Prove the kernel's values in `gpu_tests`**: dispatch the `standalone_for_spec`
   kernel on a fixture and assert element-wise equality against CPU-computed expected
   output (the `*matches_hand_formula*` pattern, e.g. `mesh_ramp.rs`). If the atom can
   fuse with a neighbor, the fused-vs-unfused fusion proof is the second mandatory check.
   **RETIRED (Peter, 2026-07-20, W1-B):** the former generated-vs-hand-KERNEL parity
   tests and their mirror `shaders/<name>.wgsl` oracles — both runtime paths are
   generated now; the hand kernels only re-proved the node-graph migration. Do not add
   new ones.

**Scope — the in/out test (2026-07-11).** An atom is IN the mandate iff its kernel is a
**barrier-free pure per-element function**: one thread computes one output element,
expressible as a `fn body(idx/uv, e_in, <inputs at their declared access kinds>,
<params>) -> Element` fragment — no `var<workgroup>` shared memory, no
`workgroupBarrier`, no atomics, no cross-frame state, and the element data originates
on the GPU. An atom that fails the test falls into exactly one of these exclusions —
name which one when you claim an exemption:

1. **Barriered reduction / multi-pass scan** — exempt. `peak` and `luminance`
   (workgroup-shared reductions), `spawn_from_mesh` / `scatter_on_mesh`
   (area→scan→place). A single dispatch that needs workgroup memory + barriers is
   structurally multi-pass — which is why "single-dispatch" was the wrong axis.
   Forcing these into one `standalone_for_spec` kernel would violate the
   no-fused-monolith rule.
2. **Cross-frame state** — exempt. `temporal` (`node.feedback`): the state texture must
   materialize in VRAM to survive the frame, so there is no round-trip to fuse away.
   The freeze compiler already fuses *around* it — state-capture wires are excluded
   from the forward graph.
3. **IO endpoints and CPU bridges** — exempt. Uploads (`image_folder`,
   `gltf_texture_source`): the data is not a function of anything on the GPU.
   Readback bridges (`color_sample`, and `peak`/`luminance` again): the consumer is
   the CPU. Either way the texture/scalar must materialize regardless.
4. **Draw-call rasterization** — exempt. The `render_*` family are render passes, not
   compute.
5. **BLOCKED is not exempt.** An atom that PASSES the test but has an input the codegen
   cannot yet express is *blocked on a tracked codegen gap* — the mandate still
   applies, and the debt lives in the compiler, not the atom. Past case (closed): the
   `draw_*` family (per-pixel bodies that index a marks `Array` — texture-domain
   codegen had no storage-array read-path). Fixed by `InputAccess::BufferIndex`
   (FUSION_SOTA_DESIGN.md D3/P4a+P4b, BUG-114) — a texture-domain atom now tags such
   an input `BufferIndex` and the codegen binds it as `buf_<port>: array<Element>`,
   synthesized from the port's `Channels[…]` layout; see `draw_dots.rs`/
   `draw_connections.rs` (the latter proves two BufferIndex-tagged inputs on one atom).

Non-GPU primitives (control-rate `value`/`math`/`lfo`, DNN/FFI/CPU atoms) are outside
the rule entirely.

Machine-check the classification you just declared: `graph_tool fusion <preset.json>`
prints every node's `fusion_kind`/`boundary_reason` and which region (if any) it
actually joined, over the real `partition_regions` pass — faster than wiring the
primitive into a preset and eyeballing the freeze debug output. See
`docs/GRAPH_TOOLING_DESIGN.md`.

## Skeleton

The skeleton below is a per-element texture atom, so it is on the codegen path per
the rule above — note `fusion_kind`/`wgsl_body` in the macro and `standalone_for_spec`
in `run()`. (`summary`/`category`/`role`/`aliases` are the current metadata fields;
`category`/`role` use the semantic enums in `primitive.rs`, distinct from the
`picker.category` palette bucket.)

```rust
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: Invert,
    type_id: "node.invert",
    purpose: "Invert RGB channels, blended against the source by intensity. The `intensity` input port is the standard port-shadow — wire any scalar producer to drive the blend in real time.",
    inputs: {
        in: Texture2D required,
        intensity: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "1:1 replacement for the legacy InvertColors effect.",
    examples: ["preset.effect.invert"],
    picker: { label: "Invert", category: Atom },
    summary: "Flips every colour to its opposite, blended against the original by intensity.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["invert", "negate", "Invert TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/invert_body.wgsl"),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertUniforms {
    intensity: f32,
    _pad: [f32; 3],
}

impl Primitive for Invert {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let intensity = match ctx.params.get("intensity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let Some(in_tex) = ctx.inputs.texture_2d("in") else { return };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else { return };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. `shaders/invert.wgsl` is
            // retained only as the gpu_tests parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.invert standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.invert",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = InvertUniforms { intensity, _pad: [0.0; 3] };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Texture { binding: 3, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "primitive.invert",
        );
    }
}
```

The `primitive!` declaration generates:

- `pub struct Invert { pub pipeline: ..., pub sampler: ... }` with `new()` and `Default`.
- `impl PrimitiveSpec for Invert` with all const arrays, AI metadata, and a `cached_type_id()` backed by a per-primitive `OnceLock`.
- The blanket `impl<P: Primitive> EffectNode for P` in `node_graph::primitive` then provides the full `EffectNode` surface — no further trait impl needed.

## Macro reference

```text
primitive! {
    name: <StructName>,                 // PascalCase struct identifier
    type_id: "node.<name>",             // stable string; renaming breaks saved graphs
    purpose: "<one sentence>",          // shown in editor + AI surface
    inputs: {
        <port_name>: <PortType> [required|optional],
        ...
    },
    outputs: {
        <port_name>: <PortType>,
        ...
    },
    params: [
        ParamDef { name: ..., label: ..., ty: ParamType::..., default: ParamValue::..., range: ..., enum_values: &[] },
        ...
    ],
    composition_notes: "<optional>",    // when to choose this over alternatives
    examples: [ "<preset_id>", ... ],   // preset graphs that use this primitive
    picker: { label: "<Display>", category: <Atom|Color|Spatial|Stylize|Filmic|Driver|Math|Source|Diagnostic> },
    summary: "<one plain-language sentence>",   // human-facing description
    category: <ColorAndTone|Geometry3D|…>,      // semantic category enum (primitive.rs)
    role: <Filter|Source|Sink|…>,               // semantic role enum (primitive.rs)
    aliases: ["<search alias>", ...],           // palette search synonyms
    fusion_kind: <Pointwise|Source|MultiInputCoincident|Boundary>,  // REQUIRED for per-element GPU atoms (freeze/classify.rs)
    wgsl_body: include_str!("shaders/<name>_body.wgsl"),            // the fusable body fragment — REQUIRED with fusion_kind
    input_access: [<Coincident|CoincidentTexel|BufferGather|...>],  // per-input; omit = Coincident. BufferGather when the body reads other indices
    extra_fields: {
        <field>: <type> = <init expr>,  // additional struct fields beyond pipeline/sampler
        ...
    },
}
```

- **`fusion_kind` + `wgsl_body` (+ `input_access`) are mandatory for every
  per-element GPU atom** — see "The codegen path is mandatory" above. Omitting
  them ships a plain-WGSL fusion-boundary atom, which is not permitted. Multi-pass
  primitives (barrier-separated passes) and non-GPU primitives don't carry them.

- `<PortType>` is one of `Texture2D`, `Texture3D`, `ScalarF32`, `ScalarV2`, `ScalarV3`, `Camera`, `Light`, `Material`, `Array(T)`, `Channels<T>`, `Channels[name: Type, ...]`, or `Channels[permissive]`. Scalar input ports are first-class — use them directly in the macro (no manual `ParamDef` workaround needed).
- **Array / Channels wires** carry a flat list of structured items (particles, vertices, blob rectangles, etc.). Three equivalent ways to declare:
  - `Array(T)` — `T` is a `#[repr(C)] + bytemuck::Pod` struct with a `KnownItem` impl that supplies `const SPECS: &[ChannelSpec]`. The macro folds T's specs into the wire's Channels signature automatically. Canonical for the seven typed families (`Particle`, `MeshVertex`, `Vec4Vertex`, `InstanceTransform`, `CurvePoint`, `EdgePair`, plus `u32`/`f32`/`[f32; 2]` for bare scalars).
  - `Channels<T>` — equivalent shorthand for `Array(T)`. Same emission. Pick whichever reads better at the declaration site.
  - `Channels[name: Type, ...]` — inline syntax for ad-hoc signatures (no `KnownItem` impl). `name` is either a bare ident resolving against `crate::node_graph::channel_names::well_known::*` (e.g. `POSITION`, `WIDTH`, `A_INDEX`) OR a string literal (e.g. `"my_local_channel"`). `Type` is one of `F32`, `I32`, `U32`, `Vec2F`, `Vec3F`, `Vec4F`. Mix idents and literals freely within the same `Channels[...]`. Used for `wgsl_compute` outputs and any primitive whose wire shape doesn't fit a typed family.
  - `Channels[permissive]` — opt-in for generic transform operators (`node.rename_channel`, `node.reorder_channels`, etc.) whose input port accepts any Channels producer regardless of signature. The `pub const PERMISSIVE_PRIMITIVE_ALLOWLIST` in `validation.rs` gates which primitives may legitimately use this — see `docs/CHANNEL_TYPE_SYSTEM.md` §11.4.
- Inputs default to `required`; mark optional with the `optional` keyword.
- **Port-shadows-param convention.** If you declare a scalar input port with the same name as a `ParamDef` (e.g. `gain` in both `inputs:` and `params:`), the wire wins when present, the param is the fallback. Standard pattern for any control-rate modulation. The graph editor disables the expose checkbox + value cell on wire-driven rows automatically.
- `picker: { label, category }` declares how the palette and effect-card UI surface this primitive. Categories used today: `Color`, `Spatial`, `Stylize`, `Filmic`, `Driver` (texture→scalar bridges), `Math` (scalar arithmetic / LFO / BeatGate), `Source` (constants / generators), `Diagnostic`.
- `composition_notes`, `examples`, `picker`, and `extra_fields` are optional. Omit the keyword entirely if you don't need it.

### Stateful primitives

Per-frame state (a previous frame's texture, a one-pole filter's running value, a background worker handle) lives in the `StateStore` keyed by `(owner_key, node_id)`. Two patterns:

- **Single-instance state on the primitive struct** — use `extra_fields:` to add fields. Fine for state that doesn't survive an effect-chain rebuild. `Feedback`'s pipeline / sampler caches use this.
- **StateStore-backed state** — implement `run` with `EffectNodeContext::state_mut::<MyState>()`. State survives chain rebuilds as long as `owner_key` is stable. `Feedback`'s `prev: RenderTarget` and `Smoothing`'s `previous: f32` use this. The chain runtime threads the `StateStore` through `execute_frame_with_state`.

Stateful primitives must also wire up `clear_state` so seek / layer-idle resets clear properly — see [EFFECT_CHAIN_LIFECYCLE.md](EFFECT_CHAIN_LIFECYCLE.md).

## Parity test (only if you're replacing a legacy effect)

`crates/manifold-renderer/tests/parity_invert.rs`:

```rust
mod parity;

use manifold_core::EffectTypeId;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

#[test]
fn invert_decomposes_pixel_exactly_across_all_fixtures() {
    let mut h = ParityHarness::new();
    let fx = make_default_effect(EffectTypeId::INVERT_COLORS);
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);
        let legacy = h.run_legacy(&fx, &input, &ctx);
        let decomposed = h.run_primitive_graph::<Invert>(&fx, &input, &ctx);
        assert_bytewise_equal(
            &format!("invert/{:?} legacy vs primitive", fixture),
            &legacy,
            &decomposed,
        );
    }
}
```

(`run_primitive_graph` will be added to the harness when the first migration lands.)

## What NOT to do

- **Don't write `impl EffectNode for <Name>` by hand.** The blanket impl in `node_graph::primitive` covers it. Hand-written `EffectNode` impls in `primitives/*.rs` predate the macro and will be migrated in place.
- **Don't hand-roll the runtime kernel for a per-element GPU atom.** Author a
  `wgsl_body` fragment and let `standalone_for_spec::<Self>()` generate the fusable
  kernel (see "The codegen path is mandatory"). You still write WGSL by hand — the body
  fragment, and the full-kernel parity oracle — you just don't bind a hand `include_str!`
  kernel as the *runtime* pipeline. `cargo test` runs `tests/wgsl_validation.rs` which
  validates every shader (bodies and oracles) via naga.
- **Don't ship a per-element GPU atom as plain WGSL that opts out of fusion.** No
  `create_compute_pipeline(include_str!("shaders/<name>.wgsl"), …)` as the runtime path —
  that's a fusion boundary. Codegen path only; the generated-vs-hand parity test proves it.
- **Don't skip the parity test when replacing an existing effect.** Strict bit-equality is the gate.
- **Don't add a primitive for speculative future use.** The `≥2-use` filter is enforced at review time.
- **Don't ship a fused single-effect / single-generator bundle.** If your primitive internally orchestrates multiple distinct dispatches that each do a different operation, that's a graph, not a primitive. Build the atoms separately and wire them in JSON. The recurring failure mode in past decomposition passes was reaching for a fused kernel to pass parity quickly; the no-fused-monolith rule prohibits this regardless of parity-test pressure. If parity drift is the concern, spec intermediate texture formats up to `Rgba32Float` to eliminate the rounding gap — the bandwidth cost is negligible on M-series and the bundle-as-primitive cost is structural.
- **Don't touch the kernel without re-reading the purpose.** `purpose` states the math (`docs/archive/NODE_VOCABULARY_AUDIT.md` §2.6) and lives right next to the shader in the same file, on screen during the edit — there's no excuse for it drifting. Touch the kernel, re-read the purpose.
- **Rasterizer with texture inputs → declare `output_canvas_scale` `(1, 1)`.** The plan compiler's default sizes a node's output as *max of its texture input dims* — right for image-processing nodes, wrong for a rasterizer whose texture inputs are scene resources (envmap, base-color/normal maps, LUT-like lookups): without the declaration your render target inherits the largest wired map's dims instead of the canvas. BUG-140 shipped exactly this — imported glb scenes rendered into the envmap's 1024×1024 and were stretched to canvas (aspect distortion + resolution loss). `render_scene` / `render_3d_mesh` / `render_instanced_3d_mesh` are the reference impls (`impl Primitive` override, one method). Explicit declarations beat the max-of-inputs heuristic since 2026-07-12.

## Parity-without-fusion

Most "we have to fuse for parity" claims don't survive scrutiny. The two real precision concerns are:

1. **Intermediate-storage format precision.** Fp16 intermediates lose bits relative to fp32 register math. Fix: spec the intermediate texture format as `Rgba32Float` (or whatever the legacy register precision was). Bit-exact parity restored.
2. **Atomic-add ordering in scatter passes.** Different dispatch counts can reshuffle atomic resolve order, producing 1-ULP-level differences. This is not really a parity issue — write the parity test to tolerate it with a small noise-floor epsilon, the way the DigitalPlants tests do.

If neither applies, the operation decomposes cleanly. The narrow exception is multi-pass shaders where the inter-pass coupling *and* the per-pass texture format choices are both load-bearing for numerical stability (FluidSim's 7-pass chain, BlackHole's geodesic + atomic splat) — those are §5 of `DECOMPOSING_GENERATORS.md`, and they reach for `wgsl_compute` as the per-pass escape hatch, not for a fused monolith.
