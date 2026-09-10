//! Live Water S7 GPU proofs — the water surface integrated in
//! `node.render_scene` (WATER_SIMULATION_DESIGN.md section 7,
//! WATER_IMPLEMENTATION_PLAN.md section 4 S7 gate).
//!
//! Static fixture through the REAL graph and `PresetRuntime` (never a
//! side-channel renderer): an overhead `node.orbit_camera`, a seeded
//! particle slab (`node.seed_water` → `node.particle_surface_depth` /
//! `node.particle_thickness` → `node.bilateral_blur` H/V ClipDepth →
//! `node.normals_from_depth` → the five render_scene water inputs), and
//! three unlit `node.grid_mesh` objects — an opaque foreground plane,
//! a partly submerged vertical wall, and a floor plane behind the water.
//! Numeric assertions only; no image-taste pass/fail.
//!
//! CPU oracle: the splat depth/coverage math is transcribed
//! independently here (same convention as `water_surface.rs` — view
//! z-negation, V1 reject rules, conservative bbox, exact ray-sphere
//! test, shared clip-depth mapping); the particle lattice mirrors
//! `seed_water_body.wgsl` (h/2 spacing, cell-centred, row-major).

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use half::f16;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::{Camera, CameraMode, delinearize_depth};
use manifold_renderer::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use manifold_renderer::node_graph::water::GRID_SPACING;
use manifold_renderer::node_graph::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamDef, PrimitiveRegistry,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

// ---------------------------------------------------------------------------
// Fixture constants — shared by the graph JSON and the CPU oracle. Any
// change here changes both sides; the camera is validated against the
// water_camera wire by VALUE (section 7), so the JSON camera params and
// `proof_camera()` must stay in lockstep.
// ---------------------------------------------------------------------------

// Camera orbited 90° so screen-down maps to world +z: the refraction
// displacement shifts samples a few px toward screen-down, and the wall
// is a z=0 plane (a one-pixel-tall screen line), so displaced samples
// stay on the floor except in a narrow band the region finder excludes.
const CAM_ORBIT: f32 = std::f32::consts::FRAC_PI_2;
const CAM_TILT: f32 = 1.5;
const CAM_DISTANCE: f32 = 8.0;
const CAM_FOV_Y: f32 = 0.6;
const CAM_NEAR: f32 = 0.05;
const CAM_FAR: f32 = 200.0;

/// Water pool box (node.seed_water params, mirrored by the oracle
/// lattice). 0.5 m deep: caps the splat thickness (~1.3 m → ~0.12 m
/// calibrated) so the refraction displacement stays under ~8 px.
const POOL_MIN: [f32; 3] = [-0.9, -0.55, -0.7];
const POOL_MAX: [f32; 3] = [0.9, -0.05, 0.7];

/// Impostor radius — `node.particle_surface_depth`'s default (0.75*h).
const SPLAT_RADIUS: f32 = 0.75 * GRID_SPACING;

// Scene object colours (unlit, opaque) — region finders key off these.
const FG_COLOR: [f32; 3] = [1.0, 0.25, 0.25]; // foreground plane
const WALL_COLOR: [f32; 3] = [1.0, 0.55, 0.10]; // partly submerged wall
const FLOOR_COLOR: [f32; 3] = [0.80, 0.80, 0.80]; // behind-water floor

/// V1 water material (section 7 defaults): PBR dielectric, IOR 1.333,
/// transmission 1, metallic 0, roughness 0.04, attenuation distance 2 m,
/// attenuation colour (0.70, 0.90, 0.95).
fn water_material_json() -> String {
    String::from(
        concat!(
            "\"color_r\":{\"type\":\"Float\",\"value\":1.0},",
            "\"color_g\":{\"type\":\"Float\",\"value\":1.0},",
            "\"color_b\":{\"type\":\"Float\",\"value\":1.0},",
            "\"color_a\":{\"type\":\"Float\",\"value\":1.0},",
            "\"metallic\":{\"type\":\"Float\",\"value\":0.0},",
            "\"roughness\":{\"type\":\"Float\",\"value\":0.04},",
            "\"alpha_mode\":{\"type\":\"Enum\",\"value\":0},",
            "\"ior\":{\"type\":\"Float\",\"value\":1.333},",
            "\"transmission\":{\"type\":\"Float\",\"value\":1.0},",
            "\"volume_attenuation_distance\":{\"type\":\"Float\",\"value\":2.0},",
            "\"volume_attenuation_color_r\":{\"type\":\"Float\",\"value\":0.70},",
            "\"volume_attenuation_color_g\":{\"type\":\"Float\",\"value\":0.90},",
            "\"volume_attenuation_color_b\":{\"type\":\"Float\",\"value\":0.95}",
        ),
    )
}

fn proof_camera() -> Camera {
    Camera::orbit_perspective(
        CAM_ORBIT, CAM_TILT, CAM_DISTANCE, CAM_FOV_Y, 0.0, 0.0, CAM_NEAR, CAM_FAR,
    )
}

// ---------------------------------------------------------------------------
// CPU oracle — independent transcription of the splat math
// (water_surface.rs convention, fov parameterised).
// ---------------------------------------------------------------------------

struct Oracle {
    cam: Camera,
    radius: f32,
    w: u32,
    h: u32,
}

impl Oracle {
    fn fov_y(&self) -> f32 {
        let CameraMode::Perspective { fov_y } = self.cam.mode else {
            panic!("proof cameras are perspective")
        };
        fov_y
    }

    /// View-space centre in the +z-forward frame (z component negated
    /// once — mirrors splat_view_center in particle_splat_common.wgsl).
    fn view_pos(&self, world: [f32; 3]) -> [f32; 3] {
        let m = &self.cam.view;
        let v = [world[0], world[1], world[2], 1.0f32];
        let mut out = [0.0f32; 3];
        for row in 0..3 {
            out[row] = m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2] + m[3][row];
        }
        out[2] = -out[2];
        out
    }

    /// V1 reject rules (mirror of splat_accept).
    fn accept(&self, c: [f32; 3]) -> bool {
        let r = self.radius;
        c.iter().all(|v| v.is_finite())
            && c[2] - r > self.cam.near
            && c[0] * c[0] + c[1] * c[1] + c[2] * c[2] >= r * r
            && c[2] - r <= self.cam.far
    }

    /// Conservative inclusive pixel bbox (mirror of splat_bbox).
    fn bbox(&self, c: [f32; 3]) -> Option<(i32, i32, i32, i32)> {
        let (w, h) = (self.w as f32, self.h as f32);
        let aspect = w / h;
        let tan_half = (self.fov_y() * 0.5).tan();
        let f_px_x = 0.5 * w / (tan_half * aspect);
        let f_px_y = 0.5 * h / tan_half;
        let z_near = c[2] - self.radius;
        if z_near <= 0.0 {
            return None;
        }
        let sx = (c[0] / c[2]) * f_px_x + 0.5 * w;
        let sy = (-c[1] / c[2]) * f_px_y + 0.5 * h;
        let r = self.radius;
        let ex = (r * (c[2] + c[0].abs()) / (z_near * c[2])) * f_px_x;
        let ey = (r * (c[2] + c[1].abs()) / (z_near * c[2])) * f_px_y;
        let x0 = ((sx - ex).floor() as i32).clamp(0, self.w as i32 - 1);
        let y0 = ((sy - ey).floor() as i32).clamp(0, self.h as i32 - 1);
        let x1 = ((sx + ex).ceil() as i32).clamp(0, self.w as i32 - 1);
        let y1 = ((sy + ey).ceil() as i32).clamp(0, self.h as i32 - 1);
        Some((x0, y0, x1, y1))
    }

    fn ray_dir(&self, px: [i32; 2]) -> [f32; 3] {
        let (w, h) = (self.w as f32, self.h as f32);
        let aspect = w / h;
        let tan_half = (self.fov_y() * 0.5).tan();
        let ndc_x = ((px[0] as f32 + 0.5) / w) * 2.0 - 1.0;
        let ndc_y = 1.0 - ((px[1] as f32 + 0.5) / h) * 2.0;
        let d = [ndc_x * tan_half * aspect, ndc_y * tan_half, 1.0f32];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        [d[0] / len, d[1] / len, d[2] / len]
    }

    /// Front hit distance along the ray, or None (mirror of splat_ray_hit).
    fn ray_hit(&self, c: [f32; 3], dir: [f32; 3]) -> Option<f32> {
        let b = dir[0] * c[0] + dir[1] * c[1] + dir[2] * c[2];
        let disc = b * b - (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]) + self.radius * self.radius;
        if disc <= 0.0 {
            return None;
        }
        let t = b - disc.sqrt();
        if t <= 0.0 {
            return None;
        }
        Some(t)
    }

    /// Per-pixel min clip depth + coverage, exactly as the splat/resolve
    /// pair computes them.
    fn depth_coverage(&self, positions: &[[f32; 3]]) -> (Vec<f32>, Vec<u8>) {
        let mut depth = vec![1.0f32; (self.w * self.h) as usize];
        let mut cov = vec![0u8; (self.w * self.h) as usize];
        for &p in positions {
            let c = self.view_pos(p);
            if !self.accept(c) {
                continue;
            }
            let Some((x0, y0, x1, y1)) = self.bbox(c) else {
                continue;
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let dir = self.ray_dir([x, y]);
                    let Some(t) = self.ray_hit(c, dir) else {
                        continue;
                    };
                    let view_z = t * dir[2];
                    if view_z <= self.cam.near {
                        continue;
                    }
                    let raw =
                        delinearize_depth(view_z, self.cam.near, self.cam.far).clamp(0.0, 0.999_999_94);
                    let i = (y as u32 * self.w + x as u32) as usize;
                    if raw < depth[i] {
                        depth[i] = raw;
                    }
                    cov[i] = 255;
                }
            }
        }
        (depth, cov)
    }
}

/// Mirror of `seed_water_body.wgsl`: h/2 spacing, cell-centred,
/// `n = floor(extent/spacing + 0.5)` records per axis.
fn seed_positions(lo: [f32; 3], hi: [f32; 3], grid_spacing: f32) -> Vec<[f32; 3]> {
    let spacing = grid_spacing * 0.5;
    // `as u32` saturates negatives to 0, matching seed_water's WGSL
    // u32() conversion of the same formula.
    let n = |extent: f32| (extent / spacing + 0.5).floor() as u32;
    let (nx, ny, nz) = (n(hi[0] - lo[0]), n(hi[1] - lo[1]), n(hi[2] - lo[2]));
    let mut out = Vec::with_capacity((nx * ny * nz) as usize);
    for iz in 0..nz {
        for iy in 0..ny {
            for ix in 0..nx {
                out.push([
                    lo[0] + (ix as f32 + 0.5) * spacing,
                    lo[1] + (iy as f32 + 0.5) * spacing,
                    lo[2] + (iz as f32 + 0.5) * spacing,
                ]);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Graph construction.
//
// Node ids: 0 generator_input, 1 scene/water camera, 2 seed_water,
// 3 particle_surface_depth, 4 particle_thickness, 5/6 bilateral H/V,
// 7 normals_from_depth, 8 water material, 9..=20 the three scene objects
// (grid mesh -> make_triangles -> transform_3d -> unlit_material each),
// 30 render_scene, 31 depth dead-end sink, 99 final_output.
// ---------------------------------------------------------------------------

/// `{"name":{"type":"Float","value":v}}` — one balanced param object, so
/// the graph builders never hand-count closing braces.
fn fparam(name: &str, value: impl std::fmt::Display) -> String {
    format!("\"{name}\":{{\"type\":\"Float\",\"value\":{value}}}")
}

fn camera_node(id: u32, distance: f32) -> String {
    let params = [
        fparam("orbit", CAM_ORBIT),
        fparam("tilt", CAM_TILT),
        fparam("distance", distance),
        fparam("fov_y", CAM_FOV_Y),
        fparam("look_y", 0.0),
        fparam("roll", 0.0),
        fparam("near", CAM_NEAR),
        fparam("far", CAM_FAR),
    ]
    .join(",");
    format!("{{\"id\":{id},\"typeId\":\"node.orbit_camera\",\"nodeId\":\"cam_{id}\",\"params\":{{{params}}}}},")
}

fn seed_water_node() -> String {
    let params = [
        fparam("pool_min_x", format!("{:.6}", POOL_MIN[0])),
        fparam("pool_min_y", format!("{:.6}", POOL_MIN[1])),
        fparam("pool_min_z", format!("{:.6}", POOL_MIN[2])),
        fparam("pool_max_x", format!("{:.6}", POOL_MAX[0])),
        fparam("pool_max_y", format!("{:.6}", POOL_MAX[1])),
        fparam("pool_max_z", format!("{:.6}", POOL_MAX[2])),
        fparam("grid_spacing", GRID_SPACING),
        fparam("rest_density", 1000.0),
        "\"max_capacity\":{\"type\":\"Int\",\"value\":131072}".to_string(),
    ]
    .join(",");
    format!("{{\"id\":2,\"typeId\":\"node.seed_water\",\"nodeId\":\"seed\",\"params\":{{{params}}}}},")
}

/// One unlit grid-mesh object, id-offset by `base`, wired into render_scene
/// object `slot` by `wire_object`. Grid lies in the XZ plane (N = +Y);
/// `rot_x` PI/2 stands it up as a world-XY wall.
fn grid_object(
    base: u32,
    slot: u32,
    pos: [f32; 3],
    rot_x: f32,
    size: [f32; 2],
    color: [f32; 3],
) -> (String, String) {
    let (grid, tris, xf, mat) = (base, base + 1, base + 2, base + 3);
    let grid_params = [
        "\"max_capacity\":{\"type\":\"Int\",\"value\":256}".to_string(),
        "\"resolution_x\":{\"type\":\"Int\",\"value\":2}".to_string(),
        "\"resolution_y\":{\"type\":\"Int\",\"value\":2}".to_string(),
        fparam("size_x", format!("{:.4}", size[0])),
        fparam("size_y", format!("{:.4}", size[1])),
    ]
    .join(",");
    let tris_params = [
        "\"src_cols\":{\"type\":\"Int\",\"value\":2}".to_string(),
        "\"src_rows\":{\"type\":\"Int\",\"value\":2}".to_string(),
    ]
    .join(",");
    let xf_params = [
        fparam("pos_x", format!("{:.4}", pos[0])),
        fparam("pos_y", format!("{:.4}", pos[1])),
        fparam("pos_z", format!("{:.4}", pos[2])),
        fparam("rot_x", format!("{:.6}", rot_x)),
    ]
    .join(",");
    let mat_params = [
        fparam("color_r", format!("{:.4}", color[0])),
        fparam("color_g", format!("{:.4}", color[1])),
        fparam("color_b", format!("{:.4}", color[2])),
        fparam("color_a", 1.0),
        "\"alpha_mode\":{\"type\":\"Enum\",\"value\":0}".to_string(),
    ]
    .join(",");
    let nodes = format!(
        "{{\"id\":{grid},\"typeId\":\"node.grid_mesh\",\"nodeId\":\"grid_{slot}\",\"params\":{{{grid_params}}}}},\
         {{\"id\":{tris},\"typeId\":\"node.make_triangles\",\"nodeId\":\"tris_{slot}\",\"params\":{{{tris_params}}}}},\
         {{\"id\":{xf},\"typeId\":\"node.transform_3d\",\"nodeId\":\"xf_{slot}\",\"params\":{{{xf_params}}}}},\
         {{\"id\":{mat},\"typeId\":\"node.unlit_material\",\"nodeId\":\"mat_{slot}\",\"params\":{{{mat_params}}}}},"
    );
    (nodes, format!("{grid}|{tris}|{xf}|{mat}"))
}

fn wire_object(ids: &str, slot: u32) -> String {
    let parts: Vec<&str> = ids.split('|').collect();
    let (grid, tris, xf, mat) = (parts[0], parts[1], parts[2], parts[3]);
    format!(
        "{{\"fromNode\":{grid},\"fromPort\":\"vertices\",\"toNode\":{tris},\"toPort\":\"in\"}},\
         {{\"fromNode\":{tris},\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"mesh_{slot}\"}},\
         {{\"fromNode\":{xf},\"fromPort\":\"transform\",\"toNode\":30,\"toPort\":\"transform_{slot}\"}},\
         {{\"fromNode\":{mat},\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"material_{slot}\"}},"
    )
}

/// A Blend pbr object (for the has_transmission rejection subcase):
/// alpha_mode Blend (2) with transmission 1 — exactly the E2a
/// transmission condition.
fn blend_object_nodes(base: u32, _slot: u32) -> String {
    let pbr_params = [
        fparam("color_r", 1.0),
        fparam("color_g", 1.0),
        fparam("color_b", 1.0),
        fparam("color_a", 0.5),
        "\"alpha_mode\":{\"type\":\"Enum\",\"value\":2}".to_string(),
        fparam("transmission", 1.0),
    ]
    .join(",");
    format!(
        "{{\"id\":{b},\"typeId\":\"node.grid_mesh\",\"nodeId\":\"grid_blend\",\"params\":{{\
            \"max_capacity\":{{\"type\":\"Int\",\"value\":256}},\
            \"resolution_x\":{{\"type\":\"Int\",\"value\":2}},\
            \"resolution_y\":{{\"type\":\"Int\",\"value\":2}},\
            \"size_x\":{{\"type\":\"Float\",\"value\":0.5}},\
            \"size_y\":{{\"type\":\"Float\",\"value\":0.5}}}}}},\
         {{\"id\":{t},\"typeId\":\"node.make_triangles\",\"nodeId\":\"tris_blend\",\"params\":{{\
            \"src_cols\":{{\"type\":\"Int\",\"value\":2}},\
            \"src_rows\":{{\"type\":\"Int\",\"value\":2}}}}}},\
         {{\"id\":{x},\"typeId\":\"node.transform_3d\",\"nodeId\":\"xf_blend\",\"params\":{{\
            \"pos_y\":{{\"type\":\"Float\",\"value\":1.4}}}}}},\
         {{\"id\":{m},\"typeId\":\"node.pbr_material\",\"nodeId\":\"mat_blend\",\"params\":{{{pbr_params}}}}},",
        b = base,
        t = base + 1,
        x = base + 2,
        m = base + 3,
    )
}

fn blend_object_wires(base: u32, slot: u32) -> String {
    format!(
        "{{\"fromNode\":{b},\"fromPort\":\"vertices\",\"toNode\":{t},\"toPort\":\"in\"}},\
         {{\"fromNode\":{t},\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"mesh_{slot}\"}},\
         {{\"fromNode\":{x},\"fromPort\":\"transform\",\"toNode\":30,\"toPort\":\"transform_{slot}\"}},\
         {{\"fromNode\":{m},\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"material_{slot}\"}},",
        b = base,
        t = base + 1,
        x = base + 2,
        m = base + 3,
    )
}

/// Which graph variant to build. `surface` = the S6 surface chain nodes
/// present; `water` = the five wires into render_scene's water inputs.
struct GraphVariant {
    surface: bool,
    water: bool,
}

/// Assemble the scene graph. `water_camera_node` overrides the node id
/// wired to `water_camera` (ortho test fixture / mismatched camera);
/// `render_scene_extra` injects extra params into node 30.
fn assemble(variant: &GraphVariant, water_camera_node: Option<u32>, render_scene_extra: &str) -> String {
    let mut nodes = String::from("{\"id\":0,\"typeId\":\"system.generator_input\",\"nodeId\":\"input\"},");
    nodes.push_str(&camera_node(1, CAM_DISTANCE));

    let mut wires = String::from(
        "{\"fromNode\":1,\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"camera\"},",
    );

    if variant.surface {
        nodes.push_str(&seed_water_node());
        nodes.push_str(concat!(
            "{\"id\":3,\"typeId\":\"node.particle_surface_depth\",\"nodeId\":\"splat\",\"params\":{}},",
            "{\"id\":4,\"typeId\":\"node.particle_thickness\",\"nodeId\":\"thick\",\"params\":{}},",
            "{\"id\":5,\"typeId\":\"node.bilateral_blur\",\"nodeId\":\"blur_h\",\"params\":{",
            "\"axis\":{\"type\":\"Enum\",\"value\":0},",
            "\"depth_sigma\":{\"type\":\"Float\",\"value\":0.05},",
            "\"value_space\":{\"type\":\"Enum\",\"value\":1}}},",
            "{\"id\":6,\"typeId\":\"node.bilateral_blur\",\"nodeId\":\"blur_v\",\"params\":{",
            "\"axis\":{\"type\":\"Enum\",\"value\":1},",
            "\"depth_sigma\":{\"type\":\"Float\",\"value\":0.05},",
            "\"value_space\":{\"type\":\"Enum\",\"value\":1}}},",
            "{\"id\":7,\"typeId\":\"node.normals_from_depth\",\"nodeId\":\"normals\",\"params\":{}},",
        ));
        nodes.push_str(&format!(
            "{{\"id\":8,\"typeId\":\"node.pbr_material\",\"nodeId\":\"water_mat\",\"params\":{{{}}}}},",
            water_material_json()
        ));
        wires.push_str(concat!(
            "{\"fromNode\":2,\"fromPort\":\"out\",\"toNode\":3,\"toPort\":\"particles\"},",
            "{\"fromNode\":2,\"fromPort\":\"out\",\"toNode\":4,\"toPort\":\"particles\"},",
            "{\"fromNode\":1,\"fromPort\":\"out\",\"toNode\":3,\"toPort\":\"camera\"},",
            "{\"fromNode\":1,\"fromPort\":\"out\",\"toNode\":4,\"toPort\":\"camera\"},",
            "{\"fromNode\":1,\"fromPort\":\"out\",\"toNode\":5,\"toPort\":\"camera\"},",
            "{\"fromNode\":1,\"fromPort\":\"out\",\"toNode\":6,\"toPort\":\"camera\"},",
            "{\"fromNode\":1,\"fromPort\":\"out\",\"toNode\":7,\"toPort\":\"camera\"},",
            "{\"fromNode\":3,\"fromPort\":\"depth\",\"toNode\":5,\"toPort\":\"in\"},",
            "{\"fromNode\":3,\"fromPort\":\"depth\",\"toNode\":5,\"toPort\":\"depth\"},",
            "{\"fromNode\":3,\"fromPort\":\"coverage\",\"toNode\":5,\"toPort\":\"coverage\"},",
            "{\"fromNode\":5,\"fromPort\":\"out\",\"toNode\":6,\"toPort\":\"in\"},",
            "{\"fromNode\":3,\"fromPort\":\"depth\",\"toNode\":6,\"toPort\":\"depth\"},",
            "{\"fromNode\":3,\"fromPort\":\"coverage\",\"toNode\":6,\"toPort\":\"coverage\"},",
            "{\"fromNode\":3,\"fromPort\":\"coverage\",\"toNode\":7,\"toPort\":\"coverage\"},",
            "{\"fromNode\":6,\"fromPort\":\"out\",\"toNode\":7,\"toPort\":\"depth\"},",
        ));
        if variant.water {
            let wc = water_camera_node.unwrap_or(1);
            wires.push_str(&format!(
                "{{\"fromNode\":6,\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"water_depth\"}},\
                 {{\"fromNode\":4,\"fromPort\":\"thickness\",\"toNode\":30,\"toPort\":\"water_thickness\"}},\
                 {{\"fromNode\":7,\"fromPort\":\"normals\",\"toNode\":30,\"toPort\":\"water_normals\"}},\
                 {{\"fromNode\":8,\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"water_material\"}},\
                 {{\"fromNode\":{wc},\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"water_camera\"}},"
            ));
        }
    }

    // Scene objects: foreground plane (in front of the water), a partly
    // submerged vertical wall, and the floor plane behind the water.
    let (fg_nodes, fg_ids) = grid_object(9, 0, [-0.55, 0.9, 0.0], 0.0, [1.0, 1.0], FG_COLOR);
    let (wall_nodes, wall_ids) =
        grid_object(13, 1, [0.55, -0.25, 0.0], std::f32::consts::FRAC_PI_2, [0.6, 1.0], WALL_COLOR);
    let (floor_nodes, floor_ids) = grid_object(17, 2, [0.0, -1.6, 0.0], 0.0, [6.0, 6.0], FLOOR_COLOR);
    nodes.push_str(&fg_nodes);
    nodes.push_str(&wall_nodes);
    nodes.push_str(&floor_nodes);
    wires.push_str(&wire_object(&fg_ids, 0));
    wires.push_str(&wire_object(&wall_ids, 1));
    wires.push_str(&wire_object(&floor_ids, 2));

    let extra = if render_scene_extra.is_empty() {
        String::new()
    } else {
        format!(",{render_scene_extra}")
    };
    nodes.push_str(&format!(
        "{{\"id\":30,\"typeId\":\"node.render_scene\",\"nodeId\":\"scene\",\"params\":{{\
            \"objects\":{{\"type\":\"Int\",\"value\":3}},\
            \"lights\":{{\"type\":\"Int\",\"value\":0}}{extra}}}}},"
    ));
    nodes.push_str("{\"id\":31,\"typeId\":\"node.invert\",\"nodeId\":\"depth_sink\",\"params\":{}}");
    nodes.push_str(",{\"id\":99,\"typeId\":\"system.final_output\",\"nodeId\":\"out\"}");

    wires.push_str(concat!(
        "{\"fromNode\":30,\"fromPort\":\"color\",\"toNode\":99,\"toPort\":\"in\"},",
        "{\"fromNode\":30,\"fromPort\":\"depth\",\"toNode\":31,\"toPort\":\"in\"}",
    ));

    format!(
        "{{\"version\":2,\"name\":\"WaterScene\",\"nodes\":[{nodes}],\"wires\":[{wires}]}}"
    )
}

// ---------------------------------------------------------------------------
// Rendering + readback helpers.
// ---------------------------------------------------------------------------

struct Rendered {
    color: Vec<u8>,
    depth: Vec<f32>,
    w: u32,
    h: u32,
}

fn readback_bytes(device: &GpuDevice, tex: &GpuTexture, bytes_per_pixel: u32) -> Vec<u8> {
    let bytes_per_row = tex.width * bytes_per_pixel;
    let total = u64::from(tex.height * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("water-scene-readback");
    enc.copy_texture_to_buffer(tex, &buf, tex.width, tex.height, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), total as usize) }.to_vec()
}

/// Build + render two frames through PresetRuntime, read back the host
/// color target and (via dump mode + the depth→invert dead-end wire, the
/// gbuffer_depth idiom) the scene's published `depth` output.
fn render_graph(
    json: &str,
    registry: &PrimitiveRegistry,
    w: u32,
    h: u32,
    format: GpuTextureFormat,
) -> Rendered {
    let harness = harness::shared();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        registry,
        Arc::clone(&harness.device),
        w,
        h,
        format,
        None,
    )
    .unwrap_or_else(|e| panic!("water scene graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = RenderTarget::new(&harness.device, w, h, format, "water-scene");
    for frame in 0..2 {
        let ctx = PresetContext {
            time: 0.1f64 + frame as f64 / 60.0,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: w as f32 / h as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = harness.device.create_encoder("water-scene-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &harness.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }

    let dumped = runtime.dump_textures_all();
    let depth_tex = dumped
        .iter()
        .find(|(node_id, port, _, _)| node_id == "scene" && port == "depth")
        .unwrap_or_else(|| {
            panic!(
                "no dumped `depth` output on node `scene` — dumped ports: {:?}",
                dumped.iter().map(|(n, p, _, _)| format!("{n}.{p}")).collect::<Vec<_>>()
            )
        })
        .3;
    assert_eq!(depth_tex.format, GpuTextureFormat::R32Float);
    let depth_bytes = readback_bytes(&harness.device, depth_tex, 4);
    let depth: Vec<f32> = depth_bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let color = readback_bytes(&harness.device, &target.texture, match format {
        GpuTextureFormat::Rgba16Float => 8,
        GpuTextureFormat::Rgba8Unorm => 4,
        other => panic!("unsupported proof target format {other:?}"),
    });
    Rendered { color, depth, w, h }
}

fn rgb16(bytes: &[u8], w: u32, x: u32, y: u32) -> [f32; 3] {
    let idx = ((y * w + x) * 8) as usize;
    [
        f16::from_le_bytes([bytes[idx], bytes[idx + 1]]).to_f32(),
        f16::from_le_bytes([bytes[idx + 2], bytes[idx + 3]]).to_f32(),
        f16::from_le_bytes([bytes[idx + 4], bytes[idx + 5]]).to_f32(),
    ]
}

fn rgba8(bytes: &[u8], w: u32, x: u32, y: u32) -> [f32; 4] {
    let idx = ((y * w + x) * 4) as usize;
    [
        bytes[idx] as f32 / 255.0,
        bytes[idx + 1] as f32 / 255.0,
        bytes[idx + 2] as f32 / 255.0,
        bytes[idx + 3] as f32 / 255.0,
    ]
}

/// True when the 3×3 block centred on (x, y) all reads `color` within
/// `tol` — edges between regions are excluded from every measurement.
fn block_is_color(rendered: &Rendered, x: u32, y: u32, color: [f32; 3], tol: f32) -> bool {
    if x == 0 || y == 0 || x + 1 >= rendered.w || y + 1 >= rendered.h {
        return false;
    }
    for dy in 0..3 {
        for dx in 0..3 {
            let rgb = rgb16(&rendered.color, rendered.w, x + dx - 1, y + dy - 1);
            for c in 0..3 {
                if (rgb[c] - color[c]).abs() > tol {
                    return false;
                }
            }
        }
    }
    true
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// TEST-ONLY node: no shipping camera source can emit an orthographic// Camera (orbit/look_at are perspective-only), so the orthographic
// water_camera rejection (section 7: perspective-only in V1) cannot be
// exercised through the builtin palette alone. Hand-rolled EffectNode —
// deliberately NOT `primitive!`, whose inventory auto-submit would leak
// into the global registry and the checked-in catalog. Registered only
// into this test's registry, same discipline as
// `test_camera_pointwise_fixture.rs`.
// ---------------------------------------------------------------------------

struct TestOrthoCamera;

const ORTHO_CAMERA_OUTPUTS: &[NodeOutput] = &[NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Camera,
    kind: PortKind::Output,
    required: false,
}];

impl EffectNode for TestOrthoCamera {
    fn type_id(&self) -> &EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new("test.ortho_water_camera"))
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        ORTHO_CAMERA_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn depth_rule(&self) -> manifold_renderer::node_graph::depth_rule::DepthRule {
        manifold_renderer::node_graph::depth_rule::DepthRule::Terminal
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Same view as the scene camera by value; only the projection
        // mode differs — the node's V1 gate must reject on mode alone.
        let mut cam = proof_camera();
        cam.mode = CameraMode::Orthographic { half_height: 4.0 };
        ctx.outputs.set_camera("out", cam);
    }
}

fn registry_with_ortho() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.ortho_water_camera", || Box::new(TestOrthoCamera));
    registry
}

// ---------------------------------------------------------------------------
// Test 1 — occlusion and depth.
// ---------------------------------------------------------------------------

/// S7 gate: opaque foreground pixels are byte-identical to the no-water
/// render (water behind opaque is depth-discarded); water-covered pixels
/// differ from the no-water render AND the published `depth` output
/// equals the CPU splat oracle; the behind-water floor shows
/// Beer-Lambert attenuation consistent with the material's attenuation
/// colour.
#[test]
fn water_scene_occlusion_and_depth() {
    let (w, h) = (harness::shared().width, harness::shared().height);
    let registry = PrimitiveRegistry::with_builtin();
    let with_water = render_graph(&assemble(&GraphVariant { surface: true, water: true }, None, ""), &registry, w, h, GpuTextureFormat::Rgba16Float);
    let no_water = render_graph(&assemble(&GraphVariant { surface: false, water: false }, None, ""), &registry, w, h, GpuTextureFormat::Rgba16Float);

    // CPU splat oracle for the exact seed_water lattice.
    let cam = proof_camera();
    let positions = seed_positions(POOL_MIN, POOL_MAX, GRID_SPACING);
    assert_eq!(positions.len(), 41_760, "seed lattice drifted — update the mirror");
    let oracle = Oracle { cam, radius: SPLAT_RADIUS, w, h };
    let (oracle_depth, oracle_cov) = oracle.depth_coverage(&positions);

    // ---- (a) foreground occlusion: byte-identical to no-water. ----
    let mut fg_px: Vec<(u32, u32)> = Vec::new();
    let mut fg_over_water = 0usize;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            if block_is_color(&no_water, x, y, FG_COLOR, 0.01) {
                fg_px.push((x, y));
                if oracle_cov[(y * w + x) as usize] == 255 {
                    fg_over_water += 1;
                }
            }
        }
    }
    assert!(fg_px.len() > 60, "foreground region too small ({} px)", fg_px.len());
    assert!(
        fg_over_water > 30,
        "fixture broken: the foreground object must overlap the water's \
         projection ({fg_over_water} px overlap) — otherwise the depth-discard \
         half of the proof is vacuous"
    );
    for &(x, y) in &fg_px {
        let i = ((y * w + x) * 8) as usize;
        assert_eq!(
            with_water.color[i..i + 8],
            no_water.color[i..i + 8],
            "foreground pixel ({x},{y}) must be byte-identical to the no-water \
             render — water behind opaque must be depth-discarded, not blended"
        );
    }

    // ---- (b) water region: differs from no-water; depth == splat. ----
    // Flat-crown guard: 3×3 floor colour AND 3×3 oracle coverage, so the
    // measurement sits on the slab's near-planar top face (the bilateral
    // preserves raw depth there — the 0.02 raw-depth tolerance is the
    // water_surface.rs silhouette-proof precedent).
    let mut water_px: Vec<(u32, u32)> = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            if !block_is_color(&no_water, x, y, FLOOR_COLOR, 0.01) {
                continue;
            }
            // The refraction displacement shifts the shader's scene
            // sample up to ~8 px toward screen-down (world +z with this
            // camera). Only measure pixels whose displaced sample still
            // lands on the floor — the wall is a z=0 plane, so this
            // excludes the narrow band of pixels sampling across it.
            let landing = (y + 8).min(h - 2);
            if !block_is_color(&no_water, x, landing, FLOOR_COLOR, 0.01) {
                continue;
            }
            let mut covered = true;
            for dy in 0..3 {
                for dx in 0..3 {
                    if oracle_cov[((y + dy - 1) * w + (x + dx - 1)) as usize] != 255 {
                        covered = false;
                    }
                }
            }
            if covered {
                water_px.push((x, y));
            }
        }
    }
    assert!(water_px.len() > 200, "water-over-floor region too small ({} px)", water_px.len());

    let mut max_depth_err = 0.0f32;
    for &(x, y) in &water_px {
        let i = (y * w + x) as usize;
        let a = rgb16(&with_water.color, w, x, y);
        let b = rgb16(&no_water.color, w, x, y);
        let delta: f32 = (0..3).map(|c| (a[c] - b[c]).abs()).sum();
        assert!(
            delta > 1e-3,
            "water-covered pixel ({x},{y}) must differ from the no-water render \
             (got {a:?} vs {b:?})"
        );
        let err = (with_water.depth[i] - oracle_depth[i]).abs();
        max_depth_err = max_depth_err.max(err);
        assert!(
            err <= 0.02,
            "published depth {} at ({x},{y}) vs CPU splat oracle {} — diff {err} \
             exceeds the 0.02 raw-depth tolerance",
            with_water.depth[i],
            oracle_depth[i]
        );
        assert!(
            with_water.depth[i] < no_water.depth[i],
            "merged depth at ({x},{y}) must be the NEARER water surface ({}), \
             not the floor depth ({})",
            with_water.depth[i],
            no_water.depth[i]
        );
    }

    // ---- (c) Beer-Lambert direction on the behind-water floor. ----
    // Assertion choice (stated for the record): with no lights wired the
    // shading is `col = scene * exp(-sigma * t)` exactly — no Sun lobe,
    // and the fresnel-mixed IBL reflection term samples an unwired
    // (zeroed) prefiltered map, so every channel must dim and the r/b
    // ratio must FALL toward the attenuation colour (0.70, 0.90, 0.95),
    // whose red coefficient exceeds blue. A multiplicative-only model
    // cannot shift ratios, so the ratio check is the Beer-Lambert
    // signature; the per-channel check pins "dimmer".
    let (mut sum_got, mut sum_ref) = ([0.0f32; 3], [0.0f32; 3]);
    for &(x, y) in &water_px {
        let a = rgb16(&with_water.color, w, x, y);
        let b = rgb16(&no_water.color, w, x, y);
        for c in 0..3 {
            assert!(
                a[c].is_finite(),
                "channel {c} at ({x},{y}) is not finite: water render {a:?} vs no-water {b:?}"
            );
            assert!(
                a[c] <= b[c] * 0.999,
                "channel {c} at ({x},{y}) must be attenuated: got {} vs no-water {}",
                a[c],
                b[c]
            );
            sum_got[c] += a[c];
            sum_ref[c] += b[c];
        }
        assert!(
            a[2] > 1e-4,
            "blue channel at ({x},{y}) is ~zero ({:?}) — the refracted sample left the \
             floor region; the 3x3 same-colour guard should have excluded edge pixels",
            a
        );
        assert!(
            (a[0] / a[2]) <= (b[0] / b[2]) * 0.995,
            "r/b ratio at ({x},{y}) must fall toward the attenuation colour \
             (Beer-Lambert direction): got {} vs no-water {} (a={a:?} b={b:?})",
            a[0] / a[2],
            b[0] / b[2]
        );
    }
    println!(
        "[water_scene] regions: fg={} px ({} over water), water={} px; depth max err {:.5}; \
         floor mean got={:?} ref={:?}",
        fg_px.len(),
        fg_over_water,
        water_px.len(),
        max_depth_err,
        [
            sum_got[0] / water_px.len() as f32,
            sum_got[1] / water_px.len() as f32,
            sum_got[2] / water_px.len() as f32,
        ],
        [
            sum_ref[0] / water_px.len() as f32,
            sum_ref[1] / water_px.len() as f32,
            sum_ref[2] / water_px.len() as f32,
        ],
    );
}

// ---------------------------------------------------------------------------
// Test 2 — no-water parity + golden anchor.
// ---------------------------------------------------------------------------

/// Golden FNV-1a hash of the no-water scene's color+depth readbacks.
/// WATER_IMPLEMENTATION_PLAN.md section 8 discipline: recorded from the
/// first passing run, then HELD — this anchors FUTURE no-water
/// regressions (any byte-level drift in the ordinary scene path, with or
/// without the water nodes present-but-unwired, breaks this constant).
const GOLDEN_NO_WATER_HASH: u64 = 0x33ec_5a37_9221_9b71;

#[test]
fn water_scene_without_water_matches_existing() {
    let (w, h) = (harness::shared().width, harness::shared().height);
    let registry = PrimitiveRegistry::with_builtin();

    // Pure scene: no water nodes at all.
    let baseline = render_graph(&assemble(&GraphVariant { surface: false, water: false }, None, ""), &registry, w, h, GpuTextureFormat::Rgba16Float);
    // Water params present (full surface chain + water material nodes
    // built every frame) but the five render_scene water inputs unwired —
    // S7 part 2's no-water paths are structurally inert, all gated on
    // has_water, so this must be byte-identical to the baseline.
    let unwired = render_graph(&assemble(&GraphVariant { surface: true, water: false }, None, ""), &registry, w, h, GpuTextureFormat::Rgba16Float);

    assert_eq!(
        baseline.color, unwired.color,
        "surface chain present-but-unwired must not change one color byte"
    );
    assert_eq!(
        baseline.depth, unwired.depth,
        "surface chain present-but-unwired must not change one depth value"
    );

    let mut hashed = baseline.color.clone();
    hashed.extend_from_slice(bytemuck::cast_slice(&baseline.depth));
    let hash = fnv1a64(&hashed);
    println!("[water_scene] no-water golden hash: 0x{hash:016x}");
    assert_eq!(
        hash, GOLDEN_NO_WATER_HASH,
        "no-water scene output drifted — if this is an intended rendering \
         change, re-record the constant from the first passing run and say so \
         in the commit; a silent drift means S7's has_water gating leaked \
         into the ordinary path"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — rejected combinations (each excluded setting gets its own
// assertion). The per-frame ctx.error channel is log-only in the
// executor, so the assertable convention is the documented visible one:
// color cleared to exactly magenta (1,0,1,1), depth cleared to 1.0.
// ---------------------------------------------------------------------------

fn assert_scene_error_convention(rendered: &Rendered, label: &str) {
    for y in 0..rendered.h {
        for x in 0..rendered.w {
            let i = ((y * rendered.w + x) * 8) as usize;
            let px = [
                f16::from_le_bytes([rendered.color[i], rendered.color[i + 1]]).to_f32(),
                f16::from_le_bytes([rendered.color[i + 2], rendered.color[i + 3]]).to_f32(),
                f16::from_le_bytes([rendered.color[i + 4], rendered.color[i + 5]]).to_f32(),
                f16::from_le_bytes([rendered.color[i + 6], rendered.color[i + 7]]).to_f32(),
            ];
            assert_eq!(
                px, [1.0, 0.0, 1.0, 1.0],
                "{label}: pixel ({x},{y}) must be the magenta scene-error clear, got {px:?}"
            );
        }
    }
    // Depth flat, not at color dims: the temporal_upscale subcase compiles
    // the depth output at reduced render res (85x85 for a 128 canvas)
    // while color upscales back to native.
    for (i, d) in rendered.depth.iter().enumerate() {
        assert_eq!(*d, 1.0, "{label}: depth entry {i} must be exactly 1.0");
    }
}

#[test]
fn water_scene_rejects_unsupported_combinations() {
    let (w, h) = (harness::shared().width, harness::shared().height);
    let registry = PrimitiveRegistry::with_builtin();
    let full = GraphVariant { surface: true, water: true };
    let render = |json: &str, registry: &PrimitiveRegistry| {
        render_graph(json, registry, w, h, GpuTextureFormat::Rgba16Float)
    };

    // 1. Partial water set: drop the water_camera wire (4/5 wired).
    {
        let mut json = assemble(&full, Some(1), "");
        let wire = "{\"fromNode\":1,\"fromPort\":\"out\",\"toNode\":30,\"toPort\":\"water_camera\"},";
        let idx = json.find(wire).expect("water_camera wire present");
        json.replace_range(idx..idx + wire.len(), "");
        assert_scene_error_convention(&render(&json, &registry), "partial water set (4/5 wired)");
    }

    // 2. Orthographic water_camera (V1 is perspective-only). No builtin
    // camera source emits ortho — the test-only node above does.
    {
        let registry = registry_with_ortho();
        let mut json = assemble(&full, Some(40), "");
        json = json.replace(
            "{\"id\":31,",
            "{\"id\":40,\"typeId\":\"test.ortho_water_camera\",\"nodeId\":\"cam_ortho\",\"params\":{}},\
             {\"id\":31,",
        );
        assert_scene_error_convention(&render(&json, &registry), "orthographic water_camera");
    }

    // 3. Camera value mismatch: a second perspective camera whose
    // view/projection differs from the scene camera.
    {
        let mut json = assemble(&full, Some(41), "");
        json = json.replace(
            "{\"id\":31,",
            &format!("{}{{\"id\":31,", camera_node(41, CAM_DISTANCE + 0.5)),
        );
        assert_scene_error_convention(&render(&json, &registry), "water_camera value mismatch");
    }

    // 4. Non-dielectric water material (metallic 0.5).
    {
        let json = assemble(&full, None, "").replace(
            "\"metallic\":{\"type\":\"Float\",\"value\":0.0}",
            "\"metallic\":{\"type\":\"Float\",\"value\":0.5}",
        );
        assert_scene_error_convention(&render(&json, &registry), "metallic water material");
    }

    // 5. Blend object in the scene (has_transmission → E2a Pass B, which
    // V1 water rejects). The Blend object is PBR, which independently
    // requires an envmap input — wire one so the WATER rejection is the
    // error under test, not the pre-existing envmap fallback (which
    // clears depth to 0, not the scene-error convention).
    {
        let mut json = assemble(&full, None, "");
        json = json.replace(
            "\"objects\":{\"type\":\"Int\",\"value\":3}",
            "\"objects\":{\"type\":\"Int\",\"value\":4}",
        );
        json = json.replace(
            "{\"id\":30,",
            &format!("{}{{\"id\":30,", blend_object_nodes(50, 3)),
        );
        json = json.replace(
            "{\"fromNode\":30,\"fromPort\":\"color\"",
            &format!("{}{{\"fromNode\":30,\"fromPort\":\"color\"", blend_object_wires(50, 3)),
        );
        json = json.replace(
            "{\"id\":31,",
            "{\"id\":43,\"typeId\":\"node.bake_environment\",\"nodeId\":\"env\",\"params\":{}},{\"id\":31,",
        );
        json = json.replace(
            "{\"fromNode\":30,\"fromPort\":\"depth\"",
            "{\"fromNode\":43,\"fromPort\":\"envmap\",\"toNode\":30,\"toPort\":\"envmap\"},\
             {\"fromNode\":30,\"fromPort\":\"depth\"",
        );
        assert_scene_error_convention(&render(&json, &registry), "Blend object in scene");
    }

    // 6. rt_enabled on.
    {
        let json = assemble(&full, None, "\"rt_enabled\":{\"type\":\"Bool\",\"value\":true}");
        assert_scene_error_convention(&render(&json, &registry), "rt_enabled");
    }

    // 7. temporal_upscale on, with depth+velocity wired so the D22 gate's
    // compiled-size condition can actually go true (dead-end sinks, the
    // gbuffer_depth idiom).
    {
        let mut json = assemble(&full, None, "\"temporal_upscale\":{\"type\":\"Bool\",\"value\":true}");
        json = json.replace(
            "{\"id\":31,",
            "{\"id\":32,\"typeId\":\"node.invert\",\"nodeId\":\"velocity_sink\",\"params\":{}},{\"id\":31,",
        );
        json = json.replace(
            "{\"fromNode\":30,\"fromPort\":\"depth\",\"toNode\":31,\"toPort\":\"in\"}",
            "{\"fromNode\":30,\"fromPort\":\"depth\",\"toNode\":31,\"toPort\":\"in\"},\
             {\"fromNode\":30,\"fromPort\":\"velocity\",\"toNode\":32,\"toPort\":\"in\"}",
        );
        assert_scene_error_convention(&render(&json, &registry), "temporal_upscale");
    }

    // 8. Volumetric shafts on (atmosphere shaft_intensity > 0 wired).
    {
        let mut json = assemble(&full, None, "");
        json = json.replace(
            "{\"id\":31,",
            "{\"id\":42,\"typeId\":\"node.atmosphere\",\"nodeId\":\"atmo\",\"params\":{\
                \"shaft_intensity\":{\"type\":\"Float\",\"value\":0.5}}},{\"id\":31,",
        );
        json = json.replace(
            "{\"fromNode\":30,\"fromPort\":\"color\"",
            "{\"fromNode\":42,\"fromPort\":\"atmosphere\",\"toNode\":30,\"toPort\":\"atmosphere\"},\
             {\"fromNode\":30,\"fromPort\":\"color\"",
        );
        assert_scene_error_convention(&render(&json, &registry), "volumetric shafts");
    }

    // 9. Non-Rgba16Float color output: the runtime canvas format itself is
    // the public color target here, so the format clause must fire (and
    // must error cleanly, not panic in pipeline creation — the rejection
    // runs before any water pipeline is ensured).
    {
        let json = assemble(&full, None, "");
        let rendered = render_graph(&json, &registry, w, h, GpuTextureFormat::Rgba8Unorm);
        for y in 0..h {
            for x in 0..w {
                let px = rgba8(&rendered.color, w, x, y);
                assert_eq!(
                    px, [1.0, 0.0, 1.0, 1.0],
                    "non-Rgba16Float color output: pixel ({x},{y}) must be magenta, got {px:?}"
                );
            }
        }
        for (i, d) in rendered.depth.iter().enumerate() {
            assert_eq!(
                *d, 1.0,
                "non-Rgba16Float color output: depth entry {i} must be exactly 1.0"
            );
        }
    }

    println!("[water_scene] all 9 rejection subcases asserted the magenta/1.0 scene-error convention");
}

// ---------------------------------------------------------------------------
// Test 4 — L2 artifact: the full graph render through the real scene
// renderer, emitted under WATER_ARTIFACT_DIR (S7 demo artifact).
// ---------------------------------------------------------------------------

#[test]
fn water_scene_artifact() {
    let (w, h) = (256u32, 256u32);
    let registry = PrimitiveRegistry::with_builtin();
    let json = assemble(&GraphVariant { surface: true, water: true }, None, "");
    let rendered = render_graph(&json, &registry, w, h, GpuTextureFormat::Rgba16Float);

    // The artifact is evidence, not a gate: computed unconditionally,
    // written only when the env var names a directory (water_surface.rs
    // pattern). Contact sheet: color left, raw depth (inverted grayscale)
    // right.
    let mut color_png = vec![0u8; (w * h * 4) as usize];
    let mut depth_png = vec![0u8; (w * h * 4) as usize];
    let mut sheet = vec![0u8; (w * h * 4 * 2) as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = y * w as usize + x;
            let rgb = rgb16(&rendered.color, w, x as u32, y as u32);
            let rgba = [
                (rgb[0].clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[1].clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[2].clamp(0.0, 1.0) * 255.0) as u8,
                255,
            ];
            color_png[i * 4..i * 4 + 4].copy_from_slice(&rgba);
            sheet[i * 4..i * 4 + 4].copy_from_slice(&rgba);
            let d = (rendered.depth[i].clamp(0.0, 1.0) * 255.0) as u8;
            let dpx = [255 - d, 255 - d, 255 - d, 255];
            depth_png[i * 4..i * 4 + 4].copy_from_slice(&dpx);
            let si = y * (2 * w) as usize + w as usize + x;
            sheet[si * 4..si * 4 + 4].copy_from_slice(&dpx);
        }
    }

    match std::env::var("WATER_ARTIFACT_DIR") {
        Ok(dir) => {
            let out_dir = PathBuf::from(&dir);
            std::fs::create_dir_all(&out_dir).expect("create WATER_ARTIFACT_DIR");
            for (name, buf, width, height) in [
                ("water_scene_color.png", &color_png, w, h),
                ("water_scene_depth.png", &depth_png, w, h),
                ("water_scene_contact_sheet.png", &sheet, w * 2, h),
            ] {
                let path = out_dir.join(name);
                image::save_buffer(&path, buf, width, height, image::ExtendedColorType::Rgba8)
                    .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
                println!("[water_scene] artifact: {}", path.display());
            }
        }
        Err(_) => {
            println!(
                "[water_scene] WATER_ARTIFACT_DIR unset — occlusion scene rendered and composed, but no files written"
            );
        }
    }
}
