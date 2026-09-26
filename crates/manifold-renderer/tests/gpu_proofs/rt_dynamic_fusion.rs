//! SCENE_MODIFIER_RT_DESIGN.md P2 (BUG-e3p6.4) — rt_dynamic_fusion proof:
//! the fused mesh-deformer kernel produces GPU output identical to the
//! unfused atom chain. Renders mesh_input → ripple_mesh → ripple_mesh →
//! render_mesh → final_output (the mesh-deformer shape that fuses today)
//! both ways on one Metal device with a deterministic 3×3 vertex grid
//! prebound to the mesh input, reads back the final vertex buffer from
//! each path, and asserts elementwise equality within f32 tolerance —
//! plus a deformation sanity check so a zero/garbage buffer can't pass
//! as "equal".
//!
//! Why ripple and not the stock wave/morph chain: every stock deformer
//! that declares `Dependencies` mesh rules (normal_wave_mesh, morph_mesh,
//! …) also declares a `weights_len` derived uniform with no registered
//! recompute, so the fail-closed gate in `fuse_canonical_def_masked`
//! keeps those regions unfused today. The declared-rule composition is
//! proven at the composition seam in freeze/install.rs
//! (`mesh_change_compose_region_rules_wave_morph`); this proof covers
//! the executable fused-vs-unfused output path.
//!
//! `time` is WIRED from a value node (ripple's own authoring pattern:
//! "drive time from a beat ramp") because the unwired-frame-clock corner
//! of fused codegen is broken independently of this proof — a
//! frame-time input that is ALSO a declared param (ripple's `time`) is
//! emitted in the fused Params struct at its param position, while the
//! derived-uniform marker's word accounting assumes it sits in the
//! derived block, so the marker covers the wrong fields and the frame
//! clock never reaches the shader. Tracked for the campaign.
//!
//! The sibling `rt_dynamic_fusion_surface_waves_fused_output_matches_unfused`
//! proof renders the stock Surface Waves chain (normal_wave_mesh →
//! mesh_spatial_mask → morph_mesh, the flattened shape of the bundled
//! SurfaceWaves preset) both ways now that BUG-7wwy + BUG-jwyh are
//! closed: the mask's `weights` input is OPTIONAL and left unwired in
//! the bundled preset (identity), and the recompute's member→fused-port
//! marker mapping (`src_<k>` for an external, `count` for morph's
//! region-internal weights register) is what lets the weights actually
//! reach the shader. A wired-weights variant with a non-unit ramp
//! exercises both mappings against values the degrade-to-1.0 path would
//! get wrong.

use std::sync::Arc;

use manifold_core::{Beats, NodeId, Seconds, effect_graph_def::EffectGraphDef};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::freeze::install::fused_generator_view_for;
use manifold_renderer::node_graph::mesh_change::PreparedMeshRules;
use manifold_renderer::node_graph::{
    EffectGraphDefExt, Executor, FrameTime, MetalBackend, PrimitiveRegistry, ResourceId, compile,
    pre_allocate_resources,
};

use crate::harness;

/// 3×3 grid in the XZ plane (y=0), smooth +Y normals, distinct UVs — the
/// layout `render_scene`'s MeshVertex carries (pos 0, normal 4, uv0 8,
/// uv1 10, tangent 12, color 16 in f32 words, stride 80 bytes).
const VERTEX_WORDS: usize = 20;
const VERTEX_COUNT: usize = 9;

fn input_vertices() -> Vec<[f32; VERTEX_WORDS]> {
    let mut verts = Vec::with_capacity(VERTEX_COUNT);
    for j in 0..3 {
        for i in 0..3 {
            let mut v = [0f32; VERTEX_WORDS];
            v[0] = (i as f32 - 1.0) * 0.5;
            v[2] = (j as f32 - 1.0) * 0.5;
            v[4] = 0.0;
            v[5] = 1.0;
            v[6] = 0.0;
            v[8] = i as f32 / 2.0;
            v[9] = j as f32 / 2.0;
            v[10] = v[8];
            v[11] = v[9];
            v[12] = 1.0;
            v[15] = -1.0;
            v[16..20].copy_from_slice(&[1.0; 4]);
            verts.push(v);
        }
    }
    verts
}

/// The same grid with a per-vertex height gradient — every row-triangle
/// has real area. `normal_wave_mesh` builds a per-triangle normal-transport
/// map and passes vertices through unchanged on degenerate triangles, so
/// the flat `input_vertices()` grid (collinear rows) would no-op the wave.
fn input_vertices_bumpy() -> Vec<[f32; VERTEX_WORDS]> {
    let mut verts = input_vertices();
    for (n, v) in verts.iter_mut().enumerate() {
        let i = n % 3;
        let j = n / 3;
        v[1] = 0.1 * (i + 3 * j) as f32;
    }
    verts
}

/// The deformer chain def. Amplitude is pinned to 0.5 in the document
/// (ripple's own default is 0.0 — a zero wave would prove nothing) and
/// `time` is wired from a value node at the last rendered frame's clock
/// (see the module doc for why it isn't left unwired).
fn deformer_def() -> EffectGraphDef {
    let json = r#"{
        "version": 1, "name": "rt_dynamic_fusion",
        "nodes": [
            { "id": 0, "typeId": "system.mesh_input", "nodeId": "mesh_in" },
            { "id": 1, "typeId": "node.ripple_mesh", "nodeId": "r1",
              "params": { "amplitude": { "type": "Float", "value": 0.5 } } },
            { "id": 2, "typeId": "node.ripple_mesh", "nodeId": "r2",
              "params": { "amplitude": { "type": "Float", "value": 0.5 } } },
            { "id": 3, "typeId": "node.value", "nodeId": "t",
              "params": { "value": { "type": "Float", "value": 0.016666668 } } },
            { "id": 4, "typeId": "node.free_camera", "nodeId": "cam" },
            { "id": 5, "typeId": "node.unlit_material", "nodeId": "mat" },
            { "id": 6, "typeId": "node.render_mesh", "nodeId": "render" },
            { "id": 7, "typeId": "system.final_output", "nodeId": "final" }
        ],
        "wires": [
            { "fromNode": 0, "fromPort": "vertices", "toNode": 1, "toPort": "in" },
            { "fromNode": 0, "fromPort": "weights", "toNode": 1, "toPort": "weights" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 0, "fromPort": "weights", "toNode": 2, "toPort": "weights" },
            { "fromNode": 3, "fromPort": "out", "toNode": 1, "toPort": "time" },
            { "fromNode": 3, "fromPort": "out", "toNode": 2, "toPort": "time" },
            { "fromNode": 4, "fromPort": "out", "toNode": 6, "toPort": "camera" },
            { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "material" },
            { "fromNode": 2, "fromPort": "out", "toNode": 6, "toPort": "vertices" },
            { "fromNode": 6, "fromPort": "color", "toNode": 7, "toPort": "in" }
        ]
    }"#;
    serde_json::from_str(json).unwrap()
}

fn frame(i: u64) -> FrameTime {
    FrameTime {
        beats: Beats(i as f64 / 30.0),
        seconds: Seconds(i as f64 / 60.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: i as i64,
    }
}

/// Render the chain `frames` times (two frames: pipeline warm-up plus a
/// time-advanced frame — the readback is from the LAST frame) and return
/// the deformer output buffer as f32 words. `weights` binds the
/// `mesh_in.weights` output when `Some` (the def wires it) and is skipped
/// when `None`; a `mesh_ref` node, when the def has one, receives the same
/// vertex buffer (the scene-modifier current/reference pair).
fn render_and_readback(
    def: &EffectGraphDef,
    fused: bool,
    input: &[[f32; VERTEX_WORDS]],
    weights: Option<&[f32]>,
    frames: u64,
    tail_node_id: &str,
) -> Vec<[f32; VERTEX_WORDS]> {
    let h = harness::shared();
    let device = &h.device;
    let registry = PrimitiveRegistry::with_builtin();

    let (mut graph, mesh_out_node) = if fused {
        let view = fused_generator_view_for(def)
            .expect("the deformer region must fuse (see the module doc)");
        let graph = (*view.def)
            .clone()
            .into_graph(&registry, &view.mesh_rules)
            .unwrap_or_else(|e| panic!("fused def with its sidecar must load: {e}"));
        let fused_node = graph
            .instance_by_node_id(&NodeId::new("fused_region_0"))
            .expect("the fused kernel node must instantiate");
        (graph, fused_node)
    } else {
        let graph = def
            .clone()
            .into_graph(&registry, &PreparedMeshRules::default())
            .unwrap_or_else(|e| panic!("canonical def must load: {e}"));
        let r2 = graph
            .instance_by_node_id(&NodeId::new(tail_node_id))
            .expect("the chain tail node must instantiate");
        (graph, r2)
    };
    let mesh_in = graph
        .instance_by_node_id(&NodeId::new("mesh_in"))
        .expect("mesh_in must instantiate");
    let mesh_ref = graph.instance_by_node_id(&NodeId::new("mesh_ref"));
    let plan = compile(&graph).unwrap_or_else(|e| panic!("plan must compile: {e:?}"));

    // Prebind the deterministic input buffers BEFORE allocation so the
    // audit sees every required array input bound and nothing re-allocates
    // them.
    let res_of = |node, port: &str| -> ResourceId {
        plan.steps()
            .iter()
            .find(|s| s.node == node)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == port))
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{port} output must exist"))
    };
    let vertices_res = res_of(mesh_in, "vertices");
    let output_res = res_of(mesh_out_node, if fused { "dst" } else { "out" });

    let mut backend = MetalBackend::new(Arc::clone(device), 64, 64, GpuTextureFormat::Rgba16Float);
    let vertex_bytes = unsafe {
        std::slice::from_raw_parts(
            input.as_ptr().cast::<u8>(),
            std::mem::size_of_val(input),
        )
    };
    let vertex_buf = device.create_buffer_shared(vertex_bytes.len() as u64);
    unsafe { vertex_buf.write(0, vertex_bytes) };
    backend.pre_bind_array(vertices_res, vertex_buf.clone());
    if let Some(mesh_ref) = mesh_ref {
        backend.pre_bind_array(res_of(mesh_ref, "vertices"), vertex_buf);
    }
    if let Some(weights) = weights {
        let weights_buf =
            device.create_buffer_shared(std::mem::size_of_val(weights) as u64);
        unsafe { weights_buf.write(0, bytemuck::cast_slice(weights)) };
        backend.pre_bind_array(res_of(mesh_in, "weights"), weights_buf);
    }
    pre_allocate_resources(&graph, &plan, device, &mut backend)
        .unwrap_or_else(|e| panic!("resource pre-allocation must pass its audit: {e:?}"));

    let mut exec = Executor::new(Box::new(backend));
    for i in 0..frames {
        let mut enc = device.create_encoder("rt-dynamic-fusion-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_gpu(&mut graph, &plan, frame(i), &mut gpu);
        }
        enc.commit_and_wait_completed();
    }

    let slot = exec
        .backend()
        .slot_for(output_res)
        .unwrap_or_else(|| panic!("output resource {output_res:?} must be bound"));
    let buf = exec
        .backend()
        .array_buffer(slot)
        .unwrap_or_else(|| panic!("output slot {slot:?} must hold a buffer"));
    assert!(
        buf.size as usize >= VERTEX_COUNT * VERTEX_WORDS * 4,
        "the deformer output must hold at least the {VERTEX_COUNT}-vertex buffer, got {} bytes",
        buf.size
    );
    let staging = device.create_buffer_shared(buf.size);
    let mut read_enc = device.create_encoder("rt-dynamic-fusion-readback");
    read_enc.copy_buffer_to_buffer(buf, &staging, buf.size);
    read_enc.commit_and_wait_completed();
    let ptr = staging
        .mapped_ptr()
        .expect("staging buffer must be CPU-mapped") as *const f32;
    let words = unsafe { std::slice::from_raw_parts(ptr, VERTEX_COUNT * VERTEX_WORDS) };
    words
        .chunks_exact(VERTEX_WORDS)
        .map(|c| {
            let mut v = [0f32; VERTEX_WORDS];
            v.copy_from_slice(c);
            v
        })
        .collect()
}

/// P2 gate: the fused mesh kernel's GPU output must match the unfused atom
/// chain's, element for element, at a real time-varying frame.
#[test]
fn rt_dynamic_fusion_fused_output_matches_unfused() {
    let def = deformer_def();
    let input = input_vertices();
    let frames = 2;
    let unit_weights = vec![1.0f32; VERTEX_COUNT];

    let unfused = render_and_readback(&def, false, &input, Some(&unit_weights), frames, "r2");
    let fused = render_and_readback(&def, true, &input, Some(&unit_weights), frames, "r2");

    // The wave actually deformed the mesh (both paths read the same buffer
    // size, so equal-and-zero could not pass — but pin it explicitly).
    let max_displacement = unfused
        .iter()
        .zip(&input)
        .map(|(out, inp)| (out[1] - inp[1]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_displacement > 1.0e-3,
        "the ripple must visibly displace the grid (max |dy| = {max_displacement})"
    );

    const TOL: f32 = 1.0e-5;
    for (i, (a, b)) in unfused.iter().zip(&fused).enumerate() {
        for (w, (x, y)) in a.iter().zip(b).enumerate() {
            assert!(
                (x - y).abs() <= TOL,
                "vertex {i} word {w}: fused {y} diverged from unfused {x}"
            );
        }
    }
}

/// The stock Surface Waves chain — normal_wave_mesh → mesh_spatial_mask →
/// morph_mesh, the flattened shape of the bundled SurfaceWaves preset —
/// fused vs unfused. `wire_mask_weights` selects the bundled shape (mask
/// weights UNWIRED → identity, the buffer-region optional-input admit) or
/// a non-unit ramp wired into the mask (the member→fused-port marker
/// mappings: external `src_<k>` for the mask, `count` for morph's
/// region-internal weights register). Both must fuse and match the
/// unfused chain element for element.
fn surface_waves_def(wire_mask_weights: bool) -> EffectGraphDef {
    let mask_weights_wire = if wire_mask_weights {
        r#",{ "fromNode": 0, "fromPort": "weights", "toNode": 2, "toPort": "weights" }"#
    } else {
        ""
    };
    let json = format!(
        r#"{{
        "version": 1, "name": "rt_dynamic_fusion_surface_waves",
        "nodes": [
            {{ "id": 0, "typeId": "system.mesh_input", "nodeId": "mesh_in" }},
            {{ "id": 1, "typeId": "system.mesh_input", "nodeId": "mesh_ref" }},
            {{ "id": 2, "typeId": "node.mesh_spatial_mask", "nodeId": "mask",
              "params": {{
                "shape": {{ "type": "Enum", "value": 1 }},
                "width": {{ "type": "Float", "value": 0.3 }},
                "feather": {{ "type": "Float", "value": 0.1 }},
                "amount": {{ "type": "Float", "value": 1.0 }},
                "invert": {{ "type": "Float", "value": 0.5 }}
              }} }},
            {{ "id": 3, "typeId": "node.normal_wave_mesh", "nodeId": "wave",
              "params": {{
                "amplitude": {{ "type": "Float", "value": 0.2 }},
                "frequency": {{ "type": "Float", "value": 1.5 }},
                "phase": {{ "type": "Float", "value": 0.25 }},
                "pitch": {{ "type": "Float", "value": 1.0 }}
              }} }},
            {{ "id": 4, "typeId": "node.morph_mesh", "nodeId": "morph",
              "params": {{
                "t": {{ "type": "Float", "value": 0.5 }},
                "blend_frames": {{ "type": "Bool", "value": false }}
              }} }},
            {{ "id": 5, "typeId": "node.free_camera", "nodeId": "cam" }},
            {{ "id": 6, "typeId": "node.unlit_material", "nodeId": "mat" }},
            {{ "id": 7, "typeId": "node.render_mesh", "nodeId": "render" }},
            {{ "id": 8, "typeId": "system.final_output", "nodeId": "final" }}
        ],
        "wires": [
            {{ "fromNode": 0, "fromPort": "vertices", "toNode": 3, "toPort": "in" }},
            {{ "fromNode": 1, "fromPort": "vertices", "toNode": 2, "toPort": "in" }},
            {{ "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "b" }},
            {{ "fromNode": 2, "fromPort": "weights", "toNode": 4, "toPort": "weights" }}
            {mask_weights_wire},
            {{ "fromNode": 0, "fromPort": "vertices", "toNode": 4, "toPort": "in" }},
            {{ "fromNode": 5, "fromPort": "out", "toNode": 7, "toPort": "camera" }},
            {{ "fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "material" }},
            {{ "fromNode": 4, "fromPort": "out", "toNode": 7, "toPort": "vertices" }},
            {{ "fromNode": 7, "fromPort": "color", "toNode": 8, "toPort": "in" }}
        ]
    }}"#
    );
    serde_json::from_str(&json).unwrap()
}

#[test]
fn rt_dynamic_fusion_surface_waves_fused_output_matches_unfused() {
    // Bumpy grid: the wave's triangle normal transport needs non-degenerate
    // triangles (the flat XZ grid's rows are collinear → wave no-ops).
    let input = input_vertices_bumpy();
    let frames = 2;
    // A non-unit ramp: weights_len == 0 (the old degrade) would read every
    // weight as 1.0 and the per-vertex mix below would go uniform — caught.
    let ramp: Vec<f32> = (0..VERTEX_COUNT)
        .map(|i| 0.25 + 0.75 * i as f32 / (VERTEX_COUNT - 1) as f32)
        .collect();

    for (wire_mask_weights, weights, label) in [
        (false, None, "bundled shape (mask weights unwired)"),
        (true, Some(ramp.as_slice()), "wired non-unit mask weights"),
    ] {
        let def = surface_waves_def(wire_mask_weights);
        // The P2 gate at the executable level: this host MUST fuse.
        assert!(
            fused_generator_view_for(&def).is_some(),
            "Surface Waves host ({label}) must fuse after BUG-7wwy + BUG-jwyh"
        );
        let unfused = render_and_readback(&def, false, &input, weights, frames, "morph");
        let fused = render_and_readback(&def, true, &input, weights, frames, "morph");

        // The chain actually deformed the mesh: wave displaces along the
        // normal (word 1 = y) and morph mixes toward it.
        let max_displacement = unfused
            .iter()
            .zip(&input)
            .map(|(out, inp)| (out[1] - inp[1]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_displacement > 1.0e-3,
            "{label}: the wave must visibly displace the grid (max |dy| = {max_displacement})"
        );

        const TOL: f32 = 1.0e-5;
        for (i, (a, b)) in unfused.iter().zip(&fused).enumerate() {
            for (w, (x, y)) in a.iter().zip(b).enumerate() {
                assert!(
                    (x - y).abs() <= TOL,
                    "{label}: vertex {i} word {w}: fused {y} diverged from unfused {x}"
                );
            }
        }
    }
}
