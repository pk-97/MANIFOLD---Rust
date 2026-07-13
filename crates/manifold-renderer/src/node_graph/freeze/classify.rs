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
    /// The region-grower leaves Source atoms unfused (its workers must read the
    /// upstream source; a 0-input producer doesn't fit the single-external model)
    /// — fusing a generator as a region producer is a follow-on; v1 Source is
    /// standalone single-source only.
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
/// GPU input access is a CLOSED, small set. The two variants here are what's
/// built; the planned additive kinds — each just one more codegen read-path +
/// one region-grow rule, never a re-tag of the atoms already shipped — are:
///   - `Gather`: read at a coordinate the body COMPUTES (the UV-warp family —
///     kaleidoscope / chromatic / voronoi). The body receives the texture +
///     sampler as a declared arg and owns the exact filter/address-mode of the
///     unfused atom (design §11.B / line 156).
///   - `BufferIndex`: read element `[i]` from a storage buffer (the particle-sim
///     lane).
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
    /// declared sampled-texture args", design §11.B / line 156). Because the read
    /// can't be threaded as a register, the region-grower treats a node with any
    /// Gather input as a boundary — v1 Gather is standalone-only (single-source);
    /// fusing a gather INTO a multi-atom region is a deeper follow-on.
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
    /// [`Gather`] — it can't be threaded as a fused register. The buffer
    /// standalone codegen binds the array as `var<storage, read>` and the body
    /// owns the indexing; the region-grower treats any buffer atom as a boundary
    /// (the texture region path already refuses a node with no texture output, so
    /// buffer atoms are standalone single-source only in v1).
    BufferGather,
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
    "node.grid_mesh",           // generate_grid_mesh
    "node.hypercube_points",    // hypercube_vertices
    "node.explosion_force",     // radial_burst_force_field
    "node.shininess",           // blinn_specular
    "node.rim_light",           // fresnel_rim
    "node.matcap_two_tone",     // matcap_two_tone
    "node.tone_map",            // tone_map
    "node.rotate_coordinates",  // rotate_2d
    "node.sine_wave",           // sin_term
    "node.watercolor",          // watercolor
    "node.brightness",          // color.rs — P2b triage: single pure per-element kernel, no barriers
    "node.channel_mixer",       // color.rs — P2b triage: single pure per-element kernel, no barriers
    "node.gradient_map",        // color.rs — P2b triage: single pure per-element kernel, no barriers
];

impl InputAccess {
    /// Whether the body computes its own read coordinate / index (a dependent
    /// read the region-grower can't thread as a register). Both texture gather
    /// flavours and the buffer gather qualify.
    pub fn is_gather(self) -> bool {
        matches!(
            self,
            InputAccess::Gather | InputAccess::GatherTexel | InputAccess::BufferGather
        )
    }
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
}
