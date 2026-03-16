use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::generator_math::{rotate_3d, PROJ_SCALE};
use crate::generators::line_pipeline::{LinePipeline, LineGeneratorHelper};

// Parameter indices matching Unity's WireframeZooGenerator
const ROT_XY: usize = 0;
const ROT_ZW: usize = 1;
const ROT_XW: usize = 2;
const LINE: usize = 3;
const SHAPE: usize = 4;
const VERTS: usize = 5;
const VSIZE: usize = 6;
const SCALE: usize = 7;

// ─── Platonic solid geometry tables ───

// Tetrahedron: 4 vertices, 6 edges
const TETRA_VERTS: [[f32; 3]; 4] = [
    [1.0, 1.0, 1.0], [1.0, -1.0, -1.0],
    [-1.0, 1.0, -1.0], [-1.0, -1.0, 1.0],
];
const TETRA_EDGES: [[usize; 2]; 6] = [
    [0, 1], [0, 2], [0, 3], [1, 2], [1, 3], [2, 3],
];

// Cube: 8 vertices, 12 edges
const CUBE_VERTS: [[f32; 3]; 8] = [
    [-1.0, -1.0, -1.0], [1.0, -1.0, -1.0],
    [1.0, 1.0, -1.0],   [-1.0, 1.0, -1.0],
    [-1.0, -1.0, 1.0],  [1.0, -1.0, 1.0],
    [1.0, 1.0, 1.0],    [-1.0, 1.0, 1.0],
];
const CUBE_EDGES: [[usize; 2]; 12] = [
    [0, 1], [1, 2], [2, 3], [3, 0],
    [4, 5], [5, 6], [6, 7], [7, 4],
    [0, 4], [1, 5], [2, 6], [3, 7],
];

// Octahedron: 6 vertices, 12 edges
const OCTA_VERTS: [[f32; 3]; 6] = [
    [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0], [0.0, -1.0, 0.0],
    [0.0, 0.0, 1.0], [0.0, 0.0, -1.0],
];
const OCTA_EDGES: [[usize; 2]; 12] = [
    [0, 2], [0, 3], [0, 4], [0, 5],
    [1, 2], [1, 3], [1, 4], [1, 5],
    [2, 4], [2, 5], [3, 4], [3, 5],
];

// Icosahedron: 12 vertices, 30 edges
const PHI: f32 = 1.618034; // golden ratio
const ICOSA_VERTS: [[f32; 3]; 12] = [
    [-1.0, PHI, 0.0],  [1.0, PHI, 0.0],
    [-1.0, -PHI, 0.0], [1.0, -PHI, 0.0],
    [0.0, -1.0, PHI],  [0.0, 1.0, PHI],
    [0.0, -1.0, -PHI], [0.0, 1.0, -PHI],
    [PHI, 0.0, -1.0],  [PHI, 0.0, 1.0],
    [-PHI, 0.0, -1.0], [-PHI, 0.0, 1.0],
];
const ICOSA_EDGES: [[usize; 2]; 30] = [
    [0, 1], [0, 5], [0, 7], [0, 10], [0, 11],
    [1, 5], [1, 7], [1, 8], [1, 9],
    [2, 3], [2, 4], [2, 6], [2, 10], [2, 11],
    [3, 4], [3, 6], [3, 8], [3, 9],
    [4, 5], [4, 9], [4, 11],
    [5, 9], [5, 11],
    [6, 7], [6, 8], [6, 10],
    [7, 8], [7, 10],
    [8, 9],
    [10, 11],
];

// Dodecahedron: 20 vertices, 30 edges
const INV_PHI: f32 = 0.618034; // 1/phi
const DODECA_VERTS: [[f32; 3]; 20] = [
    // Cube vertices (8)
    [1.0, 1.0, 1.0], [1.0, 1.0, -1.0],
    [1.0, -1.0, 1.0], [1.0, -1.0, -1.0],
    [-1.0, 1.0, 1.0], [-1.0, 1.0, -1.0],
    [-1.0, -1.0, 1.0], [-1.0, -1.0, -1.0],
    // Rectangle vertices on XY plane (4)
    [0.0, PHI, INV_PHI], [0.0, PHI, -INV_PHI],
    [0.0, -PHI, INV_PHI], [0.0, -PHI, -INV_PHI],
    // Rectangle vertices on XZ plane (4)
    [INV_PHI, 0.0, PHI], [INV_PHI, 0.0, -PHI],
    [-INV_PHI, 0.0, PHI], [-INV_PHI, 0.0, -PHI],
    // Rectangle vertices on YZ plane (4)
    [PHI, INV_PHI, 0.0], [PHI, -INV_PHI, 0.0],
    [-PHI, INV_PHI, 0.0], [-PHI, -INV_PHI, 0.0],
];
const DODECA_EDGES: [[usize; 2]; 30] = [
    [0, 8], [0, 12], [0, 16],
    [1, 9], [1, 13], [1, 16],
    [2, 10], [2, 12], [2, 17],
    [3, 11], [3, 13], [3, 17],
    [4, 8], [4, 14], [4, 18],
    [5, 9], [5, 15], [5, 18],
    [6, 10], [6, 14], [6, 19],
    [7, 11], [7, 15], [7, 19],
    [8, 9], [10, 11], [12, 14],
    [13, 15], [16, 17], [18, 19],
];

const SHAPE_COUNT: u32 = 5;

/// Maximum vertex count across all shapes (dodecahedron = 20)
const MAX_VERTS: usize = 20;
/// Maximum edge count across all shapes (icosa/dodeca = 30)
const MAX_EDGES: usize = 30;

pub struct WireframeZooGenerator {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
}

impl WireframeZooGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let line_pipeline = LinePipeline::new(device, target_format, "WireframeZoo");
        let helper = LineGeneratorHelper::new(MAX_VERTS, MAX_EDGES);

        Self {
            line_pipeline,
            helper,
        }
    }
}

impl Generator for WireframeZooGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::WireframeZoo
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        let rot_xy = if ctx.param_count > ROT_XY as u32 { ctx.params[ROT_XY] } else { 0.5 };
        let rot_zw = if ctx.param_count > ROT_ZW as u32 { ctx.params[ROT_ZW] } else { 0.3 };
        let rot_xw = if ctx.param_count > ROT_XW as u32 { ctx.params[ROT_XW] } else { 0.2 };
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.003 };
        let _shape_param = if ctx.param_count > SHAPE as u32 { ctx.params[SHAPE].round() as u32 } else { 0 };
        let show_verts = if ctx.param_count > VERTS as u32 { ctx.params[VERTS] > 0.5 } else { true };
        let vert_size = if ctx.param_count > VSIZE as u32 { ctx.params[VSIZE] } else { 1.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };

        // Shape selection via trigger_count (param is ignored in favor of trigger cycling)
        let shape_idx = (ctx.trigger_count % SHAPE_COUNT) as usize;

        // Get shape data
        let (verts_3d, edges): (&[[f32; 3]], &[[usize; 2]]) = match shape_idx {
            0 => (&TETRA_VERTS, &TETRA_EDGES),
            1 => (&CUBE_VERTS, &CUBE_EDGES),
            2 => (&OCTA_VERTS, &OCTA_EDGES),
            3 => (&ICOSA_VERTS, &ICOSA_EDGES),
            _ => (&DODECA_VERTS, &DODECA_EDGES),
        };

        let vert_count = verts_3d.len();
        let _edge_count = edges.len();

        // Resize helper arrays
        self.helper.resize_vertices(vert_count);
        self.helper.edge_a.clear();
        self.helper.edge_b.clear();
        for e in edges {
            self.helper.edge_a.push(e[0]);
            self.helper.edge_b.push(e[1]);
        }

        let t = ctx.time;
        // Use rot params as 3D rotation speeds (repurpose XY=X, ZW=Y, XW=Z)
        let (cos_x, sin_x) = (t * rot_xy).sin_cos();
        let (cos_y, sin_y) = (t * rot_zw).sin_cos();
        let (cos_z, sin_z) = (t * rot_xw).sin_cos();
        // sin_cos returns (sin, cos) — swap for our convention
        let (sin_x, cos_x) = (cos_x, sin_x);
        let (sin_y, cos_y) = (cos_y, sin_y);
        let (sin_z, cos_z) = (cos_z, sin_z);

        let proj_scale = PROJ_SCALE * scale;

        // Rotate and project (orthographic — no perspective)
        for i in 0..vert_count {
            let [mut x, mut y, mut z] = verts_3d[i];
            rotate_3d(&mut x, &mut y, &mut z, cos_x, sin_x, cos_y, sin_y, cos_z, sin_z);
            self.helper.projected_x[i] = x * proj_scale;
            self.helper.projected_y[i] = y * proj_scale;
            self.helper.projected_z[i] = z * proj_scale;
        }

        // No animation for wireframe zoo (no anim param)
        let verts = self.helper.build_vertices(
            ctx.width as f32,
            ctx.height as f32,
            line,
            show_verts,
            vert_size,
            false,
            0.0,
            0.0,
            ctx.dt,
            scale,
        );

        self.line_pipeline.draw(device, queue, encoder, target, verts, ctx.beat);
        self.helper.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
