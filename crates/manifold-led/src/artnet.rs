//! ArtNet external output — full pipeline (native Metal).
//! Dispatches the edge-extend compute shader on the compositor output,
//! reads back the tiny pixel grid via shared-memory buffer, packs into
//! DMX universes, and sends over UDP.
//! Unity equivalent: ArtNetOutput.cs

use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use manifold_gpu::{GpuDevice, GpuTexture};

use crate::blit::EdgeExtendBlit;
use crate::dmx;
use crate::readback::ReadbackRequest;
use crate::types::*;

/// ArtNet output pipeline (native Metal).
pub struct ArtNetOutput {
    // GPU — created during initialize()
    edge_blit: Option<EdgeExtendBlit>,
    readback: ReadbackRequest,
    pending_brightness: f32,
    left_edge_width: f32,
    right_edge_width: f32,
    blur_radius: f32,

    // Network
    udp_socket: Option<UdpSocket>,
    endpoint: SocketAddr,

    // Pre-allocated DMX buffers (one per universe)
    dmx_buffers: Vec<Vec<u8>>,
    artnet_packets: Vec<Vec<u8>>,

    // Config snapshot
    strip_count: u32,
    leds_per_strip: u32,
    is_bgr: bool,
    universe_count: usize,
    strip_start_channels: Vec<usize>,

    initialized: bool,

    // Connection health — pause GPU work when host is unreachable
    consecutive_failures: u32,
    /// When true, skip GPU work (process_frame/poll_readback) until a probe succeeds.
    disconnected: bool,
    last_probe: Instant,
    probe_interval_secs: u64,
    /// Successful sends since last resume from disconnected state.
    sends_since_resume: u32,
    /// Whether the initial disconnect has been logged (suppress flap spam).
    logged_disconnect: bool,

    // Debug: log first successful send
    sent_first_packet: bool,
    readback_count: u64,
}

impl Default for ArtNetOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtNetOutput {
    pub fn new() -> Self {
        Self {
            edge_blit: None,
            readback: ReadbackRequest::new(),
            pending_brightness: 1.0,
            left_edge_width: 0.2,
            right_edge_width: 0.2,
            blur_radius: 12.0,
            udp_socket: None,
            endpoint: SocketAddr::from(([0, 0, 0, 0], 0)),
            dmx_buffers: Vec::new(),
            artnet_packets: Vec::new(),
            strip_count: 0,
            leds_per_strip: 0,
            is_bgr: true,
            universe_count: 0,
            strip_start_channels: Vec::new(),
            initialized: false,
            consecutive_failures: 0,
            disconnected: false,
            last_probe: Instant::now(),
            probe_interval_secs: Self::PROBE_INTERVAL_BASE_SECS,
            sends_since_resume: 0,
            logged_disconnect: false,
            sent_first_packet: false,
            readback_count: 0,
        }
    }

    /// Initialize the ArtNet output pipeline.
    /// Returns false if initialization fails (socket error).
    pub fn initialize(&mut self, device: &GpuDevice, settings: &LedSettings) -> bool {
        self.strip_count = settings.strip_count;
        self.leds_per_strip = settings.leds_per_strip;
        self.is_bgr = settings.is_bgr;
        self.left_edge_width = settings.left_edge_width;
        self.right_edge_width = settings.right_edge_width;
        self.blur_radius = settings.blur_radius;

        // Pre-compute strip start channels and universe count
        self.strip_start_channels = vec![0usize; self.strip_count as usize];
        match settings.strip_addressing {
            StripAddressing::PerUniverse => {
                for i in 0..self.strip_count as usize {
                    self.strip_start_channels[i] = i * DMX_UNIVERSE_SIZE;
                }
                self.universe_count = self.strip_count as usize;
            }
            StripAddressing::Packed => {
                for i in 0..self.strip_count as usize {
                    self.strip_start_channels[i] =
                        i * self.leds_per_strip as usize * CHANNELS_PER_LED;
                }
                let max_channel =
                    self.strip_count as usize * self.leds_per_strip as usize * CHANNELS_PER_LED;
                self.universe_count =
                    (max_channel as f32 / DMX_UNIVERSE_SIZE as f32).ceil() as usize;
            }
        }

        // Create native Metal compute pipeline
        self.edge_blit = Some(EdgeExtendBlit::new(
            device,
            self.strip_count,
            self.leds_per_strip,
        ));

        // Pre-allocate DMX buffers and ArtNet packet headers
        self.dmx_buffers = (0..self.universe_count)
            .map(|_| vec![0u8; DMX_UNIVERSE_SIZE])
            .collect();
        self.artnet_packets = (0..self.universe_count)
            .map(|i| {
                let mut packet = vec![0u8; ARTNET_HEADER_SIZE + DMX_UNIVERSE_SIZE];
                dmx::write_artnet_header(&mut packet, settings.start_universe + i as u16);
                packet
            })
            .collect();

        // Open UDP socket
        if !self.open_socket(&settings.artnet_ip, settings.artnet_port) {
            eprintln!("[ArtNet] Socket failed to open — no LED packets will be sent.");
            self.cleanup();
            return false;
        }

        self.initialized = true;
        eprintln!(
            "[ArtNet] Initialized: {} universe(s), {}x{} LEDs, {:?} addressing, \
             BGR={}, target={}",
            self.universe_count,
            self.strip_count,
            self.leds_per_strip,
            settings.strip_addressing,
            self.is_bgr,
            self.endpoint,
        );
        true
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Returns true if the host is currently unreachable (GPU work paused).
    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    /// Dispatch edge-extend compute and submit GPU readback.
    /// Uses a dedicated native Metal encoder (separate from the content frame).
    /// `signal_value` is the GpuEvent value that will be signaled after this
    /// encoder commits — used by poll_readback to know when the GPU is done.
    pub fn process_frame(
        &mut self,
        device: &GpuDevice,
        source: &GpuTexture,
        brightness: f32,
        signal_value: u64,
        event: &manifold_gpu::GpuEvent,
    ) {
        if !self.initialized || self.disconnected {
            return;
        }
        if self.readback.is_pending() {
            return; // Prior readback still in flight — skip this frame
        }

        let blit = self.edge_blit.as_ref().unwrap();

        // Create a dedicated encoder for LED work.
        let mut enc = device.create_encoder("LED");

        // Dispatch edge-extend compute: source → tiny output texture.
        blit.blit(
            &mut enc,
            source,
            self.left_edge_width,
            self.right_edge_width,
            self.blur_radius,
        );

        // Copy tiny texture to shared-memory readback buffer.
        self.readback.submit(
            device,
            &mut enc,
            &blit.output,
            blit.width,
            blit.height,
            signal_value,
        );

        // Signal event and commit.
        enc.signal_event_value(event, signal_value);
        enc.commit();

        // Stash brightness for when readback completes
        self.pending_brightness = brightness;
    }

    /// Check if readback completed and send DMX data if so.
    pub fn poll_readback(&mut self, event: &manifold_gpu::GpuEvent) {
        if !self.initialized || self.disconnected {
            return;
        }
        if let Some(pixels) = self.readback.try_read(event) {
            self.readback_count += 1;
            if self.readback_count == 1 {
                eprintln!(
                    "[ArtNet] First readback: {} bytes ({}x{} px)",
                    pixels.len(),
                    self.strip_count,
                    self.leds_per_strip,
                );
            }
            let brightness = self.pending_brightness;
            self.pack_and_send(&pixels, brightness);
        }
    }

    /// Send all-zeros to every universe (turn off all LEDs).
    pub fn blackout(&mut self) {
        if !self.initialized {
            return;
        }
        for u in 0..self.universe_count {
            self.dmx_buffers[u].fill(0);
            self.artnet_packets[u][ARTNET_HEADER_SIZE..ARTNET_HEADER_SIZE + DMX_UNIVERSE_SIZE]
                .copy_from_slice(&self.dmx_buffers[u]);
            let packet = self.artnet_packets[u].clone();
            self.send_packet(&packet);
        }
    }

    /// Blackout then release all resources.
    pub fn shutdown(&mut self) {
        if !self.initialized {
            return;
        }
        self.blackout();
        self.cleanup();
    }

    // ── Pixel packing ──

    fn pack_and_send(&mut self, pixels: &[u8], brightness: f32) {
        // Clear all universe buffers
        for buf in &mut self.dmx_buffers {
            buf.fill(0);
        }

        // Pack each strip's pixels into DMX universes
        for strip in 0..self.strip_count as usize {
            dmx::sample_strip_to_universes(
                &mut self.dmx_buffers,
                pixels,
                self.strip_count as usize,
                strip,
                self.leds_per_strip as usize,
                self.strip_start_channels[strip],
                self.is_bgr,
                brightness,
            );
        }

        // Copy DMX data into ArtNet packets and send
        for u in 0..self.universe_count {
            self.artnet_packets[u][ARTNET_HEADER_SIZE..ARTNET_HEADER_SIZE + DMX_UNIVERSE_SIZE]
                .copy_from_slice(&self.dmx_buffers[u]);
            let packet = self.artnet_packets[u].clone();
            self.send_packet(&packet);
        }
    }

    // ── UDP socket ──

    fn open_socket(&mut self, ip: &str, port: u16) -> bool {
        match ip.parse::<std::net::IpAddr>() {
            Ok(addr) => {
                self.endpoint = SocketAddr::new(addr, port);
            }
            Err(e) => {
                log::warn!("[ArtNetOutput] Invalid IP address '{}': {}", ip, e);
                return false;
            }
        }

        match UdpSocket::bind("0.0.0.0:0") {
            Ok(sock) => {
                let _ = sock.set_broadcast(true);
                self.udp_socket = Some(sock);
                true
            }
            Err(e) => {
                log::warn!("[ArtNetOutput] Failed to open socket: {}", e);
                self.udp_socket = None;
                false
            }
        }
    }

    /// How many consecutive send failures before we pause GPU work.
    const DISCONNECT_THRESHOLD: u32 = 30;
    /// Base probe interval (seconds). Doubles on each flap, caps at MAX.
    const PROBE_INTERVAL_BASE_SECS: u64 = 3;
    const PROBE_INTERVAL_MAX_SECS: u64 = 10;
    /// Successful sends needed after resume to consider connection stable.
    const STABLE_THRESHOLD: u32 = 120;

    /// Try to reconnect by sending a single packet.
    /// Called from the controller on every tick when disconnected.
    pub fn try_probe(&mut self) {
        if !self.disconnected || !self.initialized {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last_probe).as_secs() < self.probe_interval_secs {
            return;
        }
        self.last_probe = now;

        let socket = match self.udp_socket.as_ref() {
            Some(s) => s,
            None => return,
        };
        if self.artnet_packets.is_empty() {
            return;
        }
        let probe = &self.artnet_packets[0];
        if socket.send_to(probe, self.endpoint).is_ok() {
            // UDP send_to "succeeds" even for unreachable hosts (ARP queued).
            // Don't log yet — wait for sustained successful sends in send_packet.
            self.disconnected = false;
            self.consecutive_failures = 0;
            self.sends_since_resume = 0;
        }
    }

    fn send_packet(&mut self, packet: &[u8]) {
        let socket = match self.udp_socket.as_ref() {
            Some(s) => s,
            None => return,
        };

        match socket.send_to(packet, self.endpoint) {
            Ok(_) => {
                self.consecutive_failures = 0;
                self.sends_since_resume += 1;
                if !self.sent_first_packet {
                    self.sent_first_packet = true;
                    eprintln!(
                        "[ArtNet] First data packet sent to {} ({} bytes)",
                        self.endpoint,
                        packet.len(),
                    );
                }
                // Connection confirmed stable — log resume and reset backoff
                if self.logged_disconnect
                    && self.sends_since_resume == Self::STABLE_THRESHOLD
                {
                    eprintln!("[ArtNet] Host reachable — resumed LED output");
                    self.logged_disconnect = false;
                    self.probe_interval_secs = Self::PROBE_INTERVAL_BASE_SECS;
                }
            }
            Err(e) => {
                self.consecutive_failures += 1;
                if self.consecutive_failures == Self::DISCONNECT_THRESHOLD {
                    if self.sends_since_resume < Self::STABLE_THRESHOLD {
                        // Flap: probe succeeded but real sends failed.
                        // Back off silently.
                        self.probe_interval_secs = (self.probe_interval_secs * 2)
                            .min(Self::PROBE_INTERVAL_MAX_SECS);
                    }
                    if !self.logged_disconnect {
                        eprintln!(
                            "[ArtNet] Host unreachable ({e}) — pausing LED output"
                        );
                        self.logged_disconnect = true;
                    }
                    self.disconnected = true;
                }
            }
        }
    }

    fn cleanup(&mut self) {
        self.initialized = false;
        self.udp_socket = None;
        self.edge_blit = None;
        self.dmx_buffers.clear();
        self.artnet_packets.clear();
    }
}
