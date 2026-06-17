//! macOS CoreAudio **process-tap** capture — system audio + per-app output.
//!
//! Apple's process-tap API (macOS 14.4+) captures *rendered output* — the whole
//! system mix, or a chosen set of processes — with no loopback driver and no
//! cable. The path is three OS objects wired together:
//!
//! 1. **Tap** — `AudioHardwareCreateProcessTap` from a `CATapDescription`
//!    (global, or a mixdown of specific process object ids). It defines *what*
//!    audio to capture.
//! 2. **Aggregate device** — a private aggregate that lists the tap in its tap
//!    list. This is what exposes the tapped audio as a readable input stream.
//! 3. **IO proc** — `AudioDeviceCreateIOProcID` on the aggregate; its realtime
//!    callback delivers the tapped buffers, which we interleave into the same
//!    lock-free ring buffer every other [`CaptureBackend`] feeds.
//!
//! ## Version safety
//!
//! Only `AudioHardwareCreate/DestroyProcessTap` and `CATapDescription` are new
//! in 14.4; everything else (aggregate device + IO proc) is decades old. We
//! resolve the two new C symbols with `dlsym` and look the class up by name, so
//! the binary **loads and runs on older macOS** — it just reports
//! [`is_supported`]`() == false` and the tap menu sections never appear. No hard
//! link against a symbol the OS might not have.
//!
//! ## Realtime discipline
//!
//! The IO proc runs on a realtime thread: no alloc, no lock, no log. It only
//! interleaves into a pre-sized scratch buffer and does lock-free ring writes.

use std::ffi::{c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSArray, NSNumber, NSString};
use ringbuf::HeapRb;
use ringbuf::traits::{Producer as ProducerTrait, Split};

use super::{AudioConsumer, CaptureBackend};
use crate::directory::TapHandle;

// ── CoreAudio FFI ────────────────────────────────────────────────────────

type OSStatus = i32;
type AudioObjectID = u32;
type AudioObjectPropertySelector = u32;
type AudioObjectPropertyScope = u32;
type AudioObjectPropertyElement = u32;
type AudioDeviceIOProcID = *mut c_void;
type CFDictionaryRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

/// IO proc ABI. Every parameter is a pointer; we only read `in_input_data`
/// (a `const AudioBufferList*`), so the timestamps and output list are `c_void`.
type AudioDeviceIOProc = extern "C" fn(
    in_device: AudioObjectID,
    in_now: *const c_void,
    in_input_data: *const c_void,
    in_input_time: *const c_void,
    out_output_data: *mut c_void,
    in_output_time: *const c_void,
    in_client_data: *mut c_void,
) -> OSStatus;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        in_object: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_size: u32,
        in_qualifier: *const c_void,
        io_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;

    // Aggregate device + IO proc are decades-old API — safe to hard-link.
    fn AudioHardwareCreateAggregateDevice(
        in_description: CFDictionaryRef,
        out_device: *mut AudioObjectID,
    ) -> OSStatus;
    fn AudioHardwareDestroyAggregateDevice(in_device: AudioObjectID) -> OSStatus;
    fn AudioDeviceCreateIOProcID(
        in_device: AudioObjectID,
        in_proc: AudioDeviceIOProc,
        in_client_data: *mut c_void,
        out_proc_id: *mut AudioDeviceIOProcID,
    ) -> OSStatus;
    fn AudioDeviceDestroyIOProcID(in_device: AudioObjectID, in_proc_id: AudioDeviceIOProcID) -> OSStatus;
    fn AudioDeviceStart(in_device: AudioObjectID, in_proc_id: AudioDeviceIOProcID) -> OSStatus;
    fn AudioDeviceStop(in_device: AudioObjectID, in_proc_id: AudioDeviceIOProcID) -> OSStatus;
}

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
/// `RTLD_DEFAULT` on macOS — search every loaded image for the symbol.
const RTLD_DEFAULT: *mut c_void = (-2isize) as *mut c_void;

const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const ELEMENT_MAIN: u32 = 0;
const PROP_TAP_FORMAT: u32 = fourcc(b"tfmt");
const PROP_NOMINAL_SAMPLE_RATE: u32 = fourcc(b"nsrt");

// CoreAudio aggregate-device / sub-tap dictionary keys (literal CFString values
// from <CoreAudio/AudioHardware.h>; stable across releases).
const KEY_AGG_UID: &str = "uid";
const KEY_AGG_NAME: &str = "name";
const KEY_AGG_PRIVATE: &str = "private";
const KEY_AGG_TAP_LIST: &str = "taps";
const KEY_AGG_TAP_AUTOSTART: &str = "tapautostart";
const KEY_SUBTAP_UID: &str = "uid";

// ── Dynamically-resolved 14.4 symbols ────────────────────────────────────

type CreateProcessTapFn = unsafe extern "C" fn(*mut AnyObject, *mut AudioObjectID) -> OSStatus;
type DestroyProcessTapFn = unsafe extern "C" fn(AudioObjectID) -> OSStatus;

struct TapSyms {
    create: CreateProcessTapFn,
    destroy: DestroyProcessTapFn,
}

/// Resolve the process-tap symbols + `CATapDescription` class once. `None` on
/// macOS < 14.4 (symbols absent) — the caller degrades to "tap unsupported".
fn syms() -> Option<&'static TapSyms> {
    static SYMS: OnceLock<Option<TapSyms>> = OnceLock::new();
    SYMS.get_or_init(|| unsafe {
        let create = dlsym(RTLD_DEFAULT, c"AudioHardwareCreateProcessTap".as_ptr());
        let destroy = dlsym(RTLD_DEFAULT, c"AudioHardwareDestroyProcessTap".as_ptr());
        if create.is_null() || destroy.is_null() {
            return None;
        }
        // The Objective-C description class must exist too (same 14.4 gate).
        AnyClass::get(c"CATapDescription")?;
        Some(TapSyms {
            create: std::mem::transmute::<*mut c_void, CreateProcessTapFn>(create),
            destroy: std::mem::transmute::<*mut c_void, DestroyProcessTapFn>(destroy),
        })
    })
    .as_ref()
}

/// Whether process-tap capture is available on this OS.
pub fn is_supported() -> bool {
    syms().is_some()
}

// ── Realtime callback context ────────────────────────────────────────────

/// Owned by the IO proc for the life of the stream. The aggregate's IO proc is
/// the sole writer of `producer`, so the `*mut` aliasing is exclusive in
/// practice (CoreAudio calls the proc serially).
struct TapCallbackCtx {
    producer: ringbuf::HeapProd<f32>,
    running: Arc<AtomicBool>,
    overflow: Arc<AtomicU64>,
    channels: usize,
    /// Pre-sized interleave staging buffer for planar (non-interleaved) input.
    /// Never resized in the callback.
    scratch: Vec<f32>,
}

extern "C" fn tap_ioproc(
    _dev: AudioObjectID,
    _now: *const c_void,
    in_input: *const c_void,
    _in_time: *const c_void,
    _out: *mut c_void,
    _out_time: *const c_void,
    client: *mut c_void,
) -> OSStatus {
    if client.is_null() || in_input.is_null() {
        return 0;
    }
    // SAFETY: `client` is the Box<TapCallbackCtx> raw pointer we registered;
    // CoreAudio invokes the proc serially, so the &mut is exclusive.
    let ctx = unsafe { &mut *(client as *mut TapCallbackCtx) };
    if !ctx.running.load(Ordering::Relaxed) {
        return 0;
    }
    unsafe { write_buffers(in_input, ctx) };
    0
}

/// Interleave one `AudioBufferList` into the ring buffer at `ctx.channels` wide.
///
/// SAFETY: `list_ptr` is a valid `const AudioBufferList*` for the call's
/// duration, as delivered by CoreAudio.
unsafe fn write_buffers(list_ptr: *const c_void, ctx: &mut TapCallbackCtx) {
    let TapCallbackCtx { producer, overflow, channels, scratch, .. } = ctx;
    let ch = (*channels).max(1);

    // AudioBufferList { UInt32 mNumberBuffers; AudioBuffer mBuffers[]; }
    // mBuffers is 8-byte aligned (it contains a pointer) → 4 bytes pad after the
    // count, so the array starts at offset 8.
    let n_buffers = unsafe { *(list_ptr as *const u32) } as usize;
    if n_buffers == 0 {
        return;
    }
    let buffers = unsafe { (list_ptr as *const u8).add(8) as *const AudioBuffer };

    // Interleaved already: one buffer carrying all channels. Pass it straight
    // through — the common case and the cheapest.
    if n_buffers == 1 {
        let b = unsafe { &*buffers };
        if b.data.is_null() {
            return;
        }
        let samples = b.data_byte_size as usize / std::mem::size_of::<f32>();
        if samples == 0 {
            return;
        }
        let data = unsafe { std::slice::from_raw_parts(b.data as *const f32, samples) };
        let written = producer.push_slice(data);
        if written < data.len() {
            overflow.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }

    // Planar: one mono buffer per channel. Interleave through the scratch buffer
    // in bounded chunks (no allocation).
    let frames = unsafe { (*buffers).data_byte_size } as usize / std::mem::size_of::<f32>();
    if frames == 0 {
        return;
    }
    let chunk_frames = (scratch.len() / ch).max(1);
    let mut f0 = 0;
    while f0 < frames {
        let this = chunk_frames.min(frames - f0);
        for f in 0..this {
            for (c, slot) in scratch[f * ch..f * ch + ch].iter_mut().enumerate() {
                *slot = if c < n_buffers {
                    let bc = unsafe { &*buffers.add(c) };
                    if bc.data.is_null() {
                        0.0
                    } else {
                        unsafe { *(bc.data as *const f32).add(f0 + f) }
                    }
                } else {
                    0.0
                };
            }
        }
        let slice = &scratch[..this * ch];
        let written = producer.push_slice(slice);
        if written < slice.len() {
            overflow.fetch_add(1, Ordering::Relaxed);
        }
        f0 += this;
    }
}

// ── The capture backend ──────────────────────────────────────────────────

/// A live process-tap capture. Owns the tap, the private aggregate device, the
/// IO proc, and the callback context; dropping it tears all four down in order.
pub struct ProcessTapCapture {
    tap_id: AudioObjectID,
    agg_id: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    /// Raw `Box<TapCallbackCtx>` — reclaimed in `Drop` after the IO proc is gone.
    ctx: *mut TapCallbackCtx,
    consumer: Option<AudioConsumer>,
    sample_rate: u32,
    channels: u16,
    running: Arc<AtomicBool>,
    overflow: Arc<AtomicU64>,
}

// The OS objects are owned solely by this struct and CoreAudio threads; the
// content thread is the only Rust owner. Same justification as the cpal backend.
unsafe impl Send for ProcessTapCapture {}

/// Build a tap from an already-constructed `CATapDescription` and wrap it in a
/// running-capable aggregate + IO proc. `description` is consumed.
fn build(description: Retained<AnyObject>) -> Result<Box<dyn CaptureBackend>, String> {
    let syms = syms().ok_or("process taps unavailable (requires macOS 14.4+)")?;

    // 1. Create the tap.
    let mut tap_id: AudioObjectID = 0;
    let st = unsafe { (syms.create)(Retained::as_ptr(&description) as *mut AnyObject, &mut tap_id) };
    if st != 0 || tap_id == 0 {
        return Err(format!("AudioHardwareCreateProcessTap failed (OSStatus {st})"));
    }
    // A guard so any early return below also destroys the tap.
    let tap_guard = TapGuard { tap_id, destroy: syms.destroy };

    // The tap's UUID string is its sub-tap UID in the aggregate's tap list.
    let tap_uuid = unsafe { tap_description_uuid(&description) }
        .ok_or("CATapDescription returned no UUID")?;

    // 2. Create a private aggregate device that lists the tap.
    let agg_uid = format!("com.latentspace.manifold.tap.{tap_uuid}");
    let agg_id = create_aggregate_with_tap(&agg_uid, &tap_uuid)?;
    let agg_guard = AggGuard { agg_id };

    // 3. Resolve capture format (sample rate + channel count).
    let (sample_rate, channels) = tap_format(tap_id, agg_id);

    // 4. Ring buffer sized to ~2s, bounded (mirrors the cpal backend).
    const MAX_RING_SAMPLES: usize = 4 * 1024 * 1024;
    let want = (sample_rate as usize) * (channels as usize) * 2;
    let floor = ((sample_rate as usize) * (channels as usize)) / 4;
    let capacity = want.min(MAX_RING_SAMPLES).max(floor).max(1);
    let (producer, consumer) = HeapRb::<f32>::new(capacity).split();

    let running = Arc::new(AtomicBool::new(false));
    let overflow = Arc::new(AtomicU64::new(0));
    // Scratch large enough for a generous block at this channel count.
    let scratch = vec![0.0f32; 8192.max(channels as usize * 1024)];
    let ctx = Box::into_raw(Box::new(TapCallbackCtx {
        producer,
        running: running.clone(),
        overflow: overflow.clone(),
        channels: channels as usize,
        scratch,
    }));

    // 5. Register the IO proc on the aggregate.
    let mut proc_id: AudioDeviceIOProcID = std::ptr::null_mut();
    let st = unsafe {
        AudioDeviceCreateIOProcID(agg_id, tap_ioproc, ctx as *mut c_void, &mut proc_id)
    };
    if st != 0 || proc_id.is_null() {
        // Reclaim the context box; guards destroy the aggregate + tap.
        unsafe { drop(Box::from_raw(ctx)) };
        return Err(format!("AudioDeviceCreateIOProcID failed (OSStatus {st})"));
    }

    // Everything succeeded — defuse the guards; the struct owns teardown now.
    std::mem::forget(tap_guard);
    std::mem::forget(agg_guard);

    log::info!(
        "[ProcessTap] Tap ready: {sample_rate}Hz {channels}ch (tap={tap_id}, agg={agg_id})"
    );

    Ok(Box::new(ProcessTapCapture {
        tap_id,
        agg_id,
        proc_id,
        ctx,
        consumer: Some(consumer),
        sample_rate,
        channels,
        running,
        overflow,
    }))
}

/// Read the tap's output format. Falls back to the aggregate's nominal sample
/// rate (and stereo) if the tap-format property can't be read.
fn tap_format(tap_id: AudioObjectID, agg_id: AudioObjectID) -> (u32, u16) {
    let mut asbd = AudioStreamBasicDescription::default();
    let addr = AudioObjectPropertyAddress {
        selector: PROP_TAP_FORMAT,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    };
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(
            tap_id,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut asbd as *mut _ as *mut c_void,
        )
    };
    if st == 0 && asbd.sample_rate > 0.0 {
        let ch = (asbd.channels_per_frame.max(1)).min(u16::MAX as u32) as u16;
        return (asbd.sample_rate as u32, ch);
    }

    // Fallback: aggregate nominal sample rate, assume stereo mixdown.
    let mut rate: f64 = 0.0;
    let addr = AudioObjectPropertyAddress {
        selector: PROP_NOMINAL_SAMPLE_RATE,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    };
    let mut size = std::mem::size_of::<f64>() as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(
            agg_id,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut rate as *mut _ as *mut c_void,
        )
    };
    let sr = if st == 0 && rate > 0.0 { rate as u32 } else { 48_000 };
    (sr, 2)
}

/// Build and register a private aggregate device whose tap list contains the tap
/// identified by `tap_uuid`. Returns the aggregate's object id.
fn create_aggregate_with_tap(agg_uid: &str, tap_uuid: &str) -> Result<AudioObjectID, String> {
    let sub_tap = CFDictionary::from_CFType_pairs(&[(
        CFString::new(KEY_SUBTAP_UID).as_CFType(),
        CFString::new(tap_uuid).as_CFType(),
    )]);
    let tap_list = CFArray::from_CFTypes(&[sub_tap.as_CFType()]);

    let description = CFDictionary::from_CFType_pairs(&[
        (CFString::new(KEY_AGG_UID).as_CFType(), CFString::new(agg_uid).as_CFType()),
        (CFString::new(KEY_AGG_NAME).as_CFType(), CFString::new("Manifold Tap").as_CFType()),
        (CFString::new(KEY_AGG_PRIVATE).as_CFType(), CFBoolean::true_value().as_CFType()),
        (CFString::new(KEY_AGG_TAP_AUTOSTART).as_CFType(), CFBoolean::true_value().as_CFType()),
        (CFString::new(KEY_AGG_TAP_LIST).as_CFType(), tap_list.as_CFType()),
    ]);

    let mut agg_id: AudioObjectID = 0;
    let st = unsafe {
        AudioHardwareCreateAggregateDevice(
            description.as_concrete_TypeRef() as *const c_void,
            &mut agg_id,
        )
    };
    if st != 0 || agg_id == 0 {
        return Err(format!("AudioHardwareCreateAggregateDevice failed (OSStatus {st})"));
    }
    Ok(agg_id)
}

/// `[[description UUID] UUIDString]` as a Rust string.
///
/// SAFETY: `description` is a live `CATapDescription`.
unsafe fn tap_description_uuid(description: &Retained<AnyObject>) -> Option<String> {
    let uuid: *mut AnyObject = unsafe { msg_send![&**description, UUID] };
    if uuid.is_null() {
        return None;
    }
    let uuid_str: Retained<NSString> = unsafe { msg_send![uuid, UUIDString] };
    Some(uuid_str.to_string())
}

/// Construct a `CATapDescription`. `processes` empty → a global tap of the whole
/// system mix; non-empty → a stereo mixdown of those process object ids.
fn make_description(processes: &[TapHandle]) -> Result<Retained<AnyObject>, String> {
    let cls = AnyClass::get(c"CATapDescription").ok_or("CATapDescription class unavailable")?;
    let numbers: Vec<Retained<NSNumber>> =
        processes.iter().map(|h| NSNumber::new_u32(*h as u32)).collect();
    let array = NSArray::from_retained_slice(&numbers);

    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    let desc: *mut AnyObject = if processes.is_empty() {
        unsafe { msg_send![alloc, initStereoGlobalTapButExcludeProcesses: &*array] }
    } else {
        unsafe { msg_send![alloc, initStereoMixdownOfProcesses: &*array] }
    };
    // SAFETY: init returns a +1 reference we now own.
    unsafe { Retained::from_raw(desc) }.ok_or_else(|| "CATapDescription init failed".to_string())
}

/// Open a system-audio tap (whole output mix).
pub fn open_system_audio() -> Result<Box<dyn CaptureBackend>, String> {
    if !is_supported() {
        return Err("system-audio tap requires macOS 14.4+".to_string());
    }
    build(make_description(&[])?)
}

/// Open a tap of the given application processes, mixed down.
pub fn open_apps(handles: &[TapHandle]) -> Result<Box<dyn CaptureBackend>, String> {
    if !is_supported() {
        return Err("per-application tap requires macOS 14.4+".to_string());
    }
    if handles.is_empty() {
        return Err("no application process to tap".to_string());
    }
    build(make_description(handles)?)
}

impl CaptureBackend for ProcessTapCapture {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn take_consumer(&mut self) -> Option<AudioConsumer> {
        self.consumer.take()
    }
    fn start(&self) -> Result<(), String> {
        self.running.store(true, Ordering::Release);
        let st = unsafe { AudioDeviceStart(self.agg_id, self.proc_id) };
        if st != 0 {
            return Err(format!("AudioDeviceStart failed (OSStatus {st})"));
        }
        log::info!("[ProcessTap] Capture started");
        Ok(())
    }
    fn stop(&self) {
        self.running.store(false, Ordering::Release);
        unsafe { AudioDeviceStop(self.agg_id, self.proc_id) };
    }
    fn overflow_count(&self) -> u64 {
        self.overflow.load(Ordering::Relaxed)
    }
}

impl Drop for ProcessTapCapture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        unsafe {
            AudioDeviceStop(self.agg_id, self.proc_id);
            AudioDeviceDestroyIOProcID(self.agg_id, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.agg_id);
            if let Some(syms) = syms() {
                (syms.destroy)(self.tap_id);
            }
            // The IO proc is gone; no one references the context now.
            drop(Box::from_raw(self.ctx));
        }
        log::info!("[ProcessTap] Capture torn down");
    }
}

// ── Early-return teardown guards ─────────────────────────────────────────

/// Destroys the tap if `build` returns before ownership transfers to the struct.
struct TapGuard {
    tap_id: AudioObjectID,
    destroy: DestroyProcessTapFn,
}
impl Drop for TapGuard {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.tap_id) };
    }
}

/// Destroys the aggregate device on an early return.
struct AggGuard {
    agg_id: AudioObjectID,
}
impl Drop for AggGuard {
    fn drop(&mut self) {
        unsafe { AudioHardwareDestroyAggregateDevice(self.agg_id) };
    }
}
