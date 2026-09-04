//! MVP-P2 utility-family value gates (LED_STRIPS_DESIGN.md section 5b.4):
//! per-preset value tests driving the production render path — bundled preset
//! JSON → PresetRuntime (the compositor's own def→MetalBackend constructor) →
//! clip texture on a LayerType::Dmx layer → LayerCompositor LED composite at
//! the native 8×120 grid — asserted against a CPU-computed model of each
//! pattern. Sibling of `led_preset_value_tests` (performance family), whose
//! LedPresetFixture approach this reuses; that file is untouched.
//!
//! GPU-gated like the rest of the pixel-level proofs: `#![cfg(feature =
//! "gpu-proofs")]` keeps the default nextest sweep device-free; the
//! gpu_proofs_gate run executes these.

#![cfg(feature = "gpu-proofs")]

use std::sync::Arc;

use half::f16;
use manifold_core::effect_graph_def::{EffectGraphDef, ParamSpecDef};
use manifold_core::params::{Param, ParamManifest};
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
// native LED grid in both dims (same precedent as led_preset_value_tests).
const COMP_W: u32 = 256;
const COMP_H: u32 = 256;
const DT: f32 = 1.0 / 60.0;
// Value tolerance for the 256→8 downsampled composite vs the cell-center CPU
// model (the LED composite averages each cell's region).
const TOL: f32 = 0.08;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One preset under test: a persistent PresetRuntime (stateful nodes keep
/// their StateStore across renders) plus a compositor the rendered frame is
/// pushed through as an LED layer. Same shape as the performance family's
/// fixture; adds a per-render ParamManifest so card params can be swept
/// (Studio Light's brightness gate).
struct LedUtilityFixture {
    device: Arc<GpuDevice>,
    runtime: PresetRuntime,
    target: RenderTarget,
    comp: LayerCompositor,
    specs: Vec<ParamSpecDef>,
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

fn slot(spec: &ParamSpecDef, value: f32) -> Param {
    let mut p = Param::bundled(spec.clone());
    p.value = value;
    p.base = value;
    p
}

impl LedUtilityFixture {
    fn new(id: &str) -> Self {
        let device = Arc::new(GpuDevice::new());
        let registry = PrimitiveRegistry::with_builtin();
        let def = preset_def(id);
        let specs = def
            .preset_metadata
            .as_ref()
            .expect("preset metadata")
            .params
            .clone();
        let runtime = PresetRuntime::from_def_with_device(
            def,
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
            "led-utility-value-target",
        );
        let comp = LayerCompositor::new(&device, COMP_W, COMP_H);
        Self {
            device,
            runtime,
            target,
            comp,
            specs,
            frame_count: 0,
        }
    }

    /// Render one frame of the preset at `beats` (120 bpm → time = beats/2)
    /// with every card param at its JSON default except the `overrides`
    /// entries, push it through the compositor as a LayerType::Dmx layer, and
    /// return the decoded LED composite at 8×120 (row-major, RGB triples).
    fn render_led(&mut self, beats: f64, overrides: &[(&str, f32)]) -> Vec<(f32, f32, f32)> {
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
            trigger_count: 0,
        };
        let manifest = ParamManifest::from_params(
            self.specs
                .iter()
                .map(|s| {
                    let v = overrides
                        .iter()
                        .find(|(id, _)| *id == s.id)
                        .map(|(_, v)| *v)
                        .unwrap_or(s.default_value);
                    slot(s, v)
                })
                .collect(),
        );
        let mut enc = self.device.create_encoder("led-utility-gen");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            self.runtime
                .render(&mut gpu, &self.target.texture, &ctx, &manifest);
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
            layer_type: LayerType::Dmx,
            effects: &[],
            effect_groups: &[],
            parent_layer_id: None,
            is_group: false,
            trigger_count: 0,
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
        let mut enc = self.device.create_encoder("led-utility-composite");
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

/// Mean RGB over the whole 8×120 composite (utility presets are either
/// uniform or asserted per-cell; the mean is the uniform oracle).
fn mean_rgb(pixels: &[(f32, f32, f32)]) -> [f32; 3] {
    let mut acc = [0.0f32; 3];
    for px in pixels {
        acc[0] += px.0;
        acc[1] += px.1;
        acc[2] += px.2;
    }
    let n = pixels.len() as f32;
    [acc[0] / n, acc[1] / n, acc[2] / n]
}

/// Per-column mean RGB of the LED composite (patterns are column-uniform;
/// averaging over rows kills row noise).
fn column_rgbs(pixels: &[(f32, f32, f32)]) -> [[f32; 3]; LED_W as usize] {
    let mut out = [[0.0f32; 3]; LED_W as usize];
    for (i, px) in pixels.iter().enumerate() {
        let c = &mut out[i % LED_W as usize];
        c[0] += px.0;
        c[1] += px.1;
        c[2] += px.2;
    }
    for c in &mut out {
        c[0] /= LED_H as f32;
        c[1] /= LED_H as f32;
        c[2] /= LED_H as f32;
    }
    out
}

/// Per-cell luma of the LED composite, indexed [row][col] (row-major,
/// matching the decoded buffer layout).
fn cell_lumas(pixels: &[(f32, f32, f32)]) -> [[f32; LED_W as usize]; LED_H as usize] {
    let mut out = [[0.0f32; LED_W as usize]; LED_H as usize];
    for (i, px) in pixels.iter().enumerate() {
        out[i / LED_W as usize][i % LED_W as usize] = luma([px.0, px.1, px.2]);
    }
    out
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// Studio Light: luminance scales linearly with the Brightness param, and
/// hue/saturation produce the expected tint. Swept at hue 200 / sat 1 so the
/// tint is strongly coloured (channel-level assertion, not just luma).
#[test]
fn studio_light_luminance_scales_with_brightness() {
    let mut fx = LedUtilityFixture::new("LED Studio Light");
    let tint = tint_rgb(200.0, 1.0);
    for brightness in [0.2f32, 0.5, 1.0] {
        let px = fx.render_led(0.0, &[("brightness", brightness), ("hue", 200.0), ("saturation", 1.0)]);
        let mean = mean_rgb(&px);
        for (chan, &t) in tint.iter().enumerate() {
            let want = t * brightness;
            assert!(
                (mean[chan] - want).abs() < TOL,
                "brightness {brightness}: channel {chan} mean {} want ~{want} (tint {tint:?})",
                mean[chan]
            );
        }
    }
}

/// Studio Light at its defaults: hue 45 at saturation 0.15 is the warm
/// work-light tint — channel ratios must match the CPU tint at brightness 1
/// (brightness override isolates the tint from the dimming).
#[test]
fn studio_light_default_tint_is_warm_white() {
    let mut fx = LedUtilityFixture::new("LED Studio Light");
    let px = fx.render_led(0.0, &[("brightness", 1.0)]);
    let mean = mean_rgb(&px);
    let tint = tint_rgb(45.0, 0.15);
    for (chan, &t) in tint.iter().enumerate() {
        assert!(
            (mean[chan] - t).abs() < TOL,
            "default tint: channel {chan} mean {} want ~{t} (tint {tint:?})",
            mean[chan]
        );
    }
    // A white light at low saturation: R > B, and the spread is small.
    assert!(mean[0] > mean[2], "warm tint must have R > B: {mean:?}");
    assert!(
        mean[0] - mean[2] < 0.5,
        "saturation 0.15 keeps the tint near white: {mean:?}"
    );
}

/// Strip ID: every column carries its own plateau hue (hue k*45), pairwise
/// distinct across the 8 columns.
#[test]
fn strip_id_columns_are_pairwise_distinct() {
    let mut fx = LedUtilityFixture::new("LED Strip ID");
    let px = fx.render_led(0.0, &[]);
    let cols = column_rgbs(&px);
    for (k, c) in cols.iter().enumerate() {
        let want = hsv2rgb(k as f32 * 45.0 / 360.0, 1.0, 1.0);
        for (chan, &w) in want.iter().enumerate() {
            assert!(
                (c[chan] - w).abs() < TOL,
                "column {k}: channel {chan} {} want ~{w} (col {c:?}, want {want:?})",
                c[chan]
            );
        }
    }
    for a in 0..LED_W as usize {
        for b in (a + 1)..LED_W as usize {
            let dist = (cols[a][0] - cols[b][0])
                .abs()
                .max((cols[a][1] - cols[b][1]).abs())
                .max((cols[a][2] - cols[b][2]).abs());
            assert!(
                dist > 4.0 * TOL,
                "columns {a} and {b} must be visually distinct: {:?} vs {:?} (dist {dist})",
                cols[a],
                cols[b]
            );
        }
    }
}

/// Pixel Walk: at rate 1 with the full 960 steps, the lit cell advances one
/// cell per step in x-major row order (index = row*8 + col, top row first),
/// exactly one cell lit at ~1.0 and the rest dark.
#[test]
fn pixel_walk_advances_one_cell_per_step_in_linear_order() {
    let mut fx = LedUtilityFixture::new("LED Pixel Walk");
    // beats picked mid-step: fract(rate * beats) * 960 = k + 0.5, away from
    // both step boundaries and f32 rounding of the phase.
    for k in [0usize, 1, 2, 7, 8, 9, 119, 120, 959] {
        let beats = (k as f64 + 0.5) / 960.0;
        let px = fx.render_led(beats, &[("rate", 1.0)]);
        let lumas = cell_lumas(&px);
        let lit: Vec<(usize, usize)> = lumas
            .iter()
            .enumerate()
            .flat_map(|(r, row)| {
                row.iter()
                    .enumerate()
                    .filter(|&(_, &v)| v > 0.5)
                    .map(move |(c, _)| (r, c))
            })
            .collect();
        let want = (k / LED_W as usize, k % LED_W as usize);
        assert_eq!(
            lit,
            vec![want],
            "step {k}: lit cells {lit:?}, want exactly {want:?}"
        );
        assert!(
            lumas[want.0][want.1] > 0.9,
            "lit cell at step {k} must be near full white: {}",
            lumas[want.0][want.1]
        );
        let max_dark = lumas
            .iter()
            .enumerate()
            .flat_map(|(r, row)| row.iter().enumerate().map(move |(c, &v)| (r, c, v)))
            .filter(|&(r, c, _)| (r, c) != want)
            .map(|(_, _, v)| v)
            .fold(0.0f32, f32::max);
        assert!(
            max_dark < 0.1,
            "step {k}: unlit cells must be dark, max leak {max_dark}"
        );
    }
}
