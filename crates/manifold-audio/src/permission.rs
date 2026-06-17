//! Microphone (TCC) permission status and request.
//!
//! macOS gates access to the built-in microphone behind a per-app permission
//! prompt. Virtual devices (BlackHole) and most aggregates aren't gated, but a
//! send routed to the MacBook mic returns silent zeros if permission is denied
//! — a confusing failure on stage. This module surfaces the status so the app
//! can show a clear "mic blocked" state, and can proactively trigger the
//! prompt. Requires `NSMicrophoneUsageDescription` in the app's Info.plist.
//!
//! Non-macOS targets have no equivalent gate and report [`MicPermission::Granted`].

/// Microphone authorization state, mirroring `AVAuthorizationStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermission {
    /// The user hasn't been asked yet — call [`request_microphone_access`].
    NotDetermined,
    /// Blocked by policy (MDM / parental controls); not user-fixable in-app.
    Restricted,
    /// The user denied access — capture from the mic will be silent.
    Denied,
    /// Access granted.
    Granted,
    /// Status could not be determined (framework absent, e.g. under tests).
    Unknown,
}

impl MicPermission {
    /// Whether mic capture can succeed (granted, or no gate on this platform).
    pub fn is_usable(self) -> bool {
        matches!(self, MicPermission::Granted | MicPermission::Unknown)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::MicPermission;
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use objc2_foundation::NSString;

    /// `AVMediaTypeAudio` is the constant string `@"soun"`.
    fn audio_media_type() -> objc2::rc::Retained<NSString> {
        NSString::from_str("soun")
    }

    fn av_capture_device() -> Option<&'static AnyClass> {
        AnyClass::get(c"AVCaptureDevice")
    }

    pub fn status() -> MicPermission {
        let Some(cls) = av_capture_device() else {
            return MicPermission::Unknown;
        };
        let media = audio_media_type();
        // + (AVAuthorizationStatus)authorizationStatusForMediaType:(AVMediaType)
        let raw: isize = unsafe { msg_send![cls, authorizationStatusForMediaType: &*media] };
        match raw {
            0 => MicPermission::NotDetermined,
            1 => MicPermission::Restricted,
            2 => MicPermission::Denied,
            3 => MicPermission::Granted,
            _ => MicPermission::Unknown,
        }
    }

    pub fn request() {
        let Some(cls) = av_capture_device() else {
            return;
        };
        let media = audio_media_type();
        // The request is async; the OS shows the prompt and the status updates
        // for the next query. A heap block is required since it outlives this
        // call. We don't act on the result here — the runtime re-queries.
        let handler = block2::RcBlock::new(|_granted: objc2::runtime::Bool| {});
        unsafe {
            let _: () = msg_send![
                cls,
                requestAccessForMediaType: &*media,
                completionHandler: &*handler,
            ];
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::MicPermission;
    pub fn status() -> MicPermission {
        MicPermission::Granted
    }
    pub fn request() {}
}

/// Current microphone authorization status.
pub fn status() -> MicPermission {
    imp::status()
}

/// Trigger the system permission prompt if not yet determined. No-op if already
/// decided. Async — re-query [`status`] afterward.
pub fn request_microphone_access() {
    imp::request();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_queryable_without_panic() {
        // Under `cargo test` AVFoundation may be unlinked → Unknown; in the app
        // bundle it returns a real status. Either way, no panic.
        let s = status();
        // Unknown is usable (we don't block when we can't tell).
        if s == MicPermission::Unknown {
            assert!(s.is_usable());
        }
    }
}
