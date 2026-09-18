use super::*;
use crate::headless_readback::readback_raw_halves;
use crate::node_graph::camera::CameraMode;
use manifold_gpu::GpuTextureFormat;

/// CPU port of the diagram shader's `euler_xyz` + `apply_copy`: rotate (XYZ
/// Euler), uniform scale, translate. The test oracle — a trail mark for copy
/// c at slot s must land exactly here.
fn apply_copy_cpu(p: [f32; 3], inst: InstanceTransform) -> [f32; 3] {
    let [ax, ay, az] = [inst.rot_pad[0], inst.rot_pad[1], inst.rot_pad[2]];
    let (cx, sx, cy, sy, cz, sz) = (
        ax.cos(),
        ax.sin(),
        ay.cos(),
        ay.sin(),
        az.cos(),
        az.sin(),
    );
    // WGSL mat3x3 columns, matching euler_xyz in render_mesh_diagram.wgsl.
    let rx = [
        [1.0, 0.0, 0.0],
        [0.0, cx, sx],
        [0.0, -sx, cx],
    ];
    let ry = [
        [cy, 0.0, -sy],
        [0.0, 1.0, 0.0],
        [sy, 0.0, cy],
    ];
    let rz = [
        [cz, sz, 0.0],
        [-sz, cz, 0.0],
        [0.0, 0.0, 1.0],
    ];
    let mat_mul = |a: [[f32; 3]; 3], b: [[f32; 3]; 3]| {
        // Arrays are [column][component], matching WGSL mat3x3 literals;
        // column j of a*b is a * (column j of b).
        let mut out = [[0.0f32; 3]; 3];
        for (j, column) in out.iter_mut().enumerate() {
            for (i, cell) in column.iter_mut().enumerate() {
                *cell = a[0][i] * b[j][0] + a[1][i] * b[j][1] + a[2][i] * b[j][2];
            }
        }
        out
    };
    let rot = mat_mul(rz, mat_mul(ry, rx));
    let scaled = [p[0] * inst.pos_scale[3], p[1] * inst.pos_scale[3], p[2] * inst.pos_scale[3]];
    [
        rot[0][0] * scaled[0] + rot[1][0] * scaled[1] + rot[2][0] * scaled[2] + inst.pos_scale[0],
        rot[0][1] * scaled[0] + rot[1][1] * scaled[1] + rot[2][1] * scaled[2] + inst.pos_scale[1],
        rot[0][2] * scaled[0] + rot[1][2] * scaled[1] + rot[2][2] * scaled[2] + inst.pos_scale[2],
    ]
}

#[test]
fn math_view_instance_history_records_per_copy_motion() {
    let guard = crate::test_device();
    let device = guard.arc();
    let pipeline = device.create_compute_pipeline(CAPTURE_SHADER, "cs_main", "instance-history-proof");
    let history = device.create_buffer_shared(
        HISTORY_SAMPLES as u64 * MAX_VERTICES as u64 * std::mem::size_of::<[f32; 4]>() as u64,
    );
    history.zero_fill();
    let ring = device.create_buffer_shared(
        HISTORY_SAMPLES as u64 * MAX_COPY_INSTANCES as u64 * std::mem::size_of::<InstanceTransform>() as u64,
    );
    ring.zero_fill();
    let counts = device.create_buffer_shared(HISTORY_SAMPLES as u64 * 4);
    counts.zero_fill();
    let vertex_buffer = device.create_buffer_shared(3 * std::mem::size_of::<MeshVertex>() as u64);
    let instance_buffer = device.create_buffer_shared(2 * std::mem::size_of::<InstanceTransform>() as u64);

    // Per-frame known inputs: one vertex drifts (mixed vertex+instance
    // motion), copy 1 translates and rotates, copy 0 stays identity.
    let frames = 4u32;
    let vertex_frame = |f: u32| {
        let drift = 0.05 * f as f32;
        [
            MeshVertex { position: [-0.25 + drift, -0.15, 0.0], ..bytemuck::Zeroable::zeroed() },
            MeshVertex { position: [0.25, -0.1, 0.0], ..bytemuck::Zeroable::zeroed() },
            MeshVertex { position: [0.0, 0.3, 0.0], ..bytemuck::Zeroable::zeroed() },
        ]
    };
    let instance_frame = |f: u32| {
        let identity = InstanceTransform { pos_scale: [0.0, 0.0, 0.0, 1.0], rot_pad: [0.0; 4] };
        let moving = InstanceTransform {
            pos_scale: [1.4 + 0.12 * f as f32, 0.3, 0.0, 1.0],
            rot_pad: [0.0, 0.0, 0.15 * f as f32, 0.0],
        };
        [identity, moving]
    };
    let mut expected_vertices = Vec::new();
    let mut expected_instances = Vec::new();
    for f in 0..frames {
        let vertices = vertex_frame(f);
        let instances = instance_frame(f);
        expected_vertices.push(vertices);
        expected_instances.push(instances);
        // SAFETY: exact-size shared buffers; the previous frame's command
        // buffer has completed before this write.
        unsafe { vertex_buffer.write(0, bytemuck::cast_slice(&vertices)) };
        unsafe { instance_buffer.write(0, bytemuck::cast_slice(&instances)) };
        let capture = HistoryCaptureUniforms {
            vertex_count: 3,
            history_slot: f,
            history_stride: MAX_VERTICES,
            copy_count: 2,
            instance_stride: MAX_COPY_INSTANCES,
            instance_capture: 1,
            _pad: [0; 2],
        };
        let mut encoder = device.create_encoder("instance-history-proof");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&capture) },
                GpuBinding::Buffer { binding: 1, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &history, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &instance_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &ring, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: &counts, offset: 0 },
            ],
            [1, 1, 1],
            "instance-history-proof",
        );
        encoder.commit_and_wait_completed();
    }

    let ring_ptr = ring.mapped_ptr().expect("shared ring must expose mapped pointer");
    let slot_transforms = |slot: u32| -> [[f32; 8]; 2] {
        // SAFETY: slot stays inside the zero-filled 64 x 8 ring.
        let base = unsafe { ring_ptr.add(slot as usize * MAX_COPY_INSTANCES as usize * 32) };
        let bytes = unsafe { std::slice::from_raw_parts(base, 2 * 32) };
        [*bytemuck::from_bytes(&bytes[0..32]), *bytemuck::from_bytes(&bytes[32..64])]
    };
    let as_words = |inst: InstanceTransform| -> [f32; 8] {
        *bytemuck::from_bytes(bytemuck::bytes_of(&inst))
    };
    for (f, instances) in expected_instances.iter().enumerate() {
        assert_eq!(
            slot_transforms(f as u32),
            [as_words(instances[0]), as_words(instances[1])],
            "slot {f} must hold the instances the producer published that frame"
        );
    }
    // Slots the run never reached keep their zero-fill: no phantom copies.
    assert_eq!(slot_transforms(frames), [[0.0; 8]; 2]);
    let counts_ptr = counts.mapped_ptr().expect("shared counts must expose mapped pointer");
    for f in 0..HISTORY_SAMPLES {
        let value = unsafe { (counts_ptr as *const u32).add(f as usize).read() };
        assert_eq!(value, if f < frames { 2 } else { 0 }, "per-slot copy count at slot {f}");
    }

    // Composed oracle: a copy trail mark at (slot s, copy c, vertex v) is the
    // vertex the GPU ring recorded for slot s transformed by the instance the
    // GPU ring recorded for the same slot. Test 2 asserts the rendered pixels
    // land exactly here.
    let history_ptr = history.mapped_ptr().expect("shared history must expose mapped pointer");
    let ring_vertex = |slot: u32, vertex: usize| -> [f32; 3] {
        // SAFETY: slot * MAX_VERTICES + vertex stays inside the zero-filled ring.
        let base = unsafe {
            history_ptr.add((slot as usize * MAX_VERTICES as usize + vertex) * 16)
        };
        let bytes = unsafe { std::slice::from_raw_parts(base, 16) };
        let v: [f32; 4] = *bytemuck::from_bytes(bytes);
        [v[0], v[1], v[2]]
    };
    for (f, vertices) in expected_vertices.iter().enumerate() {
        for (v, vertex) in vertices.iter().enumerate() {
            assert_eq!(
                ring_vertex(f as u32, v),
                vertex.position,
                "slot {f} vertex {v} must hold the position published that frame"
            );
        }
    }
    let composed = |slot: u32, copy: usize, vertex: usize| -> [f32; 3] {
        let words = slot_transforms(slot)[copy];
        let inst: InstanceTransform = *bytemuck::from_bytes(bytemuck::bytes_of(&words));
        apply_copy_cpu(ring_vertex(slot, vertex), inst)
    };
    assert_eq!(
        composed(3, 1, 0),
        apply_copy_cpu(expected_vertices[3][0].position, expected_instances[3][1])
    );
}

#[test]
fn math_view_trails_render_recorded_copy_motion() {
    const W: u32 = 640;
    const H: u32 = 360;
    let guard = crate::test_device();
    let device = guard.arc();
    let target = crate::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "copy-trails-proof",
    );
    let msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "copy-trails-proof",
    );
    let capture_pipeline =
        device.create_compute_pipeline(CAPTURE_SHADER, "cs_main", "copy-trails-proof");
    let pipeline = device.create_render_pipeline_msaa(
        SHADER,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba16Float,
        Some(DIAGRAM_BLEND),
        MSAA_SAMPLE_COUNT,
        "copy-trails-proof",
    );
    let history = device.create_buffer_shared(
        HISTORY_SAMPLES as u64 * MAX_VERTICES as u64 * std::mem::size_of::<[f32; 4]>() as u64,
    );
    history.zero_fill();
    let ring = device.create_buffer_shared(
        HISTORY_SAMPLES as u64 * MAX_COPY_INSTANCES as u64 * std::mem::size_of::<InstanceTransform>() as u64,
    );
    ring.zero_fill();
    let counts = device.create_buffer_shared(HISTORY_SAMPLES as u64 * 4);
    counts.zero_fill();
    let vertex_buffer = device.create_buffer_shared(3 * std::mem::size_of::<MeshVertex>() as u64);
    let instance_buffer = device.create_buffer_shared(2 * std::mem::size_of::<InstanceTransform>() as u64);

    let frames = 10u32;
    let vertex_frame = |f: u32| {
        let drift = 0.04 * f as f32;
        [
            MeshVertex { position: [-0.25 + drift, -0.15, 0.0], ..bytemuck::Zeroable::zeroed() },
            MeshVertex { position: [0.25, -0.1, 0.0], ..bytemuck::Zeroable::zeroed() },
            MeshVertex { position: [0.0, 0.3, 0.0], ..bytemuck::Zeroable::zeroed() },
        ]
    };
    let instance_frame = |f: u32| [
        InstanceTransform { pos_scale: [0.0, 0.0, 0.0, 1.0], rot_pad: [0.0; 4] },
        InstanceTransform {
            pos_scale: [1.4 + 0.12 * f as f32, 0.3, 0.0, 1.0],
            rot_pad: [0.0, 0.0, 0.15 * f as f32, 0.0],
        },
    ];
    let mut expected_vertices = Vec::new();
    for f in 0..frames {
        let vertices = vertex_frame(f);
        let instances = instance_frame(f);
        expected_vertices.push(vertices);
        // SAFETY: exact-size shared buffers; the previous frame's command
        // buffer has completed before this write.
        unsafe { vertex_buffer.write(0, bytemuck::cast_slice(&vertices)) };
        unsafe { instance_buffer.write(0, bytemuck::cast_slice(&instances)) };
        let capture = HistoryCaptureUniforms {
            vertex_count: 3,
            history_slot: f,
            history_stride: MAX_VERTICES,
            copy_count: 2,
            instance_stride: MAX_COPY_INSTANCES,
            instance_capture: 1,
            _pad: [0; 2],
        };
        let mut encoder = device.create_encoder("copy-trails-proof");
        encoder.dispatch_compute(
            &capture_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&capture) },
                GpuBinding::Buffer { binding: 1, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &history, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &instance_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &ring, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: &counts, offset: 0 },
            ],
            [1, 1, 1],
            "copy-trails-proof",
        );
        encoder.commit_and_wait_completed();
    }

    let camera = Camera::look_at(
        [0.0, 0.0, 5.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        1.0,
        0.05,
        200.0,
    );
    // Mirror the instance budget in run(): grid(1) + 3 per-copy blocks(6) +
    // arrows(2) + axes(6) + trails(32 * 3 * copies).
    let render = |copy_count: u32, wired: u32| {
        let view_proj = camera.view_proj(W as f32 / H as f32);
        let u = DiagramUniforms {
            view_proj,
            model: RenderMeshDiagram::model_matrix(Transform::default(), &camera),
            viewport: [W as f32, H as f32, 0.0, 0.0],
            radius: 0.2,
            line_width: 4.0,
            trails: 1,
            tri_count: 1,
            vertex_count: 3,
            history_head: frames % HISTORY_SAMPLES,
            history_len: frames,
            history_capacity: HISTORY_SAMPLES,
            history_stride: MAX_VERTICES,
            instance_history_stride: MAX_COPY_INSTANCES,
            copy_count,
            instances_wired: wired,
            inv_view_proj: super::super::render_scene::mat4_inverse(view_proj).unwrap(),
            camera_pos_far: [camera.pos[0], camera.pos[1], camera.pos[2], camera.far],
            event_values: [1.0, 1.0, 0.0, 0.0],
            ..bytemuck::Zeroable::zeroed()
        };
        let copies = if wired != 0 { copy_count } else { 1 };
        let instance_count = 3 * copies + copies + 1 + 6 + TRAIL_RENDER_SAMPLES * 3 * copies;
        let mut encoder = device.create_encoder("copy-trails-proof");
        encoder.draw_instanced_msaa(
            &pipeline,
            &msaa,
            &target.texture,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&u) },
                GpuBinding::Buffer { binding: 1, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &history, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 10, buffer: &instance_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 11, buffer: &ring, offset: 0 },
                GpuBinding::Buffer { binding: 12, buffer: &counts, offset: 0 },
            ],
            18,
            instance_count.max(1),
            GpuLoadAction::Clear,
            "copy-trails-proof",
        );
        encoder.commit_and_wait_completed();
        readback_raw_halves(&device, &target.texture, W, H)
    };
    let max_alpha_around = |image: &[u8], point: [f32; 3]| {
        let pixel = camera.project_to_pixel(point, W, H).unwrap();
        let cx = pixel.px as usize;
        let cy = pixel.py as usize;
        let mut alpha = 0.0f32;
        for y in cy.saturating_sub(3)..=(cy + 3).min(H as usize - 1) {
            for x in cx.saturating_sub(3)..=(cx + 3).min(W as usize - 1) {
                let offset = (y * W as usize + x) * 8 + 6;
                alpha = alpha.max(
                    half::f16::from_bits(u16::from_le_bytes([image[offset], image[offset + 1]]))
                        .to_f32(),
                );
            }
        }
        alpha
    };

    // CPU oracle: copy 1's mark between slots 7 and 8 at vertex 0 composes
    // each slot's own vertex and instance — a point today's instance buffer
    // never produces.
    let a = apply_copy_cpu(expected_vertices[7][0].position, instance_frame(7)[1]);
    let b = apply_copy_cpu(expected_vertices[8][0].position, instance_frame(8)[1]);
    let two_copies = render(2, 1);
    if let Ok(path) = std::env::var("MANIFOLD_MATH_TRAILS_PREVIEW") {
        let rgba = crate::headless_readback::readback_srgb_rgba8(&device, &target.texture, W, H);
        std::fs::write(path, crate::headless_readback::encode_rgba8_png(&rgba, W, H)).unwrap();
    }
    assert!(
        max_alpha_around(&two_copies, a) > 0.05,
        "copy trail must pass through its recorded start {a:?}"
    );
    assert!(
        max_alpha_around(&two_copies, b) > 0.05,
        "copy trail must pass through its recorded end {b:?}"
    );
    // A base-only render must not light the copy-1 path even though the same
    // instances buffer is bound — copy 1 simply has no trail marks.
    let base_only = render(1, 1);
    assert!(
        max_alpha_around(&base_only, b) < 0.01,
        "copy 1 must draw no trail marks when only copy 0 is active"
    );
    // Vertices-only regression: unwired output is byte-identical to a single
    // wired identity copy (the recorded copy-0 transform is the identity).
    let unwired = render(1, 0);
    assert_eq!(
        base_only, unwired,
        "vertices-only trail output must stay byte-identical"
    );
}


#[test]
fn math_view_instance_copies_draw_per_copy_marks_and_skip_inactive_holes() {
    const W: u32 = 640;
    const H: u32 = 360;
    let guard = crate::test_device();
    let device = guard.arc();
    let target = crate::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "instance-copies-proof",
    );
    let msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "instance-copies-proof",
    );
    let vertices = [
        MeshVertex {
            position: [-0.25, -0.15, 0.0],
            ..bytemuck::Zeroable::zeroed()
        },
        MeshVertex {
            position: [0.25, -0.1, 0.0],
            ..bytemuck::Zeroable::zeroed()
        },
        MeshVertex {
            position: [0.0, 0.3, 0.0],
            ..bytemuck::Zeroable::zeroed()
        },
    ];
    let vertex_buffer = device.create_buffer_shared(std::mem::size_of_val(&vertices[..]) as u64);
    // SAFETY: exact-size fresh shared buffer, no submitted work references it.
    unsafe { vertex_buffer.write(0, bytemuck::cast_slice(&vertices[..])) };
    let identity = InstanceTransform {
        pos_scale: [0.0, 0.0, 0.0, 1.0],
        rot_pad: [0.0; 4],
    };
    let translated = InstanceTransform {
        pos_scale: [1.6, 0.1, 0.0, 1.0],
        rot_pad: [0.0; 4],
    };
    let rotated = InstanceTransform {
        pos_scale: [-1.6, 0.1, 0.0, 0.6],
        rot_pad: [0.0, std::f32::consts::FRAC_PI_2, 0.0, 0.0],
    };
    let inactive = InstanceTransform {
        pos_scale: [0.0; 4],
        rot_pad: [0.0; 4],
    };
    let instance_buffer = |copies: &[InstanceTransform]| {
        let buffer =
            device.create_buffer_shared(std::mem::size_of_val(copies) as u64);
        // SAFETY: exact-size fresh shared buffer, no submitted work references it.
        unsafe { buffer.write(0, bytemuck::cast_slice(copies)) };
        buffer
    };
    let stub = instance_buffer(&[identity]);
    let single = instance_buffer(&[identity]);
    let three = instance_buffer(&[identity, translated, rotated]);
    let with_hole = instance_buffer(&[identity, inactive, translated, rotated]);
    let pipeline = device.create_render_pipeline_msaa(
        SHADER,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba16Float,
        Some(DIAGRAM_BLEND),
        MSAA_SAMPLE_COUNT,
        "instance-copies-proof",
    );
    let camera = Camera::look_at(
        [0.0, 0.0, 5.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        1.0,
        0.05,
        200.0,
    );
    let render = |instances: &manifold_gpu::GpuBuffer, copy_count: u32, wired: u32| {
        let view_proj = camera.view_proj(W as f32 / H as f32);
        let u = DiagramUniforms {
            view_proj,
            model: RenderMeshDiagram::model_matrix(Transform::default(), &camera),
            viewport: [W as f32, H as f32, 0.0, 0.0],
            radius: 0.2,
            line_width: 4.0,
            grid: 0,
            fragments: 1,
            ghosts: 1,
            vectors: 1,
            axes: 0,
            tri_count: 1,
            vertex_count: 3,
            history_capacity: HISTORY_SAMPLES,
            history_stride: MAX_VERTICES,
            copy_count,
            instances_wired: wired,
            brightness: [1.0; 4],
            event_values: [1.0, 1.0, 0.0, 0.0],
            inv_view_proj: super::super::render_scene::mat4_inverse(view_proj).unwrap(),
            camera_pos_far: [camera.pos[0], camera.pos[1], camera.pos[2], camera.far],
            ..bytemuck::Zeroable::zeroed()
        };
        let mut encoder = device.create_encoder("instance-copies-proof");
        encoder.draw_instanced_msaa(
            &pipeline,
            &msaa,
            &target.texture,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer { binding: 1, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: &vertex_buffer, offset: 0 },
                GpuBinding::Buffer { binding: 10, buffer: instances, offset: 0 },
            ],
            18,
            (4 * copy_count + 1 + 3 + 3).max(1),
            GpuLoadAction::Clear,
            "instance-copies-proof",
        );
        encoder.commit_and_wait_completed();
        readback_raw_halves(&device, &target.texture, W, H)
    };
    let alpha_at = |image: &[u8], point: [f32; 3]| {
        let pixel = camera.project_to_pixel(point, W, H).unwrap();
        let x = pixel.px as usize;
        let y = pixel.py as usize;
        let offset = (y * W as usize + x) * 8 + 6;
        half::f16::from_bits(u16::from_le_bytes([image[offset], image[offset + 1]])).to_f32()
    };

    let unwired = render(&stub, 1, 0);
    let wired_identity = render(&single, 1, 1);
    assert_eq!(
        unwired, wired_identity,
        "a single identity copy must be byte-identical to the unwired diagram"
    );
    let copies = render(&three, 3, 1);
    assert_ne!(unwired, copies, "copies must change the diagram output");
    // The translated copy's top vertex lands far from every base mark; it
    // must be lit in the copies image and clear in the unwired one.
    let echo_point = [1.6, 0.1 + 0.3, 0.0];
    assert!(alpha_at(&copies, echo_point) > 0.05, "translated copy ghost invisible");
    assert!(alpha_at(&unwired, echo_point) < 0.01, "unwired diagram shows no copy");
    // Inactive holes (the all-zero sentinel) are skipped, so a hole-padded
    // buffer draws exactly the same marks as its compact form.
    let hole_padded = render(&with_hole, 4, 1);
    assert_eq!(
        copies, hole_padded,
        "inactive zero-sentinel copies must not draw"
    );
    if let Ok(path) = std::env::var("MANIFOLD_MATH_COPIES_PREVIEW") {
        let rgba = crate::headless_readback::readback_srgb_rgba8(&device, &target.texture, W, H);
        std::fs::write(
            path,
            crate::headless_readback::encode_rgba8_png(&rgba, W, H),
        )
        .unwrap();
    }
}

#[test]
fn math_view_world_grid_matches_camera_and_ignores_object_transform() {
    const W: u32 = 640;
    const H: u32 = 360;
    let guard = crate::test_device();
    let device = guard.arc();
    let target = crate::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "world-grid-proof",
    );
    let msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "world-grid-proof",
    );
    let buffer = device.create_buffer_shared(3 * std::mem::size_of::<MeshVertex>() as u64);
    buffer.zero_fill();
    let pipeline = device.create_render_pipeline_msaa(
        SHADER,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba16Float,
        Some(DIAGRAM_BLEND),
        MSAA_SAMPLE_COUNT,
        "world-grid-proof",
    );
    let camera = Camera::look_at(
        [8.0, 7.0, 10.0],
        [0.0; 3],
        [0.0, 1.0, 0.0],
        1.0,
        0.05,
        200.0,
    );
    let render = |camera: Camera, transform: Transform, enabled: u32| {
        let view_proj = camera.view_proj(W as f32 / H as f32);
        let u = DiagramUniforms {
            view_proj,
            model: RenderMeshDiagram::model_matrix(transform, &camera),
            viewport: [W as f32, H as f32, 0.0, 0.0],
            radius: 0.2,
            line_width: 1.0,
            grid: enabled,
            inv_view_proj: super::super::render_scene::mat4_inverse(view_proj).unwrap(),
            camera_pos_far: [camera.pos[0], camera.pos[1], camera.pos[2], camera.far],
            brightness: [1.0;4],
            event_values: [1.0,1.0,0.0,0.0],
            ..bytemuck::Zeroable::zeroed()
        };
        let mut encoder = device.create_encoder("world-grid-proof");
        encoder.draw_instanced_msaa(
            &pipeline,
            &msaa,
            &target.texture,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &buffer,
                    offset: 0,
                },
                GpuBinding::Buffer { binding: 5, buffer: &buffer, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: &buffer, offset: 0 },
            ],
            18,
            1,
            GpuLoadAction::Clear,
            "world-grid-proof",
        );
        encoder.commit_and_wait_completed();
        readback_raw_halves(&device, &target.texture, W, H)
    };
    let mut previous = None;
    for camera in [
        camera,
        Camera {
            mode: CameraMode::Orthographic { half_height: 9.0 },
            ..camera
        },
    ] {
        let reference = render(camera, Transform::default(), 1);
        let moved = render(
            camera,
            Transform {
                pos: [9.0, -3.0, 4.0],
                rot_euler: [0.4, -0.7, 1.2],
                scale: [0.2, 3.0, 1.5],
                ..Transform::default()
            },
            1,
        );
        assert_eq!(reference, moved, "object pose must not move the world grid");
        // Known world coordinates must land on the camera's projected pixels,
        // far beyond the old radius-limited square. Test alpha directly so
        // the absence of blending cannot turn transparent cell interiors lit.
        let alpha_at = |point: [f32; 3]| {
            let pixel = camera.project_to_pixel(point, W, H).unwrap();
            assert!(
                pixel.px >= 0.0 && pixel.px < W as f32 && pixel.py >= 0.0 && pixel.py < H as f32
            );
            let x = pixel.px as usize;
            let y = pixel.py as usize;
            let offset = (y * W as usize + x) * 8 + 6;
            half::f16::from_bits(u16::from_le_bytes([
                reference[offset],
                reference[offset + 1],
            ]))
            .to_f32()
        };
        assert!(
            alpha_at([4.0, 0.0, 0.5]) > 0.08,
            "grid must align to a projected world line beyond sample radius"
        );
        assert!(
            alpha_at([4.5, 0.0, 0.5]) < 0.01,
            "cell interior must stay clear"
        );
        assert!(
            render(camera, Transform::default(), 0)
                .iter()
                .all(|value| *value == 0),
            "Grid off must clear the grid"
        );
        if let Some(previous) = previous {
            assert_ne!(
                reference, previous,
                "changing projection must move the grid"
            );
        }
        previous = Some(reference);
    }

    // A shallow, level camera exposes perspective minification at the
    // horizon that the downward-looking projection checks do not exercise.
    let grazing = Camera::look_at(
        [0.0, 1.0, 8.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        1.0,
        0.05,
        1000.0,
    );
    let image = render(grazing, Transform::default(), 1);
    assert!(image.chunks_exact(2).all(|bytes| {
        half::f16::from_bits(u16::from_le_bytes([bytes[0], bytes[1]])).is_finite()
    }));
    // A level camera's horizon crosses the image midpoint. The unresolved
    // first six rows beneath it must be clear rather than a surviving fan.
    for row in H / 2..H / 2 + 6 {
        for pixel in image[(row * W * 8) as usize..((row + 1) * W * 8) as usize].chunks_exact(8) {
            let alpha = half::f16::from_bits(u16::from_le_bytes([pixel[6], pixel[7]])).to_f32();
            assert_eq!(alpha, 0.0, "unresolved horizon graduation at row {row}");
        }
    }
    if let Ok(path) = std::env::var("MANIFOLD_MATH_GRID_PREVIEW") {
        let rgba = crate::headless_readback::readback_srgb_rgba8(&device, &target.texture, W, H);
        std::fs::write(path, crate::headless_readback::encode_rgba8_png(&rgba, W, H)).unwrap();
    }
}
