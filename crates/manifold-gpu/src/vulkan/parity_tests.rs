//! The Vulkan parity kit (design section 10): one test per backend
//! primitive, each on a tiny input, each ending with no validation errors.
//! Run on KosmicKrisp with synchronization validation:
//! `VK_DRIVER_FILES=/usr/local/share/vulkan/icd.d/libkosmickrisp_icd.json
//! MANIFOLD_VULKAN_VALIDATION=sync cargo nextest run -p manifold-gpu
//! --features vulkan parity`.

use std::sync::Arc;

use super::core::{ALL_FORMATS, validation_error_count};
use crate::*;

fn assert_no_validation_errors() {
    assert_eq!(
        validation_error_count(),
        0,
        "validation errors, see the log"
    );
}

fn usage_for(format: GpuTextureFormat) -> GpuTextureUsage {
    if format == GpuTextureFormat::Depth32Float {
        GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ
    } else {
        GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD
    }
}

#[test]
fn parity_shared_buffers_map_and_private_ones_do_not() {
    let device = GpuDevice::new();
    let shared = device.create_buffer_shared(64);
    let bytes: Vec<u8> = (0..64).map(|i| i * 3).collect();
    unsafe { shared.write(0, &bytes) };
    let ptr = shared.mapped_ptr().expect("shared buffers are mapped");
    let read = unsafe { std::slice::from_raw_parts(ptr, 64) };
    assert_eq!(read, &bytes[..]);
    shared.zero_fill();
    assert!(read.iter().all(|&b| b == 0));

    let private = device.create_buffer(64);
    assert!(private.mapped_ptr().is_none());
    assert_eq!(private.size(), 64);
    let clone = private.clone();
    assert!(clone.ptr_eq(&private) && !clone.ptr_eq(&shared));
    assert_eq!(device.create_buffer(0).size(), 0);
    assert_no_validation_errors();
}

#[test]
fn parity_every_format_creates_uploads_and_views() {
    let device = GpuDevice::new();
    for format in ALL_FORMATS {
        let texture = device.create_texture(&GpuTextureDesc {
            width: 8,
            height: 4,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: usage_for(format),
            label: "parity format",
            mip_levels: 3,
        });
        assert_eq!(texture.mip_level_count(), 3);
        if format != GpuTextureFormat::Depth32Float {
            let pixels = vec![7u8; 8 * 4 * format.bytes_per_pixel() as usize];
            device.upload_texture(&texture, &pixels);
        }
        let mip = texture.mip_level_view(2, 2, 1);
        assert_eq!((mip.width, mip.height, mip.mip_level_count()), (2, 1, 1));
        assert!(texture.clone().ptr_eq(&texture));
        assert!(!mip.ptr_eq(&texture));
    }
    let array = device.create_texture(&GpuTextureDesc {
        width: 4,
        height: 4,
        depth: 3,
        format: GpuTextureFormat::Depth32Float,
        dimension: GpuTextureDimension::D2Array,
        usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
        label: "parity layers",
        mip_levels: 1,
    });
    let layer = array.layer_view(2);
    assert_eq!((layer.depth, layer.width), (1, 4));
    let volume = device.create_texture(&GpuTextureDesc {
        width: 4,
        height: 4,
        depth: 4,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D3,
        usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
        label: "parity volume",
        mip_levels: 1,
    });
    assert!(volume.is_shader_readable());
    assert_eq!(device.allocation_counts()[1], ALL_FORMATS.len() as u64 + 2);
    assert_no_validation_errors();
}

#[test]
fn parity_samplers_cover_every_mode() {
    let device = GpuDevice::new();
    for address in [
        GpuAddressMode::ClampToEdge,
        GpuAddressMode::Repeat,
        GpuAddressMode::MirrorRepeat,
        GpuAddressMode::ClampToZero,
    ] {
        device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Nearest,
            mip_filter: GpuFilterMode::Linear,
            address_mode_u: address,
            address_mode_v: address,
            address_mode_w: address,
            max_anisotropy: 8,
            lod_max_clamp: 4.0,
            ..Default::default()
        });
    }
    device.create_sampler(&GpuSamplerDesc {
        compare: Some(GpuCompareFunction::LessEqual),
        ..Default::default()
    });
    let _ = device.linear_sampler();
    assert_no_validation_errors();
}

#[test]
fn parity_memory_snapshot_counts_new_allocations() {
    let device = GpuDevice::new();
    let Some(before) = device.modifier_memory_snapshot() else {
        // The driver lacks VK_EXT_memory_budget (D18); KosmicKrisp has it.
        return;
    };
    assert!(before.recommended_max_working_set_bytes > 0);
    let _big = device.create_buffer(64 << 20);
    let after = device
        .modifier_memory_snapshot()
        .expect("budget stays available");
    assert!(
        after.current_allocated_bytes >= before.current_allocated_bytes + (64 << 20),
        "{} -> {}",
        before.current_allocated_bytes,
        after.current_allocated_bytes
    );
    assert_no_validation_errors();
}

#[test]
fn parity_siblings_share_one_device_with_their_own_scope() {
    let device = GpuDevice::new();
    let sibling = device.sibling();
    assert_ne!(device.resource_scope_id(), sibling.resource_scope_id());
    assert_eq!(device.device_name(), sibling.device_name());
    let buffer = sibling.create_buffer_shared(16);
    assert_eq!(sibling.allocation_counts()[0], 1);
    assert_eq!(device.allocation_counts()[0], 0);
    drop(buffer);
    assert_no_validation_errors();
}

const AFFINE: &str = r#"
struct Params { scale: f32, bias: f32, count: u32, pad: u32 }
@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.count) { return; }
    dst[id.x] = src[id.x] * params.scale + params.bias;
}
"#;

fn affine_params(scale: f32, bias: f32, count: u32) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&scale.to_ne_bytes());
    bytes[4..8].copy_from_slice(&bias.to_ne_bytes());
    bytes[8..12].copy_from_slice(&count.to_ne_bytes());
    bytes
}

fn affine(
    enc: &mut GpuEncoder,
    pipeline: &GpuComputePipeline,
    src: &GpuBuffer,
    dst: &GpuBuffer,
    scale: f32,
    bias: f32,
    count: u32,
) {
    let params = affine_params(scale, bias, count);
    enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Buffer {
                binding: 0,
                buffer: src,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: dst,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 2,
                data: &params,
            },
        ],
        [count.div_ceil(64), 1, 1],
        "parity affine",
    );
}

fn shared_f32(device: &GpuDevice, values: &[f32]) -> GpuBuffer {
    let buffer = device.create_buffer_shared(values.len() as u64 * 4);
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
    unsafe { buffer.write(0, &bytes) };
    buffer
}

fn read<T: Copy>(buffer: &GpuBuffer, count: usize) -> Vec<T> {
    assert!(count * std::mem::size_of::<T>() <= buffer.size() as usize);
    let ptr = buffer.mapped_ptr().expect("readback buffers are shared");
    unsafe { std::slice::from_raw_parts(ptr.cast::<T>(), count) }.to_vec()
}

#[test]
fn parity_dispatch_chains_storage_and_uniform_bytes() {
    let device = GpuDevice::new();
    let pipeline = device.create_compute_pipeline(AFFINE, "main", "parity affine");
    assert!(pipeline.max_threads_per_threadgroup().unwrap() >= 64);
    // Not a multiple of the workgroup size.
    let n = 1000u32;
    let src = shared_f32(&device, &(0..n).map(|i| i as f32).collect::<Vec<_>>());
    let mid = device.create_buffer(u64::from(n) * 4);
    let out = device.create_buffer(u64::from(n) * 4);
    let readback = device.create_buffer_shared(u64::from(n) * 8);
    let mut enc = device.create_encoder("parity dispatch");
    affine(&mut enc, &pipeline, &src, &mid, 2.0, 1.0, n);
    affine(&mut enc, &pipeline, &mid, &out, 3.0, -1.0, n);
    enc.copy_buffer_range(&mid, 0, &readback, 0, u64::from(n) * 4);
    enc.copy_buffer_range(&out, 0, &readback, u64::from(n) * 4, u64::from(n) * 4);
    enc.commit_and_wait_completed();
    let values: Vec<f32> = read(&readback, 2 * n as usize);
    for i in 0..n as usize {
        assert_eq!(values[i], 2.0 * i as f32 + 1.0, "mid[{i}]");
        assert_eq!(values[n as usize + i], 6.0 * i as f32 + 2.0, "out[{i}]");
    }
    // The same pipeline from the cache; a second frame reuses the resources.
    let again = device.create_compute_pipeline(AFFINE, "main", "parity affine");
    assert_eq!(device.compute_pipeline_cache_len(), 1);
    let mut enc = device.create_encoder("parity dispatch 2");
    affine(&mut enc, &again, &src, &readback, -1.0, 0.0, n);
    enc.commit_and_wait_completed();
    assert_eq!(read::<f32>(&readback, 3)[..], [0.0, -1.0, -2.0]);
    assert_no_validation_errors();
}

#[test]
fn parity_indirect_dispatch_reads_gpu_written_args() {
    const ARGS: &str = r#"
struct Params { count: u32, a: u32, b: u32, c: u32 }
@group(0) @binding(0) var<storage, read_write> args: array<u32>;
@group(0) @binding(1) var<uniform> params: Params;
@compute @workgroup_size(1)
fn main() {
    args[4] = (params.count + 63u) / 64u;
    args[5] = 1u;
    args[6] = 1u;
}
"#;
    const FILL: &str = r#"
struct Params { count: u32, a: u32, b: u32, c: u32 }
@group(0) @binding(0) var<storage, read_write> dst: array<u32>;
@group(0) @binding(1) var<uniform> params: Params;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.count) { return; }
    dst[id.x] = id.x * 3u + 1u;
}
"#;
    let device = GpuDevice::new();
    let write_args = device.create_compute_pipeline(ARGS, "main", "parity args");
    let fill = device.create_compute_pipeline(FILL, "main", "parity fill");
    let n = 300u32;
    let mut params = [0u8; 16];
    params[0..4].copy_from_slice(&n.to_ne_bytes());
    let args = device.create_buffer(32);
    let out = device.create_buffer_shared(u64::from(n) * 4);
    let mut enc = device.create_encoder("parity indirect");
    enc.dispatch_compute(
        &write_args,
        &[
            GpuBinding::Buffer {
                binding: 0,
                buffer: &args,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 1,
                data: &params,
            },
        ],
        [1, 1, 1],
        "args",
    );
    enc.dispatch_compute_indirect(
        &fill,
        &[
            GpuBinding::Buffer {
                binding: 0,
                buffer: &out,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 1,
                data: &params,
            },
        ],
        &args,
        16,
        "fill",
    );
    enc.commit_and_wait_completed();
    let values: Vec<u32> = read(&out, n as usize);
    assert!(
        values
            .iter()
            .enumerate()
            .all(|(i, &v)| v == i as u32 * 3 + 1)
    );
    assert_no_validation_errors();
}

#[test]
fn parity_uploads_copies_and_clears_land_in_order() {
    let device = GpuDevice::new();
    let desc = |format, label| GpuTextureDesc {
        width: 7,
        height: 5,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
        label,
        mip_levels: 1,
    };
    let source = device.create_texture(&desc(GpuTextureFormat::Rgba8Unorm, "parity source"));
    let copy = device.create_texture(&desc(GpuTextureFormat::Rgba8Unorm, "parity copy"));
    let half = device.create_texture(&desc(GpuTextureFormat::Rgba16Float, "parity half"));
    let uint = device.create_texture(&desc(GpuTextureFormat::R32Uint, "parity uint"));
    let pixels: Vec<u8> = (0..7 * 5 * 4).map(|i| (i * 3 % 251) as u8).collect();
    // Rows padded past 7 * 4 bytes.
    let rows = device.create_buffer_shared(32 * 5);
    let halves = device.create_buffer_shared(7 * 8 * 5);
    let uints = device.create_buffer_shared(7 * 4 * 5);
    let words = device.create_buffer_shared(64);
    unsafe { words.write(0, &[0xAB; 64]) };
    let ramp = device.create_buffer_shared(64);
    unsafe { ramp.write(0, &(0..64).collect::<Vec<u8>>()) };
    let moved = device.create_buffer_shared(64);
    moved.zero_fill();

    let mut enc = device.create_encoder("parity copies");
    enc.upload_texture(&source, 7, 5, 1, &pixels);
    enc.copy_texture_to_texture(&source, &copy, 7, 5, 1);
    // Overwrite the source after the copy read it.
    enc.clear_texture(&source, 0.0, 0.0, 0.0, 0.0);
    enc.copy_texture_to_buffer(&copy, &rows, 7, 5, 32);
    enc.clear_texture(&half, 0.25, 0.5, 0.75, 1.0);
    enc.copy_texture_to_buffer(&half, &halves, 7, 5, 7 * 8);
    enc.clear_texture(&uint, 7.0, 0.0, 0.0, 0.0);
    enc.copy_texture_to_buffer(&uint, &uints, 7, 5, 7 * 4);
    enc.clear_buffer(&words);
    enc.copy_buffer_range(&ramp, 8, &moved, 40, 16);
    enc.commit_and_wait_completed();

    let rows: Vec<u8> = read(&rows, 32 * 5);
    for y in 0..5 {
        assert_eq!(
            rows[y * 32..y * 32 + 28],
            pixels[y * 28..y * 28 + 28],
            "row {y}"
        );
    }
    let halves: Vec<u16> = read(&halves, 7 * 4 * 5);
    assert!(
        halves
            .chunks(4)
            .all(|p| p == [0x3400, 0x3800, 0x3A00, 0x3C00])
    );
    assert!(read::<u32>(&uints, 35).iter().all(|&v| v == 7));
    assert!(read::<u8>(&words, 64).iter().all(|&b| b == 0));
    let moved: Vec<u8> = read(&moved, 64);
    assert_eq!(moved[40..56], (8..24).collect::<Vec<u8>>()[..]);
    assert!(moved[..40].iter().chain(&moved[56..]).all(|&b| b == 0));
    assert_no_validation_errors();
}

#[test]
fn parity_hazards_order_raw_waw_and_war() {
    let device = GpuDevice::new();
    let pipeline = device.create_compute_pipeline(AFFINE, "main", "parity affine");
    let n = 256u32;
    let a = device.create_buffer(u64::from(n) * 4);
    let b = device.create_buffer(u64::from(n) * 4);
    let zeros = device.create_buffer(u64::from(n) * 4);
    let readback = device.create_buffer_shared(u64::from(n) * 8);
    let mut enc = device.create_encoder("parity hazards");
    enc.clear_buffer(&a);
    enc.clear_buffer(&zeros);
    for _ in 0..6 {
        affine(&mut enc, &pipeline, &a, &b, 1.0, 1.0, n);
        affine(&mut enc, &pipeline, &b, &a, 1.0, 1.0, n);
    }
    // Reads `a`, then `a` is overwritten: write after read.
    affine(&mut enc, &pipeline, &a, &b, 1.0, 1.0, n);
    enc.copy_buffer_to_buffer(&zeros, &a, u64::from(n) * 4);
    enc.copy_buffer_range(&a, 0, &readback, 0, u64::from(n) * 4);
    enc.copy_buffer_range(&b, 0, &readback, u64::from(n) * 4, u64::from(n) * 4);
    enc.commit_and_wait_completed();
    let values: Vec<f32> = read(&readback, 2 * n as usize);
    assert!(values[..n as usize].iter().all(|&v| v == 0.0));
    assert!(values[n as usize..].iter().all(|&v| v == 13.0));
    assert_no_validation_errors();
}

#[test]
fn parity_mip_and_layer_views_write_only_their_subresource() {
    const STORE: &str = r#"
@group(0) @binding(0) var dst: texture_storage_2d<r32float, write>;
@group(0) @binding(1) var<uniform> base: vec4<f32>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(dst);
    if (id.x >= size.x || id.y >= size.y) { return; }
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(base.x + f32(id.x), 0.0, 0.0, 1.0));
}
"#;
    let device = GpuDevice::new();
    let pipeline = device.create_compute_pipeline(STORE, "main", "parity store");
    let usage = GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE;
    let mipped = device.create_texture(&GpuTextureDesc {
        width: 8,
        height: 8,
        depth: 1,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D2,
        usage,
        label: "parity mips",
        mip_levels: 3,
    });
    let layered = device.create_texture(&GpuTextureDesc {
        width: 4,
        height: 4,
        depth: 3,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D2Array,
        usage,
        label: "parity layers",
        mip_levels: 1,
    });
    let store = |enc: &mut GpuEncoder, view: &GpuTexture, base: f32| {
        let bytes: Vec<u8> = [base, 0.0, 0.0, 0.0]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: view,
                },
                GpuBinding::Bytes {
                    binding: 1,
                    data: &bytes,
                },
            ],
            [1, 1, 1],
            "store",
        );
    };
    let mips = [8u32, 4, 2].map(|size| device.create_buffer_shared(u64::from(size * size) * 4));
    let layers = [0, 1, 2].map(|_| device.create_buffer_shared(16 * 4));
    let mut enc = device.create_encoder("parity views");
    enc.clear_texture(&mipped, 0.0, 0.0, 0.0, 0.0);
    enc.clear_texture(&layered, 0.0, 0.0, 0.0, 0.0);
    let mip1 = mipped.mip_level_view(1, 4, 4);
    store(&mut enc, &mip1, 5.0);
    let layer1 = layered.layer_view(1);
    store(&mut enc, &layer1, 9.0);
    for (level, buffer) in mips.iter().enumerate() {
        let size = 8 >> level;
        let view = mipped.mip_level_view(level as u32, size, size);
        enc.copy_texture_to_buffer(&view, buffer, size, size, size * 4);
    }
    for (layer, buffer) in layers.iter().enumerate() {
        enc.copy_texture_to_buffer(&layered.layer_view(layer as u32), buffer, 4, 4, 16);
    }
    enc.commit_and_wait_completed();
    let expect_row = |base: f32, size: usize| -> Vec<f32> {
        (0..size * size).map(|i| base + (i % size) as f32).collect()
    };
    assert!(read::<f32>(&mips[0], 64).iter().all(|&v| v == 0.0));
    assert_eq!(read::<f32>(&mips[1], 16), expect_row(5.0, 4));
    assert!(read::<f32>(&mips[2], 4).iter().all(|&v| v == 0.0));
    assert!(read::<f32>(&layers[0], 16).iter().all(|&v| v == 0.0));
    assert_eq!(read::<f32>(&layers[1], 16), expect_row(9.0, 4));
    assert!(read::<f32>(&layers[2], 16).iter().all(|&v| v == 0.0));
    assert_no_validation_errors();
}

#[test]
fn parity_ring_and_descriptor_pools_grow_past_one_block() {
    const POKE: &str = r#"
struct Params { index: u32, value: u32 }
@group(0) @binding(0) var<storage, read_write> dst: array<u32>;
@group(0) @binding(1) var<uniform> params: Params;
@compute @workgroup_size(1)
fn main() { dst[params.index] = params.value; }
"#;
    let device = GpuDevice::new();
    let pipeline = device.create_compute_pipeline(POKE, "main", "parity poke");
    // 1100 × 256 bytes passes the first 256 KiB block, and 1100 sets pass
    // two 512-set pools.
    let n = 1100u32;
    let out = device.create_buffer_shared(u64::from(n) * 4);
    for frame in 0..2u32 {
        let mut enc = device.create_encoder("parity ring");
        for i in 0..n {
            let mut bytes = [0u8; 256];
            bytes[0..4].copy_from_slice(&i.to_ne_bytes());
            bytes[4..8].copy_from_slice(&(i * 7 + frame).to_ne_bytes());
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Buffer {
                        binding: 0,
                        buffer: &out,
                        offset: 0,
                    },
                    GpuBinding::Bytes {
                        binding: 1,
                        data: &bytes,
                    },
                ],
                [1, 1, 1],
                "poke",
            );
        }
        enc.commit_and_wait_completed();
        let values: Vec<u32> = read(&out, n as usize);
        assert!(
            values
                .iter()
                .enumerate()
                .all(|(i, &v)| v == i as u32 * 7 + frame),
            "frame {frame}"
        );
    }
    assert_no_validation_errors();
}

#[test]
fn parity_events_order_encoders_and_wake_the_host() {
    let device = GpuDevice::new();
    let pipeline = device.create_compute_pipeline(AFFINE, "main", "parity affine");
    let n = 128u32;
    let src = shared_f32(&device, &vec![2.0; n as usize]);
    let mid = device.create_buffer(u64::from(n) * 4);
    let out = device.create_buffer_shared(u64::from(n) * 4);
    let event = device.create_event();
    let other = event.second_handle();

    let mut producer = device.create_encoder("parity producer");
    affine(&mut producer, &pipeline, &src, &mid, 3.0, 0.0, n);
    producer.signal_event(&event);
    let value = event.encoded_value();
    assert_eq!(value, 1);
    let handled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = handled.clone();
    producer.add_completed_handler(move || flag.store(true, std::sync::atomic::Ordering::Release));
    producer.commit();

    let waiter = GpuFenceWaiter::new();
    let (woke, woken) = std::sync::mpsc::channel();
    waiter.register(&other, value, move || woke.send(()).expect("test alive"));

    let mut consumer = device.create_encoder("parity consumer");
    consumer.wait_event(&event, value);
    affine(&mut consumer, &pipeline, &mid, &out, 1.0, 1.0, n);
    // Host access between chunks, then more work in the same encoder.
    consumer.commit_wait_and_continue(&device);
    assert!(read::<f32>(&out, n as usize).iter().all(|&v| v == 7.0));
    affine(&mut consumer, &pipeline, &out, &out, 2.0, 0.0, n);
    consumer.signal_event_value(&event, 5);
    consumer.commit_and_wait_completed();

    assert!(read::<f32>(&out, n as usize).iter().all(|&v| v == 14.0));
    assert!(event.is_done(5) && other.signaled_value() == 5);
    assert!(event.wait_until_done_timeout(5, 10));
    assert!(!event.wait_until_done_timeout(6, 1));
    assert!(handled.load(std::sync::atomic::Ordering::Acquire));
    woken
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the fence waiter wakes");
    assert_no_validation_errors();
}

#[test]
fn parity_timed_waits_retirement_and_dropped_encoders() {
    let device = GpuDevice::new();
    let pipeline = device.create_compute_pipeline(AFFINE, "main", "parity affine");
    let n = 1 << 16;
    let a = device.create_buffer(u64::from(n) * 4);
    let b = device.create_buffer(u64::from(n) * 4);

    // Dropped before commit: its resources go back to the pool unsubmitted.
    let mut abandoned = device.create_encoder("parity abandoned");
    affine(&mut abandoned, &pipeline, &a, &b, 1.0, 0.0, n);
    drop(abandoned);

    let mut enc = device.create_encoder("parity timed");
    enc.clear_buffer(&a);
    for _ in 0..8 {
        affine(&mut enc, &pipeline, &a, &b, 1.0, 1.0, n);
        affine(&mut enc, &pipeline, &b, &a, 1.0, 1.0, n);
    }
    let seconds = enc.commit_and_wait_completed_timed();
    assert!((0.0..1.0).contains(&seconds), "{seconds}");
    if device.core.timestamps.is_some() {
        assert!(seconds > 0.0);
    }

    let held = Arc::new(());
    device.retire_after_queue(held.clone(), "parity retire");
    let mut enc = device.create_encoder("parity try");
    affine(&mut enc, &pipeline, &a, &b, 1.0, 0.0, n);
    enc.try_commit_and_wait_completed().expect("completes");
    // The waiter drops retired objects in order, before later waits return.
    assert_eq!(Arc::strong_count(&held), 1);
    assert_no_validation_errors();
}
