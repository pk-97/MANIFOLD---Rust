use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::generator_math::PROJ_SCALE;
use crate::generators::line_pipeline::{LinePipeline, LineGeneratorHelper};

// Parameter indices matching Unity's LissajousGenerator
const FREQ_X: usize = 0;
const FREQ_Y: usize = 1;
const PHASE: usize = 2;
const LINE: usize = 3;
const VERTS: usize = 4;
const VSIZE: usize = 5;
const ANIM: usize = 6;
const SPEED: usize = 7;
const WINDOW: usize = 8;
const SCALE: usize = 9;
const SNAP: usize = 10;

const VERTEX_COUNT: usize = 256;

// Snap presets for integer frequency ratios
const SNAP_A: [f32; 10] = [1.0, 1.0, 2.0, 3.0, 3.0, 4.0, 5.0, 5.0, 7.0, 3.0];
const SNAP_B: [f32; 10] = [1.0, 3.0, 3.0, 4.0, 5.0, 5.0, 6.0, 8.0, 8.0, 7.0];

pub struct LissajousGenerator {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
    // Free-running time accumulator for smooth frequency evolution
    free_time: f32,
}

impl LissajousGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let line_pipeline = LinePipeline::new(device, target_format, "Lissajous");
        let mut helper = LineGeneratorHelper::new(VERTEX_COUNT, VERTEX_COUNT);

        // Closed loop: i -> (i+1) % 256
        helper.edge_a.clear();
        helper.edge_b.clear();
        for i in 0..VERTEX_COUNT {
            helper.edge_a.push(i);
            helper.edge_b.push((i + 1) % VERTEX_COUNT);
        }

        Self {
            line_pipeline,
            helper,
            free_time: 0.0,
        }
    }
}

impl Generator for LissajousGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::Lissajous
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        let freq_x_rate = if ctx.param_count > FREQ_X as u32 { ctx.params[FREQ_X] } else { 0.13 };
        let freq_y_rate = if ctx.param_count > FREQ_Y as u32 { ctx.params[FREQ_Y] } else { 0.09 };
        let phase_rate = if ctx.param_count > PHASE as u32 { ctx.params[PHASE] } else { 0.07 };
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.002 };
        let show_verts = if ctx.param_count > VERTS as u32 { ctx.params[VERTS] > 0.5 } else { false };
        let vert_size = if ctx.param_count > VSIZE as u32 { ctx.params[VSIZE] } else { 0.5 };
        let animate = if ctx.param_count > ANIM as u32 { ctx.params[ANIM] > 0.5 } else { true };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 2.67 };
        let window = if ctx.param_count > WINDOW as u32 { ctx.params[WINDOW] } else { 0.74 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.55 };
        let snap = if ctx.param_count > SNAP as u32 { ctx.params[SNAP] > 0.5 } else { true };

        self.free_time += ctx.dt;
        let t = self.free_time;

        let (a, b, delta) = if snap {
            // Snap mode: use trigger_count to select from preset table
            let idx = (ctx.trigger_count as usize) % SNAP_A.len();
            let a = SNAP_A[idx];
            let b = SNAP_B[idx];
            let delta = std::f32::consts::FRAC_PI_2; // pi/2 offset
            (a, b, delta)
        } else {
            // Free-running: smoothly evolving frequencies
            let a_raw = 2.0 + 1.5 * (t * freq_x_rate).sin();
            let b_raw = 3.0 + 2.0 * (t * freq_y_rate).sin();
            // Interpolate toward nearest integer for cleaner shapes
            let a = a_raw;
            let b = b_raw;
            let delta = t * phase_rate;
            (a, b, delta)
        };

        let proj_scale = PROJ_SCALE * scale;
        let step = std::f32::consts::TAU / VERTEX_COUNT as f32;
        for i in 0..VERTEX_COUNT {
            let theta = i as f32 * step;
            let x = (a * theta + delta).sin();
            let y = (b * theta).sin();
            self.helper.projected_x[i] = x * proj_scale;
            self.helper.projected_y[i] = y * proj_scale;
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
        );

        self.line_pipeline.draw(device, queue, encoder, target, verts, ctx.beat);
        self.helper.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
