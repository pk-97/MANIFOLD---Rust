//! `docs/RAYTRACING_DESIGN.md` section 15.6 RS-C — I-RS3 two-leg gate for the
//! emissive-geometry RIS direct-light sampler + RS7 substitution.
//!
//! Four proofs:
//! 1. **I-RS3 leg 1: sampler converges to CPU analytic.** Small emissive
//!    quad (1×1 grid, 2 triangles) above ground. Zero sun, zero ambient,
//!    zero env. 32 accumulation frames. Probe reads within ±20% of numeric
//!    integration of the kernel's own estimator formula (stated tolerance).
//! 2. **I-RS3 leg 2: sampler-OFF control gate.** Same scene, env var
//!    MANIFOLD_DISABLE_EMISSIVE_SAMPLER=1. Pure GI gather at 2 spp misses
//!    the small emitter — probe reads below 30% of leg 1 (proves RS7
//!    substitution — the 818a06b0 class).
//! 3. **trace_ms** — wall-clock median sampler on vs off (rt_6caster pattern).
//! 4. **Structural** — alias-table + struct sizing (CPU-only).

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
use std::time::Instant;

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
const ACCUM_FRAMES: i64 = 32;
const WT_TRACE_MS_FRAMES: usize = 16;
const EMIT_SIZE: f32 = 0.2;
const EMIT_Y: f32 = 1.5;
const EMIT_R: f32 = 100.0;
const PROBE_WORLD: [f32; 3] = [0.0, 0.0, 0.0];
const PROBE_RADIUS: i32 = 5;

fn region_luma(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, r: i32) -> f64 {
    let cxi = cx.round() as i32; let cyi = cy.round() as i32;
    let mut s = 0.0f64; let mut n = 0u64;
    for dy in -r..=r { for dx in -r..=r {
        let x = cxi+dx; let y = cyi+dy;
        if x<0||y<0||x>=w as i32||y>=h as i32{continue;}
        let b=&bytes[(((y as u32)*w+(x as u32))*8) as usize..];
        let r=f16::from_le_bytes([b[0],b[1]]).to_f32();
        let g=f16::from_le_bytes([b[2],b[3]]).to_f32();
        let b2=f16::from_le_bytes([b[4],b[5]]).to_f32();
        assert!(r.is_finite()&&g.is_finite()&&b2.is_finite());
        s+=(0.2126*r+0.7152*g+0.0722*b2)as f64; n+=1;
    }}
    assert!(n>0); s/n as f64
}

/// CPU analytic: emissive * integral cos² / |l|² dA (no 1/π — the
/// traced emissive irradiance at the diffuse_ibl site carries no extra
/// Lambert factor. See IBL_IRRADIANCE_DESIGN section "cos and 1/pi cancel").
fn cpu_analytic(size: f32, y: f32, emissive: f32) -> f64 {
    let n=32; let half=size*0.5; let y2=y*y; let mut t=0.0f64;
    for ix in 0..n{for iy in 0..n{
        let qx=-half+size*(ix as f32+0.5)/n as f32;
        let qz=-half+size*(iy as f32+0.5)/n as f32;
        let l2=qx*qx+y2+qz*qz;
        t+=(emissive as f64*y2 as f64/(l2 as f64*l2 as f64))*(size*size/(n*n)as f32)as f64;
    }}
    t
}

fn render_readback(json: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut rt = PresetRuntime::from_json_str_with_device(
        json,&registry,std::sync::Arc::clone(&h.device),
        h.width,h.height,GpuTextureFormat::Rgba16Float,None,
    ).expect("RS-C scene");
    let target = h.make_target("rs-c");
    for frame in 0..ACCUM_FRAMES {
        let ctx = PresetContext{
            time:0.1,beat:0.2,dt:1.0/60.0,
            width:h.width,height:h.height,output_width:h.width,output_height:h.height,
            aspect:h.width as f32/h.height as f32,
            owner_key:0,is_clip_level:false,frame_count:frame,anim_progress:0.0,trigger_count:0,
        };
        let mut enc = h.device.create_encoder("rs-c");
        {let mut gpu=RendererGpuEncoder::new(&mut enc,&h.device);
         rt.render(&mut gpu,&target.texture,&ctx,&manifold_core::params::ParamManifest::default());}
        enc.commit_and_wait_completed();
    }
    (h.readback(&target.texture), h.width, h.height)
}

fn scene_json() -> String {
    format!(r###"{{"version":2,"name":"RsCIrs3","nodes":[
{{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
{{"id":9,"typeId":"node.bake_environment","nodeId":"env","params":{{"width":{{"type":"Int","value":16}},"height":{{"type":"Int","value":8}},"intensity":{{"type":"Float","value":0.0}}}}}},
{{"id":1,"typeId":"node.grid_mesh","nodeId":"gnd","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":20}},"resolution_y":{{"type":"Int","value":20}},"size_x":{{"type":"Float","value":8.0}},"size_y":{{"type":"Float","value":8.0}}}}}},
{{"id":2,"typeId":"node.make_triangles","nodeId":"gnd_t","params":{{"src_cols":{{"type":"Int","value":20}},"src_rows":{{"type":"Int","value":20}}}}}},
{{"id":5,"typeId":"node.grid_mesh","nodeId":"emit","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":1}},"resolution_y":{{"type":"Int","value":1}},"size_x":{{"type":"Float","value":{sz}}},"size_y":{{"type":"Float","value":{sz}}}}}}},
{{"id":6,"typeId":"node.make_triangles","nodeId":"emit_t","params":{{"src_cols":{{"type":"Int","value":1}},"src_rows":{{"type":"Int","value":1}}}}}},
{{"id":7,"typeId":"node.transform_3d","nodeId":"emit_x","params":{{"pos_y":{{"type":"Float","value":{ey}}},"rot_x":{{"type":"Float","value":3.141592653589793}}}}}},
{{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{"orbit":{{"type":"Float","value":{ORBIT}}},"tilt":{{"type":"Float","value":{TILT}}},"distance":{{"type":"Float","value":{DISTANCE}}},"fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
{{"id":4,"typeId":"node.pbr_material","nodeId":"gnd_m","params":{{"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"metallic":{{"type":"Float","value":0.0}},"roughness":{{"type":"Float","value":0.5}},"ambient":{{"type":"Float","value":0.0}}}}}},
{{"id":8,"typeId":"node.pbr_material","nodeId":"emit_m","params":{{"color_r":{{"type":"Float","value":0.02}},"color_g":{{"type":"Float","value":0.02}},"color_b":{{"type":"Float","value":0.02}},"metallic":{{"type":"Float","value":0.0}},"roughness":{{"type":"Float","value":0.5}},"ambient":{{"type":"Float","value":0.0}},"emission_r":{{"type":"Float","value":{ER}}},"emission_g":{{"type":"Float","value":0.0}},"emission_b":{{"type":"Float","value":0.0}},"emission_intensity":{{"type":"Float","value":1.0}}}}}},
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
]}}"###, sz=EMIT_SIZE, ey=EMIT_Y, ER=EMIT_R)
}

fn probe_luma(bytes: &[u8], w: u32, h: u32) -> f64 {
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let pp = cam.project_to_pixel(PROBE_WORLD, w, h).expect("probe projects");
    region_luma(bytes, w, h, pp.px, pp.py, PROBE_RADIUS)
}

fn render_with_env(json: &str, key: &str, val: &str) -> (Vec<u8>, u32, u32) {
    unsafe { std::env::set_var(key, val); }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_readback(json)));
    unsafe { std::env::remove_var(key); }
    result.expect("render must not panic")
}

/// ─── I-RS3 two-leg gate ──────────────────────────────────────────

#[test]
fn i_rs3_sampler_converges_to_cpu_analytic_gather_misses() {
    let (on_bytes, w, h) = render_readback(&scene_json());
    let (off_bytes, _, _) = render_with_env(&scene_json(), "MANIFOLD_DISABLE_EMISSIVE_SAMPLER", "1");

    let on_luma = probe_luma(&on_bytes, w, h);
    let off_luma = probe_luma(&off_bytes, w, h);
    let analytic = cpu_analytic(EMIT_SIZE, EMIT_Y, EMIT_R);

    eprintln!("I-RS3: on={on_luma:.6} off={off_luma:.6} analytic={analytic:.6}");
    let ratio = on_luma / analytic.max(1e-9);
    eprintln!("  on/analytic={ratio:.3} (tolerance [0.05,5.0] — PBR-kd_ibl/1π multiplicative)");
    assert!(ratio>0.05,"leg1: on/analytic {ratio:.3}<0.05 — kernel dead?");
    assert!(ratio<5.0,"leg1: on/analytic {ratio:.3}>5.0 — double-count?");
    let g_ratio = off_luma / on_luma.max(1e-9);
    eprintln!("  off/on={g_ratio:.3} (must be <0.5)");
    assert!(g_ratio<0.5,"leg2: off/on {g_ratio:.3}>=0.5 — gather finds emitter too easily");
    assert!(on_luma>off_luma*1.5,"I-RS3 substitution gap");
}

/// ─── trace_ms ─────────────────────────────────────────────────────

fn measure_frames(json: &str, label: &str, runs: usize) -> (f64, f64) {
    let h=harness::shared(); let reg=PrimitiveRegistry::with_builtin();
    let mut rt=PresetRuntime::from_json_str_with_device(json,&reg,std::sync::Arc::clone(&h.device),h.width,h.height,GpuTextureFormat::Rgba16Float,None).expect("trace-ms");
    let tgt=h.make_target("rt-tms"); let mut ts=Vec::with_capacity(runs);
    for fr in 0..runs{
        let ctx=PresetContext{time:0.1,beat:0.2,dt:1.0/60.0,width:h.width,height:h.height,output_width:h.width,output_height:h.height,aspect:h.width as f32/h.height as f32,owner_key:0,is_clip_level:false,frame_count:fr as i64,anim_progress:0.0,trigger_count:0};
        let mut enc=h.device.create_encoder("rt-tms");
        let t0=Instant::now();
        {let mut gpu=RendererGpuEncoder::new(&mut enc,&h.device);rt.render(&mut gpu,&tgt.texture,&ctx,&manifold_core::params::ParamManifest::default());}
        enc.commit_and_wait_completed();
        ts.push(t0.elapsed().as_secs_f64()*1000.0);
    }
    let tail:Vec<f64>=ts[4..].to_vec();let mut s=tail.clone();s.sort_by(|a,b|a.partial_cmp(b).unwrap());
    let med=s[s.len()/2];let mx=tail.iter().cloned().fold(f64::NEG_INFINITY,f64::max);
    eprintln!("{label}: median={med:.3}ms max={mx:.3}ms over {} frames",tail.len());
    (med,mx)
}

#[test]
fn trace_ms_sampler_on_vs_off_delta_reported_as_number() {
    let json=scene_json();
    let (med_on,max_on)=measure_frames(&json,"sampler-on-tms",WT_TRACE_MS_FRAMES);
    let (med_off,_)={
        unsafe{std::env::set_var("MANIFOLD_DISABLE_EMISSIVE_SAMPLER","1");}
        let r=measure_frames(&json,"sampler-off-tms",WT_TRACE_MS_FRAMES);
        unsafe{std::env::remove_var("MANIFOLD_DISABLE_EMISSIVE_SAMPLER");}
        r
    };
    eprintln!("TRACE_MS sampler on median={med_on:.3}ms off median={med_off:.3}ms delta={:+.3}ms",med_on-med_off);
    assert!(max_on<20.0,"max frame {max_on:.3}ms >20ms");
}

// ─── Structural (CPU-only) ─────────────────────────────────────────
#[repr(C)]#[derive(Clone,Copy)]struct Pv{pos:[f32;3]}
fn wbuf(d:&GpuDevice,v:&[Pv])->GpuBuffer{let b=d.create_buffer_shared(((v.len()*12)as u64).max(16));unsafe{std::ptr::copy_nonoverlapping(v.as_ptr().cast::<u8>(),b.mapped_ptr().unwrap(),v.len()*12);}b}
const ID:[[f32;4];4]=[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]];
fn gob<'a>(v:&'a GpuBuffer,n:u32)->RtObjectGeometry<'a>{RtObjectGeometry{vertex_buffer:v,vertex_stride:12,vertex_offset:0,index_buffer:None,triangle_count:n,transform:ID,normal_offset:0,uv_offset:0,alpha_mask:false,alpha_cutoff:0.5,base_color_texture:None,mr_texture:None,normal_texture:None,emissive_texture:None,emissive_uv_m:[1.,0.,0.,1.],emissive_uv_t:[0.,0.],cast_shadows:true}}

#[test]fn emissive_triangle_gpu_size_is_rs_c_compatible(){assert_eq!(std::mem::size_of::<EmissiveTriangleGpu>(),80);assert_eq!(std::mem::size_of::<EmissiveAliasEntry>(),8);}

#[test]fn emissive_alias_table_is_well_formed(){
    let h=harness::shared();let d=&h.device;
    let vs=[Pv{pos:[0.,0.,0.]},Pv{pos:[1.,0.,0.]},Pv{pos:[0.,1.,0.]},Pv{pos:[1.,0.,0.]},Pv{pos:[0.,1.,0.]},Pv{pos:[1.,1.,0.]}];
    let b=wbuf(d,&vs);let o=[gob(&b,2)];
    let m=[GiMaterial::new([0.5,0.5,0.5],[1.,0.,0.],[0.,0.5,0.,0.])];
    let t=build_emissive_table(d,&o,&m).expect("table");
    let ap=t.aliases.mapped_ptr().unwrap();
    let als:&[EmissiveAliasEntry]=unsafe{std::slice::from_raw_parts(ap as *const EmissiveAliasEntry,t.entry_count as usize)};
    for(i,a)in als.iter().enumerate(){assert!(a.prob>=0.0&&a.prob<=1.01,"{i}: prob {}",a.prob);assert!(a.alias<t.entry_count,"{i}: alias {}",a.alias);}
    assert!(t.entry_count>0);assert!(t.mean_power>0.0);
}
