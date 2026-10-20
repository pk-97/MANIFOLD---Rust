//! CINEMATIC_SCENE_TAIL P1 gate proofs.
//!
//! I1 — the assembled import tail preserves the sharp static fixture at the
//! default f/32 lens. DoF defaults On; its subpixel circle of confusion must
//! preserve full-resolution color and transparent silhouette coverage. The
//! static scene's zero velocity also collapses motion-blur taps at the default
//! shutter angle of 180 degrees. Compare against the same graph with its tail
//! surgically stripped, after both variants have loaded and converged.
//!
//! The strip is surgical: flatten the assembled def, drop the `dof` group node
//! (its inner coc/coc_dilate/bokeh atoms ride with it) and the top-level
//! `motion_blur` node plus every wire touching either, then rewire `ao.out ->
//! final.in` — the exact pre-P1 spine. Everything else (camera, lens, ao,
//! render_scene) is untouched, so a difference can only come from the tail.

use manifold_core::flatten::flatten_groups;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;


fn ctx(frame_count: i64) -> PresetContext {
    PresetContext {
        time: frame_count as f64 / 60.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: manifold_node_engine::testkit::gpu_harness::PARITY_WIDTH,
        height: manifold_node_engine::testkit::gpu_harness::PARITY_HEIGHT,
        output_width: manifold_node_engine::testkit::gpu_harness::PARITY_WIDTH,
        output_height: manifold_node_engine::testkit::gpu_harness::PARITY_HEIGHT,
        aspect: manifold_node_engine::testkit::gpu_harness::PARITY_WIDTH as f32 / manifold_node_engine::testkit::gpu_harness::PARITY_HEIGHT as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn build_variant(
    h: &manifold_node_engine::testkit::gpu_harness::ParityHarness,
    strip_tail: bool,
) -> (PresetRuntime, manifold_core::params::ParamManifest) {
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/DamagedHelmet.glb");
    assert!(glb.exists(), "I1 fixture missing: {glb:?}");
    let (def, report) = assemble_import_graph(&glb).expect("import must succeed");
    eprintln!("[cinematic-tail-I1] import report: {report:?}");

    let mut flat = flatten_groups(&def).expect("flatten_groups must succeed");
    debug_assert!(
        flat.nodes.iter().any(|n| n.type_id == "node.coc_from_depth"),
        "assembled import graph must carry the dof group's coc atom"
    );

    if strip_tail {
        // Drop the dof chain's atoms and motion_blur by TYPE, plus every wire
        // touching any of them. Flatten inlines the dof group's body (handles
        // prefixed `dof/…`, boundary nodes folded away), so there is no "dof"
        // node at this level — the atoms themselves are the executable graph.
        const TAIL_TYPES: [&str; 4] = [
            "node.coc_from_depth",
            "node.coc_dilate",
            "node.bokeh_gather",
            "node.motion_blur",
        ];
        let keep_nodes: Vec<u32> = flat
            .nodes
            .iter()
            .filter(|n| !TAIL_TYPES.contains(&n.type_id.as_str()))
            .map(|n| n.id)
            .collect();
        flat.nodes.retain(|n| keep_nodes.contains(&n.id));
        flat.wires.retain(|w| {
            keep_nodes.contains(&w.from_node) && keep_nodes.contains(&w.to_node)
        });
        // Rebuild the pre-P1 spine: render_scene → ao → final. The ao group's
        // outer node id survived (it's a real atom post-flatten — masked_mix,
        // the group's output side). Re-anchor its `out` to final.
        let ao_out_id = flat
            .nodes
            .iter()
            .find(|n| n.node_id == "ao/mask_mix")
            .map(|n| n.id)
            .or_else(|| flat.nodes.iter().find(|n| n.type_id == "node.masked_mix").map(|n| n.id))
            .expect("stripped graph keeps the ao group's final mix node");
        let final_id = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "system.final_output")
            .expect("final output node")
            .id;
        flat.wires.push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: ao_out_id,
            from_port: "out".to_string(),
            to_node: final_id,
            to_port: "in".to_string(),
        });
    }

    let registry = PrimitiveRegistry::with_builtin();
    let manifest = manifold_core::params::ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .map(|m| {
                m.params
                    .iter()
                    .cloned()
                    .map(manifold_core::params::Param::bundled)
                    .collect()
            })
            .unwrap_or_default(),
    );
    let runtime = PresetRuntime::from_def_with_device(
        flat,
        &registry,
        std::sync::Arc::clone(&h.device),
        manifold_node_engine::testkit::gpu_harness::PARITY_WIDTH,
        manifold_node_engine::testkit::gpu_harness::PARITY_HEIGHT,
        GpuTextureFormat::Rgba16Float,
        Some(&manifest),
    )
    .expect("variant must build through PresetRuntime");
    (runtime, manifest)
}

/// Render one frame into a fresh target and read back the exact bytes.
fn render_one(
    h: &manifold_node_engine::testkit::gpu_harness::ParityHarness,
    runtime: &mut PresetRuntime,
    manifest: &manifold_core::params::ParamManifest,
    f: i64,
) -> Vec<u8> {
    let target = h.make_target("cinematic-tail-I1");
    let c = ctx(f);
    manifold_node_engine::testkit::gpu_harness::retry_on_gpu_commit_error(|| {
        let mut enc = h.device.create_encoder("cinematic-tail-I1-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &c, manifest);
        }
        enc.commit_and_wait_completed();
    });
    h.readback(&target.texture)
}

/// I1 — neutral lens is a bit-clean pass-through through the injected tail:
/// the import-assembled graph (tail present) and the pre-P1 SSAO-only spine
/// (tail stripped) produce byte-identical `final` at the default lens.
#[test]
fn import_tail_is_byte_clean_passthrough_at_neutral_lens() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let (mut with_tail, manifest_tail) = build_variant(h, false);
    let (mut stripped, manifest_stripped) = build_variant(h, true);

    // Mesh parsing, texture decode and publication are asynchronous, and a
    // black pending frame is not evidence about the neutral lens. Pairs
    // rendered while either variant has background work in flight don't
    // count; once both have settled, require visible output that holds
    // byte-stable across two pairs.
    const SETTLED_PAIR_BUDGET: u32 = 64;
    let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("cinematic-tail-I1");
    let visible = |bytes: &[u8]| bytes.chunks_exact(8).any(|px| {
        half::f16::from_bits(u16::from_le_bytes([px[0], px[1]])).to_f32() > 0.03
    });
    let changed = |x: &[u8], y: &[u8]| x.chunks_exact(8).zip(y.chunks_exact(8)).filter(|(p, q)| p != q).count();
    let mut previous: Option<(Vec<u8>, Vec<u8>)> = None;
    let mut settled_pairs = 0;
    let (a, b) = loop {
        let a = render_one(h, &mut with_tail, &manifest_tail, 0);
        let b = render_one(h, &mut stripped, &manifest_stripped, 0);
        if wait.pending(&with_tail) || wait.pending(&stripped) {
            previous = None;
            continue;
        }
        if visible(&a) && visible(&b)
            && previous.as_ref().is_some_and(|(pa, pb)| *pa == a && *pb == b)
        {
            break (a, b);
        }
        settled_pairs += 1;
        assert!(settled_pairs < SETTLED_PAIR_BUDGET,
            "import variants settled but did not converge in {settled_pairs} pairs: visible={}/{}, \
             pixels changed since the last pair={:?}",
            visible(&a), visible(&b),
            previous.as_ref().map(|(pa, pb)| (changed(pa, &a), changed(pb, &b))));
        previous = Some((a, b));
    };

    let non_black = a
        .as_slice()
        .chunks(8)
        .filter(|px| {
            let v = u16::from_le_bytes([px[0], px[1]]);
            half::f16::from_bits(v).to_f32() > 0.03
        })
        .count();
    assert!(
        non_black > 0,
        "I1 fixture must render a non-black frame (choke on a converged frame, not an empty one)"
    );

    assert_eq!(
        a.len(),
        b.len(),
        "readbacks must be the same size (same dims, same format)"
    );
    let differing = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
    assert_eq!(
        differing,
        0,
        "import tail is NOT a bit-clean pass-through at default lens: {differing} bytes differ \
         between the with-tail and stripped spines (neutral camera DoF must preserve focused \
         pixels and coverage; the static scene's zero velocity must collapse every motion-blur tap even \
         at the P4 shutter=180 default)"
    );
    eprintln!(
        "[cinematic-tail-I1] PASS: {} bytes, {} differ (bit-clean pass-through at f/32, shutter=180, zero velocity)",
        a.len(),
        differing
    );
}
