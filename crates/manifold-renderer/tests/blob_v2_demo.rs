//! Deterministic end-to-end Blob Tracking V2 demonstrations.
//!
//! These are intentionally GPU proofs. They drive the production effect-chain
//! runtime with CPU-authored Rgba16Float fixtures, save the observed frames to
//! `target/blob-v2-demo`, and keep small numeric assertions beside the visual
//! artifacts. Run with `--features gpu-proofs` on a host with the native
//! detector and optical-flow bundles available.

#![cfg(feature = "gpu-proofs")]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use half::f16;
use manifold_core::PresetTypeId;
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::headless_readback::{readback_raw_halves, readback_to_srgb_png_linear};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::{ChainBuildInputs, PresetRuntime};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 160;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
const FPS: f64 = 60.0;

fn artifact_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/blob-v2-demo")
}

fn write_png(device: &GpuDevice, texture: &GpuTexture, name: &str) -> PathBuf {
    let dir = artifact_dir();
    fs::create_dir_all(&dir).expect("create blob-v2 demo directory");
    let path = dir.join(format!("{name}.png"));
    fs::write(
        &path,
        readback_to_srgb_png_linear(device, texture, WIDTH, HEIGHT),
    )
    .unwrap_or_else(|error| panic!("write {path:?}: {error}"));
    path
}

fn write_png_bytes(device: &GpuDevice, bytes: &[u8], name: &str) -> PathBuf {
    assert_eq!(bytes.len(), (WIDTH * HEIGHT * 8) as usize);
    let texture = input_texture(device, "blob-v2-demo-artifact");
    device.upload_texture(&texture, bytes);
    write_png(device, &texture, name)
}

fn write_sidecar(name: &str, body: &str) -> PathBuf {
    let dir = artifact_dir();
    fs::create_dir_all(&dir).expect("create blob-v2 demo directory");
    let path = dir.join(format!("{name}.json"));
    fs::write(&path, format!("{{\n{body}\n}}\n"))
        .unwrap_or_else(|error| panic!("write {path:?}: {error}"));
    path
}

fn input_texture(device: &GpuDevice, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: WIDTH,
        height: HEIGHT,
        depth: 1,
        format: FORMAT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    })
}

fn upload(device: &GpuDevice, texture: &GpuTexture, pixels: &[[f32; 4]]) {
    let mut bytes = Vec::with_capacity(pixels.len() * 8);
    for pixel in pixels {
        for channel in pixel {
            bytes.extend_from_slice(&f16::from_f32(*channel).to_le_bytes());
        }
    }
    device.upload_texture(texture, &bytes);
}

fn context(frame: i64) -> PresetContext {
    PresetContext {
        time: frame as f64 / FPS,
        beat: frame as f64 / 30.0,
        dt: 1.0 / FPS as f32,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: WIDTH as f32 / HEIGHT as f32,
        owner_key: 0xB10B_0002,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn set_param(effect: &mut PresetInstance, id: &str, value: f32) {
    let param = effect
        .params
        .get_mut(id)
        .unwrap_or_else(|| panic!("preset {} has no parameter {id}", effect.id));
    param.value = value;
    param.base = value;
}

fn effect(id: &'static str) -> PresetInstance {
    let ty = PresetTypeId::new(id);
    let mut instance = manifold_core::preset_definition_registry::create_default(&ty);
    let mut project = manifold_core::project::Project::default();
    project.settings.master_effects = vec![instance.clone()];
    project.reconcile_param_manifests();
    instance = project.settings.master_effects.remove(0);
    instance
}

fn build(
    device: &Arc<GpuDevice>,
    registry: &PrimitiveRegistry,
    effect: &PresetInstance,
) -> PresetRuntime {
    PresetRuntime::try_build(
        ChainBuildInputs {
            effects: std::slice::from_ref(effect),
            groups: &[],
            primitives: registry,
            device,
            pool: None,
            width: WIDTH,
            height: HEIGHT,
            preview_effect: Some(&effect.id),
        },
        None,
    )
    .unwrap_or_else(|| panic!("{} runtime build failed", effect.id))
}

fn render_capture(
    device: &Arc<GpuDevice>,
    runtime: &mut PresetRuntime,
    effect: &PresetInstance,
    input: &GpuTexture,
    frame: i64,
) -> (Vec<u8>, GpuTexture) {
    let mut encoder = device.create_encoder("blob-v2-demo-frame");
    let output = {
        let mut gpu = GpuEncoder::new(&mut encoder, device);
        runtime
            .run(
                &mut gpu,
                input,
                std::slice::from_ref(effect),
                &[],
                &context(frame),
            )
            .expect("blob-v2 runtime output")
            .clone()
    };
    encoder.commit_and_wait_completed();
    let bytes = readback_raw_halves(device, &output, WIDTH, HEIGHT);
    (bytes, output)
}

fn render(
    device: &Arc<GpuDevice>,
    runtime: &mut PresetRuntime,
    effect: &PresetInstance,
    input: &GpuTexture,
    frame: i64,
) -> Vec<u8> {
    render_capture(device, runtime, effect, input, frame).0
}

fn halves_to_pixels(bytes: &[u8]) -> impl Iterator<Item = [f32; 4]> + '_ {
    bytes.chunks_exact(8).map(|pixel| {
        [
            f16::from_le_bytes([pixel[0], pixel[1]]).to_f32(),
            f16::from_le_bytes([pixel[2], pixel[3]]).to_f32(),
            f16::from_le_bytes([pixel[4], pixel[5]]).to_f32(),
            f16::from_le_bytes([pixel[6], pixel[7]]).to_f32(),
        ]
    })
}

fn assert_finite_and_bounded(label: &str, bytes: &[u8]) {
    for (index, pixel) in halves_to_pixels(bytes).enumerate() {
        for (channel, value) in pixel.into_iter().enumerate() {
            assert!(
                value.is_finite(),
                "{label}: NaN/Inf at pixel {index} channel {channel}"
            );
            assert!(
                (-0.01..=16.0).contains(&value),
                "{label}: unbounded value {value} at pixel {index} channel {channel}"
            );
        }
    }
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0.0;
    let mut count: f32 = 0.0;
    for (pa, pb) in halves_to_pixels(a).zip(halves_to_pixels(b)) {
        for (a, b) in pa.into_iter().zip(pb) {
            sum += (a - b).abs();
            count += 1.0;
        }
    }
    sum / count
}

fn region_mean(bytes: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
    let mut sum = 0.0;
    let mut count: f32 = 0.0;
    for (index, pixel) in halves_to_pixels(bytes).enumerate() {
        let x = index as u32 % WIDTH;
        let y = index as u32 / WIDTH;
        if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
            sum += pixel[0];
            count += 1.0;
        }
    }
    sum / count.max(1.0)
}

fn max_channel(bytes: &[u8]) -> f32 {
    halves_to_pixels(bytes)
        .flat_map(|pixel| pixel.into_iter())
        .fold(0.0_f32, f32::max)
}

fn blank() -> Vec<[f32; 4]> {
    vec![[0.02, 0.02, 0.02, 1.0]; (WIDTH * HEIGHT) as usize]
}

fn ring_scene(frame: u32, colour: [f32; 3], include_small_blob: bool) -> Vec<[f32; 4]> {
    let mut pixels = blank();
    let cx = 78.0 + frame as f32 * 5.0;
    let cy = 80.0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let radius = (dx * dx + dy * dy).sqrt();
            let ring = (27.0..=37.0).contains(&radius);
            let small = include_small_blob
                && ((x as f32 - 196.0).powi(2) + (y as f32 - 48.0).powi(2) < 8.0_f32.powi(2));
            if ring || small {
                pixels[(y * WIDTH + x) as usize] = [colour[0], colour[1], colour[2], 1.0];
            }
        }
    }
    pixels
}

fn moving_blob_scene(frame: u32, colour: [f32; 3]) -> Vec<[f32; 4]> {
    let mut pixels = blank();
    let cx = 30.0 + frame as f32 * 8.0;
    let cy = 84.0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2) < 24.0_f32.powi(2) {
                pixels[(y * WIDTH + x) as usize] = [colour[0], colour[1], colour[2], 1.0];
            }
        }
    }
    pixels
}

fn render_fixture(
    device: &Arc<GpuDevice>,
    runtime: &mut PresetRuntime,
    effect: &PresetInstance,
    input: &GpuTexture,
    frame: i64,
    pixels: &[[f32; 4]],
) -> Vec<u8> {
    upload(device, input, pixels);
    render(device, runtime, effect, input, frame)
}

fn render_group_fixture(
    device: &Arc<GpuDevice>,
    runtime: &mut PresetRuntime,
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    input: &GpuTexture,
    frame: i64,
    pixels: &[[f32; 4]],
) -> Vec<u8> {
    upload(device, input, pixels);
    let mut encoder = device.create_encoder("blob-v2-group-demo-frame");
    let output = {
        let mut gpu = GpuEncoder::new(&mut encoder, device);
        runtime
            .run(&mut gpu, input, effects, groups, &context(frame))
            .expect("Blob Mask group output")
            .clone()
    };
    encoder.commit_and_wait_completed();
    readback_raw_halves(device, &output, WIDTH, HEIGHT)
}

#[derive(Clone, Copy)]
struct DemoTrack {
    id: u32,
    observed: u32,
    age: f32,
    cx: f32,
    cy: f32,
}

fn observed_tracks(runtime: &PresetRuntime, effect: &PresetInstance) -> Vec<DemoTrack> {
    let arrays = runtime.dump_arrays(&effect.id);
    let tracks = arrays
        .iter()
        .find(|array| array.type_id == "node.track_regions" && array.port == "tracks")
        .expect("mask runtime must expose tracked regions to its label rasterizer");
    assert_eq!(tracks.item_size, 64, "V2 track ABI stride");
    let pointer = tracks
        .buffer
        .mapped_ptr()
        .expect("CPU tracker output must be mapped");
    let capacity = tracks.buffer.size as usize / tracks.item_size as usize;
    let mut records = Vec::new();
    for index in 0..capacity {
        let record = unsafe { pointer.add(index * tracks.item_size as usize) };
        let id = unsafe { std::ptr::read_unaligned(record.cast::<u32>()) };
        if id == 0 {
            continue;
        }
        let observed = unsafe { std::ptr::read_unaligned(record.add(8).cast::<u32>()) };
        let age = unsafe { std::ptr::read_unaligned(record.add(12).cast::<f32>()) };
        let cx = unsafe { std::ptr::read_unaligned(record.add(32).cast::<f32>()) };
        let cy = unsafe { std::ptr::read_unaligned(record.add(36).cast::<f32>()) };
        records.push(DemoTrack {
            id,
            observed,
            age,
            cx,
            cy,
        });
    }
    records
}

#[test]
fn blob_v2_tracking_demo() {
    let device = Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let mask_effect = effect("MaskBlob");
    let mut mask_runtime = build(&device, &registry, &mask_effect);
    mask_runtime.set_dump(Some(&mask_effect.id));
    let effect = effect("BlobTrackingV2");
    let mut runtime = build(&device, &registry, &effect);
    let input = input_texture(&device, "blob-v2-tracking-input");
    let mut records = Vec::new();
    let mut last = Vec::new();
    let mut max_hud_delta = 0.0_f32;
    let mut stable_ids = Vec::new();

    for frame in 0..6_u32 {
        let source = ring_scene(frame, [0.1, 0.8, 0.1], true);
        last = render_fixture(
            &device,
            &mut runtime,
            &effect,
            &input,
            frame as i64,
            &source,
        );
        assert_finite_and_bounded("tracking", &last);
        let delta = mean_abs_diff(&last, &source_bytes(&source));
        max_hud_delta = max_hud_delta.max(delta);
        let _ = render_fixture(
            &device,
            &mut mask_runtime,
            &mask_effect,
            &input,
            frame as i64,
            &source,
        );
        let tracks = observed_tracks(&mask_runtime, &mask_effect);
        let ids: Vec<u32> = tracks
            .iter()
            .filter(|track| track.observed != 0)
            .map(|track| track.id)
            .collect();
        if frame == 2 {
            assert_eq!(ids.len(), 2, "ring and smaller blob must both be tracked");
            stable_ids = ids;
        } else if frame > 2 {
            assert_eq!(ids, stable_ids, "IDs must survive ring motion");
        }
        let track_records: Vec<String> = tracks.iter().map(|track| format!(
            "{{\"id\": {}, \"observed\": {}, \"age_seconds\": {:.6}, \"cx\": {:.6}, \"cy\": {:.6}}}",
            track.id, track.observed, track.age, track.cx, track.cy
        )).collect();
        records.push(format!(
            "    {{\"frame\": {frame}, \"mean_delta\": {:.6}, \"mask_tracks\": [{}]}}",
            delta,
            track_records.join(", ")
        ));
    }
    assert!(
        max_hud_delta > 0.0001,
        "the brightness fixture must produce visible HUD pixels"
    );
    let disappearing = blank();
    let disappeared = render_fixture(&device, &mut runtime, &effect, &input, 6, &disappearing);
    assert_finite_and_bounded("tracking-disappearance", &disappeared);
    let disappearance_delta = mean_abs_diff(&last, &disappeared);
    assert!(
        disappearance_delta > 0.0001,
        "disappearance must change the tracked observation"
    );
    runtime.reset_state(&device);
    let reset = render_fixture(
        &device,
        &mut runtime,
        &effect,
        &input,
        0,
        &ring_scene(0, [0.1, 0.8, 0.1], true),
    );
    assert_finite_and_bounded("tracking-reset", &reset);
    assert!(
        mean_abs_diff(&last, &reset) > 0.0001,
        "moving/reset fixture must produce distinct observations"
    );
    write_png(&device, &input, "blob_v2_tracking_input");
    write_png_bytes(&device, &last, "blob_v2_tracking_output");
    write_png_bytes(&device, &reset, "blob_v2_tracking_reset");
    write_sidecar(
        "blob_v2_tracking_demo",
        &format!(
            "  \"frames\": [\n{}\n  ],\n  \"disappearance_mean_delta\": {:.6},\n  \"reset_mean_delta\": {:.6}",
            records.join(",\n"),
            disappearance_delta,
            mean_abs_diff(&last, &reset)
        ),
    );
}

fn source_bytes(pixels: &[[f32; 4]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pixels.len() * 8);
    for pixel in pixels {
        for value in pixel {
            bytes.extend_from_slice(&f16::from_f32(*value).to_le_bytes());
        }
    }
    bytes
}

#[test]
fn blob_v2_mask_demo() {
    let device = Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let mut effect = effect("MaskBlob");
    let mut runtime = build(&device, &registry, &effect);
    let input = input_texture(&device, "blob-v2-mask-input");
    set_param(&mut effect, "expand", 0.0);
    set_param(&mut effect, "feather", 0.0);
    let source = ring_scene(0, [1.0, 1.0, 1.0], false);
    let mut raw = Vec::new();
    for frame in 0..6 {
        raw = render_fixture(&device, &mut runtime, &effect, &input, frame, &source);
    }
    assert_finite_and_bounded("mask-raw", &raw);
    let raw_hole = region_mean(&raw, 68, 70, 88, 90);
    set_param(&mut effect, "expand", 24.0);
    let mut expanded = Vec::new();
    for frame in 6..10 {
        expanded = render_fixture(&device, &mut runtime, &effect, &input, frame, &source);
    }
    assert_finite_and_bounded("mask-expanded", &expanded);
    let expanded_hole = region_mean(&expanded, 68, 70, 88, 90);
    assert!(
        raw_hole < 0.25,
        "ring hole must remain dry before expansion: {raw_hole:.4}"
    );
    assert!(
        expanded_hole > raw_hole + 0.05,
        "positive expansion must fill the hole: raw={raw_hole:.4}, expanded={expanded_hole:.4}"
    );
    write_png(&device, &input, "blob_v2_mask_input");
    write_png_bytes(&device, &raw, "blob_v2_mask_raw");
    write_png_bytes(&device, &expanded, "blob_v2_mask_expanded");

    let mut group = EffectGroup::new("Blob Mask Demo".into());
    let mut group_mask = self::effect("MaskBlob");
    set_param(&mut group_mask, "feather", 0.0);
    group_mask.group_id = Some(group.id.clone());
    let mut wet =
        manifold_core::preset_definition_registry::create_default(&PresetTypeId::INVERT_COLORS);
    wet.group_id = Some(group.id.clone());
    group.mask_effect_id = Some(group_mask.id.clone());
    group.wet_dry = 1.0;
    let mut effects = vec![group_mask, wet];
    let mut groups = vec![group];
    let mut group_runtime = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &effects,
            groups: &groups,
            primitives: &registry,
            device: &device,
            pool: None,
            width: WIDTH,
            height: HEIGHT,
            preview_effect: None,
        },
        None,
    )
    .expect("Blob Mask group builds");
    let mut group_raw = Vec::new();
    for frame in 0..6 {
        group_raw = render_group_fixture(
            &device,
            &mut group_runtime,
            &effects,
            &groups,
            &input,
            frame,
            &source,
        );
    }
    let dry_hole = region_mean(&group_raw, 68, 70, 88, 90);
    let wet_ring = region_mean(&group_raw, 75, 48, 81, 53);
    let dry_outside = region_mean(&group_raw, 8, 8, 16, 16);
    assert!(
        (dry_hole - dry_outside).abs() < 0.01 && wet_ring < dry_hole - 0.01,
        "group must keep the ring hole/outside dry and invert only the ring: hole={dry_hole:.3}, ring={wet_ring:.3}, outside={dry_outside:.3}"
    );
    set_param(&mut effects[0], "expand", 24.0);
    let mut group_expanded = Vec::new();
    for frame in 6..10 {
        group_expanded = render_group_fixture(
            &device,
            &mut group_runtime,
            &effects,
            &groups,
            &input,
            frame,
            &source,
        );
    }
    let expanded_group_hole = region_mean(&group_expanded, 68, 70, 88, 90);
    assert!(
        expanded_group_hole > dry_hole + 0.5,
        "expanded mask must reveal the wet effect in the ring hole: {expanded_group_hole:.3}"
    );
    groups[0].wet_dry = 0.5;
    let half_wet = render_group_fixture(
        &device,
        &mut group_runtime,
        &effects,
        &groups,
        &input,
        10,
        &source,
    );
    let half_wet_hole = region_mean(&half_wet, 68, 70, 88, 90);
    assert!(
        half_wet_hole > dry_hole + 0.2 && half_wet_hole < expanded_group_hole - 0.2,
        "group wet/dry must scale the Blob Mask coverage: {half_wet_hole:.3}"
    );
    write_png_bytes(&device, &group_raw, "blob_v2_group_raw");
    write_png_bytes(&device, &group_expanded, "blob_v2_group_expanded");
    write_sidecar(
        "blob_v2_mask_demo",
        &format!(
            "  \"raw_hole_mean\": {:.6},\n  \"expanded_hole_mean\": {:.6},\n  \"group_dry_hole_mean\": {:.6},\n  \"group_wet_ring_mean\": {:.6},\n  \"group_expanded_hole_mean\": {:.6},\n  \"group_half_wet_hole_mean\": {:.6},\n  \"raw_png\": \"blob_v2_mask_raw.png\",\n  \"expanded_png\": \"blob_v2_mask_expanded.png\",\n  \"group_raw_png\": \"blob_v2_group_raw.png\",\n  \"group_expanded_png\": \"blob_v2_group_expanded.png\"",
            raw_hole, expanded_hole, dry_hole, wet_ring, expanded_group_hole, half_wet_hole
        ),
    );
}

#[test]
fn blob_v2_source_variants_demo() {
    let device = Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let input = input_texture(&device, "blob-v2-source-variants-input");
    let red = [0.95, 0.04, 0.03];
    let blue = [0.03, 0.08, 0.95];
    let matched = ring_scene(0, red, false);
    let wrong = ring_scene(0, blue, false);

    let colour_effect = effect("BlobTrackingV2Colour");
    let mut colour_runtime = build(&device, &registry, &colour_effect);
    let mut matched_output = Vec::new();
    for frame in 0..6 {
        matched_output = render_fixture(
            &device,
            &mut colour_runtime,
            &colour_effect,
            &input,
            frame,
            &matched,
        );
    }
    colour_runtime.reset_state(&device);
    let mut wrong_output = Vec::new();
    for frame in 6..12 {
        wrong_output = render_fixture(
            &device,
            &mut colour_runtime,
            &colour_effect,
            &input,
            frame,
            &wrong,
        );
    }
    assert_finite_and_bounded("colour-matched", &matched_output);
    assert_finite_and_bounded("colour-wrong", &wrong_output);
    let matched_delta = mean_abs_diff(&matched_output, &source_bytes(&matched));
    let wrong_delta = mean_abs_diff(&wrong_output, &source_bytes(&wrong));
    assert!(
        matched_delta > wrong_delta + 0.0001,
        "matched colour must produce stronger detector output: matched={matched_delta:.5}, wrong={wrong_delta:.5}"
    );

    let motion_effect = effect("BlobTrackingV2Motion");
    let mut motion_runtime = build(&device, &registry, &motion_effect);
    let static_frame = moving_blob_scene(0, [0.8, 0.8, 0.8]);
    let mut static_output = Vec::new();
    for frame in 0..6 {
        static_output = render_fixture(
            &device,
            &mut motion_runtime,
            &motion_effect,
            &input,
            frame,
            &static_frame,
        );
    }
    let mut moving_output = Vec::new();
    for frame in 6..12 {
        moving_output = render_fixture(
            &device,
            &mut motion_runtime,
            &motion_effect,
            &input,
            frame,
            &moving_blob_scene(frame as u32, [0.8, 0.8, 0.8]),
        );
    }
    let hard_cut = vec![[1.0, 1.0, 1.0, 1.0]; (WIDTH * HEIGHT) as usize];
    let cut_output = render_fixture(
        &device,
        &mut motion_runtime,
        &motion_effect,
        &input,
        12,
        &hard_cut,
    );
    assert_finite_and_bounded("motion-static", &static_output);
    assert_finite_and_bounded("motion-moving", &moving_output);
    assert_finite_and_bounded("motion-hard-cut", &cut_output);
    let static_delta = mean_abs_diff(&static_output, &source_bytes(&static_frame));
    let moving_delta = mean_abs_diff(
        &moving_output,
        &source_bytes(&moving_blob_scene(11, [0.8, 0.8, 0.8])),
    );
    assert!(
        moving_delta > static_delta + 0.0001,
        "moving fixture must exceed static detector output: moving={moving_delta:.5}, static={static_delta:.5}"
    );

    let motion_mask_effect = effect("MaskBlobMotion");
    let mut motion_mask_runtime = build(&device, &registry, &motion_mask_effect);
    let first_mask = render_fixture(
        &device,
        &mut motion_mask_runtime,
        &motion_mask_effect,
        &input,
        0,
        &static_frame,
    );
    let first_mask_area = region_mean(&first_mask, 0, 0, WIDTH, HEIGHT);
    assert!(
        first_mask_area < 0.001,
        "first motion frame flashed coverage: {first_mask_area:.5}"
    );
    for frame in 1..6 {
        let _ = render_fixture(
            &device,
            &mut motion_mask_runtime,
            &motion_mask_effect,
            &input,
            frame,
            &static_frame,
        );
    }
    let mut moving_mask = Vec::new();
    for frame in 6..12 {
        moving_mask = render_fixture(
            &device,
            &mut motion_mask_runtime,
            &motion_mask_effect,
            &input,
            frame,
            &moving_blob_scene(frame as u32, [0.8, 0.8, 0.8]),
        );
    }
    let moving_mask_area = region_mean(&moving_mask, 0, 0, WIDTH, HEIGHT);
    assert!(
        moving_mask_area > 0.001,
        "motion mask must cover moving pixels: {moving_mask_area:.5}"
    );
    let mut cut_mask = Vec::new();
    let mut cut_mask_final = Vec::new();
    let mut cut_mask_areas = Vec::new();
    for frame in 12..16 {
        let output = render_fixture(
            &device,
            &mut motion_mask_runtime,
            &motion_mask_effect,
            &input,
            frame,
            &hard_cut,
        );
        let area = region_mean(&output, 0, 0, WIDTH, HEIGHT);
        cut_mask_areas.push(area);
        if frame == 12 {
            cut_mask = output.clone();
        }
        cut_mask_final = output;
    }
    let cut_mask_area = cut_mask_areas[0];
    assert!(
        cut_mask_areas.iter().all(|area| *area < 0.2),
        "hard cut must not flash full coverage: {cut_mask_areas:?}"
    );
    assert!(
        cut_mask_areas[3] < 0.01,
        "hard cut reset must clear stale moving coverage: {cut_mask_areas:?}"
    );
    write_png_bytes(&device, &matched_output, "blob_v2_colour_matched");
    write_png_bytes(&device, &wrong_output, "blob_v2_colour_wrong");
    write_png_bytes(&device, &static_output, "blob_v2_motion_static");
    write_png_bytes(&device, &moving_output, "blob_v2_motion_moving");
    write_png_bytes(&device, &cut_output, "blob_v2_motion_hard_cut");
    write_png_bytes(&device, &moving_mask, "blob_v2_motion_mask_moving");
    write_png_bytes(&device, &cut_mask, "blob_v2_motion_mask_hard_cut");
    write_png_bytes(&device, &cut_mask_final, "blob_v2_motion_mask_cut_settled");
    write_sidecar(
        "blob_v2_source_variants_demo",
        &format!(
            "  \"colour_matched_delta\": {:.6},\n  \"colour_wrong_delta\": {:.6},\n  \"motion_static_delta\": {:.6},\n  \"motion_moving_delta\": {:.6},\n  \"motion_first_mask_area\": {:.6},\n  \"motion_moving_mask_area\": {:.6},\n  \"motion_hard_cut_mask_area\": {:.6},\n  \"motion_cut_areas\": [{}],\n  \"hard_cut_max_channel\": {:.6},\n  \"artifact_means\": [{{\"matched\": {:.6}, \"wrong\": {:.6}, \"motion\": {:.6}, \"hard_cut\": {:.6}}}]",
            matched_delta,
            wrong_delta,
            static_delta,
            moving_delta,
            first_mask_area,
            moving_mask_area,
            cut_mask_area,
            cut_mask_areas
                .iter()
                .map(|area| format!("{area:.6}"))
                .collect::<Vec<_>>()
                .join(", "),
            max_channel(&cut_output),
            matched_delta,
            wrong_delta,
            moving_delta,
            mean_abs_diff(&cut_output, &source_bytes(&hard_cut))
        ),
    );
}
