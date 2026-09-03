//! MVP-P2 performance-family value gates (LED_STRIPS_DESIGN.md section 5b.4):
//! per-preset value tests driving the production render path — bundled preset
//! JSON → PresetRuntime (the compositor's own def→MetalBackend constructor) →
//! clip texture on a LayerType::Led layer → LayerCompositor LED composite at
//! the native 8×120 grid — asserted against a CPU-computed model of each
//! pattern. Sibling of `led_composite_pixel_tests` in layer_compositor.rs
//! (P1 routing gates), which this file deliberately does not touch.
//!
//! GPU-gated like the rest of the pixel-level proofs: `#![cfg(feature =
//! "gpu-proofs")]` keeps the default nextest sweep device-free; the
//! gpu_proofs_gate run executes these.

#![cfg(feature = "gpu-proofs")]

use std::sync::Arc;

use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::ParamManifest;
use manifold_core::{BlendMode, LayerId, LayerType};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::headless_readback::readback_raw_halves;
use manifold_renderer::layer_compositor::{CompositeClipDescriptor, LayerCompositor};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::tonemap::TonemapSettings;

const LED_W: u32 = 8;
const LED_H: u32 = 120;
// Production shape: the compositor runs at output resolution, larger than the
// native LED grid in both dims (same precedent as led_composite_pixel_tests).
const COMP_W: u32 = 256;
const COMP_H: u32 = 256;
const DT: f32 = 1.0 / 60.0;
// Value tolerance for the 256→8 downsampled composite vs the cell-center CPU
// model (the LED composite averages each cell's region).
const TOL: f32 = 0.08;
// Shared preset params across the pack (defaults from the JSON).
const HUE: f32 = 200.0;
const SAT: f32 = 1.0;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One preset under test: a persistent PresetRuntime (stateful nodes like
/// envelope_decay / clip_trigger_index keep their StateStore across renders)
/// plus a compositor the rendered frame is pushed through as an LED layer.
struct LedPresetFixture {
    device: Arc<GpuDevice>,
    runtime: PresetRuntime,
    target: RenderTarget,
    comp: LayerCompositor,
    frame_count: u64,
}

fn preset_def(id: &str) -> EffectGraphDef {
    // Filename stem = preset id (the bundle contract). Read from disk rather
    // than the catalog so the test pins exactly what ships in this tree.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/generator-presets/"
    )
    .to_string()
        + id
        + ".json";
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read bundled preset {path}: {e}"));
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

impl LedPresetFixture {
    fn new(id: &str) -> Self {
        let device = Arc::new(GpuDevice::new());
        let registry = PrimitiveRegistry::with_builtin();
        let runtime = PresetRuntime::from_def_with_device(
            preset_def(id),
            &registry,
            Arc::clone(&device),
            COMP_W,
            COMP_H,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|e| panic!("build runtime for {id:?}: {e}"));
        let target = RenderTarget::new(
            &device,
            COMP_W,
            COMP_H,
            GpuTextureFormat::Rgba16Float,
            "led-preset-value-target",
        );
        let comp = LayerCompositor::new(&device, COMP_W, COMP_H);
        Self {
            device,
            runtime,
            target,
            comp,
            frame_count: 0,
        }
    }

    /// Render one frame of the preset at `beats` (120 bpm → time = beats/2),
    /// push it through the compositor as a LayerType::Led layer, and return
    /// the decoded LED composite at 8×120 (row-major, RGB triples).
    fn render_led(&mut self, beats: f64, trigger_count: u32) -> Vec<(f32, f32, f32)> {
        self.frame_count += 1;
        let ctx = PresetContext {
            time: beats / 2.0,
            beat: beats,
            dt: DT,
            width: COMP_W,
            height: COMP_H,
            output_width: COMP_W,
            output_height: COMP_H,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: self.frame_count as i64,
            anim_progress: 0.0,
            trigger_count,
        };
        let mut enc = self.device.create_encoder("led-preset-gen");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            self.runtime
                .render(&mut gpu, &self.target.texture, &ctx, &ParamManifest::default());
        }
        enc.commit_and_wait_completed();

        let layer_id = LayerId::from("led0");
        let layers = [CompositeLayerDescriptor {
            layer_index: 0,
            layer_id: &layer_id,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            hidden: false,
            blit_to_led: false,
            layer_type: LayerType::Led,
            effects: &[],
            effect_groups: &[],
            parent_layer_id: None,
            is_group: false,
            trigger_count,
        }];
        let clips = [CompositeClipDescriptor {
            clip_id: "c0",
            texture: &self.target.texture,
            layer_index: 0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            is_muted: false,
            effects: &[],
            effect_groups: &[],
        }];
        let frame = CompositorFrame {
            time: ctx.time,
            beat: beats,
            dt: DT,
            frame_count: self.frame_count,
            compositor_dirty: true,
            clips: &clips,
            layers: &layers,
            master_effects: &[],
            master_effect_groups: &[],
            master_trigger_count: 0,
            tonemap: TonemapSettings::default(),
            led_exit_index: -1,
            led_composite_size: (LED_W, LED_H),
            output_width: COMP_W,
            output_height: COMP_H,
            occluded_layers: &[0],
            render_skip: &[],
        };
        let mut enc = self.device.create_encoder("led-preset-composite");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            self.comp.render(&mut gpu, &frame);
        }
        enc.commit_and_wait_completed();
        let tex = self
            .comp
            .led_composite_texture()
            .expect("an active LED-type layer must produce an LED composite");
        decode_halves(&readback_raw_halves(&self.device, tex, LED_W, LED_H))
    }
}

fn decode_halves(raw: &[u8]) -> Vec<(f32, f32, f32)> {
    raw.chunks_exact(8)
        .map(|px| {
            (
                f16::from_bits(u16::from_le_bytes([px[0], px[1]])).to_f32(),
                f16::from_bits(u16::from_le_bytes([px[2], px[3]])).to_f32(),
                f16::from_bits(u16::from_le_bytes([px[4], px[5]])).to_f32(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// CPU model — mirrors the WGSL bodies + scalar primitives exactly
// ---------------------------------------------------------------------------

fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// hsv2rgb, the WGSL colorize_body version (hue in turns).
fn hsv2rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let k = [1.0f32, 2.0 / 3.0, 1.0 / 3.0];
    let p = k.map(|ki| ((h + ki).fract() * 6.0 - 3.0).abs());
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    p.map(|pi| v * mix(1.0, (pi - 1.0).clamp(0.0, 1.0), s))
}

/// colorize at amount=1, focus=0: out = tint_rgb * luma(in).
fn tint_rgb(hue: f32, sat: f32) -> [f32; 3] {
    hsv2rgb(fract(hue / 360.0), sat.clamp(0.0, 1.0), 1.0)
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Column centres in UV, matching uv_field's center-of-texel convention.
fn column_center(col: usize) -> f32 {
    (col as f32 + 0.5) / LED_W as f32
}

/// Per-column mean luma of the LED composite (patterns are column-uniform;
/// averaging over rows kills row noise).
fn column_lumas(pixels: &[(f32, f32, f32)]) -> [f32; LED_W as usize] {
    let mut out = [0.0f32; LED_W as usize];
    for (i, px) in pixels.iter().enumerate() {
        out[i % LED_W as usize] += luma([px.0, px.1, px.2]);
    }
    for v in &mut out {
        *v /= LED_H as f32;
    }
    out
}

fn brightest_column(lumas: &[f32; LED_W as usize]) -> usize {
    lumas
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i)
        .unwrap()
}

/// WGSL smoothstep: Hermite t clamped to [0, 1].
fn smoothstep_wgsl(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Chase Sweep / Step Chase / Cycle comet: a flat ONE-CELL head at u = s
/// (1 - smoothstep(1/8, 1/8 + tail, fract(s - u))), tail decaying behind.
fn comet_brightness(u: f32, s: f32, tail: f32) -> f32 {
    1.0 - smoothstep_wgsl(0.125, 0.125 + tail, fract(s - u))
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// Chase Sweep: the comet head advances monotonically with beat phase. The
/// comet body trails LEFT of the head (tail = where it swept from), so the
/// brightest column sits just left of the phase position; phases are picked
/// mid-grid where that column is unambiguous.
#[test]
fn chase_sweep_position_monotonic_in_beat_phase() {
    let tint = luma(tint_rgb(HUE, SAT));
    let mut fx = LedPresetFixture::new("LED Chase Sweep");
    // bars=2 → one sweep per 2 beats; head u = phase = fract(beats/2).
    // Phases snap to cell right edges (beats = k/4) so the one-cell head
    // fills column k-1 exactly and the argmax is unambiguous.
    let beats = [0.5f64, 0.75, 1.0];
    let expected_heads = [1, 2, 3]; // plateau [s-1/8, s] == cell k-1
    let mut heads = Vec::new();
    for (i, &b) in beats.iter().enumerate() {
        let px = fx.render_led(b, 0);
        let lumas = column_lumas(&px);
        let head = brightest_column(&lumas);
        assert_eq!(
            head, expected_heads[i],
            "head column at beats {b}: got {head}, want {} (lumas {lumas:?})",
            expected_heads[i]
        );
        // Head cell sits within the flat head → near full tint.
        assert!(
            lumas[head] > 0.9 * tint,
            "head cell should be near full brightness at beats {b}: {} (tint luma {tint})",
            lumas[head]
        );
        // Tail decays behind the head (to its left) over Tail (0.35 → ~3
        // columns): the two columns just left of the head carry the tail and
        // brighten toward it; further columns are dark.
        for c in head.saturating_sub(2)..head {
            assert!(
                lumas[c] > 0.0 && lumas[c] <= lumas[c + 1] + 1e-6,
                "tail should brighten toward the head at beats {b}: col {c} {} vs col {} {}",
                lumas[c],
                c + 1,
                lumas[c + 1]
            );
        }
        // The leading edge (right of the head) is dark: the head's next column
        // over must fall off sharply.
        if head + 1 < LED_W as usize {
            assert!(
                lumas[head + 1] < 0.25 * tint,
                "past the head the field must be dark at beats {b}: col {} {} (tint {tint})",
                head + 1,
                lumas[head + 1]
            );
        }
        heads.push(head);
    }
    assert!(
        heads.windows(2).all(|w| w[0] < w[1]),
        "head must advance monotonically: {heads:?}"
    );
}

/// Pulse: brightness breathes with the lfo period (1/1 = one breath per
/// whole note = 4 beats).
#[test]
fn pulse_brightness_breathes_with_period() {
    let tint = luma(tint_rgb(HUE, SAT));
    let mut fx = LedPresetFixture::new("LED Pulse");
    // rate 1/1 → 0.25 cycles/beat; sine unipolar at beats b is
    // 0.5 * (1 + sin(2π * fract(0.25 b))).
    for (beats, want) in [(1.0f64, 1.0f32), (2.0, 0.5), (3.0, 0.0)] {
        let px = fx.render_led(beats, 0);
        let lumas = column_lumas(&px);
        let mean = lumas.iter().sum::<f32>() / LED_W as f32;
        assert!(
            (mean - want * tint).abs() < TOL,
            "breath at beats {beats}: mean luma {mean}, want ~{} (want breath {want}, tint {tint})",
            want * tint
        );
        // Vertically uniform: every column breathes together.
        let (min, max) = lumas
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        assert!(
            max - min < TOL,
            "pulse must breathe uniformly across columns at beats {beats}: {min}..{max}"
        );
    }
}

/// Step Chase: position constant between 16th-division boundaries, jumping
/// exactly at them (rate 0.5 cycles/beat, steps 8 → one column per 16th;
/// boundaries at beats k/4).
#[test]
fn step_chase_constant_between_divisions_jumps_at_boundaries() {
    let mut fx = LedPresetFixture::new("LED Step Chase");
    let pos_beats = |beats: f64| -> f32 {
        let phase = fract(0.5 * beats as f32);
        ((phase * 8.0).floor() + 1.0) / 8.0
    };
    // Inside the first division (beats 0..0.25) the position is pinned at 0.
    let a = column_lumas(&fx.render_led(0.05, 0));
    let b = column_lumas(&fx.render_led(0.20, 0));
    assert_eq!(
        brightest_column(&a),
        0,
        "head should sit at column 0 between boundaries (lumas {a:?})"
    );
    assert_eq!(
        brightest_column(&b),
        0,
        "head should still sit at column 0 before the boundary (lumas {b:?})"
    );
    let diff: f32 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .sum();
    assert!(
        diff < TOL,
        "frames inside one division must be near-identical: summed |delta| {diff}"
    );
    // Just before the 0.25 boundary: still column 0; just after: column 1.
    assert_eq!(brightest_column(&column_lumas(&fx.render_led(0.24, 0))), 0);
    assert_eq!(brightest_column(&column_lumas(&fx.render_led(0.26, 0))), 1);
    // Gate the CPU model itself: jumps land exactly on 16ths.
    assert_eq!(pos_beats(0.24), 0.125);
    assert_eq!(pos_beats(0.26), 0.25);
}

/// Step Scan: exactly one column lit per division, at the expected index.
#[test]
fn step_scan_lights_exactly_one_column_per_division() {
    let tint = luma(tint_rgb(HUE, SAT));
    let mut fx = LedPresetFixture::new("LED Step Scan");
    // rate 0.5, steps 8 → lit column index = floor(fract(0.5 b) * 8).
    for (beats, want_col) in [(0.05f64, 0usize), (0.30, 1), (0.55, 2), (0.80, 3)] {
        let lumas = column_lumas(&fx.render_led(beats, 0));
        let lit: Vec<usize> = lumas
            .iter()
            .enumerate()
            .filter(|&(_, &v)| v > 0.5 * tint)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            lit,
            vec![want_col],
            "beats {beats}: exactly column {want_col} should be lit (lumas {lumas:?})"
        );
        let leak = lumas
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != want_col)
            .map(|(_, &v)| v)
            .fold(0.0f32, f32::max);
        assert!(
            leak < 0.05 * tint,
            "beats {beats}: unlit columns must be dark, max leak {leak}"
        );
        assert!(
            lumas[want_col] > 0.9 * tint,
            "beats {beats}: lit column should be near full brightness: {}",
            lumas[want_col]
        );
    }
}

/// Burst: the flash flips state at the duty point, and a new trigger
/// restarts the envelope (retriggerable).
#[test]
fn burst_flips_at_duty_point_and_retriggers() {
    let tint = luma(tint_rgb(HUE, SAT));
    let mut fx = LedPresetFixture::new("LED Burst");
    // rate=8, duty(attack)=0.3, decay=6. flash(b) = 1 - clamp(fract(8b)/0.3).
    let flash = |beats: f64| -> f32 {
        1.0 - (fract(8.0 * beats as f32) / 0.3).clamp(0.0, 1.0)
    };
    // Frame 1 arms the envelope (no pulse on first observation).
    let armed = column_lumas(&fx.render_led(0.01, 0));
    assert!(
        armed.iter().all(|&v| v < 0.05 * tint),
        "armed frame must be dark (lumas {armed:?})"
    );
    // Trigger 1 at a bright clock phase: envelope snaps to ~e^(-6/60), flash
    // near the top of its ramp.
    let env1 = (-6.0f32 * DT).exp();
    let f1 = column_lumas(&fx.render_led(0.01, 1));
    let want1 = env1 * flash(0.01) * tint;
    assert!(
        (f1[0] - want1).abs() < TOL,
        "burst frame 1: col0 {} want ~{want1} (env {env1}, flash {})",
        f1[0],
        flash(0.01)
    );
    assert!(
        f1[0] > 0.5 * tint,
        "burst frame 1 must read bright: {} (tint {tint})",
        f1[0]
    );
    // Same trigger held, but the clock is past the duty point → dark even
    // though the envelope is still alive.
    let f2 = column_lumas(&fx.render_led(0.05, 1));
    assert!(
        f2.iter().all(|&v| v < 0.05 * tint),
        "past the duty point the burst must be dark (lumas {f2:?})"
    );
    // Retrigger: a new trigger count snaps the envelope back to ~1.
    let f3 = column_lumas(&fx.render_led(0.02, 2));
    let want3 = env1 * flash(0.02) * tint;
    assert!(
        (f3[0] - want3).abs() < TOL,
        "burst retrigger: col0 {} want ~{want3}",
        f3[0]
    );
    assert!(
        f3[0] > 0.35 * tint,
        "retriggered burst must read bright again: {} (tint {tint})",
        f3[0]
    );
}

/// Cycle: each trigger_count increment advances the variant — comet position
/// steps across the grid and the hue follows the table (i*45 deg).
#[test]
fn cycle_advances_variant_per_trigger() {
    let mut fx = LedPresetFixture::new("LED Cycle");
    // One runtime, trigger counts 0..4 in sequence (state persists).
    let mut positions = Vec::new();
    let mut head_rgbs = Vec::new();
    for tc in 0u32..4 {
        let px = fx.render_led(0.3, tc);
        let lumas = column_lumas(&px);
        let head = brightest_column(&lumas);
        assert_eq!(
            head, tc as usize,
            "trigger_count {tc}: head column {head}, want {tc} (lumas {lumas:?})"
        );
        // Head cell of the lit column (any row — column-uniform).
        let row_offset = head + LED_W as usize * 60;
        head_rgbs.push(px[row_offset]);
        positions.push(head);
    }
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "variant position must advance per trigger: {positions:?}"
    );
    // Hue follows the table: variant i → hue i*45. Compare the head cell's
    // RGB against the CPU tint at that hue, scaled by the comet brightness.
    for (tc, rgb) in head_rgbs.iter().enumerate() {
        let hue = tc as f32 * 45.0;
        let tint = tint_rgb(hue, SAT);
        // Head cell is the column the plateau fills → full brightness.
        let brightness = comet_brightness(column_center(tc), (tc as f32 + 1.0) / 8.0, 0.3);
        for (chan, &t) in tint.iter().enumerate() {
            let got = [rgb.0, rgb.1, rgb.2][chan];
            assert!(
                (got - t * brightness).abs() < 0.1,
                "variant {tc} hue {hue}: channel {chan} got {got}, want ~{}",
                t * brightness
            );
        }
    }
}
