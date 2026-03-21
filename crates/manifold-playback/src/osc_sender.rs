//! OSC position sender — sends transport state to Max4Live device.
//! Mechanical translation of Unity OscPositionSender.cs.
//!
//! OSC protocol:
//!   /manifold/play       float (beat position on play start + seek)
//!   /manifold/transport   int   (0 = stop)
//!   /manifold/position   float (beat position on seek during playback)
//!
//! Echo suppression: checks SyncArbiter.suppress_next_transport.
//! When true, consumes the flag and skips the send (prevents echo loops).

use std::net::UdpSocket;
use crate::sync::SyncArbiter;

const DESTINATION_IP: &str = "127.0.0.1";
const SEEK_THRESHOLD_BEATS: f32 = 0.5;

/// OSC position sender — sends play/stop/seek messages to DAW.
/// Port of Unity OscPositionSender.cs.
pub struct OscPositionSender {
    socket: Option<UdpSocket>,
    send_port: i32,
    is_enabled: bool,
    last_was_playing: bool,
    last_sent_beat: f32,
    last_sent_realtime: f64,
}

impl OscPositionSender {
    pub fn new() -> Self {
        Self {
            socket: None,
            send_port: 9001,
            is_enabled: false,
            last_was_playing: false,
            last_sent_beat: 0.0,
            last_sent_realtime: 0.0,
        }
    }

    pub fn is_sender_enabled(&self) -> bool { self.is_enabled }

    pub fn enable_sender(&mut self, port: i32, is_playing: bool, current_beat: f32, realtime: f64) {
        if self.is_enabled { return; }

        let addr = format!("{}:{}", DESTINATION_IP, port);
        match UdpSocket::bind("0.0.0.0:0") {
            Ok(sock) => {
                if let Err(e) = sock.connect(&addr) {
                    log::error!("[OscPositionSender] Failed to connect to {}: {}", addr, e);
                    return;
                }
                self.socket = Some(sock);
            }
            Err(e) => {
                log::error!("[OscPositionSender] Failed to bind socket: {}", e);
                return;
            }
        }

        self.send_port = port;
        self.last_was_playing = is_playing;
        self.last_sent_beat = current_beat;
        self.last_sent_realtime = realtime;
        self.is_enabled = true;

        log::info!("[OscPositionSender] Enabled — {}:{}", DESTINATION_IP, port);
    }

    pub fn disable_sender(&mut self, arbiter: &mut SyncArbiter) {
        if !self.is_enabled { return; }
        self.is_enabled = false;
        arbiter.clear_ownership();
        self.socket = None;
        log::info!("[OscPositionSender] Disabled");
    }

    /// Called after engine tick (LateUpdate equivalent).
    /// Detects transport changes and seeks, sends OSC messages.
    pub fn late_update(
        &mut self,
        is_playing: bool,
        current_beat: f32,
        seconds_per_beat: f32,
        realtime: f64,
        arbiter: &mut SyncArbiter,
    ) {
        if !self.is_enabled || self.socket.is_none() { return; }

        let now = realtime;

        // 1. Transport state change
        if is_playing != self.last_was_playing {
            // External sync triggered this — don't echo back
            if arbiter.suppress_next_transport {
                arbiter.suppress_next_transport = false;
                self.last_was_playing = is_playing;
                self.last_sent_beat = current_beat;
                self.last_sent_realtime = now;
                return;
            }

            if is_playing {
                self.try_send_float("/manifold/play", current_beat);
                arbiter.set_manifold_owns();
            } else {
                self.try_send_int("/manifold/transport", 0);
                // Don't clear ManifoldOwnsPlayback here — CLK still shows
                // playing until Ableton processes the stop. MidiClockSyncController
                // clears it once both sides agree they're stopped.
            }

            self.last_was_playing = is_playing;
            self.last_sent_beat = current_beat;
            self.last_sent_realtime = now;
            return;
        }

        // 2. Seek detection: compare current beat to expected beat
        let mut expected_beat = self.last_sent_beat;
        if is_playing && seconds_per_beat > 0.0 {
            let elapsed = (now - self.last_sent_realtime) as f32;
            expected_beat += elapsed / seconds_per_beat;
        }

        let beat_delta = (current_beat - expected_beat).abs();
        if beat_delta > SEEK_THRESHOLD_BEATS {
            self.try_send_float("/manifold/position", current_beat);
            self.last_sent_beat = current_beat;
            self.last_sent_realtime = now;
            return;
        }

        // 3. Track position for next frame's expected-beat calculation
        if is_playing {
            self.last_sent_beat = current_beat;
            self.last_sent_realtime = now;
        }
    }

    // ── OSC encoding (minimal, no external dependency) ──

    fn try_send_float(&self, address: &str, value: f32) {
        if let Some(ref socket) = self.socket {
            let packet = encode_osc_float(address, value);
            let _ = socket.send(&packet); // Silently ignore SocketException (matches Unity)
        }
    }

    fn try_send_int(&self, address: &str, value: i32) {
        if let Some(ref socket) = self.socket {
            let packet = encode_osc_int(address, value);
            let _ = socket.send(&packet);
        }
    }
}

impl Default for OscPositionSender {
    fn default() -> Self { Self::new() }
}

// ── Minimal OSC encoding ──────────────────────────────────────────

/// Encode an OSC message with a single float argument.
fn encode_osc_float(address: &str, value: f32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    // Address pattern (null-terminated, padded to 4-byte boundary)
    write_osc_string(&mut buf, address);
    // Type tag string: ",f"
    write_osc_string(&mut buf, ",f");
    // Float argument (big-endian)
    buf.extend_from_slice(&value.to_be_bytes());
    buf
}

/// Encode an OSC message with a single int argument.
fn encode_osc_int(address: &str, value: i32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    write_osc_string(&mut buf, address);
    write_osc_string(&mut buf, ",i");
    buf.extend_from_slice(&value.to_be_bytes());
    buf
}

/// Write a null-terminated string padded to 4-byte boundary.
fn write_osc_string(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0); // null terminator
    // Pad to 4-byte boundary
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_osc_float_encoding() {
        let packet = encode_osc_float("/manifold/play", 4.5);
        // Address: "/manifold/play\0" padded to 16 bytes
        assert_eq!(&packet[0..15], b"/manifold/play\0");
        assert_eq!(packet[15], 0); // padding
        // Type tag: ",f\0" padded to 4 bytes
        assert_eq!(&packet[16..18], b",f");
        // Float 4.5 in big-endian
        let float_bytes = &packet[20..24];
        let val = f32::from_be_bytes([float_bytes[0], float_bytes[1], float_bytes[2], float_bytes[3]]);
        assert!((val - 4.5).abs() < 0.001);
    }

    #[test]
    fn test_osc_int_encoding() {
        let packet = encode_osc_int("/manifold/transport", 0);
        assert!(packet.len() >= 28);
        let int_start = packet.len() - 4;
        let val = i32::from_be_bytes([packet[int_start], packet[int_start+1], packet[int_start+2], packet[int_start+3]]);
        assert_eq!(val, 0);
    }
}
