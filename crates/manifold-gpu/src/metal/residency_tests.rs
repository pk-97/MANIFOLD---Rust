use objc2_metal::MTLResidencySet;

use super::super::TexturePool;
use super::super::device::GpuDevice;
use super::super::retire::{RetireMark, RetireQueue};
use crate::{GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

fn texture_desc(mip_levels: u32) -> GpuTextureDesc<'static> {
    GpuTextureDesc {
        width: 32,
        height: 32,
        depth: 1,
        format: GpuTextureFormat::Rgba8Unorm,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::SHADER_READ,
        label: "residency-proof",
        mip_levels,
    }
}

fn residency_manager(device: &GpuDevice) -> super::GpuResidencyManager {
    device
        .create_residency_manager()
        .expect("GPU residency proofs require a supporting Metal device")
}

#[test]
fn clones_and_mip_views_keep_one_registration_until_last_drop() {
    let device = GpuDevice::new();
    let mut manager = residency_manager(&device);

    let texture = device.create_texture(&texture_desc(2));
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);

    let clone = texture.clone();
    let view = texture.mip_level_view(1, 16, 16);
    drop(texture);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    drop(clone);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    drop(view);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 0);
}

#[test]
fn fence_retirement_holds_lease_until_completion() {
    let device = GpuDevice::new();
    let mut manager = residency_manager(&device);
    let event = device.create_event();
    let (sender, mut retire_queue) = RetireQueue::new();
    device.set_retirement(RetireMark::new(event.second_handle(), sender));

    let buffer = device.create_buffer(4096);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    drop(buffer);
    retire_queue.drain();
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);

    let mut encoder = device.create_encoder("residency-retirement-proof");
    encoder.signal_event(&event);
    encoder.commit_and_wait_completed();
    retire_queue.drain();
    manager.drain();
    assert_eq!(retire_queue.pending_count(), 0);
    assert_eq!(manager.stats().allocation_count, 0);
}

#[test]
fn heap_children_share_one_registration() {
    let device = GpuDevice::new();
    let mut manager = residency_manager(&device);

    let heap = device.create_heap(128 * 1024, crate::GpuStorageMode::Private);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    let child = heap
        .new_texture(&texture_desc(1))
        .expect("heap should fit proof texture");
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    drop(heap);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    drop(child);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 0);
}

#[test]
fn pool_recycle_keeps_registration_alive() {
    let device = GpuDevice::new();
    let mut manager = residency_manager(&device);
    let pool = TexturePool::new(&device, 1);

    let first = pool.acquire(
        32,
        32,
        GpuTextureFormat::Rgba8Unorm,
        GpuTextureUsage::RENDER_TARGET_FULL,
        "residency-pool-proof",
    );
    let identity = first.identity_key();
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    pool.release(first);
    manager.drain();
    pool.begin_frame();
    let recycled = pool.acquire(
        32,
        32,
        GpuTextureFormat::Rgba8Unorm,
        GpuTextureUsage::RENDER_TARGET_FULL,
        "residency-pool-proof",
    );
    assert_eq!(recycled.identity_key(), identity);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    drop(recycled);
    pool.clear();
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 0);
}

#[test]
fn budget_transition_ends_and_restores_residency() {
    let device = GpuDevice::new();
    let mut manager = residency_manager(&device);
    let retained = device.create_buffer(4096);
    manager.drain();
    assert!(manager.stats().requested);
    device
        .create_encoder("residency-attached")
        .try_commit_and_wait_completed()
        .unwrap();
    manager.budget_bytes = manager.stats().allocated_bytes;

    let extra = device.create_buffer(64 * 1024);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 2);
    assert!(!manager.stats().requested);
    device
        .create_encoder("residency-over-budget-detached")
        .try_commit_and_wait_completed()
        .unwrap();

    drop(extra);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 1);
    assert!(manager.stats().requested);
    device
        .create_encoder("residency-reattached")
        .try_commit_and_wait_completed()
        .unwrap();
    drop(retained);
    manager.drain();
    assert_eq!(manager.stats().allocation_count, 0);
    assert!(!manager.stats().requested);

    manager.budget_bytes = 0;
    let _buffer = device.create_buffer(4096);
    manager.drain();
    assert!(!manager.stats().requested);
}

#[test]
fn manager_drop_clears_native_set() {
    let device = GpuDevice::new();
    let mut manager = residency_manager(&device);
    let set = manager.set.clone();
    let buffer = device.create_buffer(4096);
    manager.drain();
    assert_eq!(set.allocationCount(), 1);
    drop(manager);
    assert_eq!(set.allocationCount(), 0);
    device
        .create_encoder("residency-manager-dropped")
        .try_commit_and_wait_completed()
        .unwrap();
    drop(buffer);
}
