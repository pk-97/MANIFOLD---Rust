//! `node.normals_from_depth` — S6 view-normal reconstruction for the water
//! surface (docs/WATER_SIMULATION_DESIGN.md section 7).
//!
//! Reconstructs view-space positions from the raw clip-depth + coverage
//! pair (`node.particle_surface_depth`'s outputs) with the Camera, and
//! builds the surface normal from per-axis one-sided differences: on each
//! axis, among the COVERED neighbours, the pair with the smallest depth
//! discontinuity wins, so silhouettes and depth layers keep their own
//! normals instead of averaging across the edge. The position math is the
//! shared `depth_common.wgsl::view_pos_from_depth` — the same helper
//! `node.ssao_gtao` builds its normals from (refactored to share in S6), so
//! these are perspective water normals, never heightmap normals. Covered
//! centre with no covered neighbour on an axis falls back to the single
//! covered side; a covered pixel with no covered neighbours at all gets the
//! toward-camera fallback (0,0,-1). Empty pixels pass through as (0,0,0,0) —
//! uncovered pixels are never smoothed into liquid.
//!
//! Output: view-space normal in RGB (toward-camera hemisphere; -z is
//! forward in this view convention), coverage in A. Rgba16Float is the
//! backend default — no `output_format` override needed. On the codegen
//! path (fusable): 2-input Coincident, both inputs GatherTexel (integer
//! loads at ±1 texel, texel-exact for the CPU reference), camera consumed
//! entirely via the fov_y/near/far derived uniforms.

use crate::node_graph::camera::{Camera, CameraMode};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");

crate::primitive! {
    name: NormalsFromDepth,
    type_id: "node.normals_from_depth",
    purpose: "Reconstructs view-space surface normals from a raw clip-depth map + coverage mask and a Camera (design section 7): positions come from the shared view_pos_from_depth projection convention; per axis, the COVERED neighbour pair with the smallest depth discontinuity wins (one-sided difference at silhouettes and depth layers), so edges keep their own normals. Perspective water normals, not heightmap normals. Output Rgba16Float: view normal (toward-camera hemisphere) in RGB, coverage in A (0 = empty pixel, never smoothed into liquid).",
    inputs: {
        depth: Texture2D required,
        coverage: Texture2D required,
        camera: Camera required,
    },
    outputs: {
        normals: Texture2D,
    },
    params: [],
    depth_rule: Inherit,
    composition_notes: "Final stage of the S6 water surface graph: node.particle_surface_depth's depth + coverage, plus the SAME shared Camera wire, in; view normals out. S7's water shading pass consumes these normals for reflection/refraction. A covered pixel with no covered neighbour on either axis yields the toward-camera fallback (0,0,-1) — an isolated-splats artefact, not a hole.",
    examples: [],
    picker: { label: "Normals From Depth", category: Atom },
    summary: "Builds perspective-correct surface normals from a depth map, keeping silhouettes and depth layers sharp by only trusting covered neighbours.",
    category: Geometry3D,
    role: Filter,
    aliases: ["normals from depth", "depth normals", "surface normals", "water normals", "normal reconstruction"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/normals_from_depth_body.wgsl"),
    input_access: [GatherTexel, GatherTexel],
    precision_critical: ["depth"],
    derived_uniforms: ["fov_y", "near", "far"],
    wgsl_includes: [DEPTH_COMMON],
}

/// Single source of truth for the three Camera-derived scalar fields, in
/// `DERIVED_UNIFORMS` declaration order — shared by `run()` (unfused CPU
/// path) and the fused recompute below. Mirrors `ssao_gtao.rs`'s
/// `derive_view_scalars` exactly (same projection the position
/// reconstruction inverts).
fn derive_view_scalars(cam: &Camera) -> [f32; 3] {
    let fov_y = match cam.mode {
        CameraMode::Perspective { fov_y } => fov_y,
        CameraMode::Orthographic { .. } => std::f32::consts::FRAC_PI_3,
    };
    [fov_y, cam.near, cam.far]
}

inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.normals_from_depth",
        recompute: |ctx| ctx.camera.map(derive_view_scalars).map(|v| v.to_vec()),
    }
}

impl Primitive for NormalsFromDepth {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let [fov_y, near, far] = derive_view_scalars(&cam);

        // Perspective-only projection convention: orthographic depth does
        // not invert through view_pos_from_depth.
        if !matches!(cam.mode, CameraMode::Perspective { .. }) {
            ctx.error(
                "node.normals_from_depth: perspective camera required (orthographic rejected in V1)"
                    .to_string(),
            );
        }

        let Some(depth_tex) = ctx.inputs.texture_2d("depth") else {
            return;
        };
        let Some(coverage_tex) = ctx.inputs.texture_2d("coverage") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("normals") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Two-source MultiInputCoincident: `depth` + `coverage` are both
            // GatherTexel (raw handles, integer textureLoad) — an
            // all-texel atom binds NO sampler (codegen/standalone.rs). The
            // generated bindings are uniform(0)/tex_depth(1)/tex_coverage(2)/
            // dst(3). normals_from_depth_body.wgsl is the contract; the
            // gpu_tests parity check gates it.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.normals_from_depth standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.normals_from_depth",
            )
        });

        // Params: none. Uniform layout = derived fields only, in
        // DERIVED_UNIFORMS declaration order (fov_y, near, far), padded to
        // 16 bytes.
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct NormalsUniforms {
            fov_y: f32,
            near: f32,
            far: f32,
            _pad: u32,
        }
        let uniforms = NormalsUniforms {
            fov_y,
            near,
            far,
            _pad: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: depth_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: coverage_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.normals_from_depth",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn water_normals_from_depth_declares_inputs_and_output() {
        assert_eq!(NormalsFromDepth::TYPE_ID, "node.normals_from_depth");
        let names: Vec<&str> = NormalsFromDepth::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["depth", "coverage", "camera"]);
        for input in NormalsFromDepth::INPUTS {
            assert!(input.required, "{} should be required", input.name);
        }
        assert_eq!(NormalsFromDepth::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(NormalsFromDepth::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(NormalsFromDepth::INPUTS[2].ty, PortType::Camera);
        assert_eq!(NormalsFromDepth::OUTPUTS.len(), 1);
        assert_eq!(NormalsFromDepth::OUTPUTS[0].name, "normals");
        assert_eq!(NormalsFromDepth::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn water_normals_from_depth_declares_three_derived_uniforms() {
        assert_eq!(NormalsFromDepth::DERIVED_UNIFORMS, &["fov_y", "near", "far"]);
    }

    #[test]
    fn water_normals_from_depth_has_a_registered_recompute() {
        use crate::node_graph::freeze::derived_uniform_registry::has_recompute;
        assert!(has_recompute("node.normals_from_depth"));
    }

    #[test]
    fn water_normals_from_depth_derive_scalars_matches_ssao_gtao() {
        // Same projection the shared position reconstruction inverts —
        // the two atoms must derive identical scalars from one camera.
        let mut cam = Camera::default_perspective();
        cam.near = 0.2;
        cam.far = 250.0;
        let CameraMode::Perspective { fov_y } = cam.mode else {
            panic!("default camera must be perspective");
        };
        assert_eq!(derive_view_scalars(&cam), [fov_y, 0.2, 250.0]);
    }

    #[test]
    fn water_normals_from_depth_registers_as_palette_atom() {
        let prim = NormalsFromDepth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.normals_from_depth");
    }
}
