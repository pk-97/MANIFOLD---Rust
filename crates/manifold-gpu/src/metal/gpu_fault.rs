//! GPU fault registry (BUG-665r). Every command-buffer error handler
//! publishes here so hosts can react programmatically instead of parsing
//! logs. The load-bearing case: once the driver blacklists a command
//! queue ("Ignored (for causing prior/excessive GPU errors)"), every later
//! commit on that queue completes with an error and the shared event
//! never advances — the content thread wedges in permanent surface-wait
//! timeouts with no signal distinguishable from a slow GPU unless it can
//! ASK. `submissions_ignored` is that ask.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use objc2_foundation::NSError;

/// Emit Metal's encoder execution diagnostics attached to an NSError.
pub(crate) fn log_error_diagnostics(err: &NSError, buffer: &str) {
    use objc2::{msg_send, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{NSArray, NSString};
    use objc2_metal::{MTLCommandBufferEncoderInfoErrorKey, MTLCommandEncoderErrorState};
    let user_info = err.userInfo();
    let Some(infos) = user_info.objectForKey(unsafe { MTLCommandBufferEncoderInfoErrorKey }) else {
        emit_diagnostic(format_args!("[GPU] buffer {buffer}: Metal supplied no per-encoder execution details"));
        return;
    };
    // Metal documents this key as NSArray<MTLCommandBufferEncoderInfo>.
    let count: usize = unsafe { msg_send![&*infos, count] };
    for index in 0..count {
        let info: *mut AnyObject = unsafe { msg_send![&*infos, objectAtIndex: index] };
        let label: Option<Retained<NSString>> = unsafe { msg_send![info, label] };
        let state: MTLCommandEncoderErrorState = unsafe { msg_send![info, errorState] };
        let signposts: Option<Retained<NSArray<NSString>>> = unsafe { msg_send![info, debugSignposts] };
        emit_diagnostic(format_args!("[GPU] buffer {buffer} encoder[{index}] label={label:?} state={state:?} signposts={signposts:?}"));
    }
}

/// Headless proof binaries may not install a logger; don't discard their fault evidence.
fn emit_diagnostic(args: std::fmt::Arguments<'_>) {
    if log::log_enabled!(log::Level::Error) {
        log::error!("{args}");
    } else {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{args}");
    }
}

static FAULT_COUNT: AtomicU64 = AtomicU64::new(0);
static SUBMISSIONS_IGNORED: AtomicBool = AtomicBool::new(false);

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

/// Called from command-buffer completion handlers on `Error` status.
pub(crate) fn record_fault(desc: &str) {
    FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    if is_blacklist_desc(desc) {
        SUBMISSIONS_IGNORED.store(true, Ordering::Release);
    }
}

/// Total command-buffer faults observed this process.
pub fn fault_count() -> u64 {
    FAULT_COUNT.load(Ordering::Relaxed)
}

/// True once any command buffer completed with the driver's
/// queue-blacklist error. The blacklisted queue never executes again —
/// this state is unrecoverable in-process (recreating the queue is a
/// design question, not a flag flip), so hosts should fail loud, not
/// keep committing.
pub fn submissions_ignored() -> bool {
    SUBMISSIONS_IGNORED.load(Ordering::Acquire)
}

/// Opt-in incident capture; normal rendering pays only a cached flag read.
pub fn diagnostics_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("MANIFOLD_GPU_DIAGNOSTICS").as_deref() == Ok("1"))
}

/// Correlate every buffer, including asynchronous AS builds, without retaining
/// command buffers after completion. Missing completion means unknown, not hung.
pub(crate) fn trace_buffer(cb: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLCommandBuffer>) {
    if !diagnostics_enabled() { return; }
    use block2::RcBlock;
    use objc2_metal::MTLCommandBuffer;
    use std::ptr::NonNull;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let label = unsafe { cb.label() };
    let tagged = objc2_foundation::NSString::from_str(&format!("diag#{id} {}", label.as_ref().map(|v| v.to_string()).unwrap_or_default()));
    unsafe { cb.setLabel(Some(&tagged)); }
    log::info!("[GPU-DIAG] created id={id} label={label:?}");
    let scheduled = RcBlock::new(move |_cb: NonNull<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>| {
        log::info!("[GPU-DIAG] scheduled id={id} elapsed_us={}", started.elapsed().as_micros());
    });
    let complete = RcBlock::new(move |ptr: NonNull<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>| {
        let cb = unsafe { ptr.as_ref() };
        let status = unsafe { cb.status() };
        let start = unsafe { cb.GPUStartTime() };
        let end = unsafe { cb.GPUEndTime() };
        let gpu_ms = if start > 0.0 && end >= start { Some((end-start)*1000.0) } else { None };
        log::info!("[GPU-DIAG] completed id={id} label={label:?} status={status:?} elapsed_us={} gpu_ms={gpu_ms:?}", started.elapsed().as_micros());
    });
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
    use super::is_blacklist_desc;

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
