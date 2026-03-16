use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::generator_math::PROJ_SCALE;
use crate::generators::line_pipeline::{LinePipeline, LineGeneratorHelper};

// Parameter indices matching Unity's OscilloscopeXYGenerator
const LINE: usize = 0;
const VERTS: usize = 1;
const VSIZE: usize = 2;
const ANIM: usize = 3;
const SPEED: usize = 4;
const WINDOW: usize = 5;
const WAVE: usize = 6;
const SCALE: usize = 7;
const SNAP: usize = 8;

const VERTEX_COUNT: usize = 256;

// Snap ratio presets: (a, b) frequency ratios
const SNAP_RATIOS: [(f32, f32); 12] = [
    (1.0, 1.0), (1.0, 2.0), (2.0, 3.0), (1.0, 3.0),
    (3.0, 4.0), (3.0, 5.0), (2.0, 5.0), (4.0, 5.0),
    (5.0, 6.0), (5.0, 7.0), (3.0, 7.0), (7.0, 8.0),
];

pub struct OscilloscopeXYGenerator {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
    free_time: f32,
    // Hash-based smooth ratio state
    current_a: f32,
    current_b: f32,
    target_a: f32,
    target_b: f32,
    ratio_timer: f32,
}

impl OscilloscopeXYGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let line_pipeline = LinePipeline::new(device, target_format, "OscilloscopeXY");
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
            current_a: 1.0,
            current_b: 2.0,
            target_a: 1.0,
            target_b: 2.0,
            ratio_timer: 0.0,
        }
    }

    /// Simple hash for selecting ratios from time
    fn time_hash(t: f32) -> u32 {
        let bits = (t * 1000.0) as u32;
        bits.wrapping_mul(2654435761)
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
    ) -> f32 {
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.002 };
        let show_verts = if ctx.param_count > VERTS as u32 { ctx.params[VERTS] > 0.5 } else { false };
        let vert_size = if ctx.param_count > VSIZE as u32 { ctx.params[VSIZE] } else { 0.5 };
        let animate = if ctx.param_count > ANIM as u32 { ctx.params[ANIM] > 0.5 } else { true };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.63 };
        let window = if ctx.param_count > WINDOW as u32 { ctx.params[WINDOW] } else { 0.59 };
        let wave = if ctx.param_count > WAVE as u32 { ctx.params[WAVE] } else { 0.3 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.75 };
        let snap = if ctx.param_count > SNAP as u32 { ctx.params[SNAP] > 0.5 } else { true };

        self.free_time += ctx.dt;
        let t = self.free_time;

        let (a, b, delta) = if snap {
            // Snap mode: trigger_count selects from ratio table
            let idx = (ctx.trigger_count as usize) % SNAP_RATIOS.len();
            let (a, b) = SNAP_RATIOS[idx];
            (a, b, std::f32::consts::FRAC_PI_2)
        } else {
            // Free-running: hash-based ratio selection with smooth interpolation
            self.ratio_timer += ctx.dt;
            if self.ratio_timer > 3.0 {
                self.ratio_timer = 0.0;
                self.current_a = self.target_a;
                self.current_b = self.target_b;
                let h = Self::time_hash(t);
                let idx = (h as usize) % SNAP_RATIOS.len();
                self.target_a = SNAP_RATIOS[idx].0;
                self.target_b = SNAP_RATIOS[idx].1;
            }
            let interp = (self.ratio_timer / 3.0).min(1.0);
            // Smooth cubic interpolation
            let s = interp * interp * (3.0 - 2.0 * interp);
            let a = self.current_a + (self.target_a - self.current_a) * s;
            let b = self.current_b + (self.target_b - self.current_b) * s;
            (a, b, t * 0.1)
        };

        let proj_scale = PROJ_SCALE * scale;
        let step = std::f32::consts::TAU / VERTEX_COUNT as f32;

        for i in 0..VERTEX_COUNT {
            let theta = i as f32 * step;
            // Base sine wave
            let base_x = (a * theta + delta).sin();
            let base_y = (b * theta).sin();
            // Harmonic blend controlled by wave param
            let harm_x = (a * 2.0 * theta + delta).sin() * 0.3;
            let harm_y = (b * 2.0 * theta).sin() * 0.3;
            let x = base_x + harm_x * wave;
            let y = base_y + harm_y * wave;
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
