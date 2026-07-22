use std::fmt::Write as _;

use crate::node_graph::freeze::markers::Marker;
use crate::node_graph::ports::ChannelSpec;

use super::types::{channel_wgsl_ty, FusionRegion};


/// Emit a WGSL struct definition for a multi-channel element. Field names come
/// from each channel's debug name (the well-known registry), falling back to
/// `c{i}` for runtime-introduced names. std430 layout is implicit in the field
/// types — no explicit pad fields (matching how the `#[repr(C)]` element relies
/// on WGSL alignment to reproduce its stride).
pub(super) fn emit_buffer_struct(specs: &[ChannelSpec], name: &str) -> String {
    let mut s = format!("struct {name} {{\n");
    for (i, sp) in specs.iter().enumerate() {
        let field = sp
            .name
            .debug_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("c{i}"));
        writeln!(s, "    {field}: {},", channel_wgsl_ty(sp.ty)).unwrap();
    }
    s.push_str("}\n");
    s
}

/// Buffer-domain multi-atom fusion: chain a region of per-element (particle /
/// instance / curve-point) atom bodies into ONE `var<storage>` kernel. The
/// buffer analogue of [`generate_fused`]: pre-read each external array element
/// `[idx]` once, thread each body's output element as a register to the next,
/// write the region output array once. A 1D dispatch over the output array's
/// `arrayLength` (the convention `node.wgsl_compute` keys its buffer dispatch on
/// — NO `dispatch_count` uniform, unlike the standalone buffer path).
///
/// v1 scope — anything outside it returns `Err` so the card renders unfused
/// (always correct; the install pass also naga-parses the result as a final
/// guard): every member is a coincident per-element atom (no `BufferGather` —
/// those stay boundaries), each writes exactly ONE Array output, and its scalar
/// params are port-shadow uniforms. TEXTURE inputs fuse as gathered externals:
/// the kernel binds each as `src_<e>: texture_2d<f32>` plus one shared `samp`,
/// and the consuming body samples it at an element-computed coord — the same
/// `tex + samp` ABI the standalone buffer kernel passes, so the sample is
/// bit-identical (the `*_at_particles` force samplers, anti_clump's modulator).
/// Emit the D7/P0 side-channel markers a fused region's derived-uniform members
/// need (`docs/CINEMATIC_POST_DESIGN.md`, `docs/FREEZE_COMPILER_MAP.md` §5 marker
/// ABI): one `// @camera_external: camera_ext_N` per distinct Camera external the
/// region routes (`FusionRegion::camera_externals`), then one
/// `// @derived_uniform_member: <first_field> words=<n> <type_id> [<camera_port>]`
/// per member with non-empty `derived_uniforms`. `node.wgsl_compute`'s
/// introspection (`primitives/wgsl_compute.rs`) is the sole consumer: the first
/// marker synthesizes a DECLARED, non-introspected Camera-typed input port (no
/// WGSL binding exists for Camera, so naga can't discover it — this comment is
/// the only channel); the second tells `evaluate()`, every frame, which
/// contiguous uniform-field block to skip in the generic port-shadow pack and
/// instead fill via `derived_uniform_registry::recompute(type_id, ctx)`. Shared
/// by both fused paths (buffer and texture) so a member's derived-uniform
/// contract is identical regardless of which domain fuses it. Emits nothing for
/// a region with no derived-uniform members — byte-identical to prior codegen.
pub(super) fn emit_derived_uniform_markers(out: &mut String, region: &FusionRegion<'_>) {
    for e in 0..region.camera_externals {
        writeln!(out, "{}", Marker::CameraExternal { name: format!("camera_ext_{e}") }.emit())
            .unwrap();
    }
    for (i, node) in region.nodes.iter().enumerate() {
        if node.derived_uniforms.is_empty() {
            continue;
        }
        let words: u32 = node
            .derived_uniforms
            .iter()
            .map(|d| {
                let (_, dty) = d.split_once(':').unwrap_or((d, "f32"));
                if dty == "vec3" { 3 } else { 1 }
            })
            .sum();
        let (first_dname, first_dty) =
            node.derived_uniforms[0].split_once(':').unwrap_or((node.derived_uniforms[0], "f32"));
        let first_field = if first_dty == "vec3" {
            format!("n{i}_{first_dname}_x")
        } else {
            format!("n{i}_{first_dname}")
        };
        let marker = Marker::DerivedUniformMember {
            first_field,
            words,
            type_id: node.type_id.to_string(),
            camera_port: node.derived_camera_ext.map(|e| format!("camera_ext_{e}")),
        };
        writeln!(out, "{}", marker.emit()).unwrap();
    }
}
