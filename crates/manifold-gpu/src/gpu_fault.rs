//! GPU fault registry (BUG-665r). Every command-buffer error handler
//! publishes here so hosts can react programmatically instead of parsing
//! logs. The load-bearing case: once the driver blacklists a command
//! queue ("Ignored (for causing prior/excessive GPU errors)"), every later
//! commit on that queue completes with an error and the shared event
//! never advances — the content thread wedges in permanent surface-wait
//! timeouts with no signal distinguishable from a slow GPU unless it can
//! ASK. `submissions_ignored` is that ask.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

/// What a failed command buffer was, decided once where the error is read.
/// Hosts branch on this instead of parsing log text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// The GPU watchdog killed the buffer (kIOGPUCommandBufferCallbackErrorHang,
    /// kIOGPUCommandBufferCallbackErrorImpactingInteractivity, or Metal's own
    /// Timeout). The device is healthy; resubmitting can work.
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
    /// A hang, or work discarded by the GPU's recovery from a hang we did not
    /// see reported, and nothing worse: resubmitting the work is reasonable.
    pub fn is_recoverable_hang(self) -> bool {
        (self.contains(FaultKind::Hang) || self.contains(FaultKind::InnocentVictim))
            && !self.is_fatal()
            && !self.contains(FaultKind::Other)
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
        FaultKind::ALL
            .iter()
            .fold(FaultKinds::default(), |set, kind| {
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

/// Called from command-buffer completion handlers on `Error` status, with
/// the kind the backend decoded from its error.
pub(crate) fn record_fault(kind: FaultKind) {
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
/// count, not a positive attribution of the originating encoder: the driver
/// also reports innocent victims after a prior fault.
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

#[cfg(test)]
mod tests {
    use super::{
        FaultKind, FaultKinds, begin_submission, complete_submission, drain_completions,
        reset_submission_counters_for_test,
    };

    #[test]
    fn recovery_policy_follows_the_worst_kind() {
        let hang = FaultKinds::default().with(FaultKind::Hang);
        let hang_and_victim = hang.with(FaultKind::InnocentVictim);
        assert!(hang.is_recoverable_hang() && hang_and_victim.is_recoverable_hang());
        assert!(
            FaultKinds::default()
                .with(FaultKind::InnocentVictim)
                .is_recoverable_hang()
        );
        assert!(!hang_and_victim.is_fatal());
        assert!(hang.with(FaultKind::PageFault).is_fatal());
        assert!(!hang.with(FaultKind::PageFault).is_recoverable_hang());
        assert!(hang.with(FaultKind::DeviceLost).is_fatal());
        assert!(hang.with(FaultKind::SubmissionsIgnored).is_fatal());
        assert!(!hang.with(FaultKind::Other).is_recoverable_hang());
        assert!(!FaultKinds::default().with(FaultKind::Other).is_fatal());
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
