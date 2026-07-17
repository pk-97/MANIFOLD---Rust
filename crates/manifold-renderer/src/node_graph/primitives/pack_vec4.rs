//! `node.combine_xyzw` — zip four `Array<f32>` (x, y, z, w channels)
//! into a single `Array<Vec4Vertex>` that downstream 4D rotation /
//! projection / line rendering consumes.
//!
//! The 4D analogue of `node.combine_xy`. Used to assemble any
//! parametric 4D point cloud from independently-built axis chains
//! (`generate_grid_uv` → `array_math` per axis → `pack_vec4` →
//! `rotate_4d` → `project_4d` → `render_lines`). Pure structural
//! transformation — no magnitude bake (the per-shape PROJ_SCALE
//! constant is applied upstream via `array_math(ScaleOffset)` since
//! it's shape-specific: 0.125 for tesseract, 0.176776695 for
//! duocylinder, etc.). Keeps this atom reusable across every 4D
//! parametric surface.
//!
//! Per-element math: `out[i].position = vec4(x[i], y[i], z[i], w[i])`.
//!
//! CPU-only: 4D vertex grids ship at a few thousand points per frame
//! at most (64² duocylinder = 4096), faster on CPU than dispatch
//! overhead and lands the data in shared MTLBuffer for downstream
//! same-frame readers.

use crate::generators::mesh_common::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: PackVec4,
    type_id: "node.combine_xyzw",
    purpose: "Combine four Array<f32> (x, y, z, w channels) into one Array<Vec4Vertex>. The 4D analogue of node.combine_xy: zips axis-separated channels (built by generate_grid_uv → array_math chains) into the typed wire that node.rotate_4d / project_4d / render_lines consume. Pure structural transformation — no scale bake; per-shape magnitude normalisation is applied upstream via array_math(ScaleOffset) since the constant is shape-specific (0.125 for tesseract, 0.176776695 for duocylinder, …). Pair with generate_grid_uv + array_math(Cos|Sin) + edges_from_grid_uv to author any (u, v)-parametric 4D surface in pure JSON. CPU-only — runs on the content thread so downstream CPU consumers see same-frame writes.",
    inputs: {
        x: Array(f32) required,
        y: Array(f32) required,
        z: Array(f32) required,
        w: Array(f32) required,
    },
    outputs: {
        out: Array(Vec4Vertex),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows the `x` input (mirrors node.combine_xy / node.array_math) so the pack auto-sizes to whatever the upstream channel length is. Processing truncates to min(x, y, z, w, out) so a shorter channel naturally clips the vertex count. No scale or projection constant is baked — the caller is responsible for any magnitude normalisation. For 4D wireframe surfaces using the existing rotate_4d / project_4d pipeline, multiply each channel by the legacy PROJ_SCALE-equivalent (0.176776695 for duocylinder, 0.125 for tesseract) via an array_math(ScaleOffset) node placed between the trig output and this pack — keeps the magnitude convention with the rest of the 4D family.",
    examples: [],
    picker: { label: "Combine XYZW", category: Atom },
    summary: "Zips four separate number lists into one list of 4D points. The 4D counterpart to combining X and Y into a curve.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["combine xyzw", "pack vec4", "zip"],
    boundary_reason: NonGpu,
}

impl Primitive for PackVec4 {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "x")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(x_buf) = ctx.inputs.array("x") else {
            return;
        };
        let Some(y_buf) = ctx.inputs.array("y") else {
            return;
        };
        let Some(z_buf) = ctx.inputs.array("z") else {
            return;
        };
        let Some(w_buf) = ctx.inputs.array("w") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            log::warn!(
                "node.combine_xyzw: no GpuBuffer bound to output port `out` — \
                 the chain build did not pre-allocate this Array<Vec4Vertex>.",
            );
            return;
        };

        let f32_size = std::mem::size_of::<f32>() as u64;
        let vtx_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let x_cap = (x_buf.size / f32_size) as u32;
        let y_cap = (y_buf.size / f32_size) as u32;
        let z_cap = (z_buf.size / f32_size) as u32;
        let w_cap = (w_buf.size / f32_size) as u32;
        let out_cap = (out_buf.size / vtx_size) as u32;
        let count = x_cap.min(y_cap).min(z_cap).min(w_cap).min(out_cap);
        if count == 0 {
            return;
        }

        let x_ptr = x_buf
            .mapped_ptr()
            .expect("pack_vec4: `x` input must be shared-memory");
        let y_ptr = y_buf
            .mapped_ptr()
            .expect("pack_vec4: `y` input must be shared-memory");
        let z_ptr = z_buf
            .mapped_ptr()
            .expect("pack_vec4: `z` input must be shared-memory");
        let w_ptr = w_buf
            .mapped_ptr()
            .expect("pack_vec4: `w` input must be shared-memory");
        let x_slice: &[f32] =
            unsafe { std::slice::from_raw_parts(x_ptr as *const f32, x_cap as usize) };
        let y_slice: &[f32] =
            unsafe { std::slice::from_raw_parts(y_ptr as *const f32, y_cap as usize) };
        let z_slice: &[f32] =
            unsafe { std::slice::from_raw_parts(z_ptr as *const f32, z_cap as usize) };
        let w_slice: &[f32] =
            unsafe { std::slice::from_raw_parts(w_ptr as *const f32, w_cap as usize) };

        const SCRATCH_LEN: usize = 4096;
        let mut scratch = [Vec4Vertex {
            position: [0.0; 4],
        }; SCRATCH_LEN];
        let write_count = (count as usize).min(SCRATCH_LEN);
        for (i, slot) in scratch.iter_mut().take(write_count).enumerate() {
            *slot = Vec4Vertex {
                position: [x_slice[i], y_slice[i], z_slice[i], w_slice[i]],
            };
        }

        // Safety: shared-memory MTLBuffer pre-bound by the chain build;
        // write count clamped to the buffer capacity above; sequential
        // executor on the content thread means no concurrent writer.
        unsafe {
            out_buf.write(0, bytemuck::cast_slice(&scratch[..write_count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_four_required_f32_inputs_and_vec4_vertex_output() {
        use crate::node_graph::ports::{ArrayType, PortType};

        assert_eq!(PackVec4::TYPE_ID, "node.combine_xyzw");

        let f32_layout = ArrayType::of_known::<f32>();
        let vtx_layout = ArrayType::of_known::<Vec4Vertex>();

        let names = ["x", "y", "z", "w"];
        assert_eq!(PackVec4::INPUTS.len(), 4);
        for (port, expected_name) in PackVec4::INPUTS.iter().zip(names.iter()) {
            assert_eq!(port.name, *expected_name);
            assert!(port.required, "{} must be required", port.name);
            assert_eq!(port.ty, PortType::Array(f32_layout));
        }

        assert_eq!(PackVec4::OUTPUTS.len(), 1);
        assert_eq!(PackVec4::OUTPUTS[0].name, "out");
        assert_eq!(PackVec4::OUTPUTS[0].ty, PortType::Array(vtx_layout));
    }

    #[test]
    fn output_capacity_follows_x_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = PackVec4::new();
        let params = ParamValues::default();
        let inputs = [("x", 576_u32), ("y", 576_u32), ("z", 576_u32), ("w", 576_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(576),
        );
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = PackVec4::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.combine_xyzw");
    }
}
