//! The Vulkan parity kit (design section 10): one test per backend
//! primitive, each on a tiny input, each ending with no validation errors.
//! Run on KosmicKrisp with synchronization validation:
//! `VK_DRIVER_FILES=/usr/local/share/vulkan/icd.d/libkosmickrisp_icd.json
//! MANIFOLD_VULKAN_VALIDATION=sync cargo nextest run -p manifold-gpu
//! --features vulkan parity`.

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
