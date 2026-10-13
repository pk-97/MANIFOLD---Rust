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
pub(crate) fn emit_diagnostic(args: std::fmt::Arguments<'_>) {
    if log::log_enabled!(log::Level::Error) {
        log::error!("{args}");
    } else {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{args}");
    }
}

static FAULT_COUNT: AtomicU64 = AtomicU64::new(0);
static SUBMISSIONS_IGNORED: AtomicBool = AtomicBool::new(false);
// Completion handlers must never contend on a lock: they execute on Metal's
// callback threads and may be invoked while the process is tearing down.
static IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
static COMPLETED: AtomicU64 = AtomicU64::new(0);

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

/// What a failed command buffer was, decided once where the error is read.
/// Hosts branch on this instead of parsing log text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// The GPU watchdog killed the buffer (kIOGPUCommandBufferCallbackErrorHang,
    /// or Metal's own Timeout). The device is healthy; resubmitting can work.
    Hang,
    /// Bad address. GPU memory can no longer be trusted.
    PageFault,
    /// The device went away.
    DeviceLost,
    /// The driver blacklisted the queue; it never runs again.
    SubmissionsIgnored,
    /// Discarded because another buffer faulted. Never the cause.
    InnocentVictim,
    /// Anything else (out of memory, invalid resource, internal error).
    Other,
}

impl FaultKind {
    const ALL: [FaultKind; 6] = [
        FaultKind::Hang,
        FaultKind::PageFault,
        FaultKind::DeviceLost,
        FaultKind::SubmissionsIgnored,
        FaultKind::InnocentVictim,
        FaultKind::Other,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// Short name for user-facing error text.
    pub fn describe(self) -> &'static str {
        match self {
            FaultKind::Hang => "GPU hang",
            FaultKind::PageFault => "GPU page fault",
            FaultKind::DeviceLost => "GPU device lost",
            FaultKind::SubmissionsIgnored => "GPU queue blacklisted by the driver",
            FaultKind::InnocentVictim => "GPU work discarded after another fault",
            FaultKind::Other => "GPU error",
        }
    }
}

/// Classify a command-buffer error. The kIOGPU token in the description is
/// the stable identifier (its hex prefix is IOGPU's own numbering, which
/// differs from `NSError::code`); the public Metal codes cover errors that
/// carry no token.
pub(crate) fn classify_fault(code: i64, desc: &str) -> FaultKind {
    if is_blacklist_desc(desc) {
        FaultKind::SubmissionsIgnored
    } else if desc.contains("ErrorHang") {
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

static KIND_COUNTS: [AtomicU64; 6] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// The fault kinds seen since a [`FaultMark`] was taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultKinds(u8);

impl FaultKinds {
    pub fn with(self, kind: FaultKind) -> Self {
        FaultKinds(self.0 | 1 << kind.index())
    }
    pub fn contains(self, kind: FaultKind) -> bool {
        self.0 & (1 << kind.index()) != 0
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    /// The device or queue cannot be trusted: stop the process.
    pub fn is_fatal(self) -> bool {
        self.contains(FaultKind::PageFault)
            || self.contains(FaultKind::DeviceLost)
            || self.contains(FaultKind::SubmissionsIgnored)
    }
    /// A hang and nothing worse: resubmitting the work is reasonable.
    pub fn is_recoverable_hang(self) -> bool {
        self.contains(FaultKind::Hang) && !self.is_fatal() && !self.contains(FaultKind::Other)
    }
    /// The most telling kind, for an error message.
    pub fn headline(self) -> Option<FaultKind> {
        [
            FaultKind::DeviceLost,
            FaultKind::PageFault,
            FaultKind::SubmissionsIgnored,
            FaultKind::Hang,
            FaultKind::Other,
            FaultKind::InnocentVictim,
        ]
        .into_iter()
        .find(|kind| self.contains(*kind))
    }
}

/// Snapshot of the fault counters; ask it what happened afterwards.
#[derive(Clone, Copy, Debug)]
pub struct FaultMark {
    total: u64,
    kinds: [u64; 6],
}

impl FaultMark {
    pub fn total(&self) -> u64 {
        self.total
    }
    /// Kinds recorded since this mark was taken.
    pub fn kinds_since(&self) -> FaultKinds {
        let now = mark();
        FaultKind::ALL.iter().fold(FaultKinds::default(), |set, kind| {
            if now.kinds[kind.index()] != self.kinds[kind.index()] {
                set.with(*kind)
            } else {
                set
            }
        })
    }
}

/// Take a snapshot of the fault counters.
pub fn mark() -> FaultMark {
    FaultMark {
        total: FAULT_COUNT.load(Ordering::Relaxed),
        kinds: std::array::from_fn(|i| KIND_COUNTS[i].load(Ordering::Acquire)),
    }
}

/// Called from command-buffer completion handlers on `Error` status.
pub(crate) fn record_fault(code: i64, desc: &str) {
    let kind = classify_fault(code, desc);
    KIND_COUNTS[kind.index()].fetch_add(1, Ordering::Release);
    FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    if kind == FaultKind::SubmissionsIgnored {
        SUBMISSIONS_IGNORED.store(true, Ordering::Release);
    }
}

/// Mark a command buffer before committing it. The release increment pairs
/// with `complete_submission` so a fatal-exit drain observes all diagnostics
/// published by the callback.
pub(crate) fn begin_submission() {
    IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
}

/// Mark completion only after logging and publishing the command-buffer error.
pub(crate) fn complete_submission() {
    COMPLETED.fetch_add(1, Ordering::Release);
}

/// Bounded fatal-path drain. Healthy completion callbacks do not wait; this
/// is only used while exiting after a GPU fault to avoid callback-after-free.
pub fn drain_completions(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if COMPLETED.load(Ordering::Acquire) >= IN_FLIGHT.load(Ordering::Acquire) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
pub(crate) fn reset_submission_counters_for_test() {
    IN_FLIGHT.store(0, Ordering::Release);
    COMPLETED.store(0, Ordering::Release);
}

/// Total command-buffer faults observed this process. This is an observation
/// count, not a positive attribution of the originating encoder: Metal also
/// reports innocent victims after a prior fault.
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
    use super::{classify_fault, complete_submission, begin_submission, drain_completions, is_blacklist_desc,
        reset_submission_counters_for_test, FaultKind, FaultKinds};

    #[test]
    fn real_descriptions_classify_by_kind() {
        let hang = "Caused GPU Hang Error (00000003:kIOGPUCommandBufferCallbackErrorHang)";
        let page = "Caused GPU Address Fault Error (0000000b:kIOGPUCommandBufferCallbackErrorPageFault)";
        let victim = "Discarded (victim of GPU error/recovery) (00000005:kIOGPUCommandBufferCallbackErrorInnocentVictim)";
        let ignored = "Ignored (for causing prior/excessive GPU errors) (00000004:kIOGPUCommandBufferCallbackErrorSubmissionsIgnored)";
        // NSError::code is deliberately wrong here: the token decides.
        assert_eq!(classify_fault(1, hang), FaultKind::Hang);
        assert_eq!(classify_fault(1, page), FaultKind::PageFault);
        assert_eq!(classify_fault(1, victim), FaultKind::InnocentVictim);
        assert_eq!(classify_fault(1, ignored), FaultKind::SubmissionsIgnored);
        assert_eq!(classify_fault(8, "Insufficient Memory"), FaultKind::Other);
        assert_eq!(classify_fault(2, "Execution timed out"), FaultKind::Hang);
        assert_eq!(classify_fault(11, "Device removed"), FaultKind::DeviceLost);
    }

    #[test]
    fn recovery_policy_follows_the_worst_kind() {
        let hang = FaultKinds::default().with(FaultKind::Hang);
        let hang_and_victim = hang.with(FaultKind::InnocentVictim);
        assert!(hang.is_recoverable_hang() && hang_and_victim.is_recoverable_hang());
        assert!(!hang_and_victim.is_fatal());
        assert!(hang.with(FaultKind::PageFault).is_fatal());
        assert!(!hang.with(FaultKind::PageFault).is_recoverable_hang());
        assert!(hang.with(FaultKind::DeviceLost).is_fatal());
        assert!(hang.with(FaultKind::SubmissionsIgnored).is_fatal());
        assert!(!hang.with(FaultKind::Other).is_recoverable_hang());
        assert!(!FaultKinds::default().with(FaultKind::Other).is_fatal());
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

    #[test]
    fn completion_drain_tracks_callbacks_without_waiting() {
        reset_submission_counters_for_test();
        begin_submission();
        assert!(!drain_completions(std::time::Duration::ZERO));
        complete_submission();
        assert!(drain_completions(std::time::Duration::ZERO));
    }
}
