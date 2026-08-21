//! D15 — amount-0 passthrough audit for every effect preset that used
//! `skipMode: { kind: "onZero", paramId: ... }`.
//!
//! SkipMode is gone (P6). An effect whose slider is at 0 must now run as a
//! normal effect at 0, which means the output must be pixel-identical to the
//! input or previously-skipped clips will suddenly change appearance in saved
//! shows. This test renders each formerly-skippable effect preset over the
//! standard gradient with its skip-bound outer-card param forced to 0 and
//! asserts the rendered output equals the input byte-for-byte in Rgba16Float
//! space. A failure means the kernel (or the graph wiring) does not implement
//! true zero-identity and must be fixed — re-adding a skip is forbidden.

use half::f16;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::gpu_encoder::GpuEncoder;
use crate::headless_readback;
use crate::node_graph::{
    compile, EffectGraphDefExt, Executor, FrameTime, MetalBackend, ParamValue, PrimitiveRegistry,
    StateStore, FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID,
};
use crate::preset_thumbnail::{build_gradient_input, output_resource};
use crate::render_target::RenderTarget;

const SIZE: u32 = 128;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// Every bundled effect preset whose removed `skipMode` was `onZero`. The
/// second element is the outer-card param id that used to drive the skip.
///
/// Enumerated from commit 042bf0679 (warmup P6 D14: strip skipMode from preset
/// JSONs). All other presets in that commit had `kind: "never"` and therefore
/// did not change behaviour when the key was removed.
const FORMERLY_SKIPPABLE_EFFECTS: &[(&str, &str)] = &[
    ("Bloom", "amount"),
    ("ChromaticAberration", "amount"),
    ("ColorCompass", "intensity"),
    ("ColorGrade", "amount"),
    ("DigitalDrift", "amount"),
    ("Dither", "amount"),
    ("EdgeDetect", "amount"),
    ("EdgeStretch", "amount"),
    ("Glitch", "amount"),
    ("HighlightBoost", "amount"),
    ("Infrared", "amount"),
    ("Invert", "amount"),
    ("Kaleidoscope", "amount"),
    ("Mirror", "amount"),
    ("QuadMirror", "amount"),
    ("SoftFocus", "amount"),
    ("Strobe", "amount"),
    ("VoronoiPrism", "amount"),
];

/// The same non-uniform gradient the thumbnail/parity harness uses, but kept
/// as raw CPU f16 bytes so we can compare output against a deterministic
/// expected buffer without an extra GPU readback.
fn expected_gradient_bytes(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 8) as usize);
    let wm = ((w.max(1) - 1) as f32).max(1.0);
    let hm = ((h.max(1) - 1) as f32).max(1.0);
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / wm;
            let v = y as f32 / hm;
            for &c in &[u, v, (u + v) * 0.5, 1.0f32] {
                out.extend_from_slice(&f16::from_f32(c).to_bits().to_le_bytes());
            }
        }
    }
    out
}

/// Drive every binding whose outer id matches `outer_id` to the given value.
/// We intentionally set the *inner* node param directly on the built graph,
/// mirroring what the chain runtime does when the outer card slider is at 0.
fn zero_outer_param(graph: &mut crate::node_graph::Graph, def: &EffectGraphDef, outer_id: &str, value: f32) -> Result<(), String> {
    let meta = def
        .preset_metadata
        .as_ref()
        .ok_or_else(|| "preset has no metadata".to_string())?;
    let mut found = false;
    for binding in &meta.bindings {
        if binding.id != outer_id {
            continue;
        }
        let BindingTarget::Node { node_id, param } = &binding.target else {
            return Err(format!(
                "binding {outer_id} targets {:?}, expected a node",
                binding.target
            ));
        };
        let inst = graph
            .instance_by_node_id(node_id)
            .ok_or_else(|| format!("no graph instance for node_id {node_id:?}"))?;
        graph
            .set_param(inst, param, ParamValue::Float(value))
            .map_err(|e| format!("set_param {node_id:?}.{param} failed: {e:?}"))?;
        found = true;
    }
    if !found {
        return Err(format!("no binding found for outer param {outer_id}"));
    }
    Ok(())
}

/// Render `def` over the standard gradient with the skip-bound outer-card
/// param forced to `value`, then read back the raw Rgba16Float output bytes.
fn render_effect_raw(
    device: &std::sync::Arc<GpuDevice>,
    def: &EffectGraphDef,
    outer_id: &str,
    value: f32,
) -> Result<Vec<u8>, String> {
    let registry = PrimitiveRegistry::with_builtin();
    let mut graph = def
        .clone()
        .into_graph(&registry)
        .map_err(|e| format!("graph load failed: {e}"))?;
    zero_outer_param(&mut graph, def, outer_id, value)?;

    let plan = compile(&graph).map_err(|e| format!("compile failed: {e:?}"))?;

    let source_id = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == SOURCE_TYPE_ID)
        .map(|n| n.id)
        .ok_or_else(|| "preset has no system.source node".to_string())?;
    let final_id = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)
        .map(|n| n.id)
        .ok_or_else(|| "preset has no system.final_output node".to_string())?;

    let source_out = output_resource(&plan, source_id, "out")
        .ok_or_else(|| "system.source has no out resource".to_string())?;
    let final_in = plan
        .steps()
        .iter()
        .find(|s| s.node == final_id)
        .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in"))
        .map(|(_, r)| *r)
        .ok_or_else(|| "system.final_output has no bound in".to_string())?;

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), SIZE, SIZE, FORMAT);
    let input_target = build_gradient_input(device, SIZE, SIZE, FORMAT);
    let source_slot = backend.pre_bind_texture_2d(source_out, input_target);
    let output_slot = if final_in == source_out {
        source_slot
    } else {
        let out_target = RenderTarget::new(device, SIZE, SIZE, FORMAT, "amount-zero-fx-out");
        backend.pre_bind_texture_2d(final_in, out_target)
    };

    let mut state_store = StateStore::new();
    let mut native_enc = device.create_encoder("amount-zero-passthrough");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = GpuEncoder::new(&mut native_enc, device);
        exec.execute_frame_with_state(
            &mut graph,
            &plan,
            FrameTime {
                beats: Beats(2.5),
                seconds: Seconds(1.234),
                delta: Seconds(1.0 / 60.0),
                frame_count: 0,
            },
            &mut gpu,
            &mut state_store,
            0,
        );
    }
    native_enc.commit_and_wait_completed();

    let tex = exec
        .backend()
        .texture_2d(output_slot)
        .ok_or_else(|| "output texture missing after execute".to_string())?;
    Ok(headless_readback::readback_raw_halves(device, tex, SIZE, SIZE))
}

#[test]
fn amount_zero_effect_passthrough_is_identity() {
    let device = crate::test_device();
    let expected = expected_gradient_bytes(SIZE, SIZE);
    let catalog = crate::preset_loader::EFFECT_CATALOG.load();
    let mut failures: Vec<String> = Vec::new();

    for (preset_id, outer_id) in FORMERLY_SKIPPABLE_EFFECTS {
        let json = match catalog.json(preset_id) {
            Some(j) => j,
            None => {
                failures.push(format!("{preset_id}: not in effect catalog"));
                continue;
            }
        };
        let def: EffectGraphDef = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{preset_id}: parse failed: {e}"));
                continue;
            }
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_effect_raw(&device.arc(), &def, outer_id, 0.0)
        }));
        let output = match result {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => {
                failures.push(format!("{preset_id}: render error: {e}"));
                continue;
            }
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else {
                    "<non-string panic>".to_string()
                };
                failures.push(format!("{preset_id}: panic: {msg}"));
                continue;
            }
        };

        if output != expected {
            // Exact identity is the contract. Compute a per-component mean
            // absolute difference only to make the failure message useful.
            let diff = headless_readback::mean_abs_half_diff(&output, &expected);
            failures.push(format!(
                "{preset_id}: output != input at {outer_id}=0 (mean abs diff {diff:.6})"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "amount=0 is not identity for these formerly-skippable effects:\n  - {}",
        failures.join("\n  - ")
    );
}
