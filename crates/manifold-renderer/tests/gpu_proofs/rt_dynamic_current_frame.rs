//! First-frame current-geometry dispatch proof for SCENE_MODIFIER_RT_DESIGN P5.
//!
//! A tiny generated mesh is rendered through the production `PresetRuntime`
//! path, and the test requires both a complete frame status and a real RT
//! capture produced by `render_scene`'s trace branch. The pinned current
//! `MeshVertex` output also supplies an analytical centroid-ray witness, so
//! the proof cannot pass on RT dispatch alone with stale or degenerate data.

use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;

use manifold_nodes_scene::testkit::gpu_harness as harness;

use manifold_renderer::testkit::rt_dynamic::{modifier_combo_scene, scene_json};

/// Read a non-degenerate triangle from the executor's pinned Array dump and
/// construct an analytical ray through its centroid. This is derived from
/// the bytes the current frame produced: a non-black RT capture alone cannot
/// distinguish a stale source mesh from the geometry dispatched by the
/// modifier chain.
fn current_geometry_ray(runtime: &PresetRuntime) -> (String, [f32; 3], [f32; 3]) {
    let dumps = runtime.dump_arrays_all();
    for dump in dumps.iter().filter(|dump| {
        dump.item_size as usize == std::mem::size_of::<MeshVertex>()
            && dump.buffer.mapped_ptr().is_some()
    }) {
        let count = (dump.buffer.size as usize / std::mem::size_of::<MeshVertex>()) / 3 * 3;
        let ptr = dump
            .buffer
            .mapped_ptr()
            .expect("dumped mesh must be mapped") as *const MeshVertex;
        for tri in (0..count).step_by(3) {
            let vertices = unsafe {
                [
                    ptr.add(tri).read_unaligned(),
                    ptr.add(tri + 1).read_unaligned(),
                    ptr.add(tri + 2).read_unaligned(),
                ]
            };
            let e1 = [
                vertices[1].position[0] - vertices[0].position[0],
                vertices[1].position[1] - vertices[0].position[1],
                vertices[1].position[2] - vertices[0].position[2],
            ];
            let e2 = [
                vertices[2].position[0] - vertices[0].position[0],
                vertices[2].position[1] - vertices[0].position[1],
                vertices[2].position[2] - vertices[0].position[2],
            ];
            let normal = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let norm =
                (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            if !norm.is_finite() || norm < 1e-6 {
                continue;
            }
            let centroid = [
                (vertices[0].position[0] + vertices[1].position[0] + vertices[2].position[0]) / 3.0,
                (vertices[0].position[1] + vertices[1].position[1] + vertices[2].position[1]) / 3.0,
                (vertices[0].position[2] + vertices[1].position[2] + vertices[2].position[2]) / 3.0,
            ];
            let unit = [normal[0] / norm, normal[1] / norm, normal[2] / norm];
            return (
                format!("{}:{}:{}", dump.name, dump.type_id, tri),
                [
                    centroid[0] + unit[0] * 4.0,
                    centroid[1] + unit[1] * 4.0,
                    centroid[2] + unit[2] * 4.0,
                ],
                [-unit[0], -unit[1], -unit[2]],
            );
        }
    }
    panic!("current frame did not publish a mapped, non-degenerate MeshVertex triangle");
}

#[test]
fn rt_dynamic_current_frame_first_frame_dispatches() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        scene_json(),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("current-frame RT scene graph must build");
    runtime.set_dump_all(true);
    let target = h.make_target("rt-dynamic-current-frame");
    let ctx = PresetContext {
        time: 0.1,
        beat: 0.2,
        dt: 1.0 / 60.0,
        width: h.width,
        height: h.height,
        output_width: h.width,
        output_height: h.height,
        aspect: h.width as f32 / h.height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut status = None;
    let captures = harness::capture_rt_channels(|| {
        let mut enc = h.device.create_encoder("rt-dynamic-current-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
            status = Some(gpu.frame_status());
        }
        enc.commit_and_wait_completed();
    });

    assert_eq!(
        status,
        Some(FrameRenderStatus::Complete),
        "the first current-frame RT update must produce a valid frame"
    );
    assert!(
        !captures.is_empty(),
        "the first evaluated frame must produce real RT captures"
    );
    let (triangle, origin, direction) = current_geometry_ray(&runtime);
    assert!(origin.iter().all(|value| value.is_finite()));
    assert!(direction.iter().all(|value| value.is_finite()));
    println!(
        "current-frame geometry ray witness: triangle={triangle} origin={origin:?} direction={direction:?}"
    );
}

#[test]
fn rt_dynamic_current_frame_stock_modifier_combo_accepts_rt_and_dispatches() {
    use manifold_core::NodeId;
    use manifold_node_engine::parameters::ParamValue;
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let owner = modifier_combo_scene();
    assert!(
        manifold_core::scene_modifier_preset::scene_modifier_parameter_lock_reason(
            &owner,
            &NodeId::new("scan_render"),
            "rt_enabled",
        )
        .is_none(),
        "the editor must allow this stock vertex-modifier stack's RT control"
    );
    let mut runtime = PresetRuntime::from_def_with_device(
        owner,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("Vortex Fragments + Ordered Recon must load with RT enabled");
    runtime.set_dump_all(true);
    let scene = runtime
        .graph
        .instance_by_node_id(&NodeId::new("scan_render"))
        .unwrap();
    let target = h.make_target("rt-modifier-combo");
    for (frame, enabled) in [true, false, true].into_iter().enumerate() {
        runtime
            .graph
            .set_param(scene, "rt_enabled", ParamValue::Bool(enabled))
            .expect("the prepared modifier runtime must allow live RT toggles");
        let ctx = PresetContext {
            time: frame as f64 / 24.0,
            beat: frame as f64 / 12.0,
            dt: 1.0 / 24.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut status = None;
        let mut rt_updates = None;
        let mut rt_history_resets = 0;
        let mut rt_dispatches = 0;
        let captures = harness::capture_rt_channels(|| {
            let mut enc = h.device.create_encoder("rt-modifier-combo");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
                runtime.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
                status = Some(gpu.frame_status());
                rt_updates = Some(gpu.rt_updates);
                rt_history_resets = gpu.rt_history_resets;
                rt_dispatches = gpu.rt_dispatches;
            }
            enc.commit_and_wait_completed();
        });
        assert_eq!(
            status,
            Some(FrameRenderStatus::Complete),
            "combo frame {frame}"
        );
        assert_eq!(
            !captures.is_empty(),
            enabled,
            "RT dispatch must follow the live toggle on frame {frame}"
        );
        let updates = rt_updates.expect("frame must expose RT update counters");
        if enabled {
            assert_eq!(
                rt_dispatches, 1,
                "enabled combo frame {frame} must trace exactly once"
            );
        } else {
            assert_eq!(
                rt_dispatches, 0,
                "disabled combo frame {frame} must not trace"
            );
            assert_eq!(
                updates,
                Default::default(),
                "disabled combo frame {frame} must do no RT maintenance"
            );
            assert_eq!(
                rt_history_resets, 0,
                "disabled combo frame {frame} must preserve RT history"
            );
        }
        if frame == 0 && enabled {
            assert!(
                updates.tlas_builds + updates.blas_builds > 0,
                "first combo frame must build its resident AS"
            );
        }
        if frame == 2 && enabled {
            assert!(
                updates.blas_builds
                    + updates.blas_refits
                    + updates.tlas_builds
                    + updates.tlas_refits
                    > 0,
                "re-enabled changed combo frame must update its resident AS"
            );
            assert!(
                rt_history_resets > 0,
                "changed combo geometry must reset RT history"
            );
        }
        if enabled {
            let (triangle, origin, direction) = current_geometry_ray(&runtime);
            assert!(origin.iter().all(|value| value.is_finite()));
            assert!(direction.iter().all(|value| value.is_finite()));
            println!("modifier combo frame {frame} geometry ray witness: triangle={triangle}");
        }
    }
}

#[test]
fn rt_dynamic_current_frame_warmup_toggle_deform_and_idle() {
    use manifold_core::NodeId;
    use manifold_node_engine::parameters::ParamValue;
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let mut graph: serde_json::Value = serde_json::from_str(scene_json()).unwrap();
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id":30,"nodeId":"wave","typeId":"node.normal_wave_mesh",
            "params":{"amplitude":{"type":"Float","value":0.2},"phase":{"type":"Float","value":0.0}}
        }));
    for wire in graph["wires"].as_array_mut().unwrap() {
        if wire["fromNode"] == 2 && wire["toNode"] == 4 {
            wire["toNode"] = 30.into();
            wire["toPort"] = "in".into();
        }
    }
    graph["wires"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"fromNode":30,"fromPort":"out","toNode":4,"toPort":"vertices"}));
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &graph.to_string(),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap();
    let scene = runtime
        .graph
        .instance_by_node_id(&NodeId::new("scene"))
        .unwrap();
    let wave = runtime
        .graph
        .instance_by_node_id(&NodeId::new("wave"))
        .unwrap();
    let target = h.make_target("rt-warmup-toggle");
    let mut prepared_allocations = [0; 3];
    for (frame, (enabled, preparing, phase)) in [
        (false, true, 0.0),
        (true, false, 0.0),
        (true, false, 0.4),
        (true, false, 0.4),
    ]
    .into_iter()
    .enumerate()
    {
        runtime
            .graph
            .set_param(scene, "rt_enabled", ParamValue::Bool(enabled))
            .unwrap();
        runtime
            .graph
            .set_param(wave, "phase", ParamValue::Float(phase))
            .unwrap();
        let context = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 0.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut encoder = h.device.create_encoder("warmup toggle deformation");
        let (status, updates, resets, dispatches) = {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, &h.device);
            gpu.preparing = preparing;
            runtime.render(&mut gpu, &target.texture, &context, &Default::default());
            (
                gpu.frame_status(),
                gpu.rt_updates,
                gpu.rt_history_resets,
                gpu.rt_dispatches,
            )
        };
        encoder.commit_and_wait_completed();
        assert_eq!(status, FrameRenderStatus::Complete, "frame {frame}");
        assert_eq!(dispatches, u32::from(enabled), "frame {frame}");
        match frame {
            0 => {
                assert_eq!((updates.blas_builds, updates.tlas_builds), (1, 1));
                prepared_allocations = h.device.allocation_counts();
            }
            1 => {
                assert_eq!(
                    (
                        updates.blas_builds,
                        updates.blas_refits,
                        updates.tlas_refits
                    ),
                    (0, 0, 0),
                    "RT toggle reuses the prepared unchanged mesh"
                );
                let current = h.device.allocation_counts();
                assert_eq!(
                    (current[0], current[2]),
                    (prepared_allocations[0], prepared_allocations[2]),
                    "toggle allocates no buffers/AS"
                );
            }
            2 => {
                assert_eq!(
                    (
                        updates.blas_builds,
                        updates.blas_refits,
                        updates.tlas_refits
                    ),
                    (0, 1, 1)
                );
                assert_eq!(resets, 1);
            }
            3 => {
                assert_eq!(updates, Default::default());
                assert_eq!(resets, 0);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn rt_dynamic_history_reset_and_resume() {
    use manifold_core::NodeId;
    use manifold_node_engine::parameters::ParamValue;

    const SENTINEL: f32 = 123.0;
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        scene_json(),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("history proof scene graph must build");
    let mut reference = PresetRuntime::from_json_str_with_device(
        scene_json(),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("aligned history reference must build");
    let material = runtime
        .graph
        .instance_by_node_id(&NodeId::new("material"))
        .expect("history proof material");
    let target = h.make_target("rt-dynamic-history-reset");

    let render_frame = |runtime: &mut PresetRuntime, frame: i64, capture_geometry: bool| {
        let context = PresetContext {
            time: frame as f64 / 60.0,
            beat: frame as f64 / 30.0,
            dt: if frame == 0 { 0.0 } else { 1.0 / 60.0 },
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut status = None;
        let mut resets = 0;
        let captures = harness::capture_rt_channels(|| {
            let mut encoder = h.device.create_encoder("rt-dynamic-history-reset");
            {
                let mut gpu = RendererGpuEncoder::new(&mut encoder, &h.device);
                gpu.capture_rt_geometry = capture_geometry;
                runtime.render(
                    &mut gpu,
                    &target.texture,
                    &context,
                    &manifold_core::params::ParamManifest::default(),
                );
                status = Some(gpu.frame_status());
                resets = gpu.rt_history_resets;
            }
            encoder.commit_and_wait_completed();
        });
        (status, resets, captures)
    };

    let (status, _, _) = render_frame(&mut runtime, 0, true);
    assert_eq!(status, Some(FrameRenderStatus::Complete));
    // The first maintenance pass creates the resident history pair after the
    // geometry probe is captured, so refresh the probe on one settled frame.
    let (status, _, _) = render_frame(&mut runtime, 1, true);
    assert_eq!(status, Some(FrameRenderStatus::Complete));
    render_frame(&mut reference, 0, false);
    render_frame(&mut reference, 1, false);
    let reference_material = reference
        .graph
        .instance_by_node_id(&NodeId::new("material"))
        .unwrap();
    reference
        .graph
        .set_param(reference_material, "color_r", ParamValue::Float(0.15))
        .unwrap();
    let (_, reference_resets, fresh) = render_frame(&mut reference, 2, false);
    assert!(reference_resets > 0);
    manifold_nodes_scene::node_graph::primitives::render_scene::rt_proof::rt_probe_scene(&runtime)
        .expect("settled RT frame must publish resident histories")
        .inject_history_sentinel(&h.device, f64::from(SENTINEL));

    runtime
        .graph
        .set_param(material, "color_r", ParamValue::Float(0.15))
        .expect("material change must be accepted");
    let (status, resets, changed) = render_frame(&mut runtime, 2, true);
    assert_eq!(status, Some(FrameRenderStatus::Complete));
    assert!(
        resets > 0,
        "changed material must request a shared history reset"
    );
    let changed_irr = changed
        .iter()
        .find(|capture| capture.label == "irr_accum")
        .expect("changed frame must publish accumulated irradiance");
    let changed_pixels = harness::read_rt_channel(&h.device, changed_irr);
    assert!(changed_pixels.iter().all(|value| value.is_finite()));
    assert!(
        changed_pixels
            .iter()
            .all(|value| (*value - SENTINEL).abs() > 1.0),
        "changed frame retained the injected temporal sentinel"
    );
    // Both runtimes have the same frame/RNG sequence and appearance reset.
    // Any surviving fraction of the poisoned history must differ from this
    // unpoisoned fresh-history reference, not merely from the sentinel itself.
    for label in ["irr_accum", "refl_history_write", "mask", "sv_hold"] {
        let actual = changed
            .iter()
            .find(|capture| capture.label == label)
            .unwrap();
        let expected = fresh.iter().find(|capture| capture.label == label).unwrap();
        let actual = harness::read_rt_channel(&h.device, actual);
        let expected = harness::read_rt_channel(&h.device, expected);
        assert_eq!(actual.len(), expected.len());
        for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                (actual - expected).abs() <= 2e-3,
                "{label}[{index}]: poisoned {actual}, fresh {expected}"
            );
        }
    }
    let changed_moments = changed
        .iter()
        .find(|capture| capture.label == "moments")
        .expect("changed frame must publish temporal moments");
    let changed_counts = read_rgba32_channel(&h.device, changed_moments);
    assert!(
        changed_counts
            .chunks_exact(4)
            .all(|pixel| pixel[3].is_finite() && pixel[3] <= 1.1)
    );

    let (status, resets, resumed) = render_frame(&mut runtime, 3, false);
    assert_eq!(status, Some(FrameRenderStatus::Complete));
    assert_eq!(resets, 0, "unchanged frame must resume accumulation");
    let resumed_moments = resumed
        .iter()
        .find(|capture| capture.label == "moments")
        .expect("resumed frame must publish temporal moments");
    let resumed_counts = read_rgba32_channel(&h.device, resumed_moments);
    assert!(
        resumed_counts
            .chunks_exact(4)
            .any(|pixel| pixel[3].is_finite() && pixel[3] > 1.1),
        "unchanged frame did not resume a resident temporal history"
    );
}

fn read_rgba32_channel(
    device: &manifold_gpu::GpuDevice,
    capture: &manifold_nodes_scene::node_graph::primitives::render_scene::RtCaptureSlot,
) -> Vec<f32> {
    assert_eq!(capture.tex.format, GpuTextureFormat::Rgba32Float);
    let bytes_per_row = capture.w * 16;
    let total = u64::from(capture.h * bytes_per_row);
    let buffer = device.create_buffer_shared(total);
    let mut encoder = device.create_encoder("rt-history-proof-readback");
    encoder.copy_texture_to_buffer(&capture.tex, &buffer, capture.w, capture.h, bytes_per_row);
    encoder.commit_and_wait_completed();
    let ptr = buffer
        .mapped_ptr()
        .expect("history proof readback buffer must be mapped");
    let raw = unsafe { std::slice::from_raw_parts(ptr, total as usize) };
    raw.chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}
