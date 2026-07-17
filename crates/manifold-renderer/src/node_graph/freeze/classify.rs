//! Fusion classification metadata (design doc §12, §3).
//!
//! Every primitive declares — via the `primitive!` macro, defaulting to the
//! conservative [`FusionKind::Boundary`] — whether and how it can fold into a
//! fused kernel. The fusion region-grower reads this off each node (through
//! [`EffectNode::fusion_kind`](crate::node_graph::effect_node::EffectNode::fusion_kind))
//! to grow maximal same-domain pure regions and cut at the rest. Conservative
//! by construction: an unclassified atom never fuses.

/// How a primitive participates in fusion.
///
/// For v1 (texture-pointwise), the two fusable kinds carry an implied
/// contract that keeps the classifier simple: both iterate **output-sized**
/// (grid from the destination) and read every input at the **same element**
/// (own pixel / coincident UV). Richer per-input read-semantics — a
/// texel-load atom (dither) that can't cross a resolution seam, or a
/// dependent gather — get their own variants + per-input markers when the
/// first such atom is converted; adding them is additive and does not
/// invalidate existing `Pointwise`/`MultiInputCoincident` atoms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FusionKind {
    /// Not fusable — the default for every primitive until it opts in.
    /// CPU/control nodes, stateful nodes (feedback/accumulators), gathers,
    /// resamples, IO endpoints. A region is cut at every boundary, so the
    /// compiler only ever fuses what an atom explicitly declares fusable.
    #[default]
    Boundary,
    /// Reads only its own element (own pixel / own particle) and writes one
    /// element. Output-sized iteration, same-element read. The textbook
    /// fusable atom — gain, contrast, saturation, hue_saturation, colorize,
    /// clamp_texture.
    Pointwise,
    /// Reads the SAME element from N≥2 inputs (coincident) and writes one.
    /// Output-sized iteration. Fusable when all inputs resolve to the same
    /// element-space (the DD10 resolution-seam guard enforces this). e.g.
    /// `node.mix` — `a` and `b` sampled at the same UV.
    MultiInputCoincident,
    /// Generator: reads NO texture input, produces one element from the
    /// fragment's position + params (checkerboard, uv_field, gradients, noise,
    /// voronoi, the fold coordinate-fields). The body is `fn body(uv, dims,
    /// ...params)` — no colour arg. Output-sized iteration. The standalone kernel
    /// binds no textures/sampler beyond its output (and no uniform if paramless).
    /// A Source atom CAN head a region as its producer — the region-grower
    /// admits a 0-input generator as the region's sole entry point and threads
    /// its output to downstream members as a register
    /// (`source_generator_heads_a_region`); buffer-domain generators fuse the
    /// same way into buffer regions (FluidSim, DigitalPlants).
    Source,
}

impl FusionKind {
    /// Whether this primitive can be folded into a fused kernel at all.
    /// `Boundary` is the only non-fusable kind.
    pub fn is_fusable(self) -> bool {
        !matches!(self, FusionKind::Boundary)
    }
}

/// How a single texture input is READ by a fusable atom's body — the
/// read-semantics axis, orthogonal to the channel/type axis (what's *on* the
/// wire). A fusable atom tags each texture input with one of these via
/// `INPUT_ACCESS` (aligned to the TEXTURE inputs in `INPUTS` order); the codegen
/// emits one read-path per kind, and the region-grower enforces each kind's
/// fusion constraint. This is the unit that lets a new atom slot in by "tag your
/// inputs" instead of growing a bespoke node category each time.
///
/// GPU input access is a CLOSED, small set, extended additively as each new
/// read-path is built — never a re-tag of the atoms already shipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputAccess {
    /// Read at the fragment's own coordinate, resolution-ROBUST: the codegen
    /// samples through a sampler at the fragment UV (standalone) or threads the
    /// in-region register (fused). The default for every texture input — covers
    /// pointwise (own pixel) and coincident multi-input (mix). A differently
    /// sized producer is rescaled by the sampler, so it fuses across a resolution
    /// seam safely.
    #[default]
    Coincident,
    /// Read at the fragment's own integer texel, EXACT (`textureLoad`, no
    /// filter). Correct only when the producer matches the output resolution —
    /// sampling would blend neighbours and corrupt the value (e.g. dither's
    /// ordered-threshold pattern, where each texel IS a distinct threshold). The
    /// region-grower must refuse to fuse a `CoincidentTexel` input across a
    /// resolution seam (design §11.B / line 147).
    CoincidentTexel,
    /// Read at a coordinate the BODY computes — a dependent sample (the UV-warp
    /// family: remap, chromatic_displace, uv_displace_by_flow). The codegen
    /// CANNOT pre-sample this into a register (it doesn't know the coord), so the
    /// body receives the texture + sampler as ARGS and samples them itself,
    /// owning the exact filter/address-mode of the unfused atom ("pure modulo
    /// declared sampled-texture args", design §11.B / line 156). A node with a
    /// Gather input CAN be a region member — the region-grower just refuses to
    /// union the WIRE feeding it (a gather-consumed wire never merges into a
    /// threaded register; the gathered producer stays an external the body
    /// samples via its own sampler), not the node itself. Stencil absorption
    /// (`absorb_virtual_chains`) goes further and can fold a short producer
    /// chain into the fetch, recomputed at each tap instead of a canvas
    /// round-trip.
    Gather,
    /// Like [`Gather`], but the body reads via INTEGER `textureLoad` at a voxel/
    /// texel coordinate it computes — NO sampler, no filtering. The neighbourhood
    /// finite-difference / toroidal-wrap family that loads exact integer texels
    /// (gradient_central_diff_3d, curl_slope_force_3d, the wrap-modulo fields). The
    /// codegen binds the texture but no sampler, and the body receives only the
    /// texture handle (it computes the integer coord from `uv`/`dims`). Same
    /// region-boundary treatment as `Gather`.
    GatherTexel,
    /// Buffer-domain gather: the body reads arbitrary elements of an input
    /// storage `array` (grid neighbours, scatter targets, random-access lookups).
    /// It references the codegen-emitted input array global `buf_<port>` and
    /// computes its own element indices, so — exactly like the texture
    /// [`Gather`] — the wire feeding it never unions into a threaded register;
    /// the gathered array stays a bound `var<storage, read>` input the body
    /// indexes itself. Buffer atoms DO fuse into multi-node buffer regions
    /// (`classify_buffer_node` in `region.rs`) — FluidSim / FluidSim3D /
    /// DigitalPlants ship as fused, bit-exact buffer regions (freeze §7.3); a
    /// `BufferGather` input just means that one wire stays external, same as
    /// texture `Gather`.
    BufferGather,
    /// TEXTURE-domain read of a storage `Array`/`Channels` input, by indices
    /// the body computes — the array-into-texture read path (closes BUG-114,
    /// `docs/FUSION_SOTA_DESIGN.md` D3). Only ever tags an `Array`-typed input
    /// on an otherwise texture-domain atom (the `draw_*` family: a soft dot /
    /// marker / tick / gauge / scanline / connection layer that reads a
    /// detections array while writing the output pixel). Semantically this is
    /// [`BufferGather`]'s convention — the body references the codegen-emitted
    /// global `buf_<port>` directly and indexes it itself, no pre-read, no
    /// body arg — just HOSTED IN a texture-domain kernel instead of a buffer
    /// one. The region-grower never unions across a `BufferIndex`-consumed
    /// wire (same "gather never unions" contract as texture `Gather` and
    /// `BufferGather`): the array producer stays external, bound as
    /// `var<storage, read> buf_<port>: array<ExtK>` (standalone) or
    /// `src_<slot>` (fused, riding the existing external-slot numbering
    /// texture externals already use — an external is just a producer +
    /// element-type pair, texture or array). `ExtK` is synthesized from the
    /// port's `Channels[…]` layout by the same helpers the buffer codegen
    /// path already uses (`buffer_element_type`/`emit_buffer_struct`), so the
    /// mechanism generalizes to every `draw_*` atom's own detections/marks
    /// signature, not just `draw_dots`' `Detection`.
    BufferIndex,
}

/// Why a Boundary primitive is excused from the codegen-path mandate
/// (`docs/ADDING_PRIMITIVES.md` §"The codegen path is mandatory",
/// `docs/GRAPH_TOOLING_DESIGN.md` D4). A closed enum: every currently-Boundary
/// primitive declares exactly one of these reasons (via the `primitive!`
/// macro's `boundary_reason:` field, or a direct
/// [`EffectNode::boundary_reason`](crate::node_graph::effect_node::EffectNode::boundary_reason)
/// override for hand-impl primitives). The compiler stays
/// conservative — `FusionKind` still defaults to `Boundary` — this enum is
/// the POLICY layer that makes every atom's excuse for staying Boundary
/// visible and enforced (`every_boundary_atom_declares_its_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryReason {
    /// CPU / control-rate op — no GPU kernel to fuse at all (value, math,
    /// lfo, camera/light/material param setters, CPU array ops).
    NonGpu,
    /// Workgroup-barrier reduction or multi-pass scan/reduce — a single
    /// dispatch would need `var<workgroup>` + barriers, which the
    /// no-fused-monolith rule forbids folding into one kernel (`peak`,
    /// `luminance`, `spawn_from_mesh`, `scatter_on_mesh`).
    BarrieredReduction,
    /// Cross-frame GPU state — the primitive's output must materialize in
    /// VRAM to survive into next frame's input, so there is no VRAM
    /// round-trip to fuse away (`node.feedback`, `node.array_feedback`).
    CrossFrameState,
    /// Upload, readback, or DNN/FFI bridge — the data enters or leaves the
    /// GPU and is not purely a function of GPU inputs (`image_folder`,
    /// `gltf_texture_source`, `gltf_mesh_source`, `color_sample`,
    /// depth-estimator / blob-detector / optical-flow / person-segment
    /// atoms, `wgsl_compute`'s user-authored full kernel).
    IoBridge,
    /// `render_*` rasterization pass — a draw call, not a compute dispatch.
    DrawCall,
    /// Fused bundle awaiting decomposition into atoms — dies with the bundle
    /// (`cylinder_wrap_field`, `torus_wrap_field`, `digital_plants_render`,
    /// `nested_cubes_geometry`).
    FusedBundle,
    /// Passes the barrier-free per-element scope test, but the codegen
    /// can't yet express one of its inputs (the `draw_*` family's
    /// array-into-texture read — tracked BUG-114/115). BLOCKED is not
    /// exempt: the debt lives in the compiler, not the atom.
    Blocked,
    /// Owed a `wgsl_body` conversion — legal ONLY for the `type_id`s in
    /// `CONVERSION_DEBT_LEDGER` (seeded from the 2026-07-11 sweep triage).
    /// Converting an atom removes it from the ledger; the meta-test fails
    /// if a listed atom becomes fusable (stale ledger) or if an
    /// undeclared atom claims this reason without a ledger entry.
    ConversionDebt,
}

/// The exact set of `type_id`s legally allowed to declare
/// `BoundaryReason::ConversionDebt` (design doc D5). Seeded verbatim from the
/// 2026-07-11 conversion-sweep triage's wave 1–3 atom list, transcribed at
/// P2 time — two triage names (`affine_transform`/`node.transform`,
/// `lambert_directional`/`node.basic_light`) had already been converted to
/// `FusionKind::Pointwise` by 2026-07-13 and are correctly NOT here (see the
/// P2 landing report's escalation list). Converting an atom removes it from
/// this list — a deliberate, review-visible edit; adding an atom without
/// converting it is not permitted (the meta-test below checks both
/// directions).
pub const CONVERSION_DEBT_LEDGER: &[&str] = &[
    // watercolor — NOT a mechanical wgsl_body conversion candidate: this is
    // a 7-pass sequential composite (grain+max, flow-gen, displace,
    // diffusion blur, slope displace, luma blur into persistent
    // cross-frame feedback, wet/dry blend), not a single barrier-free
    // per-element function. It fails the codegen-path scope test on two
    // independent grounds — cross-frame GPU state (the `feedback` texture
    // must survive into next frame, matching `BoundaryReason::CrossFrameState`'s
    // own `node.feedback` example) and multi-pass dependent composition
    // (matching `BoundaryReason::FusedBundle`'s "awaiting decomposition
    // into atoms" — `docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md`
    // already tracks the real fix: decompose into `flow_field_noise` +
    // `uv_displace_by_flow` (both registered, unused) + existing
    // blur/displace atoms, composed as a graph). Left in this ledger
    // (2026-07-14 wave 3) rather than reclassified, pending that
    // decomposition design — same category as DigitalPlants/NestedCubes,
    // explicitly out of scope for a mechanical wgsl_body conversion.
    "node.watercolor",
];

impl InputAccess {
    /// Whether the body computes its own read coordinate / index (a dependent
    /// read the region-grower can't thread as a register). Both texture gather
    /// flavours and the buffer gather qualify.
    pub fn is_gather(self) -> bool {
        matches!(
            self,
            InputAccess::Gather
                | InputAccess::GatherTexel
                | InputAccess::BufferGather
                | InputAccess::BufferIndex
        )
    }

    /// Whether this access reads through an EXACT `textureLoad` (or the
    /// buffer-domain equivalent, an indexed storage-array read) rather than a
    /// filtering sampler. `docs/DEPTH_RELIGHT_DESIGN.md` D6(a): only these
    /// variants are safe to back with a non-filterable `Rgba32Float`
    /// intermediate on Apple GPUs — `Coincident`/`Gather` bind a real
    /// sampler (`textureSampleLevel`) that a non-filterable format can't
    /// serve.
    pub fn is_texel_exact(self) -> bool {
        matches!(
            self,
            InputAccess::CoincidentTexel
                | InputAccess::GatherTexel
                | InputAccess::BufferGather
                | InputAccess::BufferIndex
        )
    }

    /// Whether this access reads through a FILTERING sampler
    /// (`textureSampleLevel` with a possibly-linear filter) — the converse
    /// of [`is_texel_exact`](Self::is_texel_exact) for the two texture-domain
    /// variants a non-filterable `Rgba32Float` producer cannot serve.
    pub fn is_filtering_sampler(self) -> bool {
        matches!(self, InputAccess::Coincident | InputAccess::Gather)
    }
}

/// The [`InputAccess`] a specific texture input port actually uses, resolved
/// the same way the fusion codegen resolves it: `node.input_access()` is
/// aligned to the node's TEXTURE-typed inputs in declaration order (scalar/
/// control ports skipped), defaulting to [`InputAccess::Coincident`] for any
/// port past the end of the declared list (`PrimitiveSpec::INPUT_ACCESS`'s
/// own documented default). Returns `None` if `port_name` doesn't name a
/// texture input on this node at all.
pub fn input_access_of(
    node: &dyn crate::node_graph::effect_node::EffectNode,
    port_name: &str,
) -> Option<InputAccess> {
    let access = node.input_access();
    let mut texture_index = 0usize;
    for input in node.inputs() {
        if !matches!(
            input.ty,
            crate::node_graph::ports::PortType::Texture2D
                | crate::node_graph::ports::PortType::Texture2DTyped(_)
                | crate::node_graph::ports::PortType::Texture3D
        ) {
            continue;
        }
        if input.name.as_ref() == port_name {
            return Some(access.get(texture_index).copied().unwrap_or_default());
        }
        texture_index += 1;
    }
    None
}

/// Render a node's fusion classification as the single stable string both
/// `catalog_gen` (the `fusion` catalog field, design D3) and `graph_tool
/// fusion` (design D2/D10) print — one implementation, so the catalog and
/// the CLI can never disagree about what a `FusionKind`/`BoundaryReason`
/// pair means: `"pointwise"` | `"source"` | `"multi_input_coincident"` |
/// `"boundary:<reason_snake_case>"`.
pub fn fusion_kind_str(node: &dyn crate::node_graph::effect_node::EffectNode) -> String {
    match node.fusion_kind() {
        FusionKind::Pointwise => "pointwise".to_string(),
        FusionKind::Source => "source".to_string(),
        FusionKind::MultiInputCoincident => "multi_input_coincident".to_string(),
        FusionKind::Boundary => match node.boundary_reason() {
            Some(BoundaryReason::NonGpu) => "boundary:non_gpu".to_string(),
            Some(BoundaryReason::BarrieredReduction) => "boundary:barriered_reduction".to_string(),
            Some(BoundaryReason::CrossFrameState) => "boundary:cross_frame_state".to_string(),
            Some(BoundaryReason::IoBridge) => "boundary:io_bridge".to_string(),
            Some(BoundaryReason::DrawCall) => "boundary:draw_call".to_string(),
            Some(BoundaryReason::FusedBundle) => "boundary:fused_bundle".to_string(),
            Some(BoundaryReason::Blocked) => "boundary:blocked".to_string(),
            Some(BoundaryReason::ConversionDebt) => "boundary:conversion_debt".to_string(),
            None => "boundary:undeclared".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::FusionKind;
    use crate::node_graph::effect_node::EffectNode;
    use crate::node_graph::primitives::Gain;

    /// Every registered (non-fixture) primitive is either fusable or names
    /// its `BoundaryReason` — the enforcement half of D4/D5
    /// (docs/GRAPH_TOOLING_DESIGN.md). `node.__*` fixtures only register
    /// under `cfg(test)` and are excluded the same way
    /// `catalog_gen::is_test_fixture` and
    /// `primitives::mod::every_conventional_array_port_declares_a_channels_signature`
    /// already carve them out. Every primitive must satisfy
    /// `is_fusable() XOR boundary_reason().is_some()` — there is no
    /// undeclared middle.
    #[test]
    fn every_boundary_atom_declares_its_reason() {
        use super::{BoundaryReason, CONVERSION_DEBT_LEDGER};
        use crate::node_graph::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();
        let mut conversion_debt_holders: Vec<&str> = Vec::new();

        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));

            let fusable = node.fusion_kind().is_fusable();
            let reason = node.boundary_reason();

            if reason == Some(BoundaryReason::ConversionDebt) {
                conversion_debt_holders.push(type_id);
            }

            if fusable == reason.is_some() {
                violations.push(format!(
                    "{type_id}: fusable={fusable}, boundary_reason={reason:?} — \
                     every primitive must be fusable XOR declare a BoundaryReason \
                     (fusable atoms must NOT also declare a reason; Boundary atoms \
                     MUST declare exactly one)",
                ));
            }
        }

        for &ledger_id in CONVERSION_DEBT_LEDGER {
            if !conversion_debt_holders.contains(&ledger_id) {
                violations.push(format!(
                    "{ledger_id}: listed in CONVERSION_DEBT_LEDGER but the registered \
                     primitive no longer declares BoundaryReason::ConversionDebt — either \
                     it was converted (remove it from the ledger) or the declaration was lost",
                ));
            }
        }
        for &holder in &conversion_debt_holders {
            if !CONVERSION_DEBT_LEDGER.contains(&holder) {
                violations.push(format!(
                    "{holder}: declares BoundaryReason::ConversionDebt but is not in \
                     CONVERSION_DEBT_LEDGER — add it deliberately or use a different reason",
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "boundary_reason declaration violations:\n  {}",
            violations.join("\n  "),
        );
    }

    /// `docs/DEPTH_RELIGHT_DESIGN.md` D6(a): a `precision_critical` input
    /// declares that its producer benefits from an `Rgba32Float` intermediate
    /// — but that promotion is only safe if THIS atom itself reads the input
    /// via an exact `textureLoad` (`InputAccess::is_texel_exact`), never a
    /// filtering sampler. Marking a `Coincident`/`Gather` input critical
    /// would be self-defeating: the format-selection seam would hand this
    /// very atom a non-filterable texture its own `textureSampleLevel` read
    /// can't correctly serve on Apple GPUs. Walks every registered primitive
    /// and asserts every name in `precision_critical_inputs()` both (a)
    /// resolves to a real texture input and (b) is texel-exact.
    #[test]
    fn precision_critical_inputs_are_texel_exact() {
        use super::input_access_of;
        use crate::node_graph::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();

        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));

            for &name in node.precision_critical_inputs() {
                match input_access_of(node.as_ref(), name) {
                    None => violations.push(format!(
                        "{type_id}: precision_critical names \"{name}\", which is not a \
                         declared texture input on this node",
                    )),
                    Some(access) if !access.is_texel_exact() => violations.push(format!(
                        "{type_id}.{name}: precision_critical requires a texel-exact \
                         InputAccess (CoincidentTexel/GatherTexel); this input is \
                         {access:?} (a filtering sampler read) — Rgba32Float is \
                         non-filterable on Apple GPUs, so this atom's own read would break",
                    )),
                    Some(_) => {}
                }
            }
        }

        assert!(
            violations.is_empty(),
            "precision_critical / InputAccess mismatches:\n  {}",
            violations.join("\n  "),
        );
    }

    /// The `RangeContract` declared-excuse pattern (`docs/PARAM_RANGE_CONTRACT_DESIGN.md`
    /// D5), transcribed verbatim from `every_boundary_atom_declares_its_reason`
    /// above: walks every registered primitive's params via
    /// `EffectNode::param_contract`, and asserts the set of
    /// `(type_id, param name, reason)` triples carrying a contract EXACTLY
    /// EQUALS this curated table. P1 ships this table EMPTY — no contract
    /// exists in production yet (D6: remove-by-default, no kernel proof, no
    /// contract) — so this test proves the mechanism, not any real boundary.
    /// `node.__`-prefixed test fixtures are excluded, same as the
    /// boundary-reason walk (their contracts are test scaffolding, not
    /// production facts this ledger tracks).
    #[test]
    fn every_range_contract_names_a_real_boundary() {
        use crate::node_graph::PrimitiveRegistry;

        // Curated table: every `(type_id, param_id, reason)` any registered
        // primitive is allowed to declare a `RangeContract` for. Empty in
        // P1; seeded in P2 (PARAM_RANGE_CONTRACT_DESIGN.md §2/D6) — each
        // entry names its kernel/shader evidence file:line so a contract
        // can't creep back onto a merely-conventional range.
        //
        // node.switch_texture (mux_texture.rs) — hand-`impl EffectNode`,
        // `param_contract` override:
        //   - selector: mux_texture.rs:197-200 `resolve_selector_index`
        //     rounds+clamps to [0, num_inputs); absolute index space is
        //     [0, MAX_INPUTS-1] (mux_texture.rs:45).
        //   - num_inputs: mux_texture.rs:148-149 `rebuild_ports` clamps
        //     n to [1, MAX_INPUTS] before slicing the static
        //     IN_PORT_NAMES table.
        // node.multi_blend (multi_blend.rs) — hand-`impl EffectNode`,
        // `param_contract` override:
        //   - num_inputs: multi_blend.rs:191 `reconfigure` clamps to
        //     [2, MAX_INPUTS] before `rebuild_ports` slices IN_PORT_NAMES.
        // node.connect_nearest (array_connect_nearest.rs) — `primitive!`
        // macro `param_contracts:` field:
        //   - max_edges: array_connect_nearest.rs `array_output_capacity`
        //     returns `Some(max_edges)` verbatim as the allocated `edges`
        //     array capacity — sizes a real allocation.
        //
        // §2 VERIFY reads performed, all REJECTED (evidence in the P2
        // session report, not repeated here — no contract added):
        //   - connect_nearest.max_distance: only ever squared into a
        //     comparison threshold, no division, no degenerate collapse
        //     at 0 — stays a display hint.
        //   - render.window (node.draw_lines, render_lines.rs): consumed
        //     only via `window_edges = (segments*window).ceil().max(1)` —
        //     already div-by-zero-proof independent of `window`'s value.
        //   - content_window.width (node.edge_stretch, uv_strip_clamp_body.wgsl):
        //     `clamp(uv, lo, hi)` is well-defined even at width=0 (lo==hi
        //     collapses to a single valid coordinate, not undefined math).
        //   - split.amount (node.rgb_split, chromatic_displace_body.wgsl):
        //     the sampler clamps at the texture edge, not at a fixed
        //     ±32 — the actual dead-input point depends on velocity
        //     magnitude and canvas dims, not a fixed physical bound.
        const CURATED: &[(&str, &str, manifold_core::effects::RangeReason)] = &[
            (
                "node.switch_texture",
                "selector",
                manifold_core::effects::RangeReason::Index,
            ),
            (
                "node.switch_texture",
                "num_inputs",
                manifold_core::effects::RangeReason::Count,
            ),
            (
                "node.multi_blend",
                "num_inputs",
                manifold_core::effects::RangeReason::Count,
            ),
            (
                "node.connect_nearest",
                "max_edges",
                manifold_core::effects::RangeReason::Count,
            ),
        ];

        let registry = PrimitiveRegistry::with_builtin();
        let mut found: Vec<(String, String, manifold_core::effects::RangeReason)> = Vec::new();

        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            for param in node.parameters() {
                if let Some(contract) = node.param_contract(&param.name) {
                    found.push((type_id.to_string(), param.name.to_string(), contract.reason));
                }
            }
        }

        let mut violations: Vec<String> = Vec::new();
        for (type_id, param_id, reason) in &found {
            if !CURATED
                .iter()
                .any(|(t, p, r)| t == type_id && p == param_id && r == reason)
            {
                violations.push(format!(
                    "{type_id}.{param_id}: declares RangeContract (reason {reason:?}) but is \
                     not in the curated RANGE_CONTRACT table — add it deliberately with the \
                     kernel/shader evidence, or remove the contract",
                ));
            }
        }
        for (type_id, param_id, reason) in CURATED {
            if !found
                .iter()
                .any(|(t, p, r)| t == *type_id && p == *param_id && r == reason)
            {
                violations.push(format!(
                    "{type_id}.{param_id}: listed in the curated RANGE_CONTRACT table \
                     (reason {reason:?}) but the registered primitive declares no such \
                     contract — either it was removed (drop the table entry) or the \
                     declaration was lost",
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "range_contract declaration violations:\n  {}",
            violations.join("\n  "),
        );
    }

    #[test]
    fn default_is_boundary() {
        assert_eq!(FusionKind::default(), FusionKind::Boundary);
        assert!(!FusionKind::Boundary.is_fusable());
        assert!(FusionKind::Pointwise.is_fusable());
        assert!(FusionKind::MultiInputCoincident.is_fusable());
    }

    /// The macro slot propagates a converted atom's kind + body through the
    /// `EffectNode` trait object (the surface the region-grower + codegen read).
    #[test]
    fn converted_atom_exposes_kind_and_body() {
        let g = Gain::new();
        let node: &dyn EffectNode = &g;
        assert_eq!(node.fusion_kind(), FusionKind::Pointwise);
        let body = node.wgsl_body().expect("converted gain exposes a fusable body");
        assert!(body.contains("fn body"), "body fragment must define `fn body`");
        assert!(body.contains("gain"), "gain body must reference the gain param");
    }

    /// Wave 1 of the conversion-debt sweep (2026-07-14): the three buffer/
    /// texture SOURCE atoms `node.grid_mesh`, `node.hypercube_points`,
    /// `node.explosion_force` moved off `CONVERSION_DEBT_LEDGER` onto the
    /// codegen path — `FusionKind::Source`, `standalone_for_spec` builds the
    /// runtime pipeline, generated-vs-hand parity tests live in each atom's
    /// own `gpu_tests` module. Per the v1 Source contract (this file's
    /// `FusionKind::Source` doc comment): a Source atom is standalone
    /// single-source only — the region-grower does NOT fold it into a
    /// multi-node fused region (same as the already-converted
    /// `node.cube_mesh` / `node.platonic_solid_points`). Verified against the
    /// shipped presets that reference these atoms
    /// (`assets/generator-presets/MetallicGlass.json` for grid_mesh,
    /// `Tesseract.json` for hypercube_points — neither ships `explosion_force`
    /// yet) via `graph_tool fusion`: each node reports `[source] — unfused`
    /// (`cut: fusable atom, cut by a graph-specific gate`), replacing the
    /// prior `[boundary:conversion_debt] — unfused` verdict — the debt is
    /// gone, region-fusion eligibility for Source atoms is a later compiler
    /// phase (design doc's "fusing a generator as a region producer is a
    /// follow-on").
    #[test]
    fn wave1_conversion_debt_atoms_are_now_source() {
        use crate::node_graph::PrimitiveRegistry;
        let registry = PrimitiveRegistry::with_builtin();
        for type_id in ["node.grid_mesh", "node.hypercube_points", "node.explosion_force"] {
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            assert_eq!(node.fusion_kind(), FusionKind::Source, "{type_id} fusion_kind");
            assert!(node.boundary_reason().is_none(), "{type_id} must not declare a BoundaryReason");
            let body = node
                .wgsl_body()
                .unwrap_or_else(|| panic!("{type_id} has no wgsl_body"));
            assert!(body.contains("fn body"), "{type_id} body must define `fn body`");
        }
    }

    /// All 7 ColorGrade atoms are now classified + carry a body fragment that
    /// defines `fn body` (the codegen entry). mix is the one coincident atom.
    #[test]
    fn all_seven_colorgrade_atoms_classified() {
        use crate::node_graph::PrimitiveRegistry;
        let registry = PrimitiveRegistry::with_builtin();
        let expected = [
            ("node.exposure", FusionKind::Pointwise),
            ("node.saturation", FusionKind::Pointwise),
            ("node.hue_saturation", FusionKind::Pointwise),
            ("node.contrast", FusionKind::Pointwise),
            ("node.colorize", FusionKind::Pointwise),
            ("node.mix", FusionKind::MultiInputCoincident),
            ("node.clamp", FusionKind::Pointwise),
        ];
        for (type_id, kind) in expected {
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            assert_eq!(node.fusion_kind(), kind, "{type_id} fusion_kind");
            let body = node
                .wgsl_body()
                .unwrap_or_else(|| panic!("{type_id} has no wgsl_body"));
            assert!(body.contains("fn body"), "{type_id} body must define `fn body`");
        }
    }

    /// P3 wave 2 (2026-07-14): the seven shading-family + color.rs atoms
    /// converted off `ConversionDebt` this wave are all `Pointwise` with a
    /// real `wgsl_body`, and none declare a `BoundaryReason` any more (the
    /// meta-test `every_boundary_atom_declares_its_reason` also proves the
    /// ledger no longer names them). Six of the seven carry a Color/Vec3/Vec4
    /// param — `region.rs`'s scalar-only cut rule means they stay
    /// individually-fusable rather than joining a multi-node region; see
    /// `region::tests::wave2_color_param_atoms_stay_boundary_in_shipped_presets`
    /// for the real-preset proof of that finding, and
    /// `region::tests::tone_map_fuses_gradient_map_stays_boundary_next_to_a_fusable_neighbor`
    /// for the seventh (`node.tone_map` has no non-scalar params at all, so
    /// it's the one atom this wave that DOES join a multi-node region).
    #[test]
    fn wave2_conversion_debt_atoms_are_now_pointwise() {
        use crate::node_graph::PrimitiveRegistry;
        let registry = PrimitiveRegistry::with_builtin();
        for type_id in [
            "node.shininess",
            "node.rim_light",
            "node.matcap_two_tone",
            "node.tone_map",
            "node.brightness",
            "node.channel_mixer",
            "node.gradient_map",
        ] {
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            assert_eq!(node.fusion_kind(), FusionKind::Pointwise, "{type_id} fusion_kind");
            assert!(node.boundary_reason().is_none(), "{type_id} must not declare a BoundaryReason");
            let body = node
                .wgsl_body()
                .unwrap_or_else(|| panic!("{type_id} has no wgsl_body"));
            assert!(body.contains("fn body"), "{type_id} body must define `fn body`");
        }
    }

    /// Per-input read-semantics: dither tags BOTH its inputs `CoincidentTexel`
    /// (exact-texel, no sampler), while a plain color atom leaves `INPUT_ACCESS`
    /// empty (every input defaults to `Coincident`).
    #[test]
    fn input_access_tags_dither_texel_and_defaults_color_coincident() {
        use super::InputAccess;
        use crate::node_graph::PrimitiveRegistry;
        let registry = PrimitiveRegistry::with_builtin();

        let dither = registry.construct("node.dither").expect("registry missing node.dither");
        assert_eq!(
            dither.input_access(),
            &[InputAccess::CoincidentTexel, InputAccess::CoincidentTexel],
            "dither's in + pattern are both exact-texel"
        );

        let gain = registry.construct("node.exposure").expect("registry missing node.exposure");
        assert!(
            gain.input_access().is_empty(),
            "a color atom leaves INPUT_ACCESS empty (= all Coincident by default)"
        );
        assert_eq!(InputAccess::default(), InputAccess::Coincident);
    }

    /// D3 (BUG-114): `BufferIndex` is gather-shaped for the region-grower's
    /// "never unions a gather-consumed wire" contract, same as `Gather` /
    /// `GatherTexel` / `BufferGather`.
    #[test]
    fn buffer_index_is_gather() {
        use super::InputAccess;
        assert!(InputAccess::BufferIndex.is_gather());
    }
}
