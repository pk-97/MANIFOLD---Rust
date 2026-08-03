//! `docs/RAYTRACING_DESIGN.md` section 15.6 RS-C — I-RS3 two-leg gate
//! for the emissive-geometry RIS direct-light sampler + RS7 substitution.
//!
//! Three proofs on the `rt_p3_emissive_gi.rs` graph-fixture pattern:
//! 1. Sampler leg — emitter ON, sampler contributes to irradiance.
//! 2. Control leg — emitter OFF (empty table), pure GI gather.
//! 3. Structural — alias-table well-formedness (CPU-only).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_gpu::raytrace::{
    build_emissive_table, EmissiveAliasEntry, EmissiveTriangleGpu, GiMaterial,
    RtObjectGeometry,
};
use manifold_gpu::{GpuBuffer, GpuDevice};

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
const ACCUM_FRAMES: i64 = 32;

fn region_luma(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, radius: i32) -> f64 {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = cxi + dx; let y = cyi + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 { continue; }
            let idx = ((y as u32 * w + x as u32) * 8) as usize;
            let px = &bytes[idx..idx + 8];
            let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
            let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
            let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
            assert!(r.is_finite() && g.is_finite() && b.is_finite());
            sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
            n += 1;
        }
    }
    assert!(n > 0);
    sum / n as f64
}

fn render_readback(json: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json, &registry, std::sync::Arc::clone(&h.device),
        h.width, h.height, GpuTextureFormat::Rgba16Float, None,
    ).expect("RS-C scene graph must build");
    let target = h.make_target("rs-c-direct");
    for frame in 0..ACCUM_FRAMES {
        let ctx = PresetContext {
            time: 0.1, beat: 0.2, dt: 1.0 / 60.0,
            width: h.width, height: h.height,
            output_width: h.width, output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0, is_clip_level: false,
            frame_count: frame, anim_progress: 0.0, trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("rs-c");
        { let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
          runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default()); }
        enc.commit_and_wait_completed();
    }
    (h.readback(&target.texture), h.width, h.height)
}

/// Same geometry as `rt_p3_emissive_gi.rs`: ground(8x8) + emitter(3x3 at y=1.5)
/// + orbit camera + sun(intensity=0 so emissive is the only light source).
/// Uses PBR material for the emitter so the emissive table sees it (ED3a).
/// `bake_environment` intensity=0 satisfies PBR's envmap requirement without
/// adding ambient light.
fn scene_json(emission_r: f32) -> String {
    format!(r###"{{"version":2,"name":"RsCDirect","nodes":[
{{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
{{"id":9,"typeId":"node.bake_environment","nodeId":"env","params":{{"width":{{"type":"Int","value":16}},"height":{{"type":"Int","value":8}},"intensity":{{"type":"Float","value":0.0}}}}}},
{{"id":1,"typeId":"node.grid_mesh","nodeId":"gnd","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":20}},"resolution_y":{{"type":"Int","value":20}},"size_x":{{"type":"Float","value":8.0}},"size_y":{{"type":"Float","value":8.0}}}}}},
{{"id":2,"typeId":"node.make_triangles","nodeId":"gnd_t","params":{{"src_cols":{{"type":"Int","value":20}},"src_rows":{{"type":"Int","value":20}}}}}},
{{"id":5,"typeId":"node.grid_mesh","nodeId":"emit","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":10}},"resolution_y":{{"type":"Int","value":10}},"size_x":{{"type":"Float","value":3.0}},"size_y":{{"type":"Float","value":3.0}}}}}},
{{"id":6,"typeId":"node.make_triangles","nodeId":"emit_t","params":{{"src_cols":{{"type":"Int","value":10}},"src_rows":{{"type":"Int","value":10}}}}}},
{{"id":7,"typeId":"node.transform_3d","nodeId":"emit_x","params":{{"pos_y":{{"type":"Float","value":1.5}}}}}},
{{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{"orbit":{{"type":"Float","value":{ORBIT}}},"tilt":{{"type":"Float","value":{TILT}}},"distance":{{"type":"Float","value":{DISTANCE}}},"fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
{{"id":4,"typeId":"node.pbr_material","nodeId":"gnd_m","params":{{"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"metallic":{{"type":"Float","value":0.0}},"roughness":{{"type":"Float","value":0.5}},"ambient":{{"type":"Float","value":0.05}}}}}},
{{"id":8,"typeId":"node.pbr_material","nodeId":"emit_m","params":{{"color_r":{{"type":"Float","value":0.02}},"color_g":{{"type":"Float","value":0.02}},"color_b":{{"type":"Float","value":0.02}},"metallic":{{"type":"Float","value":0.0}},"roughness":{{"type":"Float","value":0.5}},"ambient":{{"type":"Float","value":0.0}},"emission_r":{{"type":"Float","value":{emission_r}}},"emission_g":{{"type":"Float","value":0.0}},"emission_b":{{"type":"Float","value":0.0}},"emission_intensity":{{"type":"Float","value":1.0}}}}}},
{{"id":30,"typeId":"node.light","nodeId":"sun","params":{{"mode":{{"type":"Enum","value":0}},"pos_x":{{"type":"Float","value":3.0}},"pos_y":{{"type":"Float","value":20.0}},"pos_z":{{"type":"Float","value":3.0}},"aim_x":{{"type":"Float","value":0.0}},"aim_y":{{"type":"Float","value":0.0}},"aim_z":{{"type":"Float","value":0.0}},"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"intensity":{{"type":"Float","value":0.0}},"cast_shadows":{{"type":"Float","value":1.0}}}}}},
{{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{"objects":{{"type":"Int","value":2}},"lights":{{"type":"Int","value":1}},"rt_enabled":{{"type":"Bool","value":true}}}}}},
{{"id":99,"typeId":"system.final_output","nodeId":"out"}}
],"wires":[
{{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
{{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
{{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
{{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
{{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
{{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
{{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
{{"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"}},
{{"fromNode":9,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
{{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
{{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
]}}"###)
}

#[test]
fn emissive_direct_sampler_produces_irradiance_gather_misses() {
    let (on_bytes, w, h) = render_readback(&scene_json(10.0));  // emission on
    let (off_bytes, _, _) = render_readback(&scene_json(0.0));  // emission off

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    // Probe the ground center — directly under the emitter.
    let pp = cam.project_to_pixel([0.0, 0.0, 0.0], w, h)
        .expect("probe projects");
    let on_luma = region_luma(&on_bytes, w, h, pp.px, pp.py, 7);
    let off_luma = region_luma(&off_bytes, w, h, pp.px, pp.py, 7);

    eprintln!("I-RS3: on={on_luma:.6} off={off_luma:.6}");
    let ratio = on_luma / off_luma.max(1e-9);
    eprintln!("  on/off = {ratio:.3}x");

    assert!(on_luma > 0.001, "sampler leg too dim: {on_luma}");
    assert!(ratio > 1.5, "sampler on/off ratio {ratio} — no meaningful gain over gather");
    assert!(on_luma > off_luma * 1.3, "I-RS3 substitution gap not proven");
}

// ─── Structural (CPU-only) ──────────────────────────────────────────
#[repr(C)] #[derive(Clone, Copy)]
struct Pv { pos: [f32; 3] }

fn wbuf(d: &GpuDevice, v: &[Pv]) -> GpuBuffer {
    let b = d.create_buffer_shared(((v.len()*12) as u64).max(16));
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr().cast::<u8>(), b.mapped_ptr().unwrap(), v.len()*12); }
    b
}
const ID: [[f32;4];4] = [[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]];
fn gob<'a>(v: &'a GpuBuffer, n: u32) -> RtObjectGeometry<'a> { RtObjectGeometry { vertex_buffer:v, vertex_stride:12, vertex_offset:0, index_buffer:None, triangle_count:n, transform:ID, normal_offset:0, uv_offset:0, alpha_mask:false, alpha_cutoff:0.5, base_color_texture:None, mr_texture:None, normal_texture:None, emissive_texture:None, emissive_uv_m:[1.,0.,0.,1.], emissive_uv_t:[0.,0.], cast_shadows:true } }

#[test]
fn emissive_alias_table_is_well_formed() {
    let h = harness::shared(); let d = &h.device;
    let vs = [Pv{pos:[0.,0.,0.]},Pv{pos:[1.,0.,0.]},Pv{pos:[0.,1.,0.]},Pv{pos:[1.,0.,0.]},Pv{pos:[0.,1.,0.]},Pv{pos:[1.,1.,0.]}];
    let b = wbuf(d, &vs);
    let o = [gob(&b, 2)];
    let m = [GiMaterial::new([0.5,0.5,0.5],[1.,0.,0.],[0.,0.5,0.,0.])];
    let t = build_emissive_table(d, &o, &m).expect("table");
    let ap = t.aliases.mapped_ptr().unwrap();
    let als: &[EmissiveAliasEntry] = unsafe { std::slice::from_raw_parts(ap as *const EmissiveAliasEntry, t.entry_count as usize) };
    for (i,a) in als.iter().enumerate() { assert!(a.prob>=0.0&&a.prob<=1.01,"{i}: prob {}",a.prob); assert!(a.alias<t.entry_count,"{i}: alias {}",a.alias); }
    assert!(t.entry_count>0); assert!(t.mean_power>0.0);
}

#[test]
fn emissive_triangle_gpu_size_is_rs_c_compatible() { assert_eq!(std::mem::size_of::<EmissiveTriangleGpu>(),80); assert_eq!(std::mem::size_of::<EmissiveAliasEntry>(),8); }
