//! Metal's presented handler for the stage output, routed to the content
//! thread as `ContentCommand::FramePresented` (the screen delay, D19) and to
//! the sync rig log. The handler runs on Metal's thread and takes a plain
//! function, so the command sender is installed once, here.

use std::sync::OnceLock;

use crossbeam_channel::Sender;

use crate::content_command::ContentCommand;

static TX: OnceLock<Sender<ContentCommand>> = OnceLock::new();

/// Once, before the content thread starts.
pub fn install(tx: &Sender<ContentCommand>) {
    if TX.set(tx.clone()).is_err() {
        return;
    }
    #[cfg(target_os = "macos")]
    manifold_gpu::set_presented_observer(on_presented);
}

#[cfg(target_os = "macos")]
fn on_presented(drawable_id: u64, presented_s: f64) {
    use manifold_playback::rig_log;
    rig_log::emit(rig_log::RigRecord::Presented { drawable_id, presented_s, at_ns: rig_log::now_ns() });
    if let (Some(tx), Some(at)) = (TX.get(), host_time::instant_of(presented_s)) {
        let _ = tx.send(ContentCommand::FramePresented { drawable_id, at });
    }
}

#[cfg(target_os = "macos")]
mod host_time {
    use std::time::{Duration, Instant};

    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }

    /// `presentedTime` (host seconds on the `mach_absolute_time` base) as an
    /// `Instant`, by one paired read of both clocks. 0 means never shown.
    pub fn instant_of(host_s: f64) -> Option<Instant> {
        if !(host_s.is_finite() && host_s > 0.0) {
            return None;
        }
        let mut tb = Timebase { numer: 0, denom: 0 };
        // SAFETY: writes the two fields of a valid struct.
        if unsafe { mach_timebase_info(&mut tb) } != 0 || tb.denom == 0 {
            return None;
        }
        let now = Instant::now();
        // SAFETY: a plain clock read.
        let ticks = unsafe { mach_absolute_time() };
        let host_now = ticks as f64 * f64::from(tb.numer) / f64::from(tb.denom) / 1e9;
        let back = host_now - host_s;
        if back >= 0.0 {
            now.checked_sub(Duration::from_secs_f64(back))
        } else {
            Some(now + Duration::from_secs_f64(-back))
        }
    }
}
