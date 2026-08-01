//! GPU fault registry (BUG-665r). Every command-buffer error handler
//! publishes here so hosts can react programmatically instead of parsing
//! logs. The load-bearing case: once the driver blacklists a command
//! queue ("Ignored for causing prior/excessive GPU errors"), every later
//! commit on that queue completes with an error and the shared event
//! never advances — the content thread wedges in permanent surface-wait
//! timeouts with no signal distinguishable from a slow GPU unless it can
//! ASK. `submissions_ignored` is that ask.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static FAULT_COUNT: AtomicU64 = AtomicU64::new(0);
static SUBMISSIONS_IGNORED: AtomicBool = AtomicBool::new(false);

/// Called from command-buffer completion handlers on `Error` status.
/// The blacklist signature has no stable numeric code exposed to us, so
/// match the driver's description text — it is the only observable the
/// logs (and the BUG-665r repro) carry.
pub(crate) fn record_fault(desc: &str) {
    FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    if desc.contains("Ignored for causing prior") {
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
