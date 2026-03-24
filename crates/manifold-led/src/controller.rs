//! LED output controller — thin orchestrator.
//! Owns an ArtNetOutput and manages enable/disable, blackout on no content.
//! Unity equivalent: LEDOutputController.cs (energy gate deferred).

use crate::artnet::ArtNetOutput;
use crate::types::LedSettings;

/// Orchestrates LED output. Owns the ArtNet backend.
pub struct LedOutputController {
    output: ArtNetOutput,
    initialized: bool,
    enabled: bool,
    sent_blackout: bool,
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
        }
    }

    /// Initialize the LED output pipeline.
    pub fn initialize(&mut self, device: &wgpu::Device, settings: &LedSettings) -> bool {
        if self.initialized {
            self.shutdown();
        }

        self.enabled = settings.enabled;

        if !self.output.initialize(device, settings) {
            log::warn!(
                "[LedOutputController] ArtNet output failed to initialize."
            );
            return false;
        }

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

    /// Process a frame: blit compositor output through edge-extend and submit
    /// readback. Call BEFORE queue.submit().
    ///
    /// `active_clip_count`: number of active clips. If 0, sends blackout.
    pub fn process_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        active_clip_count: usize,
    ) {
        if !self.initialized || !self.enabled {
            return;
        }

        // No active content → blackout
        if active_clip_count == 0 {
            if !self.sent_blackout {
                self.output.blackout();
                self.sent_blackout = true;
            }
            return;
        }

        self.sent_blackout = false;

        // Brightness is always 1.0 (energy gate deferred)
        self.output
            .process_frame(device, queue, encoder, source, 1.0);
    }

    /// Poll GPU readback and send DMX data if ready. Call AFTER device.poll().
    pub fn poll_readback(&mut self, device: &wgpu::Device) {
        if !self.initialized || !self.enabled {
            return;
        }
        self.output.poll_readback(device);
    }

    /// Shut down LED output: blackout + release resources.
    pub fn shutdown(&mut self) {
        if !self.initialized {
            return;
        }
        self.output.shutdown();
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
