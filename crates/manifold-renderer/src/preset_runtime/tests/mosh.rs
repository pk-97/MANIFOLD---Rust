//! Multi-frame evidence through the real effect-chain and parameter binding path.
use super::*;
use crate::gpu_encoder::GpuEncoder;
use crate::headless_readback::readback_raw_halves;
use crate::preset_context::PresetContext;
use half::f16;
use manifold_core::PresetTypeId;
use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

const SIZE: u32 = 512;

fn context(frame: i64, trigger_count: u32) -> PresetContext {
    PresetContext {
        time: frame as f64 / 60.0,
        beat: frame as f64 / 30.0,
        dt: 1.0 / 60.0,
        width: SIZE,
        height: SIZE,
        output_width: SIZE,
        output_height: SIZE,
        aspect: 1.0,
        owner_key: 1,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count,
    }
}

fn set(fx: &mut PresetInstance, id: &str, value: f32) {
    let p = fx
        .params
        .get_mut(id)
        .unwrap_or_else(|| panic!("missing {id}"));
    p.value = value;
    p.base = value;
}

fn make_effect(name: &'static str, fused: bool, registry: &PrimitiveRegistry) -> PresetInstance {
    let ty = PresetTypeId::new(name);
    let view = loaded_preset_view_by_id(&ty).expect("mosh preset registered");
    let mut fx = manifold_core::preset_definition_registry::create_default(&ty);
    if fused {
        let result =
            crate::node_graph::freeze::install::fuse_canonical_def(&view.canonical_def, registry)
                .expect("mosh has fusable regions");
        fx.graph = Some(result.def);
        fx.bump_graph_structure_version();
    }
    // Actual instance serialization, then animate its controls after reload.
    let reloaded = serde_json::from_str(&serde_json::to_string(&fx).unwrap()).unwrap();
    let mut project = manifold_core::project::Project::default();
    project.settings.master_effects = vec![reloaded];
    project.reconcile_param_manifests();
    project.settings.master_effects.remove(0)
}

fn build(device: &GpuDevice, registry: &PrimitiveRegistry, fx: &PresetInstance) -> PresetRuntime {
    PresetRuntime::try_build(
        ChainBuildInputs {
            effects: std::slice::from_ref(fx),
            groups: &[],
            primitives: registry,
            device,
            pool: None,
            width: SIZE,
            height: SIZE,
            // Explicit canonical or explicitly compiled graph: no async compiler race.
            preview_effect: Some(&fx.id),
        },
        None,
    )
    .expect("mosh runtime builds")
}

fn input(device: &GpuDevice) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: SIZE,
        height: SIZE,
        depth: 1,
        format: GRAPH_FORMAT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "mosh-input",
        mip_levels: 1,
    })
}

// A textured moving object on a static background: global-motion compensation
// cannot mistake this for camera translation. The colour changes after reset.
fn pixels(frame: u32, alternate: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((SIZE * SIZE * 8) as usize);
    let offset = 32 + frame * 6;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside = x >= offset && x < offset + 176 && (96..416).contains(&y);
            let p = if inside {
                let u = x - offset;
                let grain = ((u * 17 + y * 29 + u * y) % 31) as f32 / 31.0;
                if alternate {
                    [0.05, 0.3 + grain * 0.6, 0.7, 1.0]
                } else {
                    [0.3 + grain * 0.6, 0.1 + grain * 0.2, 0.1, 1.0]
                }
            } else {
                [0.02, 0.03, 0.04, 1.0]
            };
            for c in p {
                bytes.extend_from_slice(&f16::from_f32(c).to_le_bytes());
            }
        }
    }
    bytes
}

fn diff(a: &[u8], b: &[u8]) -> (f32, f32) {
    let (mut sum, mut max) = (0.0f32, 0.0f32);
    for (a, b) in a.chunks_exact(2).zip(b.chunks_exact(2)) {
        let a = f16::from_le_bytes([a[0], a[1]]).to_f32();
        let b = f16::from_le_bytes([b[0], b[1]]).to_f32();
        assert!(
            a.is_finite() && (0.0..=1.01).contains(&a),
            "unbounded output {a}"
        );
        let d = (a - b).abs();
        sum += d;
        max = max.max(d);
    }
    (sum / (a.len() / 2) as f32, max)
}

fn run(
    device: &GpuDevice,
    cg: &mut PresetRuntime,
    fx: &PresetInstance,
    tex: &GpuTexture,
    frame: i64,
    trigger: u32,
) -> (Vec<u8>, f64, f64) {
    let mut enc = device.create_encoder("mosh-contract");
    let start = std::time::Instant::now();
    let out = {
        let mut gpu = GpuEncoder::new(&mut enc, device);
        cg.run(
            &mut gpu,
            tex,
            std::slice::from_ref(fx),
            &[],
            &context(frame, trigger),
        )
        .expect("mosh output")
        .clone()
    };
    let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
    let profile = enc.commit_and_wait_profiled(device);
    (
        readback_raw_halves(device, &out, SIZE, SIZE),
        cpu_ms,
        profile.total_ms,
    )
}

fn artifact(name: &str, pixels: &[u8]) {
    let Ok(dir) = std::env::var("MOSH_PROBE_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let bytes: Vec<u8> = pixels
        .chunks_exact(2)
        .map(|p| (f16::from_le_bytes([p[0], p[1]]).to_f32().clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    image::RgbaImage::from_raw(SIZE, SIZE, bytes)
        .unwrap()
        .save(std::path::Path::new(&dir).join(format!("{name}.png")))
        .unwrap();
}

#[test]
fn mosh_motion_data_recovery_reset_triggers_and_cost() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    for name in ["MotionMosh", "DataMosh"] {
        let mut fx = make_effect(name, false, &registry);
        let mut cg = build(&device, &registry, &fx);
        let tex = input(&device);
        let (mut cpu, mut gpu) = (Vec::new(), Vec::new());
        let mut last = Vec::new();
        for frame in 0..24 {
            let source = pixels(frame, false);
            device.upload_texture(&tex, &source);
            let (out, c, g) = run(&device, &mut cg, &fx, &tex, frame as i64, 0);
            if frame >= 8 {
                cpu.push(c);
                gpu.push(g);
            }
            if frame == 23 {
                assert!(
                    diff(&out, &source).0 > 0.001,
                    "{name} must visibly retain moving imagery"
                );
                artifact(&format!("{name}-source"), &source);
                artifact(&format!("{name}-retained"), &out);
            }
            last = out;
        }
        let source = pixels(24, true);
        device.upload_texture(&tex, &source);
        set(&mut fx, "recover", 1.0);
        for frame in 24..27 {
            let (out, _, _) = run(&device, &mut cg, &fx, &tex, frame, 0);
            assert!(
                diff(&out, &source).1 < 0.002,
                "{name} recovery must be clean on every held frame"
            );
            artifact(&format!("{name}-recovered"), &out);
        }
        set(&mut fx, "recover", 0.0);
        let (retriggered, _, _) = run(&device, &mut cg, &fx, &tex, 27, 1);
        assert!(
            diff(&retriggered, &source).1 < 0.002,
            "{name} clip recovery"
        );
        assert!(
            diff(&last, &source).0 > 0.01,
            "recovery fixture must differ from retained history"
        );
        cg.clear_state();
        let (reset, _, _) = run(&device, &mut cg, &fx, &tex, 0, 0);
        let mut fresh = build(&device, &registry, &fx);
        let (first, _, _) = run(&device, &mut fresh, &fx, &tex, 0, 0);
        assert!(
            diff(&reset, &first).1 < 0.002,
            "{name} reset must match a fresh instance"
        );
        // Same sequential frames after a reset match a fresh export/playback
        // run, including pending native flow from the previous generation.
        for frame in 1..8 {
            device.upload_texture(&tex, &pixels(frame, true));
            let (reset, _, _) = run(&device, &mut cg, &fx, &tex, frame as i64, 0);
            let (fresh, _, _) = run(&device, &mut fresh, &fx, &tex, frame as i64, 0);
            assert!(
                diff(&reset, &fresh).1 < 0.002,
                "{name} reset replay frame {frame}"
            );
        }
        device.upload_texture(&tex, &source);
        fx.enabled = false;
        assert!(
            PresetRuntime::try_build(
                ChainBuildInputs {
                    effects: std::slice::from_ref(&fx),
                    groups: &[],
                    primitives: &registry,
                    device: &device,
                    pool: None,
                    width: SIZE,
                    height: SIZE,
                    preview_effect: Some(&fx.id),
                },
                Some(&mut cg)
            )
            .is_none()
        );
        fx.enabled = true;
        cg = build(&device, &registry, &fx);
        let (reentry, _, _) = run(&device, &mut cg, &fx, &tex, 0, 0);
        assert!(
            diff(&reentry, &first).1 < 0.002,
            "{name} bypass re-entry is fresh"
        );
        // Zero persistence is clean even with corrupted state and external automation.
        set(&mut fx, "persistence", 0.0);
        let (zero, _, _) = run(&device, &mut cg, &fx, &tex, 1, 0);
        assert!(diff(&zero, &source).1 < 0.002, "{name} zero persistence");
        portrait_probe(&device, &registry, name);
        cpu.sort_by(f64::total_cmp);
        gpu.sort_by(f64::total_cmp);
        eprintln!(
            "[mosh cost] {name} {SIZE}x{SIZE}, 16 steady frames: CPU median {:.3}ms max {:.3}ms; GPU median {:.3}ms max {:.3}ms (readback excluded)",
            cpu[cpu.len() / 2],
            cpu[cpu.len() - 1],
            gpu[gpu.len() / 2],
            gpu[gpu.len() - 1]
        );
    }
}

#[test]
fn mosh_fused_graphs_match_unfused_over_retained_frames() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    for name in ["MotionMosh", "DataMosh"] {
        let plain = make_effect(name, false, &registry);
        let fused = make_effect(name, true, &registry);
        let mut a = build(&device, &registry, &plain);
        let mut b = build(&device, &registry, &fused);
        let tex = input(&device);
        let mut cost = [0.0; 4];
        for frame in 0..12 {
            device.upload_texture(&tex, &pixels(frame, false));
            let (a, ac, ag) = run(&device, &mut a, &plain, &tex, frame as i64, 0);
            let (b, bc, bg) = run(&device, &mut b, &fused, &tex, frame as i64, 0);
            if frame >= 4 {
                for (sum, sample) in cost.iter_mut().zip([ac, ag, bc, bg]) {
                    *sum += sample / 8.0;
                }
            }
            let (mean, max) = diff(&a, &b);
            assert!(
                mean < 0.002 && max < 0.04,
                "{name} frame {frame}: fusion mean={mean} max={max}"
            );
        }
        eprintln!(
            "[mosh comparison] {name} {SIZE}x{SIZE}, 8 paired steady frames: canonical CPU/GPU {:.3}/{:.3}ms; fused CPU/GPU {:.3}/{:.3}ms",
            cost[0], cost[1], cost[2], cost[3]
        );
    }
}

#[test]
fn mosh_clip_variants_change_retained_images_without_clocks() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    for name in ["MotionMosh", "DataMosh"] {
        for mode in [1.0, 2.0] {
            let mut enabled = make_effect(name, false, &registry);
            set(&mut enabled, "trigger_visual", mode);
            let mut disabled = enabled.clone();
            set(&mut disabled, "clip_trigger", 0.0);
            let mut a = build(&device, &registry, &enabled);
            let mut b = build(&device, &registry, &disabled);
            let tex = input(&device);
            let mut difference = 0.0f32;
            for frame in 0..12 {
                device.upload_texture(&tex, &pixels(frame, false));
                let trigger = u32::from(frame >= 10);
                let (on, _, _) = run(&device, &mut a, &enabled, &tex, frame as i64, trigger);
                let (off, _, _) = run(&device, &mut b, &disabled, &tex, frame as i64, trigger);
                if frame < 10 {
                    assert!(
                        diff(&on, &off).1 < 0.002,
                        "{name}: enabling triggers must not change baseline"
                    );
                } else {
                    difference = difference.max(diff(&on, &off).0);
                    artifact(&format!("{name}-clip-mode-{mode}"), &on);
                }
            }
            assert!(
                difference > 0.0001,
                "{name} clip mode {mode} must visibly respond: {difference}"
            );
        }
    }
}

// Optional user-supplied footage participates in the same bounded proof. The
// fixture stays outside the repository; no private reference is embedded.
fn portrait_probe(device: &GpuDevice, registry: &PrimitiveRegistry, name: &'static str) {
    let Ok(path) = std::env::var("MOSH_PROBE_IMAGE") else {
        return;
    };
    let base = image::open(path)
        .unwrap()
        .resize_exact(SIZE, SIZE, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let fx = make_effect(name, true, registry);
    let mut cg = build(device, registry, &fx);
    let tex = input(device);
    let mut frames = Vec::new();
    for frame in 0..24 {
        let mut bytes = Vec::with_capacity((SIZE * SIZE * 8) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let px = base.get_pixel(x.saturating_sub(frame * 3), y);
                for c in px.0 {
                    bytes.extend_from_slice(&f16::from_f32(c as f32 / 255.0).to_le_bytes());
                }
            }
        }
        device.upload_texture(&tex, &bytes);
        let (out, _, _) = run(device, &mut cg, &fx, &tex, frame as i64, 0);
        if frame == 23 {
            artifact(&format!("{name}-portrait-source"), &bytes);
            artifact(&format!("{name}-portrait-mosh"), &out);
        }
        if std::env::var_os("MOSH_PROBE_DIR").is_some() {
            let rgba = out
                .chunks_exact(2)
                .map(|p| (f16::from_le_bytes([p[0], p[1]]).to_f32().clamp(0.0, 1.0) * 255.0) as u8)
                .collect();
            frames.push(image::Frame::from_parts(
                image::RgbaImage::from_raw(SIZE, SIZE, rgba).unwrap(),
                0,
                0,
                image::Delay::from_numer_denom_ms(50, 1),
            ));
        }
    }
    if let Ok(dir) = std::env::var("MOSH_PROBE_DIR") {
        let file =
            std::fs::File::create(std::path::Path::new(&dir).join(format!("{name}-portrait.gif")))
                .unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        encoder.encode_frames(frames).unwrap();
    }
}
