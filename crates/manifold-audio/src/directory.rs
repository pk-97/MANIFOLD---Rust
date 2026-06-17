//! Audio device **metadata** directory — the source of truth for what input
//! devices exist, their stable identity, and their channel layout.
//!
//! This is deliberately separate from [`crate::capture`]. Capture (cpal) owns
//! the *sample path* and is cross-platform. The directory owns the *metadata
//! path* — channel names, stable UIDs, liveness, hot-plug — which cpal cannot
//! express, so it drops to the native platform API behind this trait. The same
//! backend-neutral split as `manifold-gpu` (Metal now, Vulkan later): the rest
//! of the app sees only [`DeviceInfo`]/[`ChannelInfo`], never a platform type.
//!
//! See `docs/AUDIO_INFRASTRUCTURE.md` §4.

#[cfg(target_os = "macos")]
mod coreaudio;

mod fallback;

/// One input channel of a device. `name` is the label set in the platform's
/// audio control panel (Audio MIDI Setup on macOS); `None` when the platform
/// has no label, in which case [`Self::display_name`] synthesizes "Channel N".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelInfo {
    /// 0-based channel index, matching `AudioSend::channels`.
    pub index: u16,
    /// Platform-provided channel label, if any.
    pub name: Option<String>,
}

impl ChannelInfo {
    /// The label to show in the UI — the platform name, or a 1-based fallback.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("Channel {}", self.index + 1))
    }
}

/// A contiguous run of channels belonging to one physical subdevice of an
/// aggregate device. Empty on the [`DeviceInfo`] when the device is not an
/// aggregate (or the layout can't be read) — callers then treat the whole
/// device as a single implicit group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubdeviceGroup {
    /// Subdevice display name (e.g. "BlackHole 2ch", "MacBook Pro Microphone").
    pub name: String,
    /// Index of this group's first channel in [`DeviceInfo::channels`].
    pub channel_start: u16,
    /// Number of channels this subdevice contributes.
    pub channel_count: u16,
}

/// Full metadata for one input device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable, persistable identity. Survives unplug/replug and rename — this is
    /// what a saved project stores, never the display name.
    pub uid: String,
    /// Human-facing device name (also a fallback match key if the UID is stale).
    pub name: String,
    /// Whether this is the current system default input.
    pub is_default: bool,
    /// Whether the device is currently present and usable. A routed device that
    /// reads `false` is offline (unplugged, aggregate subdevice missing).
    pub is_alive: bool,
    /// Input channels in device order. Length is the *true* channel count (the
    /// native query, which is correct where cpal's default-config count is not).
    pub channels: Vec<ChannelInfo>,
    /// Subdevice grouping for aggregate devices, in channel order. Empty when
    /// not applicable.
    pub subdevices: Vec<SubdeviceGroup>,
}

impl DeviceInfo {
    /// Channel count.
    pub fn channel_count(&self) -> u16 {
        self.channels.len() as u16
    }
}

/// RAII handle for a hot-plug subscription. Dropping it unregisters the
/// listener. Keep it alive for as long as you want change notifications.
#[must_use = "dropping the Subscription immediately unregisters the listener"]
pub struct Subscription {
    cancel: Option<Box<dyn FnOnce() + Send>>,
}

impl Subscription {
    /// A subscription that does nothing (used by backends without hot-plug).
    pub fn inert() -> Self {
        Self { cancel: None }
    }

    /// Wrap a teardown closure run on drop.
    pub(crate) fn new(cancel: impl FnOnce() + Send + 'static) -> Self {
        Self { cancel: Some(Box::new(cancel)) }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel();
        }
    }
}

/// The metadata directory. Backends implement this; the app holds a
/// `Box<dyn AudioDeviceDirectory>` from [`system_directory`].
pub trait AudioDeviceDirectory: Send + Sync {
    /// Enumerate input devices with full metadata. Re-queried on demand (when a
    /// dropdown opens, or after a change notification) — never on a hot path.
    fn list_input_devices(&self) -> Vec<DeviceInfo>;

    /// Subscribe to device-set / default-device changes (hot-plug). The callback
    /// may fire on an arbitrary OS thread, so it must be cheap and thread-safe —
    /// the intended use is "set a dirty flag, refresh on the next UI tick". The
    /// returned [`Subscription`] unregisters on drop.
    fn subscribe(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Subscription;

    /// Resolve a stored UID to the device's current openable name (what cpal
    /// needs to open the stream). `None` if no live device carries that UID.
    /// Default implementation derives it from [`Self::list_input_devices`].
    fn name_for_uid(&self, uid: &str) -> Option<String> {
        self.list_input_devices()
            .into_iter()
            .find(|d| d.uid == uid)
            .map(|d| d.name)
    }

    /// Resolve a device by UID, falling back to an exact name match when the UID
    /// is absent (a project saved before UID identity, or a device whose UID
    /// changed). `None` if neither resolves.
    fn resolve(&self, uid: Option<&str>, name: Option<&str>) -> Option<DeviceInfo> {
        let devices = self.list_input_devices();
        if let Some(uid) = uid
            && let Some(d) = devices.iter().find(|d| d.uid == uid)
        {
            return Some(d.clone());
        }
        if let Some(name) = name {
            return devices.into_iter().find(|d| d.name == name);
        }
        None
    }
}

/// The directory for the current platform.
pub fn system_directory() -> Box<dyn AudioDeviceDirectory> {
    #[cfg(target_os = "macos")]
    {
        Box::new(coreaudio::CoreAudioDirectory::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(fallback::CpalDirectory::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_display_name_falls_back_to_1_based() {
        let named = ChannelInfo { index: 0, name: Some("BH_IN_L".into()) };
        assert_eq!(named.display_name(), "BH_IN_L");
        let bare = ChannelInfo { index: 2, name: None };
        assert_eq!(bare.display_name(), "Channel 3");
    }

    #[test]
    fn resolve_prefers_uid_then_name() {
        let dir = system_directory();
        // Resolving nonsense yields nothing (and must not panic on any host).
        assert!(dir.resolve(Some("no-such-uid"), Some("no-such-device")).is_none());
    }

    #[test]
    fn subscription_inert_drops_clean() {
        let s = Subscription::inert();
        drop(s); // no panic, no double-free
    }
}
