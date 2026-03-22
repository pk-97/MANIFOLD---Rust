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

// Snap presets for integer frequency ratios — matches Unity snapA/snapB arrays
const SNAP_A: [f32; 10] = [1.0, 1.0, 2.0, 3.0, 3.0, 4.0, 5.0, 5.0, 7.0, 3.0];
const SNAP_B: [f32; 10] = [2.0, 3.0, 3.0, 4.0, 5.0, 5.0, 6.0, 8.0, 8.0, 7.0];

pub struct LissajousGenerator {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
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
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32 {
        let freq_x_rate = if ctx.param_count > FREQ_X as u32 { ctx.params[FREQ_X] } else { 0.13 };
        let freq_y_rate = if ctx.param_count > FREQ_Y as u32 { ctx.params[FREQ_Y] } else { 0.09 };
        let phase_rate = if ctx.param_count > PHASE as u32 { ctx.params[PHASE] } else { 0.07 };
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.002 };
        let show_verts = if ctx.param_count > VERTS as u32 { ctx.params[VERTS] > 0.5 } else { true };
        let vert_size = if ctx.param_count > VSIZE as u32 { ctx.params[VSIZE] } else { 1.0 };
        let animate = if ctx.param_count > ANIM as u32 { ctx.params[ANIM] > 0.5 } else { false };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let window = if ctx.param_count > WINDOW as u32 { ctx.params[WINDOW] } else { 0.1 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let snap = if ctx.param_count > SNAP as u32 { ctx.params[SNAP] > 0.5 } else { false };

        // Use clip-relative time from context (matches Unity ctx.Time)
        let time = ctx.time;

        let (a, b, phase) = if snap {
            // Snap mode: use trigger_count to select from preset table
            let idx = (ctx.trigger_count as usize) % SNAP_A.len();
            let a = SNAP_A[idx];
            let b = SNAP_B[idx];
            // Phase evolves over time at param-driven rate (Unity line 70-71)
            let phase = time * phase_rate;
            (a, b, phase)
        } else {
            // Free-running: smoothly evolving frequencies (Unity lines 75-80)
            let a = 2.0 + 1.5 * (time * freq_x_rate).sin();
            let b = 3.0 + 2.0 * (time * freq_y_rate).sin();
            let phase = time * phase_rate;
            (a, b, phase)
        };

        // Interpolate between integer Lissajous curves for smooth closed shapes
        // Matches Unity lines 84-110
        let a_lo = a.floor();
        let a_hi = a.ceil();
        let a_lerp = a - a_lo;

        let b_lo = b.floor();
        let b_hi = b.ceil();
        let b_lerp = b - b_lo;

        for i in 0..VERTEX_COUNT {
            let t = i as f32 / VERTEX_COUNT as f32 * std::f32::consts::TAU;

            // Sample both integer-parameter curves and interpolate
            let x_lo = (a_lo * t + phase).sin();
            let x_hi = (a_hi * t + phase).sin();
            let x = x_lo + (x_hi - x_lo) * a_lerp;

            let y_lo = (b_lo * t).sin();
            let y_hi = (b_hi * t).sin();
            let y = y_lo + (y_hi - y_lo) * b_lerp;

            self.helper.projected_x[i] = x * PROJ_SCALE;
            self.helper.projected_y[i] = y * PROJ_SCALE;
            self.helper.projected_z[i] = i as f32 / VERTEX_COUNT as f32;
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
            0.5, // dot_scale: Unity LissajousGenerator.GetDotScale() returns 0.5
        );

        self.line_pipeline.draw(device, queue, encoder, target, verts, ctx.beat, profiler, "Lissajous", ctx.width, ctx.height);
        self.helper.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
