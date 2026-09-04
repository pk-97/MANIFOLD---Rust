//! TEMP deferred-destruction probe for BUG-l7t4 (audition page fault)
//! diagnosis. NOT production code — env-gated, default off, and to be
//! removed once the mechanism is named.
//!
//! `MANIFOLD_DEFER_DROP_FRAMES=<N>` intercepts `GpuTexture`/`GpuBuffer`
//! drops: instead of releasing the Metal object, the `Retained` moves
//! into a queue that survives `N` calls to [`pump_deferred_drops`]
//! (the harness pumps once per rendered frame). If the audition fault
//! vanishes with deferral on, the mechanism is a drop-while-in-flight
//! lifetime hole — and deferring only resources whose label matches a
//! pattern (`MANIFOLD_DEFER_DROP_LABEL`) names the guilty class.
//!
//! Env is read once per process; unset or non-numeric = disabled
//! (zero behavioral change).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBuffer, MTLResource, MTLTexture};

struct SendTexture(Retained<ProtocolObject<dyn MTLTexture>>);
unsafe impl Send for SendTexture {}

struct SendBuffer(Retained<ProtocolObject<dyn MTLBuffer>>);
unsafe impl Send for SendBuffer {}

static DEFER_FRAMES: OnceLock<Option<usize>> = OnceLock::new();
static LABEL_PATTERN: OnceLock<Option<String>> = OnceLock::new();
static KIND: OnceLock<Option<String>> = OnceLock::new();
static TEXTURES: Mutex<VecDeque<(usize, Option<String>, SendTexture)>> =
    Mutex::new(VecDeque::new());
static BUFFERS: Mutex<VecDeque<(usize, Option<String>, SendBuffer)>> =
    Mutex::new(VecDeque::new());

fn defer_kind() -> Option<String> {
    KIND.get_or_init(|| std::env::var("MANIFOLD_DEFER_DROP_KIND").ok().filter(|s| !s.is_empty()))
        .clone()
}

fn kind_matches(is_buffer: bool) -> bool {
    match defer_kind() {
        None => true, // defer both classes
        Some(k) if is_buffer => k == "buffer",
        Some(k) => k == "texture",
    }
}

fn defer_frames() -> Option<usize> {
    *DEFER_FRAMES.get_or_init(|| {
        std::env::var("MANIFOLD_DEFER_DROP_FRAMES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
    })
}

fn label_pattern() -> Option<String> {
    LABEL_PATTERN
        .get_or_init(|| std::env::var("MANIFOLD_DEFER_DROP_LABEL").ok().filter(|s| !s.is_empty()))
        .clone()
}

fn label_matches(label: Option<&str>) -> bool {
    match label_pattern() {
        None => true, // no pattern = defer everything
        Some(pat) => label.is_some_and(|l| l.contains(&pat)),
    }
}

fn backtrace_enabled() -> bool {
    static BT: OnceLock<bool> = OnceLock::new();
    *BT.get_or_init(|| std::env::var("MANIFOLD_DEFER_DROP_BACKTRACE").is_ok())
}

fn maybe_log_enqueue(kind: &str, label: &Option<String>) {
    if !backtrace_enabled() {
        return;
    }
    eprintln!(
        "[defer-drop] enqueue {kind} {} (remaining will count down)",
        label.as_deref().unwrap_or("(unlabeled)")
    );
    let bt = std::backtrace::Backtrace::force_capture();
    eprintln!("{bt}");
}

/// Whether texture deferral is armed (harness prints it for log honesty).
pub fn defer_probe_enabled() -> bool {
    defer_frames().is_some()
}

/// Consume a texture drop: defer it if the probe is armed and the label
/// matches, otherwise release immediately. Always takes ownership.
pub fn drop_texture(raw: Retained<ProtocolObject<dyn MTLTexture>>) {
    let Some(frames) = defer_frames() else {
        drop(raw);
        return;
    };
    if !kind_matches(false) {
        drop(raw);
        return;
    }
    let label = unsafe { raw.label() }
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    if !label_matches(label.as_deref()) {
        drop(raw);
        return;
    }
    maybe_log_enqueue("texture", &label);
    TEXTURES
        .lock()
        .expect("defer TEXTURES poisoned")
        .push_back((frames, label, SendTexture(raw)));
}

/// Consume a buffer drop. Same contract as [`drop_texture`].
pub fn drop_buffer(raw: Retained<ProtocolObject<dyn MTLBuffer>>) {
    let Some(frames) = defer_frames() else {
        drop(raw);
        return;
    };
    if !kind_matches(true) {
        drop(raw);
        return;
    }
    let label = unsafe { raw.label() }
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    if !label_matches(label.as_deref()) {
        drop(raw);
        return;
    }
    maybe_log_enqueue("buffer", &label);
    BUFFERS
        .lock()
        .expect("defer BUFFERS poisoned")
        .push_back((frames, label, SendBuffer(raw)));
}

/// One generation: every queued object's remaining count drops by 1;
/// objects at 0 release their Metal handle. The harness calls this once
/// per rendered frame.
pub fn pump_deferred_drops() {
    let mut textures = TEXTURES.lock().expect("defer TEXTURES poisoned");
    let n = textures.len();
    for _ in 0..n {
        let Some((mut remaining, label, item)) = textures.pop_front() else {
            break;
        };
        remaining = remaining.saturating_sub(1);
        if remaining == 0 {
            let label = label.unwrap_or_else(|| "(unlabeled)".into());
            eprintln!("[defer-drop] releasing texture {label}");
            drop(item.0);
        } else {
            textures.push_back((remaining, label, item));
        }
    }
    drop(textures);

    let mut buffers = BUFFERS.lock().expect("defer BUFFERS poisoned");
    let n = buffers.len();
    for _ in 0..n {
        let Some((mut remaining, label, item)) = buffers.pop_front() else {
            break;
        };
        remaining = remaining.saturating_sub(1);
        if remaining == 0 {
            let label = label.unwrap_or_else(|| "(unlabeled)".into());
            eprintln!("[defer-drop] releasing buffer {label}");
            drop(item.0);
        } else {
            buffers.push_back((remaining, label, item));
        }
    }
}
