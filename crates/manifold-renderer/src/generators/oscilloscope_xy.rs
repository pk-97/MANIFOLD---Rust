use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::generator_math::{PROJ_SCALE, hash_beat};
use crate::generators::line_pipeline::{LinePipeline, LineGeneratorHelper};

// Parameter indices matching Unity's OscilloscopeXYGenerator.cs
const LINE: usize = 0;
const VERTS: usize = 1;
const VSIZE: usize = 2;
#[allow(dead_code)]
const ANIM: usize = 3;
const SPEED: usize = 4;
const WINDOW: usize = 5;
const WAVE: usize = 6;
const SCALE: usize = 7;
const SNAP: usize = 8;

const SAMPLES: usize = 256;
const RATIO_COUNT: usize = 10;

// Unity: OscilloscopeXYGenerator.cs lines 38-39
const RATIO_A: [f32; 10] = [1.0, 2.0, 3.0, 3.0, 4.0, 5.0, 3.0, 5.0, 2.0, 7.0];
const RATIO_B: [f32; 10] = [2.0, 3.0, 4.0, 5.0, 5.0, 6.0, 7.0, 8.0, 5.0, 8.0];

pub struct OscilloscopeXYGenerator {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
}

impl OscilloscopeXYGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let line_pipeline = LinePipeline::new(device, target_format, "OscilloscopeXY");
        let mut helper = LineGeneratorHelper::new(SAMPLES, SAMPLES);

        // Closed loop: i -> (i+1) % 256
        helper.edge_a.clear();
        helper.edge_b.clear();
        for i in 0..SAMPLES {
            helper.edge_a.push(i);
            helper.edge_b.push((i + 1) % SAMPLES);
        }

        Self {
            line_pipeline,
            helper,
        }
    }
}

impl Generator for OscilloscopeXYGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::OscilloscopeXY
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
        // Unity defaults from LineGeneratorBase virtual properties
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.002 };
        let show_verts = if ctx.param_count > VERTS as u32 { ctx.params[VERTS] > 0.5 } else { true };
        let vert_size = if ctx.param_count > VSIZE as u32 { ctx.params[VSIZE] } else { 1.0 };
        let animate = true; // Unity: AlwaysAnimate => true
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let window = if ctx.param_count > WINDOW as u32 { ctx.params[WINDOW] } else { 0.1 };
        let wave_speed = if ctx.param_count > WAVE as u32 { ctx.params[WAVE] } else { 0.5 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let snap = ctx.param_count > SNAP as u32 && ctx.params[SNAP] > 0.5;

        // Unity: Project() — lines 57-130
        let (a_main, b_main, a_harm, b_harm);

        if snap {
            // Trigger-driven: fixed ratio per trigger (Unity lines 65-73)
            let main_idx = (ctx.trigger_count as usize) % RATIO_COUNT;
            let harm_idx = (ctx.trigger_count as usize + 3) % RATIO_COUNT;
            a_main = RATIO_A[main_idx];
            b_main = RATIO_B[main_idx];
            a_harm = RATIO_A[harm_idx];
            b_harm = RATIO_B[harm_idx];
        } else {
            // Beat-driven: hash-based ratio selection with smooth interpolation (Unity lines 77-95)
            let beat_idx = ctx.beat.floor() as i32;
            let beat_frac = ctx.beat - beat_idx as f32;

            let seed1 = hash_beat(beat_idx as f32);
            let ratio_idx1 = ((seed1 * RATIO_A.len() as f32) as usize) % RATIO_A.len();
            let seed1_next = hash_beat(beat_idx as f32 + 1.0);
            let ratio_idx1_next = ((seed1_next * RATIO_A.len() as f32) as usize) % RATIO_A.len();

            a_main = RATIO_A[ratio_idx1] + (RATIO_A[ratio_idx1_next] - RATIO_A[ratio_idx1]) * beat_frac;
            b_main = RATIO_B[ratio_idx1] + (RATIO_B[ratio_idx1_next] - RATIO_B[ratio_idx1]) * beat_frac;

            let seed2 = hash_beat(beat_idx as f32 + 73.0);
            let ratio_idx2 = ((seed2 * RATIO_A.len() as f32) as usize) % RATIO_A.len();
            let seed2_next = hash_beat(beat_idx as f32 + 1.0 + 73.0);
            let ratio_idx2_next = ((seed2_next * RATIO_A.len() as f32) as usize) % RATIO_A.len();

            a_harm = RATIO_A[ratio_idx2] + (RATIO_A[ratio_idx2_next] - RATIO_A[ratio_idx2]) * beat_frac;
            b_harm = RATIO_B[ratio_idx2] + (RATIO_B[ratio_idx2_next] - RATIO_B[ratio_idx2]) * beat_frac;
        }

        // Floor/ceil interpolation for smooth closed shapes (Unity lines 98-101)
        let a_lo = a_main.floor();
        let a_hi = a_main.ceil();
        let a_lerp = a_main - a_lo;
        let b_lo = b_main.floor();
        let b_hi = b_main.ceil();
        let b_lerp = b_main - b_lo;
        let a2_lo = a_harm.floor();
        let a2_hi = a_harm.ceil();
        let a2_lerp = a_harm - a2_lo;
        let b2_lo = b_harm.floor();
        let b2_hi = b_harm.ceil();
        let b2_lerp = b_harm - b2_lo;

        // Phase from clip time (Unity line 103)
        let phase = ctx.time * wave_speed * 0.3;
        let proj_scale = PROJ_SCALE;
        let two_pi = std::f32::consts::TAU;

        for i in 0..SAMPLES {
            let t = i as f32 / SAMPLES as f32 * two_pi;

            // Main: interpolate between integer-frequency curves (Unity lines 112-115)
            let x_lo = (a_lo * t + phase).sin();
            let x_hi = (a_hi * t + phase).sin();
            let y_lo = (b_lo * t).sin();
            let y_hi = (b_hi * t).sin();

            // Harmonic: interpolate with phase modulation (Unity lines 118-121)
            let hx_lo = (a2_lo * t * 2.0 + phase * 1.3).sin();
            let hx_hi = (a2_hi * t * 2.0 + phase * 1.3).sin();
            let hy_lo = (b2_lo * t * 2.0 + phase * 0.7).sin();
            let hy_hi = (b2_hi * t * 2.0 + phase * 0.7).sin();

            // Blend main + harmonic (Unity lines 123-124)
            let x = (x_lo + (x_hi - x_lo) * a_lerp) + 0.3 * (hx_lo + (hx_hi - hx_lo) * a2_lerp);
            let y = (y_lo + (y_hi - y_lo) * b_lerp) + 0.3 * (hy_lo + (hy_hi - hy_lo) * b2_lerp);

            self.helper.projected_x[i] = x * proj_scale;
            self.helper.projected_y[i] = y * proj_scale;
            self.helper.projected_z[i] = i as f32 / SAMPLES as f32;
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
            0.5, // dot_scale: Unity OscilloscopeXYGenerator.GetDotScale() returns 0.5
        );

        self.line_pipeline.draw(device, queue, encoder, target, verts, ctx.beat, profiler, "OscilloscopeXY", ctx.width, ctx.height);
        self.helper.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
