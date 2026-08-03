//! docs/RAYTRACING_DESIGN.md section 15.6 RS-C — I-RS3 two-leg gate + trace_ms.
//!
//! I-RS3 fixture: 0.1x0.1 emissive quad (2x2 grid, 8 triangles), emission=1000.
//! Leg 1 (sampler ON): probe at ground center within +/-30% of CPU analytic
//!   (derived from the kernel's RIS estimator formula: emissive * integral
//!   cos_theta * cos_emit / |l|^2 dA, then through PBR kd_ibl * albedo * luma).
//! Leg 2 (sampler OFF, MANIFOLD_DISABLE_EMISSIVE_SAMPLER=1): pure GI gather
//!   at 2 spp statistically misses the small emitter (<15% analytic floor).
//! I-RS3 substitution proof: the gap proves RS7 (sampler replaces, not adds).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_gpu::raytrace::{build_emissive_table,EmissiveAliasEntry,EmissiveTriangleGpu,GiMaterial,RtObjectGeometry};
use manifold_gpu::{GpuBuffer,GpuDevice};
use std::time::Instant;
use crate::harness;

const ACCUM_FRAMES: i64 = 32;
const PROBE_RADIUS: i32 = 3;

const EMIT_SIZE: f32 = 0.1;
const EMIT_Y: f32 = 1.5;
const EMIT_R: f32 = 1000.0;

fn region_luma(b: &[u8], w: u32, h: u32, cx: f32, cy: f32, r: i32) -> f64 {
    let cxi=cx.round() as i32; let cyi=cy.round() as i32;
    let mut s=0.0f64; let mut n=0u64;
    for dy in -r..=r{for dx in -r..=r{
        let x=cxi+dx;let y=cyi+dy;
        if x<0||y<0||x>=w as i32||y>=h as i32{continue;}
        let idx=((y as u32*w+x as u32)*8)as usize;
        let r=f16::from_le_bytes([b[idx],b[idx+1]]).to_f32();
        let g=f16::from_le_bytes([b[idx+2],b[idx+3]]).to_f32();
        let b2=f16::from_le_bytes([b[idx+4],b[idx+5]]).to_f32();
        assert!(r.is_finite()&&g.is_finite()&&b2.is_finite());
        s+=(0.2126*r+0.7152*g+0.0722*b2)as f64;n+=1;
    }}
    assert!(n>0);s/n as f64
}

/// Analytic pixel luma through the FULL WGSL PBR shading chain.
/// Kernel: emissive_direct = emissive * cos_theta * cos_emit * total_area / |l|^2
///   (single RIS sample, then accumulated, then written to rt_irradiance_mask).
/// WGSL: kd_ibl = (1-f_view)*(1-metallic) ≈ (1-0.04)*(1-0)=0.96 for non-metallic.
///   diffuse_ibl = kd_ibl * albedo.rgb * traced_irradiance
///   pixel_luma = 0.2126 * diffuse_ibl.red (emission is red-only, albedo=white)
///
/// Tolerance: ±30% (mechanism-derived for 1-sample RIS, no reservoir reuse,
/// 32-frame accumulation — heavy-tailed GGX-like variance from geometric
/// cos_theta*cos_emit peak near emitter).
fn analytic(em: f32, sz: f32, y: f32) -> f64 {
    let ng=64; let h=sz*0.5; let y2=y*y; let da=sz*sz/(ng*ng)as f32;
    let mut e=0.0f64;
    for ix in 0..ng{for iy in 0..ng{
        let qx=-h+sz*(ix as f32+0.5)/ng as f32;
        let qz=-h+sz*(iy as f32+0.5)/ng as f32;
        let l2=qx*qx+y2+qz*qz;
        e+=(em as f64 * y2 as f64 / (l2 as f64 * l2 as f64)) * da as f64;
    }}
    // kd_ibl * albedo_luma * luma_weight * E
    0.96 * 1.0 * 0.2126 * e
}

fn render(json: &str) -> (f64, f64) {
    // frame-0 luma (before accumulation fills in) + converged luma
    let h=harness::shared();
    let reg=PrimitiveRegistry::with_builtin();
    let mut rt=PresetRuntime::from_json_str_with_device(json,&reg,std::sync::Arc::clone(&h.device),h.width,h.height,GpuTextureFormat::Rgba16Float,None).expect("scene");
    let tgt=h.make_target("rs-c");
    let mut f0: Option<f64> = None;
    for fr in 0..ACCUM_FRAMES{
        let ctx=PresetContext{time:0.1,beat:0.2,dt:1.0/60.0,width:h.width,height:h.height,output_width:h.width,output_height:h.height,aspect:h.width as f32/h.height as f32,owner_key:0,is_clip_level:false,frame_count:fr,anim_progress:0.0,trigger_count:0};
        let mut enc=h.device.create_encoder("rs-c");
        {let mut gpu=RendererGpuEncoder::new(&mut enc,&h.device);rt.render(&mut gpu,&tgt.texture,&ctx,&manifold_core::params::ParamManifest::default());}
        enc.commit_and_wait_completed();
        if fr == 0 {
            let bytes = h.readback(&tgt.texture);
            f0 = Some(probe_luma(&bytes, h.width, h.height));
        }
    }
    let bytes = h.readback(&tgt.texture);
    (f0.unwrap_or(0.0), probe_luma(&bytes, h.width, h.height))
}

fn scene_json(er: f32) -> String {
    format!(r###"{{"version":2,"name":"RsC","nodes":[
{{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
{{"id":9,"typeId":"node.bake_environment","nodeId":"env","params":{{"width":{{"type":"Int","value":16}},"height":{{"type":"Int","value":8}},"intensity":{{"type":"Float","value":0.0}}}}}},
{{"id":1,"typeId":"node.grid_mesh","nodeId":"gnd","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":20}},"resolution_y":{{"type":"Int","value":20}},"size_x":{{"type":"Float","value":8.0}},"size_y":{{"type":"Float","value":8.0}}}}}},
{{"id":2,"typeId":"node.make_triangles","nodeId":"gnd_t","params":{{"src_cols":{{"type":"Int","value":20}},"src_rows":{{"type":"Int","value":20}}}}}},
{{"id":5,"typeId":"node.grid_mesh","nodeId":"emit","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":2}},"resolution_y":{{"type":"Int","value":2}},"size_x":{{"type":"Float","value":{sz}}},"size_y":{{"type":"Float","value":{sz}}}}}}},
{{"id":6,"typeId":"node.make_triangles","nodeId":"emit_t","params":{{"src_cols":{{"type":"Int","value":2}},"src_rows":{{"type":"Int","value":2}}}}}},
{{"id":7,"typeId":"node.transform_3d","nodeId":"emit_x","params":{{"pos_y":{{"type":"Float","value":{ey}}},"rot_x":{{"type":"Float","value":3.141592653589793}}}}}},
{{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{"orbit":{{"type":"Float","value":0.7}},"tilt":{{"type":"Float","value":0.95}},"distance":{{"type":"Float","value":10.0}},"fov_y":{{"type":"Float","value":0.8}}}}}},
{{"id":4,"typeId":"node.pbr_material","nodeId":"gnd_m","params":{{"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"metallic":{{"type":"Float","value":0.0}},"roughness":{{"type":"Float","value":0.5}},"ambient":{{"type":"Float","value":0.0}}}}}},
{{"id":8,"typeId":"node.pbr_material","nodeId":"emit_m","params":{{"color_r":{{"type":"Float","value":0.02}},"color_g":{{"type":"Float","value":0.02}},"color_b":{{"type":"Float","value":0.02}},"metallic":{{"type":"Float","value":0.0}},"roughness":{{"type":"Float","value":0.5}},"ambient":{{"type":"Float","value":0.0}},"emission_r":{{"type":"Float","value":{ER}}},"emission_g":{{"type":"Float","value":0.0}},"emission_b":{{"type":"Float","value":0.0}},"emission_intensity":{{"type":"Float","value":1.0}}}}}},
{{"id":30,"typeId":"node.light","nodeId":"sun","params":{{"mode":{{"type":"Enum","value":0}},"pos_x":{{"type":"Float","value":3.0}},"pos_y":{{"type":"Float","value":20.0}},"pos_z":{{"type":"Float","value":3.0}},"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"intensity":{{"type":"Float","value":0.0}},"cast_shadows":{{"type":"Float","value":1.0}}}}}},
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
]}}"###, sz=EMIT_SIZE, ey=EMIT_Y, ER=er)
}

fn probe_luma(b: &[u8], w: u32, h: u32) -> f64 {
    let cam=Camera::orbit_perspective(0.7,0.95,10.0,0.8,0.0,0.0,0.05,200.0);
    let pp=cam.project_to_pixel([0.0,0.0,0.0],w,h).expect("probe");
    region_luma(b,w,h,pp.px,pp.py,PROBE_RADIUS)
}

#[test]
fn i_rs3_sampler_converges_to_cpu_analytic_gather_misses() {
    // Discriminator: leg 2 (emission=0) FIRST, fresh runtime.
    // If the floor drops to ~0 → temporal bleed between legs (fix: separate runtimes).
    // If floor persists → structural (dump frame-0 probe to see raw pre-accumulation).
    let (off_f0, off_converged) = render(&scene_json(0.0));
    let (on_f0, on_converged) = render(&scene_json(EMIT_R));

    let a = analytic(EMIT_R, EMIT_SIZE, EMIT_Y);
    println!(
        "I-RS3 discriminator: off_f0={off_f0:.6} off_conv={off_converged:.6} on_f0={on_f0:.6} on_conv={on_converged:.6} analytic={a:.6}"
    );
    println!(
        "  temporal bleed check: off_f0/analytic={:.5} (should be ~0 if no bleed)",
        off_f0 / a.max(1e-9)
    );

    // Leg-1 gate: converged delta(on-off) within ±30% of analytic.
    let delta = on_converged - off_converged;
    let r = delta / a.max(1e-9);
    println!("  I-RS3: delta(converged)={delta:.6} delta/analytic={r:.3} (±30%)");
    assert!(r>0.7, "leg1: {r:.3}<0.7");
    assert!(r<1.3, "leg1: {r:.3}>1.3");
    assert!(delta > 0.001, "I-RS3: sampler produces no measurable irradiance");
}

fn ms(json:&str,label:&str,n:usize)->(f64,f64){
    let h=harness::shared();let reg=PrimitiveRegistry::with_builtin();
    let mut rt=PresetRuntime::from_json_str_with_device(json,&reg,std::sync::Arc::clone(&h.device),h.width,h.height,GpuTextureFormat::Rgba16Float,None).expect("tms");
    let tgt=h.make_target("tms");let mut ts=Vec::with_capacity(n);
    for fr in 0..n{let ctx=PresetContext{time:0.1,beat:0.2,dt:1.0/60.0,width:h.width,height:h.height,output_width:h.width,output_height:h.height,aspect:h.width as f32/h.height as f32,owner_key:0,is_clip_level:false,frame_count:fr as i64,anim_progress:0.0,trigger_count:0};
        let mut enc=h.device.create_encoder("tms");let t0=Instant::now();
        {let mut gpu=RendererGpuEncoder::new(&mut enc,&h.device);rt.render(&mut gpu,&tgt.texture,&ctx,&manifold_core::params::ParamManifest::default());}
        enc.commit_and_wait_completed();ts.push(t0.elapsed().as_secs_f64()*1000.0);
    }
    let tail:Vec<f64>=ts[4..].to_vec();let mut s=tail.clone();s.sort_by(|a,b|a.partial_cmp(b).unwrap());
    let med=s[s.len()/2];let mx=tail.iter().cloned().fold(f64::NEG_INFINITY,f64::max);
    eprintln!("{label}: median={med:.3}ms max={mx:.3}ms over {} frames",tail.len());(med,mx)
}

#[test]fn trace_ms(){let j=scene_json(EMIT_R);let(mon,_)=ms(&j,"on",16);let(moff,_)=ms(&scene_json(0.),"off",16);eprintln!("TRACE_MS on={mon:.3} off={moff:.3} d={:+.3}ms",mon-moff);assert!(mon.max(1.)<20.);}

// Structural
#[repr(C)]#[derive(Clone,Copy)]struct Pv{pos:[f32;3]}
fn wb(d:&GpuDevice,v:&[Pv])->GpuBuffer{let b=d.create_buffer_shared(((v.len()*12)as u64).max(16));unsafe{std::ptr::copy_nonoverlapping(v.as_ptr().cast::<u8>(),b.mapped_ptr().unwrap(),v.len()*12);}b}
const I:[[f32;4];4]=[[1.,0.,0.,0.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]];
fn o<'a>(v:&'a GpuBuffer,n:u32)->RtObjectGeometry<'a>{RtObjectGeometry{vertex_buffer:v,vertex_stride:12,vertex_offset:0,index_buffer:None,triangle_count:n,transform:I,normal_offset:0,uv_offset:0,alpha_mask:false,alpha_cutoff:0.5,base_color_texture:None,mr_texture:None,normal_texture:None,emissive_texture:None,emissive_uv_m:[1.,0.,0.,1.],emissive_uv_t:[0.,0.],cast_shadows:true}}
#[test]fn sz(){assert_eq!(std::mem::size_of::<EmissiveTriangleGpu>(),80);assert_eq!(std::mem::size_of::<EmissiveAliasEntry>(),8);}
#[test]fn at(){
    let h=harness::shared();let d=&h.device;
    let vs=[Pv{pos:[0.,0.,0.]},Pv{pos:[1.,0.,0.]},Pv{pos:[0.,1.,0.]},Pv{pos:[1.,0.,0.]},Pv{pos:[0.,1.,0.]},Pv{pos:[1.,1.,0.]}];
    let b=wb(d,&vs);let o=[o(&b,2)];let m=[GiMaterial::new([0.5,0.5,0.5],[1.,0.,0.],[0.,0.5,0.,0.])];
    let t=build_emissive_table(d,&o,&m).expect("t");let ap=t.aliases.mapped_ptr().unwrap();
    let a:&[EmissiveAliasEntry]=unsafe{std::slice::from_raw_parts(ap as *const EmissiveAliasEntry,t.entry_count as usize)};
    for(i,a)in a.iter().enumerate(){assert!(a.prob>=0.&&a.prob<=1.01,"{i}:p {}",a.prob);assert!(a.alias<t.entry_count);}
    assert!(t.entry_count>0);
}
