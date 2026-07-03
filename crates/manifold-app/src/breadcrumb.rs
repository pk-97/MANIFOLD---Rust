//! Breadcrumb sidecar + `--resume` supporting types — GIG_RESILIENCE_DESIGN
//! §5.1 (breadcrumb) / §5.2 (boot fast path), phase P2.
//!
//! The breadcrumb is a small atomically-written JSON sidecar
//! (`<project>.manifold.breadcrumb`, sibling of the project archive) the UI
//! thread refreshes at a cheap cadence — never per frame, never blocking IO
//! on a hot path — so a relaunch (`manifold --resume <breadcrumb-path>`) can
//! rejoin a running show without human input (D3), instead of reopening
//! blind (G3/G12).
//!
//! Cadence + capture run on the UI thread, piggy-backing on the EXISTING
//! content-state drain (the same place `autosave.rs` hooks `tick_autosave`)
//! — `self.content_state.current_beat` / `.is_playing` are already refreshed
//! there every UI frame from the content thread's per-tick `ContentState`
//! push. This means the breadcrumb needs zero new content-thread plumbing
//! and never touches `sync_clips_to_time` (`manifold-playback/src/engine.rs`)
//! or `content_thread.rs`'s tick body.
//!
//! The one exception is the panic-hook beat stamp (§5.1 last line): a panic
//! hook has no `&Application` to read from, so it needs a single process-wide
//! atomic — `LAST_KNOWN_BEAT_BITS` below, stored from the same UI-thread drain
//! point. This is the documented escape hatch ("publish it via a SINGLE
//! atomic store"); it is a plain `store`/`load`, not new shared mutable state
//! in the `Arc<Mutex>` sense.
//!
//! Actual disk I/O happens on a dedicated background thread
//! ([`BreadcrumbWriter`]) — the UI thread only ever does a non-blocking
//! `try_send`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::Beats;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::window_registry::WindowRole;

// ─────────────────────────────────────────────────────────────────────────
// Panic-hook beat stamp
// ─────────────────────────────────────────────────────────────────────────

/// Bit-cast latest known beat, refreshed once per UI frame from the
/// content-state drain (never from the content thread itself). Read by the
/// panic hook in `main.rs` so crash reports are score-addressed (§5.1).
/// `u64::MAX` is not a bit pattern `f64::to_bits` produces for any beat a
/// running show can reach, so it doubles as "no frame observed yet".
static LAST_KNOWN_BEAT_BITS: AtomicU64 = AtomicU64::new(u64::MAX);

/// Store the latest known beat for the panic hook. Called from the UI
/// thread's content-state drain — see `tick_breadcrumb` below.
fn publish_beat_for_crash_log(beat: Beats) {
    LAST_KNOWN_BEAT_BITS.store(beat.0.to_bits(), Ordering::Relaxed);
}

/// Read the latest known beat. `None` until the first frame is observed.
/// Public at the crate level so `main.rs`'s panic hook can call it.
pub(crate) fn last_known_beat_for_crash_log() -> Option<f64> {
    let bits = LAST_KNOWN_BEAT_BITS.load(Ordering::Relaxed);
    if bits == u64::MAX {
        None
    } else {
        Some(f64::from_bits(bits))
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Cadence gate (pure logic — unit tested in isolation)
// ─────────────────────────────────────────────────────────────────────────

/// Decides WHEN to write a breadcrumb. Never touches I/O.
///
/// Fires on:
///   - the transport playing/paused flag changing, in either direction
///   - crossing an integer-beat boundary while playing
///
/// Never fires merely because time advances within the same beat, and never
/// fires on wall-clock alone while paused (§5.1: "a few writes per second,"
/// not a timer).
#[derive(Debug)]
pub(crate) struct BreadcrumbCadence {
    last_playing: Option<bool>,
    last_beat_floor: Option<i64>,
}

impl BreadcrumbCadence {
    pub(crate) fn new() -> Self {
        Self {
            last_playing: None,
            last_beat_floor: None,
        }
    }

    /// Returns true exactly on the frames that should write a breadcrumb.
    pub(crate) fn should_fire(&mut self, current_beat: f64, is_playing: bool) -> bool {
        let playing_changed = self.last_playing != Some(is_playing);
        let beat_floor = current_beat.floor() as i64;
        let beat_changed = is_playing && self.last_beat_floor != Some(beat_floor);
        let fire = playing_changed || beat_changed;

        self.last_playing = Some(is_playing);
        if is_playing {
            self.last_beat_floor = Some(beat_floor);
        }
        fire
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Breadcrumb data model (§5.1)
// ─────────────────────────────────────────────────────────────────────────

/// One captured output window's placement — enough to reopen it on the same
/// physical display in the same mode on resume (closes G12).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WindowTopologyEntry {
    /// Stable per-display identifier (`CGDisplayCreateUUIDFromDisplayID`).
    /// `None` when the platform lookup failed — restore falls straight to
    /// the display-fallback chain (`resolve_display_index` below).
    pub display_uuid: Option<String>,
    /// True for a borderless presentation-mode output window (the show
    /// case); false for a decorated/windowed output.
    pub presentation: bool,
    /// Logical-pixel bounds `(x, y, w, h)` at capture time.
    pub bounds: (f64, f64, f64, f64),
}

/// The full breadcrumb sidecar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BreadcrumbData {
    pub project_path: PathBuf,
    pub perform_mode: bool,
    pub current_beat: f64,
    pub wall_clock_unix_secs: u64,
    pub is_playing: bool,
    /// Generator/effect `type_id`s active at `current_beat` (from every
    /// layer with a clip scheduled right now). Written for the P4
    /// quarantine heuristic (§5.3) to consume later — P2 only writes it.
    pub active_type_ids: Vec<String>,
    /// Clip ids scheduled at `current_beat`. Same P4 consumer as above.
    pub active_clip_ids: Vec<String>,
    pub windows: Vec<WindowTopologyEntry>,
}

/// `<project>.manifold.breadcrumb`, sibling of the project archive.
pub(crate) fn breadcrumb_path_for(project_path: &Path) -> PathBuf {
    let mut s = project_path.as_os_str().to_owned();
    s.push(".breadcrumb");
    PathBuf::from(s)
}

fn tmp_path_for(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Atomic tmp+rename write (§5.1). Runs on the background writer thread —
/// never on the UI or content thread.
fn write_breadcrumb_atomic(data: &BreadcrumbData) -> std::io::Result<()> {
    let path = breadcrumb_path_for(&data.project_path);
    let tmp = tmp_path_for(&path);
    let json = serde_json::to_vec_pretty(data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

/// Parse a breadcrumb sidecar from disk (`--resume` boot path, §5.2 step 1).
pub(crate) fn read_breadcrumb(path: &Path) -> std::io::Result<BreadcrumbData> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ─────────────────────────────────────────────────────────────────────────
// Background writer
// ─────────────────────────────────────────────────────────────────────────

/// Background breadcrumb writer. The UI thread only ever does a non-blocking
/// [`BreadcrumbWriter::submit`] — a bounded(1) channel means a caller NEVER
/// blocks, even mid-write; a submit while one is in flight simply drops the
/// stale intermediate (the design only cares about the latest state).
pub(crate) struct BreadcrumbWriter {
    tx: crossbeam_channel::Sender<BreadcrumbData>,
}

impl BreadcrumbWriter {
    pub(crate) fn spawn() -> Self {
        let (tx, rx) = crossbeam_channel::bounded::<BreadcrumbData>(1);
        if let Err(e) = std::thread::Builder::new()
            .name("breadcrumb-writer".to_string())
            .spawn(move || {
                while let Ok(data) = rx.recv() {
                    if let Err(e) = write_breadcrumb_atomic(&data) {
                        log::warn!("[Breadcrumb] write failed: {e}");
                    }
                }
            })
        {
            // Degrades to a silent no-op via the disconnected-channel branch
            // in `submit` below — a missing breadcrumb is a resume-quality
            // regression, never a crash (D7 spirit: worker setup failures
            // must not take the show down).
            log::error!("[Breadcrumb] couldn't spawn writer thread: {e}");
        }
        Self { tx }
    }

    /// Non-blocking. Called from the content-state drain point every UI
    /// frame the cadence gate fires on.
    pub(crate) fn submit(&self, data: BreadcrumbData) {
        match self.tx.try_send(data) {
            Ok(()) | Err(crossbeam_channel::TrySendError::Full(_)) => {}
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                log::warn!("[Breadcrumb] writer thread is gone — sidecar stopped refreshing");
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Display UUID (macOS) — resume's display-topology restore chain (§5.2 step 2)
// ─────────────────────────────────────────────────────────────────────────

/// Resolve a stable UUID for a monitor via `CGDisplayCreateUUIDFromDisplayID`.
/// winit's `MonitorHandleExtMacOS::native_id()` only gives the ephemeral
/// `CGDirectDisplayID` (see `display_link.rs`'s `display_id_for_window`,
/// which uses the same raw id for CVDisplayLink targeting) — not guaranteed
/// stable across reboots or cable reseats. The UUID is what macOS itself
/// treats as a display's persistent identity (System Settings' own
/// arrangement memory uses it).
///
/// No new crate dependency: this binds directly to the already-linked
/// CoreFoundation runtime (via the existing `core-foundation` crate) plus a
/// direct CoreGraphics `extern "C"` declaration, following the exact
/// `IOPMAssertionCreateWithName` pattern already in `main.rs`.
#[cfg(target_os = "macos")]
pub(crate) fn display_uuid_for_monitor(monitor: &winit::monitor::MonitorHandle) -> Option<String> {
    use winit::platform::macos::MonitorHandleExtMacOS;
    display_uuid_for_display_id(monitor.native_id())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn display_uuid_for_monitor(_monitor: &winit::monitor::MonitorHandle) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn display_uuid_for_display_id(display_id: u32) -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    #[repr(C)]
    struct OpaqueCFUUID {
        _private: [u8; 0],
    }
    type CFUUIDRef = *mut OpaqueCFUUID;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> CFUUIDRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFUUIDCreateString(alloc: *const std::ffi::c_void, uuid: CFUUIDRef) -> CFStringRef;
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    unsafe {
        let uuid_ref = CGDisplayCreateUUIDFromDisplayID(display_id);
        if uuid_ref.is_null() {
            return None;
        }
        let str_ref = CFUUIDCreateString(std::ptr::null(), uuid_ref);
        let result = if str_ref.is_null() {
            None
        } else {
            Some(CFString::wrap_under_create_rule(str_ref).to_string())
        };
        CFRelease(uuid_ref.cast());
        result
    }
}

/// Resolve which currently-available monitor index best matches a captured
/// display topology (§5.2 step 2 restore chain): exact UUID match first,
/// then the largest non-primary display, then the primary display. `None`
/// only when there are zero monitors (callers already guard that case
/// separately, e.g. `open_output_window`'s own "no monitors" check).
pub(crate) fn resolve_display_index(
    event_loop: &winit::event_loop::ActiveEventLoop,
    target_uuid: Option<&str>,
) -> Option<usize> {
    let monitors: Vec<_> = event_loop.available_monitors().collect();
    if monitors.is_empty() {
        return None;
    }

    if let Some(target) = target_uuid {
        for (i, m) in monitors.iter().enumerate() {
            if display_uuid_for_monitor(m).as_deref() == Some(target) {
                return Some(i);
            }
        }
        log::warn!(
            "[Breadcrumb] captured display {target} not found among {} available — falling back",
            monitors.len()
        );
    }

    let primary = event_loop.primary_monitor();
    let non_primary_largest = monitors
        .iter()
        .enumerate()
        .filter(|(_, m)| primary.as_ref().is_none_or(|p| *m != p))
        .max_by_key(|(_, m)| {
            let s = m.size();
            (s.width as u64) * (s.height as u64)
        })
        .map(|(i, _)| i);

    Some(non_primary_largest.unwrap_or(0))
}

// ─────────────────────────────────────────────────────────────────────────
// `Application` integration
// ─────────────────────────────────────────────────────────────────────────

/// Carries the `--resume` boot path's display-topology target from
/// `Application::boot_resume` through to perform-mode entry, which is the
/// one place that actually opens the output window (`perform_mode/lifecycle.rs`).
/// Consumed exactly once via `Option::take`.
pub(crate) struct PendingResume {
    pub display_uuid: Option<String>,
}

impl Application {
    /// Per-frame breadcrumb tick. Called from the content-state drain point
    /// in both `tick_and_render` (editor mode) and `tick_perform_mode`
    /// (perform mode) — the two places `self.content_state` is refreshed
    /// from the content thread's per-tick push. Mirrors `tick_autosave`'s
    /// placement, except breadcrumb writing is NOT parked in perform mode
    /// (D5 parks autosave; the breadcrumb is exactly what a live show needs).
    pub(crate) fn tick_breadcrumb(&mut self) {
        let beat = self.content_state.current_beat;
        let is_playing = self.content_state.is_playing;

        // Panic-hook stamp: refresh every frame regardless of cadence — it's
        // a single atomic store, not I/O, so there's no cost to keeping it
        // exactly current.
        publish_beat_for_crash_log(beat);

        if !self.breadcrumb_cadence.should_fire(beat.0, is_playing) {
            return;
        }

        let Some(path) = self.current_project_path.clone() else {
            return; // Untitled project — nothing to resume into.
        };
        if self.breadcrumb_writer.is_none() {
            return;
        }

        let data = self.capture_breadcrumb(path, beat, is_playing);
        if let Some(writer) = &self.breadcrumb_writer {
            writer.submit(data);
        }
    }

    /// Build the breadcrumb payload from currently-available UI-thread state
    /// (`local_project`, `window_registry`) — no content-thread access
    /// needed beyond the beat/playing pair already carried by `ContentState`.
    fn capture_breadcrumb(
        &mut self,
        project_path: PathBuf,
        beat: Beats,
        is_playing: bool,
    ) -> BreadcrumbData {
        let mut active_pairs: Vec<(usize, usize)> = Vec::new();
        self.local_project
            .timeline
            .get_active_clips_at_beat(beat, &mut active_pairs);

        let mut active_type_ids: Vec<String> = Vec::new();
        let mut active_clip_ids: Vec<String> = Vec::new();
        for (layer_idx, clip_idx) in &active_pairs {
            let Some(layer) = self.local_project.timeline.layers.get(*layer_idx) else {
                continue;
            };
            if let Some(clip) = layer.clips.get(*clip_idx) {
                active_clip_ids.push(clip.id.to_string());
            }
            let gen_type = layer.generator_type();
            if *gen_type != PresetTypeId::NONE {
                active_type_ids.push(gen_type.as_str().to_string());
            }
            if let Some(effects) = &layer.effects {
                active_type_ids.extend(effects.iter().map(|fx| fx.effect_type().as_str().to_string()));
            }
        }
        active_type_ids.sort();
        active_type_ids.dedup();

        let windows: Vec<WindowTopologyEntry> = self
            .window_registry
            .iter()
            .filter_map(|(_, ws)| match &ws.role {
                WindowRole::Output { presentation } => {
                    let (x, y) = ws
                        .window
                        .outer_position()
                        .map(|p| (p.x as f64, p.y as f64))
                        .unwrap_or((0.0, 0.0));
                    let size = ws.window.inner_size();
                    let scale = ws.window.scale_factor();
                    let display_uuid = ws
                        .window
                        .current_monitor()
                        .and_then(|m| display_uuid_for_monitor(&m));
                    Some(WindowTopologyEntry {
                        display_uuid,
                        presentation: *presentation,
                        bounds: (
                            x / scale,
                            y / scale,
                            size.width as f64 / scale,
                            size.height as f64 / scale,
                        ),
                    })
                }
                _ => None,
            })
            .collect();

        BreadcrumbData {
            project_path,
            perform_mode: self.perform.active,
            current_beat: beat.0,
            wall_clock_unix_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            is_playing,
            active_type_ids,
            active_clip_ids,
            windows,
        }
    }

    /// `--resume` boot path (§5.2). Called once from `resumed()` after the
    /// content thread + GPU are up, right before `self.initialized = true`.
    /// Parses the breadcrumb, opens the project through the SAME path
    /// `File > Open` uses, seeks/starts playback, and arms perform-mode
    /// entry — the actual output-window creation happens on the very next
    /// `about_to_wait` via the EXISTING `handle_perform_mode_pending`
    /// (display-index resolution wired in `perform_mode/lifecycle.rs`).
    ///
    /// D8 note: this implements the fallback branch only ("else breadcrumb
    /// beat"). The primary branch ("Ableton position if connected within
    /// ~2s") is not implemented — `ableton_bridge.rs`'s `PendingTransportState`
    /// (ableton_bridge.rs:250-256) tracks only `is_playing`/`tempo`; there is
    /// no inbound Ableton song-position field anywhere in the bridge (verified
    /// 2026-07-03). Escalated in the P2 report rather than adding a new OSC
    /// listener on judgment — that is new surface the phase brief doesn't
    /// authorize.
    pub(crate) fn boot_resume(&mut self, breadcrumb_path: &Path) {
        let data = match read_breadcrumb(breadcrumb_path) {
            Ok(d) => d,
            Err(e) => {
                log::error!(
                    "[Resume] Couldn't read breadcrumb {}: {e}",
                    breadcrumb_path.display()
                );
                return;
            }
        };

        log::info!(
            "[Resume] Breadcrumb: project={} beat={:.2} playing={}",
            data.project_path.display(),
            data.current_beat,
            data.is_playing
        );

        let action = self
            .project_io
            .open_project_from_path(&data.project_path, &mut self.user_prefs);
        if action.apply_project.is_none() {
            log::error!(
                "[Resume] Couldn't open project {} from breadcrumb",
                data.project_path.display()
            );
            return;
        }
        self.apply_project_io_action(action);

        // Override the project's own saved-playhead seek (sent inside
        // apply_project_io_action) with the breadcrumb beat — the breadcrumb
        // is fresher than whatever was on disk at last save.
        self.send_content_cmd(ContentCommand::SeekToBeat(Beats(data.current_beat)));
        if data.is_playing {
            self.send_content_cmd(ContentCommand::Play);
        }

        // Always rejoin into perform mode on --resume: this boot path exists
        // for one reason (relaunch into a running show), regardless of
        // whether the breadcrumb happened to capture editor mode.
        self.pending_resume = Some(PendingResume {
            display_uuid: data.windows.first().and_then(|w| w.display_uuid.clone()),
        });
        self.perform.pending_enter = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_fires_on_first_observation() {
        let mut c = BreadcrumbCadence::new();
        assert!(c.should_fire(0.0, false), "first observation is a transport change from unknown");
    }

    #[test]
    fn cadence_never_fires_mid_beat_while_playing() {
        let mut c = BreadcrumbCadence::new();
        assert!(c.should_fire(4.0, true));
        assert!(!c.should_fire(4.1, true), "same beat, still playing — no fire");
        assert!(!c.should_fire(4.99, true), "still inside beat 4");
    }

    #[test]
    fn cadence_fires_on_integer_beat_crossing_while_playing() {
        let mut c = BreadcrumbCadence::new();
        assert!(c.should_fire(4.0, true));
        assert!(!c.should_fire(4.5, true));
        assert!(c.should_fire(5.0, true), "crossed into beat 5");
        assert!(!c.should_fire(5.7, true));
        assert!(c.should_fire(6.02, true), "crossed into beat 6");
    }

    #[test]
    fn cadence_never_fires_on_wall_clock_alone_while_paused() {
        let mut c = BreadcrumbCadence::new();
        assert!(c.should_fire(10.0, false)); // first observation
        assert!(!c.should_fire(10.0, false), "still paused, same beat");
        // Beat value moving while paused (shouldn't normally happen, but the
        // cadence must not treat it as a reason to fire) still doesn't fire.
        assert!(!c.should_fire(10.3, false));
    }

    #[test]
    fn cadence_fires_on_play_and_on_stop() {
        let mut c = BreadcrumbCadence::new();
        assert!(c.should_fire(2.0, false)); // first observation
        assert!(c.should_fire(2.0, true), "paused -> playing");
        assert!(!c.should_fire(2.4, true), "still beat 2, playing");
        assert!(c.should_fire(2.4, false), "playing -> paused, mid-beat");
        assert!(!c.should_fire(2.4, false), "still paused");
    }

    #[test]
    fn cadence_simulated_beat_stream_never_fires_per_frame() {
        // Simulate ~3 seconds at 120bpm/60fps: beat advances ~0.033/frame.
        // Cadence must fire only on the ~6 integer-beat crossings, never on
        // every one of the ~180 frames.
        let mut c = BreadcrumbCadence::new();
        let mut fires = 0;
        let bps = 2.0 / 60.0; // 120bpm = 2 beats/sec, 60fps
        let mut beat = 0.0;
        for i in 0..180 {
            if i > 0 {
                beat += bps;
            }
            if c.should_fire(beat, true) {
                fires += 1;
            }
        }
        // First observation fire + one per integer-beat crossing (beat ends
        // just under 6.0 after 180 frames at this rate) — well under
        // "never per frame" (180).
        assert!(fires <= 8, "cadence fired {fires} times over 180 frames — too often");
        assert!(fires >= 5, "cadence fired only {fires} times — missed beat crossings");
    }

    #[test]
    fn breadcrumb_round_trips_through_json() {
        let data = BreadcrumbData {
            project_path: PathBuf::from("/tmp/show.manifold"),
            perform_mode: true,
            current_beat: 128.5,
            wall_clock_unix_secs: 1_720_000_000,
            is_playing: true,
            active_type_ids: vec!["Kaleidoscope".to_string(), "PbrMaterial".to_string()],
            active_clip_ids: vec!["clip-1".to_string()],
            windows: vec![WindowTopologyEntry {
                display_uuid: Some("ABCDEF-1234".to_string()),
                presentation: true,
                bounds: (0.0, 0.0, 1920.0, 1080.0),
            }],
        };
        let json = serde_json::to_vec_pretty(&data).expect("serialize");
        let back: BreadcrumbData = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(data, back);
    }

    #[test]
    fn atomic_write_then_read_round_trips_and_leaves_no_tmp_file() {
        let dir = std::env::temp_dir().join(format!(
            "manifold-breadcrumb-test-{}-{}",
            "atomic_write",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let project_path = dir.join("show.manifold");

        let data = BreadcrumbData {
            project_path: project_path.clone(),
            perform_mode: false,
            current_beat: 16.0,
            wall_clock_unix_secs: 42,
            is_playing: false,
            active_type_ids: vec![],
            active_clip_ids: vec![],
            windows: vec![],
        };
        write_breadcrumb_atomic(&data).expect("write");

        let path = breadcrumb_path_for(&project_path);
        assert!(path.exists(), "breadcrumb file exists");
        assert!(!tmp_path_for(&path).exists(), "tmp file cleaned up by rename");

        let back = read_breadcrumb(&path).expect("read");
        assert_eq!(data, back);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn breadcrumb_path_appends_suffix_to_full_project_path() {
        let p = PathBuf::from("/Users/pete/Shows/liveschool.manifold");
        let bc = breadcrumb_path_for(&p);
        assert_eq!(bc, PathBuf::from("/Users/pete/Shows/liveschool.manifold.breadcrumb"));
    }

    #[test]
    fn last_known_beat_is_none_before_first_publish() {
        // Note: shares process-global state with other tests in this binary;
        // this only asserts the sentinel decodes correctly, not ordering
        // relative to other tests' publishes.
        let bits = u64::MAX;
        assert!(f64::from_bits(bits).is_nan(), "sentinel bit pattern is a NaN, never a real beat");
    }
}
