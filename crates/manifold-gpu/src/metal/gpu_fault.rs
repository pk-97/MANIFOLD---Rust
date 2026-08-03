//! GPU fault registry (BUG-665r). Every command-buffer error handler
//! publishes here so hosts can react programmatically instead of parsing
//! logs. The load-bearing case: once the driver blacklists a command
//! queue ("Ignored (for causing prior/excessive GPU errors)"), every later
//! commit on that queue completes with an error and the shared event
//! never advances — the content thread wedges in permanent surface-wait
//! timeouts with no signal distinguishable from a slow GPU unless it can
//! ASK. `submissions_ignored` is that ask.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
