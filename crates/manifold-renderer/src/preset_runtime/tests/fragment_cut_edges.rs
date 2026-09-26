//! Rendered cut-edge coverage for every stock fragment modifier.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::frame_status::FrameRenderStatus;
use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::headless_readback::{encode_rgba8_png, readback_raw_halves, readback_tonemapped_rgba8};
use crate::node_graph::PrimitiveRegistry;
use crate::node_graph::gltf_import::assemble_import_graph;
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
use crate::render_target::RenderTarget;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::id::NodeId;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat};

fn attach(owner: &EffectGraphDef, name: &str) -> EffectGraphDef {
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    let recipe = crate::node_graph::bundled_preset_def(&manifold_core::PresetTypeId::from_string(
        name.to_owned(),
    ))
    .unwrap();
    let scene = owner
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .unwrap();
    let instance = crate::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        owner,
        recipe,
        NodeId::new("fragment_cut_audit"),
        SceneNodeRef {
            scope: vec![],
            node: scene.node_id.clone(),
        },
        SceneTargetSelection::AllObjects,
    )
    .unwrap();
    manifold_core::scene_modifier_edit::insert_scene_modifier(
        owner,
        owner.scene_modifiers.len(),
        instance,
    )
    .unwrap()
    .graph
}

const WIDTH: u32 = 960;
const HEIGHT: u32 = 960;
const MAX_FRAMES: u32 = 60;

struct Observation {
    raw: Vec<u8>,
    rgba: Vec<u8>,
}

fn fixture_path() -> PathBuf {
    std::env::var_os("MANIFOLD_CUT_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/gltf/practice_head_sculpt.glb")
        })
}

fn default_manifest(def: &EffectGraphDef) -> ParamManifest {
    let metadata = def.preset_metadata.as_ref().expect("import metadata");
    ParamManifest::from_params(
        metadata
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    )
}

fn modifier_binding_id(def: &EffectGraphDef, modifier_id: &str, param_id: &str) -> String {
    def.preset_metadata
        .as_ref()
        .expect("host metadata")
        .bindings
        .iter()
        .find_map(|binding| {
            matches!(
                &binding.target,
                BindingTarget::SceneModifier {
                    modifier_id: owner,
                    param_id: parameter,
                } if owner == &NodeId::new(modifier_id) && parameter == param_id
            )
            .then(|| binding.id.clone())
        })
        .unwrap_or_else(|| panic!("missing {modifier_id}/{param_id} scene binding"))
}

fn audit_manifest(
    def: &EffectGraphDef,
    modifier_id: &str,
    values: &[(&str, f32)],
) -> ParamManifest {
    let mut manifest = default_manifest(def);
    for (param_id, value) in values {
        let id = modifier_binding_id(def, modifier_id, param_id);
        let param = manifest
            .get_mut(&id)
            .unwrap_or_else(|| panic!("missing host parameter {id}"));
        param.value = *value;
        param.base = *value;
    }
    manifest
}

fn visible_count(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4)
        .filter(|px| px[..3].iter().any(|value| *value > 8))
        .count()
}

fn mixed_alpha_count(raw: &[u8]) -> usize {
    raw.chunks_exact(8)
        .filter(|px| {
            let alpha = half::f16::from_bits(u16::from_le_bytes([px[6], px[7]])).to_f32();
            alpha > 0.01 && alpha < 0.99
        })
        .count()
}

fn alpha_at(raw: &[u8], x: usize, y: usize) -> f32 {
    let index = (y * WIDTH as usize + x) * 8 + 6;
    half::f16::from_bits(u16::from_le_bytes([raw[index], raw[index + 1]])).to_f32()
}

/// Count fractional-alpha pixels that sit inside the baseline silhouette,
/// with a fully opaque 5x5 baseline neighborhood. This excludes the
/// imported model's original outside contour from the cut-edge witness.
fn interior_cut_edge_count(split: &[u8], baseline: &[u8]) -> usize {
    let width = WIDTH as usize;
    let height = HEIGHT as usize;
    let mut count = 0;
    for y in 2..height.saturating_sub(2) {
        for x in 2..width.saturating_sub(2) {
            let alpha = alpha_at(split, x, y);
            if !(alpha > 0.01 && alpha < 0.99) {
                continue;
            }
            if (y - 2..=y + 2).all(|ny| (x - 2..=x + 2).all(|nx| alpha_at(baseline, nx, ny) > 0.99))
            {
                count += 1;
            }
        }
    }
    count
}

fn render_until_ready(
    runtime: &mut PresetRuntime,
    device: &GpuDevice,
    target: &RenderTarget,
    manifest: &ParamManifest,
    hit: bool,
    label: &str,
) -> Observation {
    let mut previous = None;
    let mut stable_frames = 0;
    let mut triggered = !hit;
    for frame in 0..MAX_FRAMES {
        let trigger_count = u32::from(hit && triggered);
        let context = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: WIDTH,
            height: HEIGHT,
            output_width: WIDTH,
            output_height: HEIGHT,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count,
        };
        let mut encoder = device.create_encoder("fragment-cut-audit");
        let status = {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
            runtime.render(&mut gpu, &target.texture, &context, manifest);
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        let raw = readback_raw_halves(device, &target.texture, WIDTH, HEIGHT);
        let rgba = readback_tonemapped_rgba8(device, &target.texture, WIDTH, HEIGHT);
        let visible = visible_count(&rgba);
        eprintln!(
            "[fragment-cut-audit] {label} frame={frame} status={status:?} visible={visible} mixed_alpha={}",
            mixed_alpha_count(&raw)
        );
        if status == FrameRenderStatus::Complete && visible > 32 && frame >= 2 {
            if !triggered {
                // Warm the imported mesh before firing, as the app does
                // before playback. Loading frames are not trigger evidence.
                runtime.note_modifier_clip_event(None);
                triggered = true;
                previous = None;
                continue;
            }
            if previous.as_ref() == Some(&raw) {
                stable_frames += 1;
            } else {
                stable_frames = 0;
            }
            if stable_frames >= 2 {
                return Observation { raw, rgba };
            }
        }
        previous = Some(raw);
    }
    panic!("{label}: no ready visible frame within {MAX_FRAMES} frames")
}

fn write_observation(out: &Path, label: &str, observation: &Observation) {
    std::fs::create_dir_all(out).expect("create cut audit output directory");
    std::fs::write(
        out.join(format!("{label}.png")),
        encode_rgba8_png(&observation.rgba, WIDTH, HEIGHT),
    )
    .expect("write cut audit PNG");
    std::fs::write(out.join(format!("{label}.rgba16f")), &observation.raw)
        .expect("write cut audit raw readback");
}

fn render_case(
    base: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &Arc<GpuDevice>,
    preset: Option<&str>,
    fused: bool,
) -> Observation {
    let (owner, manifest, hit) = match preset {
        None => (base.clone(), default_manifest(base), false),
        Some(name) => {
            let owner = attach(base, name);
            let values: &[(&str, f32)] = match name {
                "OrderedRecon" => &[
                    ("progress", 0.58),
                    ("bands", 12.0),
                    ("separation", 0.22),
                    ("spread", 0.08),
                ],
                "OrderedReconHit" => &[
                    ("progress", 0.2),
                    ("bands", 12.0),
                    ("separation", 0.22),
                    ("clip_trigger", 1.0),
                ],
                "SurfacePeel" => &[("lift", 0.28), ("spread", 0.08), ("detail", 1.0)],
                "SurfacePeelHit" => &[
                    ("lift", 0.22),
                    ("burst_strength", 0.45),
                    ("clip_trigger", 1.0),
                ],
                // Keep the authored active mask: this is the path whose
                // centroid mask can expose jagged cut boundaries.
                "MaskedPeel" => &[("lift", 0.28), ("mask_amount", 1.0)],
                "VortexFragments" => &[("orbit", 1.3), ("rise", 0.22), ("separation", 0.18)],
                other => panic!("unknown fragment audit preset {other}"),
            };
            (
                owner.clone(),
                audit_manifest(&owner, "fragment_cut_audit", values),
                name.ends_with("Hit"),
            )
        }
    };
    let runtime = PresetRuntime::from_def_for_render(owner, registry, Some(&manifest), fused)
        .unwrap_or_else(|error| panic!("{preset:?}: prepare runtime: {error:?}"))
        .with_generator_device(
            Arc::clone(device),
            WIDTH,
            HEIGHT,
            GpuTextureFormat::Rgba16Float,
        )
        .unwrap_or_else(|error| panic!("{preset:?}: build runtime: {error:?}"));
    let mut runtime = runtime;
    let target = RenderTarget::new(
        device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "fragment-cut-audit",
    );
    let observation = render_until_ready(
        &mut runtime,
        device,
        &target,
        &manifest,
        hit,
        preset.unwrap_or("baseline"),
    );
    assert!(
        runtime.errors().is_empty(),
        "{} runtime errors: {:?}",
        preset.unwrap_or("baseline"),
        runtime.errors()
    );
    observation
}

fn audit(fused: bool) {
    let model = fixture_path();
    assert!(
        model.exists(),
        "practice head fixture missing: {}",
        model.display()
    );
    let (base, report) = assemble_import_graph(&model).expect("practice head import");
    eprintln!("[fragment-cut-audit] import report: {report:?}");
    let device = crate::test_device();
    let device = device.arc();
    let registry = PrimitiveRegistry::with_builtin();
    let baseline = render_case(&base, &registry, &device, None, fused);
    assert!(
        visible_count(&baseline.rgba) > 32,
        "baseline render is empty"
    );
    if let Some(out) = std::env::var_os("MANIFOLD_CUT_AUDIT_OUT") {
        write_observation(&PathBuf::from(out), "baseline", &baseline);
    }

    for preset in [
        "OrderedRecon",
        "OrderedReconHit",
        "SurfacePeel",
        "SurfacePeelHit",
        "MaskedPeel",
        "VortexFragments",
    ] {
        let observation = render_case(&base, &registry, &device, Some(preset), fused);
        if let Some(out) = std::env::var_os("MANIFOLD_CUT_AUDIT_OUT") {
            write_observation(&PathBuf::from(out), preset, &observation);
        }
        assert_ne!(
            observation.raw, baseline.raw,
            "{preset} split render is byte-identical to the no-modifier baseline"
        );
        let interior_cut_edges = interior_cut_edge_count(&observation.raw, &baseline.raw);
        assert!(
            interior_cut_edges > 8,
            "{preset} produced no measurable interior cut coverage (count={interior_cut_edges}); \
             inspect raw RGBA16F output for an opaque-output limitation"
        );
        eprintln!(
            "[fragment-cut-audit] {preset}: visible={} mixed_alpha={} interior_cut_edges={interior_cut_edges}",
            visible_count(&observation.rgba),
            mixed_alpha_count(&observation.raw)
        );
    }
}

#[test]
fn fragment_cut_all_stock_render_edges() {
    audit(false);
}

#[test]
fn fragment_cut_all_stock_fused_render_edges() {
    audit(true);
}
