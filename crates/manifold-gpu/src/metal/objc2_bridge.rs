//! Bridging helpers between the `metal` crate (via `foreign_types`) and
//! the `objc2-metal` ecosystem. Used while the two coexist inside
//! manifold-gpu — remove once the full migration to `objc2-metal` lands.
//!
//! Both representations are pointers to the same Objective-C object. The
//! casts here are safe in the sense that they don't touch memory; the
//! `unsafe` is for the type-system lie that the opaque `metal::*Ref` is
//! structurally identical to `ProtocolObject<dyn MTL*>`. In practice both
//! are `NSObject*` at runtime, and objc2's `ProtocolObject<dyn T>` is
//! repr-transparent over `AnyObject`.

use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLDevice, MTLTexture};

/// Borrow a `metal::DeviceRef` as an `objc2-metal` device reference.
///
/// # Safety
/// The returned reference is valid for the same lifetime as `dev`. The
/// underlying Objective-C object must still respond to `MTLDevice` selectors.
#[inline]
pub(crate) unsafe fn device_as_objc2(
    dev: &metal::DeviceRef,
) -> &ProtocolObject<dyn MTLDevice> {
    unsafe { &*(dev as *const metal::DeviceRef as *const ProtocolObject<dyn MTLDevice>) }
}

/// Borrow a `metal::TextureRef` as an `objc2-metal` texture reference.
///
/// # Safety
/// Same as `device_as_objc2`.
#[inline]
pub(crate) unsafe fn texture_as_objc2(
    tex: &metal::TextureRef,
) -> &ProtocolObject<dyn MTLTexture> {
    unsafe { &*(tex as *const metal::TextureRef as *const ProtocolObject<dyn MTLTexture>) }
}

/// Borrow a `metal::CommandBufferRef` as an `objc2-metal` command buffer reference.
///
/// # Safety
/// Same as `device_as_objc2`.
#[inline]
pub(crate) unsafe fn cmd_buf_as_objc2(
    cb: &metal::CommandBufferRef,
) -> &ProtocolObject<dyn MTLCommandBuffer> {
    unsafe {
        &*(cb as *const metal::CommandBufferRef as *const ProtocolObject<dyn MTLCommandBuffer>)
    }
}
