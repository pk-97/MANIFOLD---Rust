//! ArtNet external output — full pipeline.
//! Blits the compositor frame through the edge-extend shader, reads back
//! the tiny pixel grid asynchronously, packs into DMX universes, and sends
//! over UDP. Background reachability probe thread.
//! Unity equivalent: ArtNetOutput.cs

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::blit::EdgeExtendBlit;
use crate::dmx;
use crate::readback::ReadbackRequest;
use crate::types::*;

/// ArtNet output pipeline.
pub struct ArtNetOutput {
    // GPU — created during initialize()
    edge_blit: Option<EdgeExtendBlit>,
    readback: ReadbackRequest,
    pending_brightness: f32,
    left_edge_width: f32,
    right_edge_width: f32,

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

    // Reachability probe
    host_reachable: Arc<AtomicBool>,
    shutdown_flag: Arc<AtomicBool>,
    probe_thread: Option<JoinHandle<()>>,

    // Warning throttle
    next_send_warning: Instant,
    suppressed_warnings: u32,

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
            host_reachable: Arc::new(AtomicBool::new(true)),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            probe_thread: None,
            next_send_warning: Instant::now(),
            suppressed_warnings: 0,
            sent_first_packet: false,
            readback_count: 0,
        }
    }

    /// Initialize the ArtNet output pipeline.
    /// Returns false if initialization fails (socket error).
    pub fn initialize(&mut self, device: &wgpu::Device, settings: &LedSettings) -> bool {
        self.strip_count = settings.strip_count;
        self.leds_per_strip = settings.leds_per_strip;
        self.is_bgr = settings.is_bgr;
        self.left_edge_width = settings.left_edge_width;
        self.right_edge_width = settings.right_edge_width;

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

        // Create GPU pipeline
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
            log::error!(
                "[ArtNetOutput] Socket failed to open — no LED packets will be sent."
            );
            self.cleanup();
            return false;
        }

        // Start background reachability probe
        self.shutdown_flag.store(false, Ordering::SeqCst);
        self.host_reachable.store(true, Ordering::SeqCst);

        let reachable = Arc::clone(&self.host_reachable);
        let shutdown = Arc::clone(&self.shutdown_flag);
        let probe_endpoint = self.endpoint;
        self.probe_thread = Some(
            thread::Builder::new()
                .name("ArtNet-ReachabilityProbe".into())
                .spawn(move || {
                    reachability_probe_loop(probe_endpoint, reachable, shutdown);
                })
                .expect("Failed to spawn ArtNet probe thread"),
        );

        self.initialized = true;
        log::info!(
            "[ArtNetOutput] Initialized: {} universe(s), {}x{} LEDs, {:?} addressing, \
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

    /// Blit compositor output through edge-extend shader and submit GPU readback.
    /// Call this BEFORE queue.submit().
    pub fn process_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        brightness: f32,
    ) {
        if !self.initialized {
            return;
        }
        if self.readback.is_pending() {
            return; // Prior readback still in flight — skip this frame
        }

        let blit = self.edge_blit.as_ref().unwrap();

        // GPU blit through edge-extend shader into tiny sample RT
        blit.blit(
            device,
            queue,
            encoder,
            source,
            self.left_edge_width,
            self.right_edge_width,
        );

        // Submit readback of the tiny texture
        self.readback.submit(
            device,
            encoder,
            blit.output_texture(),
            blit.width,
            blit.height,
        );

        // Stash brightness for when readback completes
        self.pending_brightness = brightness;
    }

    /// Check if readback completed and send DMX data if so.
    /// Call this AFTER device.poll().
    pub fn poll_readback(&mut self, device: &wgpu::Device) {
        if !self.initialized {
            return;
        }
        if let Some(pixels) = self.readback.try_read(device) {
            self.readback_count += 1;
            if self.readback_count == 1 {
                log::info!(
                    "[ArtNetOutput] First readback received: {} bytes, {}x{} pixels",
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
            self.artnet_packets[u]
                [ARTNET_HEADER_SIZE..ARTNET_HEADER_SIZE + DMX_UNIVERSE_SIZE]
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
            self.artnet_packets[u]
                [ARTNET_HEADER_SIZE..ARTNET_HEADER_SIZE + DMX_UNIVERSE_SIZE]
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

    fn send_packet(&mut self, packet: &[u8]) {
        let socket = match self.udp_socket.as_ref() {
            Some(s) => s,
            None => return,
        };
        if !self.host_reachable.load(Ordering::Relaxed) {
            return;
        }

        match socket.send_to(packet, self.endpoint) {
            Ok(_) => {
                if !self.sent_first_packet {
                    self.sent_first_packet = true;
                    log::info!(
                        "[ArtNetOutput] First ArtNet packet sent to {} ({} bytes)",
                        self.endpoint,
                        packet.len(),
                    );
                }
            }
            Err(e) => {
                // Immediate backoff
                self.host_reachable.store(false, Ordering::Relaxed);

                let now = Instant::now();
                if now >= self.next_send_warning {
                    let suffix = if self.suppressed_warnings > 0 {
                        format!(
                            " (suppressed {} identical warnings)",
                            self.suppressed_warnings
                        )
                    } else {
                        String::new()
                    };
                    log::warn!("[ArtNetOutput] Send failed: {}{}", e, suffix);
                    self.next_send_warning = now + std::time::Duration::from_secs(5);
                    self.suppressed_warnings = 0;
                } else {
                    self.suppressed_warnings += 1;
                }
            }
        }
    }

    fn cleanup(&mut self) {
        self.initialized = false;

        // Stop probe thread before closing socket
        self.shutdown_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.probe_thread.take() {
            let _ = handle.join();
        }

        self.udp_socket = None;
        self.edge_blit = None;
        self.dmx_buffers.clear();
        self.artnet_packets.clear();
    }
}

// ── Reachability probe (background thread) ──

/// Pre-built ArtPoll packet (14 bytes, never changes).
fn build_artpoll_packet() -> [u8; 14] {
    let mut p = [0u8; 14];
    // "Art-Net\0"
    p[0] = 0x41;
    p[1] = 0x72;
    p[2] = 0x74;
    p[3] = 0x2D;
    p[4] = 0x4E;
    p[5] = 0x65;
    p[6] = 0x74;
    p[7] = 0x00;
    // OpCode 0x2000 (little-endian)
    p[8] = (ARTNET_OP_POLL & 0xFF) as u8;
    p[9] = (ARTNET_OP_POLL >> 8) as u8;
    // Protocol version 14 (big-endian)
    p[10] = (ARTNET_PROTOCOL_VERSION >> 8) as u8;
    p[11] = (ARTNET_PROTOCOL_VERSION & 0xFF) as u8;
    // TalkToMe: 0x00, Priority: 0x00
    p
}

fn probe_via_artpoll(endpoint: SocketAddr) -> bool {
    let artpoll = build_artpoll_packet();

    let Ok(probe) = UdpSocket::bind("0.0.0.0:0") else {
        return false;
    };
    let _ = probe.set_read_timeout(Some(std::time::Duration::from_secs(1)));

    if probe
        .send_to(
            &artpoll,
            SocketAddr::new(endpoint.ip(), DEFAULT_ARTNET_PORT),
        )
        .is_err()
    {
        return false;
    }

    let mut buf = [0u8; 256];
    match probe.recv_from(&mut buf) {
        Ok((len, _)) => {
            // Valid ArtNet response starts with "Art-Net\0"
            len >= 10
                && buf[0..8] == [0x41, 0x72, 0x74, 0x2D, 0x4E, 0x65, 0x74, 0x00]
        }
        Err(_) => false,
    }
}

/// Background thread: probes endpoint reachability every 5 seconds via ArtPoll.
fn reachability_probe_loop(
    endpoint: SocketAddr,
    host_reachable: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) {
    let mut was_reachable = true;

    while !shutdown.load(Ordering::SeqCst) {
        let reachable = probe_via_artpoll(endpoint);

        if reachable != was_reachable {
            was_reachable = reachable;
            host_reachable.store(reachable, Ordering::SeqCst);
            if reachable {
                log::info!("[ArtNetOutput] Host reachable — resuming LED sends.");
            } else {
                log::warn!(
                    "[ArtNetOutput] Host unreachable — pausing LED sends until next probe."
                );
            }
        } else {
            host_reachable.store(reachable, Ordering::Relaxed);
        }

        // Sleep in short intervals so shutdown is responsive
        for _ in 0..50 {
            if shutdown.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
