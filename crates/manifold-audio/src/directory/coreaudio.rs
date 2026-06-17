//! macOS CoreAudio implementation of [`AudioDeviceDirectory`].
//!
//! CoreAudio (the HAL) is Apple's native audio API; cpal wraps it on macOS but
//! flattens away exactly the metadata we want — channel names, the stable
//! device UID, liveness, and hot-plug events. We query the HAL directly through
//! `AudioObjectGetPropertyData` and friends, and use the `core-foundation`
//! crate to turn the `CFStringRef`s it hands back (UIDs, names) into `String`.
//!
//! Every query is defensive: a failed property read degrades (empty name, no
//! subdevice grouping, channel count 0) rather than panicking. The directory
//! must survive any device topology a stage throws at it.

use std::ffi::c_void;

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};

use super::{
    AppAudioSource, AudioDeviceDirectory, ChannelInfo, DeviceInfo, SubdeviceGroup, Subscription,
    TapCapabilities, TapHandle,
};

// ── CoreAudio FFI types ──────────────────────────────────────────────────

type OSStatus = i32;
type AudioObjectID = u32;
type AudioObjectPropertySelector = u32;
type AudioObjectPropertyScope = u32;
type AudioObjectPropertyElement = u32;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
}

type ListenerProc = extern "C" fn(
    AudioObjectID,
    u32,
    *const AudioObjectPropertyAddress,
    *mut c_void,
) -> OSStatus;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        in_object: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_size: u32,
        in_qualifier: *const c_void,
        out_size: *mut u32,
    ) -> OSStatus;

    fn AudioObjectGetPropertyData(
        in_object: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_size: u32,
        in_qualifier: *const c_void,
        io_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;

    fn AudioObjectAddPropertyListener(
        in_object: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_proc: ListenerProc,
        in_client_data: *mut c_void,
    ) -> OSStatus;

    fn AudioObjectRemovePropertyListener(
        in_object: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_proc: ListenerProc,
        in_client_data: *mut c_void,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(arr: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(arr: *const c_void, idx: isize) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

// ── Constants (FourCharCode property selectors) ──────────────────────────

const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

const SYSTEM_OBJECT: AudioObjectID = 1; // kAudioObjectSystemObject

const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const SCOPE_INPUT: u32 = fourcc(b"inpt");
const ELEMENT_MAIN: u32 = 0;

const PROP_DEVICES: u32 = fourcc(b"dev#");
const PROP_DEFAULT_INPUT: u32 = fourcc(b"dIn ");
const PROP_TRANSLATE_UID: u32 = fourcc(b"uidd");
const PROP_NAME: u32 = fourcc(b"lnam");
const PROP_ELEMENT_NAME: u32 = fourcc(b"lchn");
const PROP_UID: u32 = fourcc(b"uid ");
const PROP_IS_ALIVE: u32 = fourcc(b"livn");
const PROP_STREAM_CONFIG: u32 = fourcc(b"slay");
const PROP_SUBDEVICE_LIST: u32 = fourcc(b"grup");

// Process objects — for per-application audio tapping (macOS 14.4+).
const PROP_PROCESS_LIST: u32 = fourcc(b"prs#"); // kAudioHardwarePropertyProcessObjectList
const PROP_PROCESS_PID: u32 = fourcc(b"ppid"); // kAudioProcessPropertyPID
const PROP_PROCESS_BUNDLE_ID: u32 = fourcc(b"pbid"); // kAudioProcessPropertyBundleID
const PROP_PROCESS_RUNNING_OUTPUT: u32 = fourcc(b"piro"); // kAudioProcessPropertyIsRunningOutput

const fn addr(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { selector, scope, element: ELEMENT_MAIN }
}

const DEVICES_ADDR: AudioObjectPropertyAddress = addr(PROP_DEVICES, SCOPE_GLOBAL);
const DEFAULT_INPUT_ADDR: AudioObjectPropertyAddress = addr(PROP_DEFAULT_INPUT, SCOPE_GLOBAL);
const PROCESS_LIST_ADDR: AudioObjectPropertyAddress = addr(PROP_PROCESS_LIST, SCOPE_GLOBAL);

// ── Low-level property helpers ───────────────────────────────────────────

/// Byte size of a property, or `None` if the device doesn't expose it.
fn property_size(obj: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<usize> {
    let mut size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(obj, a, 0, std::ptr::null(), &mut size)
    };
    (status == 0).then_some(size as usize)
}

/// Read a fixed-size POD property (`UInt32`, `AudioObjectID`, …).
fn property_pod<T: Copy + Default>(
    obj: AudioObjectID,
    a: &AudioObjectPropertyAddress,
) -> Option<T> {
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            obj,
            a,
            0,
            std::ptr::null(),
            &mut size,
            &mut value as *mut T as *mut c_void,
        )
    };
    (status == 0).then_some(value)
}

/// Read a variable-size property into a byte buffer.
fn property_bytes(obj: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<Vec<u8>> {
    let size = property_size(obj, a)?;
    if size == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; size];
    let mut io = size as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            obj,
            a,
            0,
            std::ptr::null(),
            &mut io,
            buf.as_mut_ptr() as *mut c_void,
        )
    };
    (status == 0).then_some(buf)
}

/// Read a `CFStringRef`-valued property and convert to a `String`. Empty
/// strings collapse to `None`.
fn property_cfstring(obj: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<String> {
    let mut cfref: CFStringRef = std::ptr::null();
    let mut size = std::mem::size_of::<CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            obj,
            a,
            0,
            std::ptr::null(),
            &mut size,
            &mut cfref as *mut CFStringRef as *mut c_void,
        )
    };
    if status != 0 || cfref.is_null() {
        return None;
    }
    // CFString-valued device properties follow the create rule (+1 retain); the
    // wrapper releases on drop.
    let s = unsafe { CFString::wrap_under_create_rule(cfref) }.to_string();
    (!s.is_empty()).then_some(s)
}

/// Number of input channels from the device's input stream configuration. The
/// `AudioBufferList` is parsed byte-wise to avoid any alignment assumptions.
fn input_channel_count(obj: AudioObjectID) -> u16 {
    let a = addr(PROP_STREAM_CONFIG, SCOPE_INPUT);
    let Some(bytes) = property_bytes(obj, &a) else {
        return 0;
    };
    if bytes.len() < 4 {
        return 0;
    }
    // struct AudioBufferList { UInt32 mNumberBuffers; AudioBuffer mBuffers[]; }
    // AudioBuffer { UInt32 mNumberChannels; UInt32 mDataByteSize; void* mData; }
    // mBuffers is 8-byte aligned → 4 bytes pad after the count; each buffer = 16 B.
    let n = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let mut total: u32 = 0;
    for i in 0..n {
        let off = 8 + i * 16;
        if off + 4 > bytes.len() {
            break;
        }
        total += u32::from_ne_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
    }
    total.min(u16::MAX as u32) as u16
}

/// The label for one input channel (1-based element), or `None` if unnamed.
fn channel_name(obj: AudioObjectID, channel_index: u16) -> Option<String> {
    // Element 0 is the master/main element; channel N is element N+1.
    let a = AudioObjectPropertyAddress {
        selector: PROP_ELEMENT_NAME,
        scope: SCOPE_INPUT,
        element: channel_index as u32 + 1,
    };
    property_cfstring(obj, &a)
}

/// Translate a device UID string to its current `AudioObjectID`.
fn device_for_uid(uid: &str) -> Option<AudioObjectID> {
    let cf = CFString::new(uid);
    let cfref = cf.as_concrete_TypeRef();
    let a = addr(PROP_TRANSLATE_UID, SCOPE_GLOBAL);
    let mut out: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &a,
            std::mem::size_of::<CFStringRef>() as u32,
            &cfref as *const CFStringRef as *const c_void,
            &mut size,
            &mut out as *mut AudioObjectID as *mut c_void,
        )
    };
    // Unknown UID resolves to 0 (kAudioObjectUnknown).
    (status == 0 && out != 0).then_some(out)
}

/// Subdevice grouping for an aggregate device, in channel order. Empty for a
/// plain device (the property read fails) or if the layout can't be resolved.
fn subdevice_groups(obj: AudioObjectID) -> Vec<SubdeviceGroup> {
    let a = addr(PROP_SUBDEVICE_LIST, SCOPE_GLOBAL);
    let mut arr: *const c_void = std::ptr::null();
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            obj,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            &mut arr as *mut *const c_void as *mut c_void,
        )
    };
    if status != 0 || arr.is_null() {
        return Vec::new();
    }

    let mut groups = Vec::new();
    let mut next_channel: u16 = 0;
    let count = unsafe { CFArrayGetCount(arr) };
    for i in 0..count {
        let uid_ref = unsafe { CFArrayGetValueAtIndex(arr, i) } as CFStringRef;
        if uid_ref.is_null() {
            continue;
        }
        // Array elements follow the get rule.
        let uid = unsafe { CFString::wrap_under_get_rule(uid_ref) }.to_string();
        let Some(sub_id) = device_for_uid(&uid) else {
            continue;
        };
        let channel_count = input_channel_count(sub_id);
        if channel_count == 0 {
            continue; // output-only subdevice contributes no input channels
        }
        let name = property_cfstring(sub_id, &addr(PROP_NAME, SCOPE_GLOBAL))
            .unwrap_or_else(|| uid.clone());
        groups.push(SubdeviceGroup {
            name,
            channel_start: next_channel,
            channel_count,
        });
        next_channel = next_channel.saturating_add(channel_count);
    }
    unsafe { CFRelease(arr) };
    groups
}

/// Full metadata for one device id, or `None` if it has no input channels.
fn device_info(obj: AudioObjectID, default_input: AudioObjectID) -> Option<DeviceInfo> {
    let channel_count = input_channel_count(obj);
    if channel_count == 0 {
        return None; // not an input device
    }
    let uid = property_cfstring(obj, &addr(PROP_UID, SCOPE_GLOBAL))?;
    let name = property_cfstring(obj, &addr(PROP_NAME, SCOPE_GLOBAL))
        .unwrap_or_else(|| uid.clone());
    let is_alive = property_pod::<u32>(obj, &addr(PROP_IS_ALIVE, SCOPE_GLOBAL))
        .map(|v| v != 0)
        .unwrap_or(true);

    let channels = (0..channel_count)
        .map(|index| ChannelInfo { index, name: channel_name(obj, index) })
        .collect();

    // Only present subdevice grouping if it exactly accounts for every input
    // channel. A subdevice that can't be resolved (or an output-only one) would
    // otherwise leave the groups misaligned with the channel list — worse than
    // no grouping. This makes alignment self-validating.
    let mut subdevices = subdevice_groups(obj);
    let grouped: u16 = subdevices.iter().map(|g| g.channel_count).sum();
    if grouped != channel_count {
        subdevices.clear();
    }

    Some(DeviceInfo {
        uid,
        name,
        is_default: obj == default_input,
        is_alive,
        channels,
        subdevices,
    })
}

// ── Process objects (per-app audio tapping) ──────────────────────────────

/// All CoreAudio process objects (one per process that has touched audio).
/// Empty on macOS < 14.4 (the property doesn't exist) or if the read fails.
fn all_process_ids() -> Vec<AudioObjectID> {
    let Some(size) = property_size(SYSTEM_OBJECT, &PROCESS_LIST_ADDR) else {
        return Vec::new();
    };
    let count = size / std::mem::size_of::<AudioObjectID>();
    if count == 0 {
        return Vec::new();
    }
    let mut ids = vec![0u32; count];
    let mut io = size as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &PROCESS_LIST_ADDR,
            0,
            std::ptr::null(),
            &mut io,
            ids.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids
}

/// Build the tappable-app record for one process object, or `None` if it has no
/// bundle id (background audio daemons, the system process, etc.).
fn app_source(process_obj: AudioObjectID) -> Option<AppAudioSource> {
    let bundle_id = property_cfstring(process_obj, &addr(PROP_PROCESS_BUNDLE_ID, SCOPE_GLOBAL))?;
    let pid = property_pod::<i32>(process_obj, &addr(PROP_PROCESS_PID, SCOPE_GLOBAL)).unwrap_or(-1);
    let is_alive = property_pod::<u32>(process_obj, &addr(PROP_PROCESS_RUNNING_OUTPUT, SCOPE_GLOBAL))
        .map(|v| v != 0)
        .unwrap_or(false);
    let name = app_display_name(pid).unwrap_or_else(|| bundle_id.clone());
    Some(AppAudioSource {
        bundle_id,
        name,
        pid,
        handle: process_obj as TapHandle,
        is_alive,
    })
}

/// A friendly app name from `NSRunningApplication.localizedName`, looked up by
/// pid. `None` when AppKit isn't loaded (headless / tests) or the pid is gone —
/// callers fall back to the bundle id. Resolved dynamically so this crate never
/// links AppKit; the class is present whenever the GUI app is running.
fn app_display_name(pid: i32) -> Option<String> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    if pid < 0 {
        return None;
    }
    let cls = AnyClass::get(c"NSRunningApplication")?;
    // + (instancetype)runningApplicationWithProcessIdentifier:(pid_t)pid
    let app: *mut AnyObject =
        unsafe { msg_send![cls, runningApplicationWithProcessIdentifier: pid] };
    if app.is_null() {
        return None;
    }
    let name: *mut NSString = unsafe { msg_send![app, localizedName] };
    if name.is_null() {
        return None;
    }
    // localizedName is autoreleased; retain into a Retained for a safe read.
    let name: Retained<NSString> = unsafe { Retained::retain(name)? };
    let s = name.to_string();
    (!s.is_empty()).then_some(s)
}

// ── Hot-plug listener plumbing ───────────────────────────────────────────

struct ListenerContext {
    on_change: Box<dyn Fn() + Send + Sync>,
}

extern "C" fn listener_proc(
    _id: AudioObjectID,
    _n: u32,
    _addrs: *const AudioObjectPropertyAddress,
    data: *mut c_void,
) -> OSStatus {
    if !data.is_null() {
        let ctx = unsafe { &*(data as *const ListenerContext) };
        (ctx.on_change)();
    }
    0
}

// ── The directory ────────────────────────────────────────────────────────

/// Stateless macOS device directory. All state lives in CoreAudio; this just
/// queries it.
pub struct CoreAudioDirectory;

impl CoreAudioDirectory {
    pub fn new() -> Self {
        Self
    }

    fn all_device_ids() -> Vec<AudioObjectID> {
        let Some(size) = property_size(SYSTEM_OBJECT, &DEVICES_ADDR) else {
            return Vec::new();
        };
        let count = size / std::mem::size_of::<AudioObjectID>();
        if count == 0 {
            return Vec::new();
        }
        let mut ids = vec![0u32; count];
        let mut io = size as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &DEVICES_ADDR,
                0,
                std::ptr::null(),
                &mut io,
                ids.as_mut_ptr() as *mut c_void,
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids
    }
}

impl AudioDeviceDirectory for CoreAudioDirectory {
    fn list_input_devices(&self) -> Vec<DeviceInfo> {
        let default_input =
            property_pod::<AudioObjectID>(SYSTEM_OBJECT, &DEFAULT_INPUT_ADDR).unwrap_or(0);
        Self::all_device_ids()
            .into_iter()
            .filter_map(|id| device_info(id, default_input))
            .collect()
    }

    fn tap_capabilities(&self) -> TapCapabilities {
        // Both modes ride the same process-tap API; if it's present, both work.
        let supported = crate::capture::tap_supported();
        TapCapabilities { system_audio: supported, app_audio: supported }
    }

    fn list_audio_apps(&self) -> Vec<AppAudioSource> {
        let mut apps: Vec<AppAudioSource> =
            all_process_ids().into_iter().filter_map(app_source).collect();
        // Stable, friendly order: live (currently producing) first, then by name.
        apps.sort_by(|a, b| {
            b.is_alive
                .cmp(&a.is_alive)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        apps
    }

    fn resolve_app(&self, bundle_id: &str) -> Option<AppAudioSource> {
        all_process_ids()
            .into_iter()
            .filter_map(app_source)
            .find(|a| a.bundle_id == bundle_id)
    }

    fn subscribe(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Subscription {
        let ctx = Box::into_raw(Box::new(ListenerContext { on_change }));
        let ctx_addr = ctx as usize;
        let addrs = [DEVICES_ADDR, DEFAULT_INPUT_ADDR];
        for a in &addrs {
            unsafe {
                AudioObjectAddPropertyListener(
                    SYSTEM_OBJECT,
                    a,
                    listener_proc,
                    ctx as *mut c_void,
                );
            }
        }
        Subscription::new(move || {
            let ctx = ctx_addr as *mut ListenerContext;
            for a in &addrs {
                unsafe {
                    AudioObjectRemovePropertyListener(
                        SYSTEM_OBJECT,
                        a,
                        listener_proc,
                        ctx as *mut c_void,
                    );
                }
            }
            // No listener references the context now — reclaim and drop it.
            unsafe { drop(Box::from_raw(ctx)) };
        })
    }

    fn subscribe_processes(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Subscription {
        let ctx = Box::into_raw(Box::new(ListenerContext { on_change }));
        let ctx_addr = ctx as usize;
        unsafe {
            AudioObjectAddPropertyListener(
                SYSTEM_OBJECT,
                &PROCESS_LIST_ADDR,
                listener_proc,
                ctx as *mut c_void,
            );
        }
        Subscription::new(move || {
            let ctx = ctx_addr as *mut ListenerContext;
            unsafe {
                AudioObjectRemovePropertyListener(
                    SYSTEM_OBJECT,
                    &PROCESS_LIST_ADDR,
                    listener_proc,
                    ctx as *mut c_void,
                );
                drop(Box::from_raw(ctx));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_without_panicking() {
        // On CI there may be no input devices — the call must still succeed and
        // return well-formed (possibly empty) data.
        let dir = CoreAudioDirectory::new();
        let devices = dir.list_input_devices();
        for d in &devices {
            assert!(!d.uid.is_empty(), "every device must report a UID");
            assert_eq!(d.channels.len(), d.channel_count() as usize);
            // Subdevice groups, if any, must cover a prefix of the channels in
            // order and never exceed the channel count.
            let mut expected_start = 0u16;
            for g in &d.subdevices {
                assert_eq!(g.channel_start, expected_start);
                expected_start = expected_start.saturating_add(g.channel_count);
            }
            assert!(expected_start <= d.channel_count());
        }
    }

    #[test]
    fn unknown_uid_does_not_resolve() {
        assert!(device_for_uid("definitely-not-a-real-device-uid").is_none());
    }

    #[test]
    fn subscribe_and_drop_is_clean() {
        let dir = CoreAudioDirectory::new();
        let sub = dir.subscribe(Box::new(|| {}));
        drop(sub); // unregisters + frees context without panic/leak
    }

    #[test]
    fn tap_queries_are_well_formed() {
        let dir = CoreAudioDirectory::new();
        // Capabilities follow the capture backend's symbol availability.
        let caps = dir.tap_capabilities();
        assert_eq!(caps.system_audio, crate::capture::tap_supported());
        assert_eq!(caps.app_audio, crate::capture::tap_supported());

        // Every listed app must carry a stable bundle id and a non-empty name.
        for app in dir.list_audio_apps() {
            assert!(!app.bundle_id.is_empty(), "app source needs a bundle id");
            assert!(!app.name.is_empty(), "app source needs a display name");
        }

        // A bogus bundle id never resolves; the listener round-trips cleanly.
        assert!(dir.resolve_app("definitely.not.a.real.bundle.id").is_none());
        let sub = dir.subscribe_processes(Box::new(|| {}));
        drop(sub);
    }

    #[test]
    #[ignore = "hardware-dependent; run with --ignored --nocapture to eyeball real devices"]
    fn dump_devices() {
        let dir = CoreAudioDirectory::new();
        for d in dir.list_input_devices() {
            eprintln!(
                "DEVICE uid={} name={:?} default={} alive={} ch={}",
                d.uid, d.name, d.is_default, d.is_alive, d.channels.len()
            );
            for c in &d.channels {
                eprintln!("    ch{} -> {}", c.index, c.display_name());
            }
            for g in &d.subdevices {
                eprintln!("    SUB {:?} start={} count={}", g.name, g.channel_start, g.channel_count);
            }
        }
    }
}
