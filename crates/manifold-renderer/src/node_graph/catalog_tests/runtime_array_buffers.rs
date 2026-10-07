//! Array-buffer allocation, aliasing, and live resize regressions.
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::runtime::*;
use crate::node_graph::*;
use manifold_node_engine::persistence::PrimitiveRegistry;

#[cfg(feature = "gpu-proofs")]
#[test]
fn resize_re_pre_allocates_array_buffers() {
    use manifold_node_engine::{exec::backend::Backend, ports::PortType};
    let device = manifold_gpu::testkit::test_device();
    let json = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/Lissajous.json"));
    let mut g = PresetRuntime::from_json_str_with_device(
        json,
        &PrimitiveRegistry::with_builtin(),
        device.arc(),
        1920,
        1080,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("Lissajous preset must load");

    let array_resources: Vec<ResourceId> = (0..g.plan.resource_count() as u32)
        .map(ResourceId)
        .filter(|id| matches!(g.plan.resource_type(*id), Some(PortType::Array(_))))
        .collect();
    assert!(
        !array_resources.is_empty(),
        "Lissajous preset must produce at least one Array<T> wire",
    );

    {
        let metal = manifold_node_engine::runtime::testkit::metal_backend(&mut g);
        for &res in &array_resources {
            let slot = metal
                .slot_for(res)
                .unwrap_or_else(|| panic!("Array resource {res:?} unbound after construction"));
            assert!(
                Backend::array_buffer(metal, slot).is_some(),
                "Array resource {res:?} has no backing buffer after construction",
            );
        }
    }

    let old_buffers = array_resources.iter().map(|&id| {
        let backend = g.backend_for_test();
        backend.array_buffer(backend.slot_for(id).unwrap()).unwrap().clone()
    }).collect::<Vec<_>>();
    g.resize(&device, 1280, 720).unwrap();
    for (&id, before) in array_resources.iter().zip(&old_buffers) {
        let backend = g.backend_for_test();
        let after = backend.array_buffer(backend.slot_for(id).unwrap()).unwrap();
        assert!(before.ptr_eq(after), "resolution-independent arrays must keep physical storage");
    }

    let metal = manifold_node_engine::runtime::testkit::metal_backend(&mut g);
    for &res in &array_resources {
        let slot = metal
            .slot_for(res)
            .unwrap_or_else(|| panic!("Array resource {res:?} unbound after resize"));
        assert!(
            Backend::array_buffer(metal, slot).is_some(),
            "Array resource {res:?} has no backing buffer after resize",
        );
    }
}

/// Live project-resolution change must not kill a particle preset
/// (Peter's report on Cymatics, 2026-07-16: "breaks when I change
/// project resolution"). `resize()` wipes every pinned binding
/// including Array<T> wires; a particle sim whose state rides those
/// buffers (or whose re-seed never re-fires) comes back dead — black
/// output, sand gone. This renders warm-up frames, resizes, renders
/// again, and asserts the output still carries energy.
#[cfg(feature = "gpu-proofs")]
#[test]
fn cymatics_survives_live_resize() {
    use manifold_node_engine::runtime::preset_context::PresetContext;
    let device = manifold_gpu::testkit::test_device();
    let json = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/Cymatics.json"));
    let registry = PrimitiveRegistry::with_builtin();
    let format = GpuTextureFormat::Rgba16Float;
    let (w0, h0) = (512u32, 512u32);
    let mut g = PresetRuntime::from_json_str_with_device(
        json, &registry, device.arc(), w0, h0, format, None,
    )
    .expect("Cymatics preset must load");

    let max_luma = |g: &mut PresetRuntime, w: u32, h: u32, frames: u32, base: u32| -> f32 {
        let target = RenderTarget::new(&device, w, h, format, "cymatics-resize-test");
        for f in 0..frames {
            let ctx = PresetContext {
                time: (base + f) as f64 / 60.0,
                beat: 0.0,
                dt: 1.0 / 60.0,
                width: w,
                height: h,
                output_width: w,
                output_height: h,
                aspect: w as f32 / h as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: i64::from(base + f),
                anim_progress: 0.0,
                trigger_count: 0,
            };
            let mut enc = device.create_encoder("cymatics-resize-frame");
            {
                let mut gpu = manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut enc, &device);
                g.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
            enc.commit_and_wait_completed();
        }
        let bytes_per_row = w * 8;
        let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut rb = device.create_encoder("cymatics-resize-readback");
        rb.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
        rb.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared buffer mapped");
        let px: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        px.chunks(4)
            .map(|c| half::f16::from_bits(c[0]).to_f32())
            .fold(0.0f32, f32::max)
    };

    let before = max_luma(&mut g, w0, h0, 90, 0);
    assert!(
        before > 0.05,
        "Cymatics must render visible sand before resize (max luma {before})"
    );

    let (w1, h1) = (384u32, 640u32);
    g.resize(&device, w1, h1).unwrap();

    let after = max_luma(&mut g, w1, h1, 90, 90);
    assert!(
        after > 0.05,
        "Cymatics must still render visible sand after a live resize \
         (max luma {after} — resize killed the particle state)"
    );
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn aliased_array_io_routes_in_and_out_to_one_physical_slot() {
    use manifold_node_engine::exec::backend::Backend;
    let device = manifold_gpu::testkit::test_device();
    let json = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/StrangeAttractor.json"));
    let mut g = PresetRuntime::from_json_str_with_device(
        json,
        &PrimitiveRegistry::with_builtin(),
        device.arc(),
        1920,
        1080,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("StrangeAttractor preset must load");

    let find_node = |type_id: &str| -> NodeInstanceId {
        for step in g.plan.steps() {
            let inst = g.graph.get_node(step.node).expect("step's node");
            if inst.node.type_id().as_str() == type_id {
                return step.node;
            }
        }
        panic!("node `{type_id}` not in compiled plan");
    };
    let integrate_node = find_node("node.wgsl_compute");
    let scatter_node = find_node("node.draw_particles");

    let resource_for = |node: NodeInstanceId, port: &str, is_input: bool| -> ResourceId {
        for step in g.plan.steps() {
            if step.node == node {
                let ports = if is_input { &step.inputs } else { &step.outputs };
                for &(name, id) in ports {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!(
            "missing {} port `{port}` on node {node:?}",
            if is_input { "input" } else { "output" }
        );
    };

    let integrate_in_res = resource_for(integrate_node, "particles", true);
    let integrate_out_res = resource_for(integrate_node, "particles", false);
    let scatter_in_res = resource_for(scatter_node, "particles", true);

    let metal = manifold_node_engine::runtime::testkit::metal_backend(&mut g);

    let in_slot = metal.slot_for(integrate_in_res).expect("integrate.in bound");
    let out_slot = metal.slot_for(integrate_out_res).expect("integrate.out bound");
    let scatter_slot = metal.slot_for(scatter_in_res).expect("scatter.particles bound");

    assert_eq!(in_slot, out_slot, "aliased_array_io in→out must share a slot");
    assert_eq!(
        out_slot, scatter_slot,
        "integrate.out and scatter.particles must resolve to the same slot",
    );
    assert!(
        Backend::array_buffer(metal, in_slot).is_some(),
        "the shared slot must back a real GpuBuffer",
    );
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn canvas_sized_array_outputs_scale_buffer_with_backend_canvas_dims() {
    use manifold_node_engine::exec::backend::Backend;
    let device = manifold_gpu::testkit::test_device();
    let json = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/StrangeAttractor.json"));

    let cases = [(1280u32, 720u32), (3840u32, 2160u32)];
    for (w, h) in cases {
        let mut g = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("preset must load");

        let scatter = (|| {
            for step in g.plan.steps() {
                let inst = g.graph.get_node(step.node).expect("step's node");
                if inst.node.type_id().as_str() == "node.draw_particles" {
                    return step.node;
                }
            }
            panic!("scatter node missing");
        })();
        let accum_res = (|| {
            for step in g.plan.steps() {
                if step.node == scatter {
                    for &(name, id) in &step.outputs {
                        if name == "accum" {
                            return id;
                        }
                    }
                }
            }
            panic!("scatter.accum resource missing");
        })();

        let metal = manifold_node_engine::runtime::testkit::metal_backend(&mut g);
        let slot = metal.slot_for(accum_res).expect("scatter.accum unbound");
        let buf = Backend::array_buffer(metal, slot).expect("no backing buffer");
        let expected = (w as u64) * (h as u64) * 4;
        assert_eq!(
            buf.size, expected,
            "scatter.accum at canvas {w}x{h} should be {expected} bytes, got {}",
            buf.size,
        );
    }
}
