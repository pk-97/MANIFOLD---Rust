//! Reading Metal's command-buffer errors: the fault kind, the per-encoder
//! diagnostics, and opt-in buffer tracing. The counters live in
//! [`crate::gpu_fault`].

use crate::gpu_fault::{FaultKind, diagnostics_enabled, emit_diagnostic};
use objc2_foundation::NSError;
use std::sync::atomic::{AtomicU64, Ordering};

/// Emit Metal's encoder execution diagnostics attached to an NSError.
pub(crate) fn log_error_diagnostics(err: &NSError, buffer: &str) {
    use objc2::{msg_send, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{NSArray, NSString};
    use objc2_metal::{MTLCommandBufferEncoderInfoErrorKey, MTLCommandEncoderErrorState};
    let user_info = err.userInfo();
    let Some(infos) = user_info.objectForKey(unsafe { MTLCommandBufferEncoderInfoErrorKey }) else {
        emit_diagnostic(format_args!(
            "[GPU] buffer {buffer}: Metal supplied no per-encoder execution details"
        ));
        return;
    };
    // Metal documents this key as NSArray<MTLCommandBufferEncoderInfo>.
    let count: usize = unsafe { msg_send![&*infos, count] };
    for index in 0..count {
        let info: *mut AnyObject = unsafe { msg_send![&*infos, objectAtIndex: index] };
        let label: Option<Retained<NSString>> = unsafe { msg_send![info, label] };
        let state: MTLCommandEncoderErrorState = unsafe { msg_send![info, errorState] };
        let signposts: Option<Retained<NSArray<NSString>>> =
            unsafe { msg_send![info, debugSignposts] };
        emit_diagnostic(format_args!(
            "[GPU] buffer {buffer} encoder[{index}] label={label:?} state={state:?} signposts={signposts:?}"
        ));
    }
}

/// True for the driver's queue-blacklist error description. The blacklist
/// signature has no stable numeric code exposed to us, so match the
/// description text — it is the only observable the logs carry. The
/// observed text (BUG-84fv, 2026-08-02 incident) is "Ignored (for causing
/// prior/excessive GPU errors)
/// (00000004:kIOGPUCommandBufferCallbackErrorSubmissionsIgnored)" — BUG-665r
/// shipped matching "Ignored for causing prior", which the parenthesis in
/// the real string defeats, so the wedge guard never fired in the field.
/// Match the two fragments that survive Apple's punctuation: the reason
/// clause and the kIOGPU code.
fn is_blacklist_desc(desc: &str) -> bool {
    desc.contains("for causing prior") || desc.contains("SubmissionsIgnored")
}

/// Classify a command-buffer error. The kIOGPU token in the description is
/// the stable identifier (its hex prefix is IOGPU's own numbering, which
/// differs from `NSError::code`); the public Metal codes cover errors that
/// carry no token.
pub(crate) fn classify_fault(code: i64, desc: &str) -> FaultKind {
    if is_blacklist_desc(desc) {
        FaultKind::SubmissionsIgnored
    } else if desc.contains("ErrorHang") || desc.contains("ErrorImpactingInteractivity") {
        FaultKind::Hang
    } else if desc.contains("ErrorPageFault") {
        FaultKind::PageFault
    } else if desc.contains("InnocentVictim") {
        FaultKind::InnocentVictim
    } else if desc.contains("ErrorDeviceRemoved") {
        FaultKind::DeviceLost
    } else if desc.contains("kIOGPU") {
        FaultKind::Other
    } else {
        // MTLCommandBufferError: Timeout 2, PageFault 3, DeviceRemoved 11.
        match code {
            2 => FaultKind::Hang,
            3 => FaultKind::PageFault,
            11 => FaultKind::DeviceLost,
            _ => FaultKind::Other,
        }
    }
}

/// Decode a failed command buffer's error and record it.
pub(crate) fn record_fault(code: i64, desc: &str) {
    crate::gpu_fault::record_fault(classify_fault(code, desc));
}

/// Correlate every buffer, including asynchronous AS builds, without retaining
/// command buffers after completion. Missing completion means unknown, not hung.
pub(crate) fn trace_buffer(cb: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLCommandBuffer>) {
    if !diagnostics_enabled() {
        return;
    }
    use block2::RcBlock;
    use objc2_metal::MTLCommandBuffer;
    use std::ptr::NonNull;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let label = unsafe { cb.label() };
    let tagged = objc2_foundation::NSString::from_str(&format!(
        "diag#{id} {}",
        label.as_ref().map(|v| v.to_string()).unwrap_or_default()
    ));
    unsafe {
        cb.setLabel(Some(&tagged));
    }
    log::info!("[GPU-DIAG] created id={id} label={label:?}");
    let scheduled = RcBlock::new(
        move |_cb: NonNull<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>| {
            log::info!(
                "[GPU-DIAG] scheduled id={id} elapsed_us={}",
                started.elapsed().as_micros()
            );
        },
    );
    let complete = RcBlock::new(
        move |ptr: NonNull<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>| {
            let cb = unsafe { ptr.as_ref() };
            let status = unsafe { cb.status() };
            let start = unsafe { cb.GPUStartTime() };
            let end = unsafe { cb.GPUEndTime() };
            let gpu_ms = if start > 0.0 && end >= start {
                Some((end - start) * 1000.0)
            } else {
                None
            };
            log::info!(
                "[GPU-DIAG] completed id={id} label={label:?} status={status:?} elapsed_us={} gpu_ms={gpu_ms:?}",
                started.elapsed().as_micros()
            );
        },
    );
    unsafe {
        cb.addScheduledHandler(RcBlock::as_ptr(&scheduled));
        cb.addCompletedHandler(RcBlock::as_ptr(&complete));
    }
}

#[cfg(test)]
mod tests {
    // The exact description strings the driver produced in the BUG-84fv
    // incident log (2026-08-02) — pinning the REAL text, not a remembered
    // paraphrase, is what the BUG-665r original lacked.
    use super::{classify_fault, is_blacklist_desc};
    use crate::gpu_fault::{FaultKind, FaultKinds};

    #[test]
    fn real_descriptions_classify_by_kind() {
        let hang = "Caused GPU Hang Error (00000003:kIOGPUCommandBufferCallbackErrorHang)";
        let page =
            "Caused GPU Address Fault Error (0000000b:kIOGPUCommandBufferCallbackErrorPageFault)";
        let victim = "Discarded (victim of GPU error/recovery) (00000005:kIOGPUCommandBufferCallbackErrorInnocentVictim)";
        let ignored = "Ignored (for causing prior/excessive GPU errors) (00000004:kIOGPUCommandBufferCallbackErrorSubmissionsIgnored)";
        let interactivity = "Impacting Interactivity (0000000e:kIOGPUCommandBufferCallbackErrorImpactingInteractivity)";
        // NSError::code is deliberately wrong here: the token decides.
        assert_eq!(classify_fault(1, hang), FaultKind::Hang);
        assert_eq!(classify_fault(1, page), FaultKind::PageFault);
        assert_eq!(classify_fault(1, victim), FaultKind::InnocentVictim);
        assert_eq!(classify_fault(1, ignored), FaultKind::SubmissionsIgnored);
        assert_eq!(classify_fault(1, interactivity), FaultKind::Hang);
        assert!(
            FaultKinds::default()
                .with(classify_fault(1, interactivity))
                .is_recoverable_hang()
        );
        assert_eq!(classify_fault(8, "Insufficient Memory"), FaultKind::Other);
        assert_eq!(classify_fault(2, "Execution timed out"), FaultKind::Hang);
        assert_eq!(classify_fault(11, "Device removed"), FaultKind::DeviceLost);
    }

    #[test]
    fn blacklist_description_matches() {
        assert!(is_blacklist_desc(
            "Ignored (for causing prior/excessive GPU errors) \
             (00000004:kIOGPUCommandBufferCallbackErrorSubmissionsIgnored)"
        ));
    }

    #[test]
    fn incident_sessions_other_faults_do_not_match() {
        assert!(!is_blacklist_desc(
            "Discarded (victim of GPU error/recovery) \
             (00000005:kIOGPUCommandBufferCallbackErrorInnocentVictim)"
        ));
        assert!(!is_blacklist_desc(
            "Caused GPU Hang Error (00000003:kIOGPUCommandBufferCallbackErrorHang)"
        ));
        assert!(!is_blacklist_desc(
            "Caused GPU Address Fault Error \
             (0000000b:kIOGPUCommandBufferCallbackErrorPageFault)"
        ));
    }
}
