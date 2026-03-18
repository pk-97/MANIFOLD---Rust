use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::generator_math::{rotate_4d, project_4d};
use crate::generators::line_pipeline::{LinePipeline, LineGeneratorHelper};

// Parameter indices matching Unity's DuocylinderGenerator
const ROT_XY: usize = 0;
const ROT_ZW: usize = 1;
const ROT_XW: usize = 2;
const LINE: usize = 3;
const DIST: usize = 4;
const VERTS: usize = 5;
const VSIZE: usize = 6;
const ANIM: usize = 7;
const SPEED: usize = 8;
const WINDOW: usize = 9;
const SCALE: usize = 10;

const GRID_SIZE: usize = 24;
const VERTEX_COUNT: usize = GRID_SIZE * GRID_SIZE; // 576
const EDGE_COUNT: usize = VERTEX_COUNT * 2;         // 1152

pub struct DuocylinderGenerator {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
    base_verts: Vec<[f32; 4]>,
}

impl DuocylinderGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let line_pipeline = LinePipeline::new(device, target_format, "Duocylinder");
        let mut helper = LineGeneratorHelper::new(VERTEX_COUNT, EDGE_COUNT);

        // Parametric 4D torus: (cos(u), sin(u), cos(v), sin(v))
        let mut base_verts = Vec::with_capacity(VERTEX_COUNT);
        let step = std::f32::consts::TAU / GRID_SIZE as f32;
        for iu in 0..GRID_SIZE {
            let u = iu as f32 * step;
            let (su, cu) = u.sin_cos();
            for iv in 0..GRID_SIZE {
                let v = iv as f32 * step;
                let (sv, cv) = v.sin_cos();
                base_verts.push([cu, su, cv, sv]);
            }
        }

        // Edges: connect neighbors in both u and v directions (wrapping)
        helper.edge_a.clear();
        helper.edge_b.clear();
        for iu in 0..GRID_SIZE {
            for iv in 0..GRID_SIZE {
                let idx = iu * GRID_SIZE + iv;
                // u-direction neighbor
                let nu = ((iu + 1) % GRID_SIZE) * GRID_SIZE + iv;
                helper.edge_a.push(idx);
                helper.edge_b.push(nu);
                // v-direction neighbor
                let nv = iu * GRID_SIZE + ((iv + 1) % GRID_SIZE);
                helper.edge_a.push(idx);
                helper.edge_b.push(nv);
            }
        }

        Self {
            line_pipeline,
            helper,
            base_verts,
        }
    }
}

impl Generator for DuocylinderGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::Duocylinder
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        let rot_xy = if ctx.param_count > ROT_XY as u32 { ctx.params[ROT_XY] } else { 0.4 };
        let rot_zw = if ctx.param_count > ROT_ZW as u32 { ctx.params[ROT_ZW] } else { 0.25 };
        let rot_xw = if ctx.param_count > ROT_XW as u32 { ctx.params[ROT_XW] } else { 0.15 };
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.0015 };
        let dist = if ctx.param_count > DIST as u32 { ctx.params[DIST] } else { 3.0 };
        let show_verts = if ctx.param_count > VERTS as u32 { ctx.params[VERTS] > 0.5 } else { true };
        let vert_size = if ctx.param_count > VSIZE as u32 { ctx.params[VSIZE] } else { 1.0 };
        let animate = if ctx.param_count > ANIM as u32 { ctx.params[ANIM] > 0.5 } else { false };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let window = if ctx.param_count > WINDOW as u32 { ctx.params[WINDOW] } else { 0.1 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };

        let t = ctx.time;
        let angle_xy = t * rot_xy;
        let angle_zw = t * rot_zw;
        let angle_xw = t * rot_xw;

        for i in 0..VERTEX_COUNT {
            let [mut x, mut y, mut z, mut w] = self.base_verts[i];
            rotate_4d(&mut x, &mut y, &mut z, &mut w, angle_xy, angle_zw, angle_xw);
            let (px, py, _pz) = project_4d(x, y, z, w, dist);
            // Store raw projected values — scale is applied ONCE by build_vertices
            // (Unity: DuocylinderGenerator.Project() stores raw; scale applied by LineGeneratorBase.Render)
            self.helper.projected_x[i] = px;
            self.helper.projected_y[i] = py;
        }

        let verts = self.helper.build_vertices(
            ctx.width as f32,
            ctx.height as f32,
            line,
            show_verts,
            vert_size,
            animate,
            speed,
            window,
            ctx.dt,
            scale,
            1.0, // dot_scale: default
        );

        self.line_pipeline.draw(device, queue, encoder, target, verts, ctx.beat);
        self.helper.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
