//! Cross-platform fallback [`AudioDeviceDirectory`] built on cpal.
//!
//! Used on every non-macOS platform until a native backend (PipeWire/JACK on
//! Linux, WASAPI on Windows) lands. cpal only knows device names and channel
//! counts, so this backend fills what it can and leans on the graceful
//! degradation the trait is designed around: the UID is the device name (the
//! only stable handle cpal offers), channels are unnamed (→ "Channel N"),
//! liveness is assumed, and there is no hot-plug subscription.
//!
//! This keeps the *whole app* platform-agnostic above the trait: the UI, the
//! save format, and routing all work on `DeviceInfo` regardless of host.

#![cfg_attr(target_os = "macos", allow(dead_code))]

use cpal::traits::{DeviceTrait, HostTrait};

use super::{AudioDeviceDirectory, ChannelInfo, DeviceInfo, Subscription};

pub struct CpalDirectory;

impl CpalDirectory {
    pub fn new() -> Self {
        Self
    }
}

impl AudioDeviceDirectory for CpalDirectory {
    fn list_input_devices(&self) -> Vec<DeviceInfo> {
        let host = cpal::default_host();
        let default_name = host.default_input_device().and_then(|d| d.name().ok());

        let Ok(inputs) = host.input_devices() else {
            return Vec::new();
        };

        inputs
            .filter_map(|device| {
                let name = device.name().ok()?;
                let channel_count = device
                    .default_input_config()
                    .map(|c| c.channels())
                    .unwrap_or(0);
                if channel_count == 0 {
                    return None;
                }
                let channels = (0..channel_count)
                    .map(|index| ChannelInfo { index, name: None })
                    .collect();
                Some(DeviceInfo {
                    is_default: default_name.as_deref() == Some(&name),
                    // cpal has no stable UID — the name is the only handle.
                    uid: name.clone(),
                    name,
                    is_alive: true,
                    channels,
                    subdevices: Vec::new(),
                })
            })
            .collect()
    }

    fn subscribe(&self, _on_change: Box<dyn Fn() + Send + Sync>) -> Subscription {
        // cpal exposes no device-change notification; callers fall back to
        // re-querying on demand (e.g. each time a dropdown opens).
        Subscription::inert()
    }
}
