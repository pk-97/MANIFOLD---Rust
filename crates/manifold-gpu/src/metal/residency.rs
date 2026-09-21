//! Content-thread ownership of a Metal residency set.
//!
//! `MTLResidencySet` mutators are deliberately confined to the content
//! thread. Resource drops and allocations may happen elsewhere, so they send
//! registration changes through an `mpsc` channel. A lease keeps the native
//! allocation alive until its remove message has been applied.

use ahash::AHashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLAllocation, MTLDevice, MTLResidencySet, MTLResidencySetDescriptor};

use super::device::GpuDevice;

#[derive(Clone)]
pub(crate) struct ResidencySender {
    tx: Sender<ResidencyChange>,
}

enum ResidencyChange {
    Add(Arc<GpuResidencyLease>),
    Remove {
        allocation: Retained<ProtocolObject<dyn MTLAllocation>>,
        key: usize,
    },
}

struct RegisteredAllocation {
    allocation: Retained<ProtocolObject<dyn MTLAllocation>>,
    bytes: u64,
}

/// A retained Metal allocation registered with the device's residency set.
///
/// The lease is shared by resource clones and mip views. Its final drop only
/// sends a remove request; the content-thread manager performs the Metal call.
pub(crate) struct GpuResidencyLease {
    sender: ResidencySender,
    allocation: Retained<ProtocolObject<dyn MTLAllocation>>,
    key: usize,
    bytes: u64,
}

// The lease only retains an allocation and sends channel messages. Native
// resource retain/release is thread-safe; no residency-set call happens here.
unsafe impl Send for GpuResidencyLease {}
unsafe impl Sync for GpuResidencyLease {}

impl Drop for GpuResidencyLease {
    fn drop(&mut self) {
        let _ = self.sender.tx.send(ResidencyChange::Remove {
            allocation: self.allocation.clone(),
            key: self.key,
        });
    }
}

/// A copy of the manager's accounting, safe to pass across thread boundaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuResidencyStats {
    pub allocation_count: usize,
    pub allocated_bytes: u64,
    pub budget_bytes: u64,
    pub requested: bool,
}

/// The one mutable residency-set owner for a Metal device.
pub struct GpuResidencyManager {
    set: Retained<ProtocolObject<dyn MTLResidencySet>>,
    rx: Receiver<ResidencyChange>,
    allocations: AHashMap<usize, RegisteredAllocation>,
    allocated_bytes: u64,
    budget_bytes: u64,
    requested: bool,
    warned_over_budget: bool,
}

// Metal's residency set is intentionally not Sync. The manager is moved to
// the content thread once and all set calls happen in `drain` there.
unsafe impl Send for GpuResidencyManager {}

impl GpuResidencyManager {
    pub(crate) fn new(device: &GpuDevice) -> Result<(Self, ResidencySender), String> {
        // Residency sets require macOS 15 and a supporting device. Check before
        // looking up the descriptor class or invoking the newer selector.
        if objc2::runtime::AnyClass::get(c"MTLResidencySetDescriptor").is_none()
            || !unsafe {
                objc2::msg_send![device.raw_device(), respondsToSelector:
                    objc2::sel!(newResidencySetWithDescriptor:error:)]
            }
        {
            return Err("Metal residency sets are unsupported on this system".into());
        }
        let descriptor = MTLResidencySetDescriptor::new();
        let set = device
            .raw_device()
            .newResidencySetWithDescriptor_error(&descriptor)
            .map_err(|error| format!("MTLResidencySet creation failed: {error:?}"))?;
        // Explicit requests prepare memory ahead of command submission. Do not
        // attach the set to the queue: every commit would then request the
        // entire set even when our working-set budget has disabled preparation.
        let (tx, rx) = channel();
        let sender = ResidencySender { tx };
        Ok((
            Self {
                set,
                rx,
                allocations: AHashMap::new(),
                allocated_bytes: 0,
                budget_bytes: device.raw_device().recommendedMaxWorkingSetSize(),
                requested: false,
                warned_over_budget: false,
            },
            sender,
        ))
    }

    /// Apply queued registration changes. Call once from the content thread.
    pub fn drain(&mut self) {
        let mut changed = false;
        while let Ok(change) = self.rx.try_recv() {
            match change {
                ResidencyChange::Add(lease) => {
                    if self.allocations.contains_key(&lease.key) {
                        continue;
                    }
                    self.set.addAllocation(&lease.allocation);
                    self.allocated_bytes = self.allocated_bytes.saturating_add(lease.bytes);
                    self.allocations.insert(
                        lease.key,
                        RegisteredAllocation {
                            allocation: lease.allocation.clone(),
                            bytes: lease.bytes,
                        },
                    );
                    changed = true;
                }
                ResidencyChange::Remove { allocation, key } => {
                    if let Some(existing) = self.allocations.remove(&key) {
                        self.set.removeAllocation(&existing.allocation);
                        self.allocated_bytes = self.allocated_bytes.saturating_sub(existing.bytes);
                        changed = true;
                    }
                    drop(allocation);
                }
            }
        }
        if !changed {
            return;
        }

        let over_budget = self.budget_bytes == 0 || self.allocated_bytes > self.budget_bytes;
        if over_budget {
            if !self.warned_over_budget {
                log::warn!(
                    "GPU residency request exceeds recommended working-set budget: {} > {} bytes",
                    self.allocated_bytes,
                    self.budget_bytes
                );
                self.warned_over_budget = true;
            }
            if self.requested {
                self.set.endResidency();
                self.requested = false;
            }
        } else {
            self.warned_over_budget = false;
        }

        if self.allocations.is_empty() && self.requested {
            self.set.endResidency();
            self.requested = false;
        }
        self.set.commit();
        if !over_budget && !self.allocations.is_empty() && !self.requested {
            self.set.requestResidency();
            self.requested = true;
        }
    }

    pub fn stats(&self) -> GpuResidencyStats {
        GpuResidencyStats {
            allocation_count: self.allocations.len(),
            allocated_bytes: self.allocated_bytes,
            budget_bytes: self.budget_bytes,
            requested: self.requested,
        }
    }
}

impl Drop for GpuResidencyManager {
    fn drop(&mut self) {
        if self.requested {
            self.set.endResidency();
            self.requested = false;
        }
        if !self.allocations.is_empty() {
            self.set.removeAllAllocations();
            self.set.commit();
        }
    }
}

pub(crate) fn lease_for_allocation(
    sender: &ResidencySender,
    allocation: Retained<ProtocolObject<dyn MTLAllocation>>,
) -> Arc<GpuResidencyLease> {
    let key = Retained::as_ptr(&allocation) as *const () as usize;
    let lease = Arc::new(GpuResidencyLease {
        sender: sender.clone(),
        bytes: allocation.allocatedSize() as u64,
        allocation,
        key,
    });
    let _ = sender.tx.send(ResidencyChange::Add(lease.clone()));
    lease
}

pub(crate) fn lease_for_texture(
    sender: &ResidencySender,
    raw: &ProtocolObject<dyn objc2_metal::MTLTexture>,
) -> Arc<GpuResidencyLease> {
    let ptr = raw as *const ProtocolObject<dyn objc2_metal::MTLTexture>
        as *mut ProtocolObject<dyn MTLAllocation>;
    let allocation = unsafe { Retained::retain(ptr) }.expect("Metal texture retain returned nil");
    lease_for_allocation(sender, allocation)
}

pub(crate) fn lease_for_buffer(
    sender: &ResidencySender,
    raw: &ProtocolObject<dyn objc2_metal::MTLBuffer>,
) -> Arc<GpuResidencyLease> {
    let ptr = raw as *const ProtocolObject<dyn objc2_metal::MTLBuffer>
        as *mut ProtocolObject<dyn MTLAllocation>;
    let allocation = unsafe { Retained::retain(ptr) }.expect("Metal buffer retain returned nil");
    lease_for_allocation(sender, allocation)
}

pub(crate) fn lease_for_heap(
    sender: &ResidencySender,
    raw: &ProtocolObject<dyn objc2_metal::MTLHeap>,
) -> Arc<GpuResidencyLease> {
    let ptr = raw as *const ProtocolObject<dyn objc2_metal::MTLHeap>
        as *mut ProtocolObject<dyn MTLAllocation>;
    let allocation = unsafe { Retained::retain(ptr) }.expect("Metal heap retain returned nil");
    lease_for_allocation(sender, allocation)
}

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "residency_tests.rs"]
mod tests;
