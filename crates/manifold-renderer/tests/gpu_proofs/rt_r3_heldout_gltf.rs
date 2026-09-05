//! `docs/RAYTRACING_DESIGN.md` section 9.6 Textured roughness (R3) gate — held-out
//! real-asset proof. `DamagedHelmet.glb` carries a real metallic-roughness
//! map the R3 kernel path was never tuned against (no kernel constant in
//! this phase is retunable per-asset — if this fails, the fix is a
//! diagnosis, not a fit). Same real-import-path pattern as
//! `rt_bug326_fix_gate.rs` (`assemble_import_graph` -> `PresetRuntime`),
//! not the raw `RtObjectGeometry` harness.
//!
//! No test-visible accessor exposes `RtNormalSource::mr_tex_index` through
//! the real scene-graph render path (that GPU buffer is internal to
//! `render_scene`'s RT machinery) — adding one is out of this phase's
//! deliverables. Deviation from the brief's literal "assert
//! `RtNormalSource::mr_tex_index` is bound" ask, named here: assertion (1)
//! is a STRUCTURAL proxy — the imported graph actually wires an `mr_map`
//! port into the helmet's `node.scene_object` bind node (glTF import
//! fidelity's own contract), which is what feeds `RtObjectGeometry::
//! mr_texture` at `render_scene.rs`'s RT-object construction site.
//! Assertion (2) is the real functional proof: with that SAME wire present
//! vs. surgically removed (forcing `mr_texture: None` for that object),
//! the traced reflection output differs numerically by more than a stated
//! floor — proving the value actually reaches the kernel, not just the
//! graph.

use manifold_core::flatten::flatten_groups;
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const W: u32 = 512;
const H: u32 = 512;

fn ctx(frame_count: i64) -> PresetContext {
    PresetContext {
        time: frame_count as f64 / 60.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
        gpu_signal_committed: 0,
        gpu_signaled: 0,
    }
}

fn frame(
    runtime: &mut PresetRuntime,
    h: &harness::ParityHarness,
    target: &manifold_gpu::GpuTexture,
    f: i64,
    params: &manifold_core::params::ParamManifest,
) {
    let c = ctx(f);
    // A commit can be an InnocentVictim of a shared-GPU contention transient
    // (BUG-m0c9); re-rendering the same idempotent frame absorbs it. A real
    // wedge still panics after the single retry.
    harness::retry_on_gpu_commit_error(|| {
        let mut enc = h.device.create_encoder("r3-heldout-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, target, &c, params);
        }
        enc.commit_and_wait_completed();
    });
}

fn non_black_fraction_rgbf32(px: &[f32]) -> f64 {
    let n = px.len() / 4;
    if n == 0 {
        return 0.0;
    }
    let mut non_black = 0usize;
    for i in 0..n {
        if px[i * 4] > 0.03 || px[i * 4 + 1] > 0.03 || px[i * 4 + 2] > 0.03 {
            non_black += 1;
        }
    }
    non_black as f64 / n as f64
}

fn readback_rgba_f32(device: &manifold_gpu::GpuDevice, texture: &manifold_gpu::GpuTexture) -> Vec<f32> {
    let bytes_per_row = W * 8;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("r3-heldout-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (W * H * 4) as usize) };
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for &v in halves {
        out.push(half::f16::from_bits(v).to_f32());
    }
    out
}

fn make_512_target(device: &GpuDevice, label: &str) -> manifold_gpu::GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label,
        mip_levels: 1,
    })
}

/// Import the held-out asset, enable `rt_enabled`/`rt_reflections`, and
/// (when `strip_mr_map` is set) surgically delete every wire feeding a
/// `node.scene_object` bind node's `mr_map` port — forcing
/// `RtObjectGeometry::mr_texture: None` for every object at the RT
/// construction site (`render_scene.rs`), the "factors-only" baseline.
/// `flatten_groups` first (a documented no-op when the def has no node
/// groups) so the mutation always sees the real, executable wire list
/// regardless of whether the importer happened to group this asset.
fn build_variant(
    h: &harness::ParityHarness,
    strip_mr_map: bool,
) -> (PresetRuntime, manifold_gpu::GpuTexture, manifold_core::params::ParamManifest, bool) {
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/DamagedHelmet.glb");
    assert!(glb.exists(), "held-out fixture missing: {glb:?}");
    let (def, report) = assemble_import_graph(&glb).expect("import must succeed");
    eprintln!("[r3-heldout] import report: {report:?}");

    let mut flat = flatten_groups(&def).expect("flatten_groups must succeed (no-op if already flat)");

    // RT comes on through the outer-card manifest, never the `render_scene`
    // node params — see `harness::import_rt_manifest` for why the node-param
    // route is a silent no-op (it is what made this gate vacuous).
    let params = harness::import_rt_manifest(&flat, true, true);

    // Structural proxy for "the imported object's MR map is bound" (see
    // module doc's named deviation): at least one scene_object bind node
    // has an `mr_map` wire BEFORE any stripping.
    let scene_object_ids: Vec<_> = flat
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.scene_object")
        .map(|n| n.id)
        .collect();
    assert!(!scene_object_ids.is_empty(), "imported def has no node.scene_object bind nodes");
    let mr_wire_count = flat
        .wires
        .iter()
        .filter(|w| scene_object_ids.contains(&w.to_node) && w.to_port == "mr_map")
        .count();
    assert!(
        mr_wire_count > 0,
        "DamagedHelmet.glb's import wired no `mr_map` port — held-out asset assumption violated \
         (this asset is expected to carry a real glTF metallic-roughness map)"
    );

    if strip_mr_map {
        flat.wires
            .retain(|w| !(scene_object_ids.contains(&w.to_node) && w.to_port == "mr_map"));
    }

    let registry = PrimitiveRegistry::with_builtin();
    let runtime = PresetRuntime::from_def_with_device(
        flat,
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("imported def must build a runtime");

    let target = make_512_target(&h.device, if strip_mr_map { "r3-heldout-stripped" } else { "r3-heldout-bound" });
    (runtime, target, params, mr_wire_count > 0)
}

/// Poll (same discipline as `rt_bug326_fix_gate.rs`: the async accel build
/// and rerun-suppression window makes a fixed frame count flaky) until the
/// render is lit relative to an rt=0 baseline fraction, returning the
/// readback at that point.
fn render_until_lit(
    h: &harness::ParityHarness,
    runtime: &mut PresetRuntime,
    target: &manifold_gpu::GpuTexture,
    params: &manifold_core::params::ParamManifest,
    baseline_frac: f64,
) -> Vec<f32> {
    let threshold = 0.20 * baseline_frac;
    let mut best = vec![0.0f32; (W * H * 4) as usize];
    let mut best_frac = 0.0f64;
    for f in 0..600 {
        frame(runtime, h, target, f, params);
        if f >= 84 && f % 5 == 4 {
            let px = readback_rgba_f32(&h.device, target);
            let frac = non_black_fraction_rgbf32(&px);
            if frac > best_frac {
                best_frac = frac;
                best = px;
            }
            if frac >= threshold {
                break;
            }
        }
    }
    eprintln!("[r3-heldout] converged non-black fraction={best_frac:.4} (threshold={threshold:.4})");
    best
}

/// Held-out gate: MR-bound vs. MR-forcibly-unbound reflections must differ
/// numerically. No kernel constant here is tuned against this asset — a
/// failure means diagnose the seam (binding/plumbing), never retune a
/// threshold to make it pass.
#[test]
fn heldout_helmet_mr_map_changes_traced_reflection() {
    let h = harness::shared();

    // rt=0 baseline (both variants poll against this SAME lit-fraction floor).
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/DamagedHelmet.glb");
    let (def0, _) = assemble_import_graph(&glb).expect("import must succeed");
    let registry = PrimitiveRegistry::with_builtin();
    let mut rt_off = PresetRuntime::from_def_with_device(
        def0,
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("baseline def must build a runtime");
    let tex_off = make_512_target(&h.device, "r3-heldout-baseline");
    // Bounded retry: this fixture shares `harness::shared()`'s one resident
    // device with every other test in the gpu-proofs binary — a pure-raster
    // baseline (no RT, no async accel dependency at all) reading back
    // all-black after the first attempt has been observed only under heavy
    // cumulative GPU load from the ~50 OTHER tests that already ran in this
    // process (reproduces 0/3 in isolation) — extra frames + one retry
    // absorbs that transient without weakening the check itself (still a
    // hard failure if it never lights up).
    let rt_off_params = manifold_core::params::ParamManifest::default();
    let mut baseline_frac = 0.0f64;
    for attempt in 0..3 {
        for f in 0..(90 + attempt * 60) {
            frame(&mut rt_off, h, &tex_off, f, &rt_off_params);
        }
        baseline_frac = non_black_fraction_rgbf32(&readback_rgba_f32(&h.device, &tex_off));
        if baseline_frac > 0.01 {
            break;
        }
        eprintln!("[r3-heldout] baseline attempt {attempt} read back black (frac={baseline_frac:.4}), retrying");
    }
    // Sanity, same discipline as `rt_r1_reflection.rs`'s vacuous-proofing
    // check: a baseline of exactly 0 makes `render_until_lit`'s threshold
    // 0 too, which "converges" on the very first poll without ever
    // confirming real content — turn that into a loud, correctly-attributed
    // failure (raster baseline render/harness issue) instead of a silently
    // vacuous pass or fail downstream.
    assert!(
        baseline_frac > 0.01,
        "rt=0 raster baseline for DamagedHelmet.glb rendered ~black (frac={baseline_frac:.4}) after \
         retries — a harness/import issue unrelated to R3, not a real reflection-lobe result"
    );

    let (mut bound_runtime, bound_target, bound_params, mr_was_wired) = build_variant(h, false);
    assert!(mr_was_wired, "structural proxy failed before any rendering — see assertion inside build_variant");
    let bound_px = render_until_lit(h, &mut bound_runtime, &bound_target, &bound_params, baseline_frac);
    // The whole gate is about the RT kernel's reflection lobe, so prove the
    // kernel ran before believing any number below.
    harness::assert_rt_dispatched(
        || frame(&mut bound_runtime, h, &bound_target, 601, &bound_params),
        "r3 held-out MR-bound variant",
    );

    let (mut stripped_runtime, stripped_target, stripped_params, _) = build_variant(h, true);
    let stripped_px =
        render_until_lit(h, &mut stripped_runtime, &stripped_target, &stripped_params, baseline_frac);

    let n = bound_px.len() / 4;
    let mut differing = 0usize;
    let mut max_abs_diff = 0.0f32;
    for i in 0..n {
        let mut px_diff = 0.0f32;
        for c in 0..3 {
            let d = (bound_px[i * 4 + c] - stripped_px[i * 4 + c]).abs();
            px_diff = px_diff.max(d);
        }
        max_abs_diff = max_abs_diff.max(px_diff);
        if px_diff > 0.02 {
            differing += 1;
        }
    }
    let differing_fraction = differing as f64 / n as f64;
    eprintln!(
        "[r3-heldout] differing_fraction={differing_fraction:.5} max_abs_diff={max_abs_diff:.4} \
         (bound vs. MR-map-stripped, same rt+reflections config)"
    );

    const MIN_DIFFERING_FRACTION: f64 = 0.0005; // >= ~130 px at 512x512 — reflection lobe is a minority of the frame
    assert!(
        differing_fraction >= MIN_DIFFERING_FRACTION,
        "R3: binding DamagedHelmet's real MR map made no measurable difference to the traced \
         reflection ({differing_fraction:.5} < {MIN_DIFFERING_FRACTION}) — the per-texel roughness \
         path is not reaching the kernel for this asset"
    );
}
