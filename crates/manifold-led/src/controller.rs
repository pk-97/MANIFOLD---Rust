//! LED output controller — thin orchestrator.
//! Owns an ArtNetOutput and manages enable/disable, blackout on no content.
//! Unity equivalent: LEDOutputController.cs (energy gate deferred).

use manifold_gpu::{GpuDevice, GpuTexture};

use crate::artnet::ArtNetOutput;
use crate::types::LedSettings;

/// Orchestrates LED output. Owns the ArtNet backend.
pub struct LedOutputController {
    output: ArtNetOutput,
    initialized: bool,
    enabled: bool,
    sent_blackout: bool,
    /// Dedicated GpuEvent for tracking LED GPU readback completion.
    /// Created during initialize(), used by process_frame/poll_readback.
    event: Option<manifold_gpu::GpuEvent>,
    /// Signal value counter for the LED event.
    signal_counter: u64,
}

impl Default for LedOutputController {
    fn default() -> Self {
        Self::new()
    }
}

impl LedOutputController {
    pub fn new() -> Self {
        Self {
            output: ArtNetOutput::new(),
            initialized: false,
            enabled: true,
            sent_blackout: false,
            event: None,
            signal_counter: 0,
        }
    }

    /// Initialize the LED output pipeline with native Metal GPU.
    pub fn initialize(&mut self, device: &GpuDevice, settings: &LedSettings) -> bool {
        if self.initialized {
            self.shutdown();
        }

        self.enabled = settings.enabled;

        if !self.output.initialize(device, settings) {
            log::warn!("[LedOutputController] ArtNet output failed to initialize.");
            return false;
        }

        self.event = Some(device.create_event());
        self.signal_counter = 0;
        self.initialized = true;
        log::info!("[LedOutputController] Initialized.");
        true
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn is_enabled(&self) -> bool {
        self.initialized && self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled && self.initialized {
            self.output.blackout();
        }
    }

    /// Process a frame: dispatch edge-extend compute and submit readback.
    ///
    /// `source`: compositor output texture (native GpuTexture).
    /// `active_clip_count`: number of active clips. If 0, sends blackout.
    pub fn process_frame(
        &mut self,
        device: &GpuDevice,
        source: &GpuTexture,
        active_clip_count: usize,
        brightness: f32,
    ) {
        if !self.initialized || !self.enabled {
            return;
        }

        // No active content → blackout. Cancel any in-flight readback first so
        // a stale completion can't overwrite the blackout a frame or two later.
        if active_clip_count == 0 {
            self.output.discard_pending_readback();
            if !self.sent_blackout {
                self.output.blackout();
                self.sent_blackout = true;
            }
            return;
        }

        self.sent_blackout = false;

        // Increment signal value for this LED frame.
        self.signal_counter += 1;

        self.output.process_frame(
            device,
            source,
            brightness.clamp(0.0, 1.0),
            self.signal_counter,
            self.event.as_ref().unwrap(),
        );
    }

    /// Poll GPU readback and send DMX data if ready.
    pub fn poll_readback(&mut self) {
        if !self.initialized || !self.enabled {
            return;
        }
        if let Some(ref event) = self.event {
            self.output.poll_readback(event);
        }
    }

    /// Shut down LED output: blackout + release resources.
    pub fn shutdown(&mut self) {
        if !self.initialized {
            return;
        }
        self.output.shutdown();
        self.event = None;
        self.initialized = false;
        self.sent_blackout = false;
    }
}

impl Drop for LedOutputController {
    fn drop(&mut self) {
        if self.initialized {
            self.output.blackout();
        }
    }
}
