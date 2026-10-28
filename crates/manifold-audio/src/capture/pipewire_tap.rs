//! System-audio capture on Linux. cpal's PipeWire host opens the default
//! sink as an input, and PipeWire serves that as the sink's monitor: the mix
//! every application plays, with no loopback cable or driver. Capturing one
//! application's audio is not built here.

use cpal::traits::HostTrait;

use super::{AudioCaptureDevice, CaptureBackend};
use crate::directory::TapHandle;

/// System audio needs a running PipeWire session.
pub fn is_supported() -> bool {
    cpal::available_hosts().contains(&cpal::HostId::PipeWire)
}

pub fn open_system_audio() -> Result<Box<dyn CaptureBackend>, String> {
    let host = cpal::host_from_id(cpal::HostId::PipeWire)
        .map_err(|e| format!("PipeWire is not available: {e}"))?;
    let sink = host
        .default_output_device()
        .ok_or_else(|| "PipeWire has no default output".to_string())?;
    Ok(Box::new(AudioCaptureDevice::from_device(sink)?))
}

pub fn open_apps(_handles: &[TapHandle]) -> Result<Box<dyn CaptureBackend>, String> {
    Err("per-application capture is not supported on Linux".to_string())
}
