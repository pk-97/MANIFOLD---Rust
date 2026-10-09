use std::fmt::Write as _;

use crate::freeze::markers::Marker;
use crate::parameters::ParamType;
use crate::ports::ChannelSpec;

use super::types::{InputSource, channel_wgsl_ty, FusionRegion};


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

/// Emit the shared marker ABI for texture and buffer fusion: Bool field types,
/// camera inputs and derived-uniform blocks. WgslCompute uses these facts to
/// restore types that WGSL cannot express and recompute camera/array-derived
/// values. Buffer members additionally map their declared array inputs to the
/// fused port names. See FREEZE_COMPILER_MAP.md, marker ABI.
pub(super) fn emit_derived_uniform_markers(
    out: &mut String,
    region: &FusionRegion<'_>,
    buffer_domain: bool,
) {
    emit_bool_uniform_markers(out, region);
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
        // Array-port mapping for the recompute: a fused kernel renames inputs
        // to `src_<k>`, so the recompute's `array_len("<port>")` lookup needs
        // the member port resolved to its fused name. Only ports the
        // recompute DECLARES (`derived_uniform_registry::array_ports`) are
        // mapped — everything else keeps the pre-extension marker text
        // byte-identical. An external maps to its `src_<e>` binding; a
        // region-internal register has no port — its length IS the kernel's
        // element count, emitted as the `count` sentinel; an unwired optional
        // maps to nothing (the recompute's lookup degrades to 0, matching
        // `run()`). BUFFER domains only: their array ports lead `inputs`
        // (the prefix convention), so the position lookup is exact; a
        // texture-domain member's array ports (BufferIndex) trail the texture
        // entries and would misalign.
        let array_ports: Vec<(String, String)> = if buffer_domain {
            crate::freeze::derived_uniform_registry::array_ports(&node.type_id)
                .iter()
                .filter_map(|member_port| {
                    let arr: Vec<&crate::ports::NodeInput> = node
                        .node_inputs
                        .iter()
                        .filter(|p| {
                            matches!(p.ty, crate::ports::PortType::Array(_))
                        })
                        .collect();
                    let idx = arr.iter().position(|p| p.name.as_ref() == *member_port)?;
                    match node.inputs.get(idx)? {
                        InputSource::External(e) => {
                            Some((member_port.to_string(), format!("src_{e}")))
                        }
                        InputSource::Node(_) | InputSource::NodeOutput(..) => {
                            Some((member_port.to_string(), "count".to_string()))
                        }
                        _ => None,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let marker = Marker::DerivedUniformMember {
            first_field,
            words,
            type_id: node.type_id.to_string(),
            camera_port: node.derived_camera_ext.map(|e| format!("camera_ext_{e}")),
            array_ports,
        };
        writeln!(out, "{}", marker.emit()).unwrap();
    }
}

/// Emit the typed side-channel for Bool fields in either fused uniform path.
/// The ABI has one helper for both texture and buffer kernels so marker order
/// and field naming cannot drift between the two code generators.
fn emit_bool_uniform_markers(out: &mut String, region: &FusionRegion<'_>) {
    // Bool params lower to `u32` in WGSL, which naga cannot distinguish from
    // Int during host introspection. Keep the authored type on the marker ABI
    // so fused WgslCompute nodes retain BoolThreshold-compatible packing.
    for (i, node) in region.nodes.iter().enumerate() {
        for param in node.params {
            if param.ty == ParamType::Bool {
                writeln!(
                    out,
                    "{}",
                    Marker::BoolUniform { field: format!("n{i}_{}", param.name) }.emit()
                )
                .unwrap();
            }
        }
    }
}
