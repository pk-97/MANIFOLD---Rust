use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::generator_math::{rotate_4d, project_4d};
use crate::generators::line_pipeline::{LinePipeline, LineGeneratorHelper};

// Parameter indices matching Unity's TesseractGenerator
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

const VERTEX_COUNT: usize = 16;
const EDGE_COUNT: usize = 32;

pub struct TesseractGenerator {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
    // Base 4D vertex positions (unit hypercube, -1 to 1)
    base_verts: [[f32; 4]; VERTEX_COUNT],
}

impl TesseractGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let line_pipeline = LinePipeline::new(device, target_format, "Tesseract");
        let mut helper = LineGeneratorHelper::new(VERTEX_COUNT, EDGE_COUNT);

        // 16 vertices from 4-bit patterns
        let mut base_verts = [[0.0f32; 4]; VERTEX_COUNT];
        for (i, vert) in base_verts.iter_mut().enumerate() {
            vert[0] = if (i & 1) != 0 { 1.0 } else { -1.0 };
            vert[1] = if (i & 2) != 0 { 1.0 } else { -1.0 };
            vert[2] = if (i & 4) != 0 { 1.0 } else { -1.0 };
            vert[3] = if (i & 8) != 0 { 1.0 } else { -1.0 };
        }

        // 32 edges: connect i to i^1, i^2, i^4, i^8 where j > i
        helper.edge_a.clear();
        helper.edge_b.clear();
        for i in 0..VERTEX_COUNT {
            for bit in [1usize, 2, 4, 8] {
                let j = i ^ bit;
                if j > i {
                    helper.edge_a.push(i);
                    helper.edge_b.push(j);
                }
            }
        }

        Self {
            line_pipeline,
            helper,
            base_verts,
        }
    }
}

impl Generator for TesseractGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::Tesseract
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32 {
        let rot_xy = if ctx.param_count > ROT_XY as u32 { ctx.params[ROT_XY] } else { 0.6 };
        let rot_zw = if ctx.param_count > ROT_ZW as u32 { ctx.params[ROT_ZW] } else { 0.4 };
        let rot_xw = if ctx.param_count > ROT_XW as u32 { ctx.params[ROT_XW] } else { 0.25 };
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.002 };
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
        // Rotate and project each vertex
        for i in 0..VERTEX_COUNT {
            let [mut x, mut y, mut z, mut w] = self.base_verts[i];
            rotate_4d(&mut x, &mut y, &mut z, &mut w, angle_xy, angle_zw, angle_xw);
            let (px, py, _pz) = project_4d(x, y, z, w, dist);
            // Store raw projected values — scale is applied ONCE by build_vertices
            // (Unity: TesseractGenerator.Project() stores raw; scale applied by LineGeneratorBase.Render)
            self.helper.projected_x[i] = px;
            self.helper.projected_y[i] = py;
        }

        let verts = self.helper.build_vertices(
            ctx.width as f32,
            ctx.height as f32,
            ctx.aspect,
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

        self.line_pipeline.draw(device, queue, encoder, target, verts, ctx.beat, profiler, "Tesseract", ctx.width, ctx.height);
        self.helper.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
