//! Philips Hue Entertainment API streaming.
//!
//! Drives a configured Hue *Entertainment Area* (a group of bulbs the user
//! set up in the Hue app — e.g. "Studio") at 25 Hz over DTLS-PSK on UDP/2100,
//! sending one dominant+brightest color extracted from the LED-strip output.
//! The bridge then drives all bulbs in the area at full Zigbee rate.
//!
//! ## Wire protocol
//!
//! Hue Entertainment uses a simple stateless UDP datagram protocol authenticated
//! with a pre-shared key (DTLS 1.2, ciphersuite `TLS_PSK_WITH_AES_128_GCM_SHA256`).
//! The PSK identity is the bridge's "username" string (the hex blob the bridge
//! returns at pairing time); the PSK itself is the "clientkey" (32 hex chars =
//! 16 binary bytes). Both are tied to one bridge — re-pair to get new ones.
//!
//! Each datagram is `HueStream` v2:
//!
//! ```text
//!   bytes 0..9     "HueStream"
//!   byte 9         version major = 2
//!   byte 10        version minor = 0
//!   byte 11        sequence id (ignored by bridge, can be 0)
//!   bytes 12..14   reserved (0)
//!   byte 14        color space (0 = RGB, 1 = XY+brightness)
//!   byte 15        reserved (0)
//!   bytes 16..52   entertainment_configuration UUID (36 ASCII bytes, hyphens included)
//!   per channel:   1 byte channel id, then 6 bytes RGB (3× big-endian uint16)
//! ```
//!
//! Sending must start within ~10 s of the HTTP "start" action below or the
//! bridge drops the stream. Same applies for inter-packet gaps mid-stream —
//! at 25 Hz we send every 40 ms with plenty of margin.
//!
//! ## Bridge handshake (REST)
//!
//! - `POST  /api`                                       → pair (returns username + clientkey)
//! - `GET   /clip/v2/resource/entertainment_configuration` → list areas
//! - `PUT   /clip/v2/resource/entertainment_configuration/<id>` body `{"action":"start"}` → claim stream
//! - `PUT   .../<id>` body `{"action":"stop"}`          → release (also auto-released on socket close)
//!
//! The bridge serves these over HTTPS with a self-signed cert; we accept
//! whatever it presents (we're on the same LAN and authenticate via PSK
//! anyway). Verification could be tightened later by pinning the bridge's
//! CN against its bridgeID.

use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use openssl::ssl::{
    SslConnector, SslContext, SslContextBuilder, SslMethod, SslStream, SslVerifyMode,
};
use parking_lot::RwLock;

use manifold_gpu::GpuBuffer;

use crate::SharedState;

// ─── Public config ───────────────────────────────────────────────────────────

/// Hue connection parameters. Cheap to clone (a handful of short strings),
/// so the runtime can swap one in mid-session via `HueRuntime::apply`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HueConfig {
    pub bridge_ip: String,
    /// "username" string from pairing — used as DTLS PSK identity AND
    /// as `hue-application-key` HTTP header on REST calls.
    pub app_key: String,
    /// Hex-encoded "clientkey" from pairing — decoded to raw bytes for PSK.
    pub psk_hex: String,
    /// Entertainment configuration UUID (36-char ASCII with hyphens).
    pub entertainment_id: String,
    /// Friendly area name (e.g. "Studio") — used for log messages and the
    /// GUI status readout. Not authoritative; the UUID above is what the
    /// bridge actually keys on.
    pub area_name: String,
    /// Number of channels in the entertainment configuration. Resolved from
    /// the bridge at startup so we know how many per-channel chunks to pack.
    pub channel_count: u8,
}

/// Live-tunable Hue knobs (mirrored into [`crate::DynamicConfig`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HueDynamic {
    /// Master brightness 0..1 applied after color reduction.
    pub gain: f32,
    /// Saturation boost around the reduced color's BT.709 luma. 1.0 = none.
    /// Values above 1.0 punch color away from gray; useful because the
    /// dominant-color average naturally desaturates compared to any single
    /// source pixel.
    pub saturation: f32,
    /// Bitmask: bit `i` set = channel `i` receives the SECONDARY (second-
    /// dominant hue) color, else the PRIMARY (most-dominant hue) color.
    /// Mask `0` = single-color mode: every channel gets the primary, and we
    /// skip the histogram pass entirely (cheaper, identical behaviour to
    /// the old reducer). Most users will set exactly one bit — e.g. "1" for
    /// "channel 0 = TV strip (primary), channel 1 = ceiling bulb (secondary)."
    pub secondary_channel_mask: u32,
    /// Per-channel temporal smoothing (EMA) applied AFTER color reduction.
    /// 1.0 = no smoothing (every tick passes the new color straight through),
    /// 0.3 ≈ 3 frames of inertia at 25 Hz (~120 ms latency).
    ///
    /// Independent of the slicer's `--smoothing` (which smooths source
    /// pixels). This matters because the secondary-hue extraction is
    /// *discrete* — when two histogram bins have similar weights, the
    /// "winner" can flip frame-to-frame, causing the secondary bulb to
    /// flicker between hues even when source pixels are stable. Smoothing
    /// the OUTPUT colors morphs between bin-winners gradually instead of
    /// snapping.
    pub smoothing: f32,
}

impl HueDynamic {
    /// True iff at least one channel is mapped to the secondary color.
    pub fn dual_color(&self) -> bool {
        self.secondary_channel_mask != 0
    }
}

impl Default for HueDynamic {
    fn default() -> Self {
        Self {
            gain: 1.0,
            saturation: 1.4,
            secondary_channel_mask: 0,
            smoothing: 0.3,
        }
    }
}

// ─── Pairing / area discovery (CLI helper, not part of the run loop) ────────

/// `--hue-pair --hue-bridge <ip>` flow. Walks the user through registering
/// our app on the bridge, then prints the credentials + entertainment area
/// list so the user can paste the right flags into flags.conf.
pub fn pair_and_list(bridge_ip: &str) -> Result<(), String> {
    eprintln!();
    eprintln!("Pairing with Hue bridge at {bridge_ip}...");
    eprintln!("Press the round LINK button on top of the bridge, then press Enter.");
    let mut _line = String::new();
    let _ = std::io::stdin().read_line(&mut _line);

    // Retry a few times — pairing succeeds on the first POST after the
    // button press, but if the user pressed it a moment too early the
    // bridge will reject with error 101.
    let mut creds: Option<(String, String)> = None;
    for attempt in 1..=10 {
        match pair_once(bridge_ip) {
            Ok(c) => {
                creds = Some(c);
                break;
            }
            Err(e) => {
                if attempt == 10 {
                    return Err(format!(
                        "pairing failed after 10 attempts: {e}\n\
                         Did you press the LINK button within ~30s before pairing?"
                    ));
                }
                eprintln!("attempt {attempt}: {e} — retrying in 1s...");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    let (username, clientkey) = creds.expect("loop returned without setting creds");

    eprintln!();
    eprintln!("Paired! Add these to ~/Library/Application Support/TVLEDMirror/flags.conf:");
    eprintln!();
    eprintln!("    --hue-bridge {bridge_ip}");
    eprintln!("    --hue-app-key {username}");
    eprintln!("    --hue-psk {clientkey}");
    eprintln!();

    // Fetch the entertainment areas so the user can pick one by name.
    eprintln!("Entertainment areas on this bridge:");
    let areas = list_entertainment_areas(bridge_ip, &username)?;
    if areas.is_empty() {
        eprintln!("  (none configured)");
        eprintln!();
        eprintln!("Open the Hue app → Settings → Entertainment areas, create one,");
        eprintln!("then re-run --hue-pair to see it listed.");
    } else {
        for a in &areas {
            eprintln!(
                "  - name: \"{}\"   id: {}   channels: {}",
                a.name, a.id, a.channel_count
            );
        }
        eprintln!();
        eprintln!("Add EITHER (matched case-insensitively against the names above):");
        eprintln!("    --hue-area \"{}\"", areas[0].name);
        eprintln!("or the explicit UUID:");
        eprintln!("    --hue-entertainment-id {}", areas[0].id);
    }
    eprintln!();
    Ok(())
}

#[derive(Debug, Clone)]
pub struct AreaSummary {
    pub id: String,
    pub name: String,
    pub channel_count: u8,
}

/// Resolve `--hue-area "Studio"` (name) OR `--hue-entertainment-id <uuid>`
/// against the bridge's actual entertainment_configuration list, returning
/// the canonical UUID, channel count, and resolved name.
pub fn resolve_area(
    bridge_ip: &str,
    app_key: &str,
    name_or_uuid: &str,
) -> Result<(String, u8, String), String> {
    let areas = list_entertainment_areas(bridge_ip, app_key)?;
    let needle = name_or_uuid.trim();
    // UUID match first (case-sensitive, exact).
    if let Some(a) = areas.iter().find(|a| a.id == needle) {
        return Ok((a.id.clone(), a.channel_count, a.name.clone()));
    }
    // Fall back to case-insensitive name match.
    let lc = needle.to_ascii_lowercase();
    if let Some(a) = areas.iter().find(|a| a.name.to_ascii_lowercase() == lc) {
        return Ok((a.id.clone(), a.channel_count, a.name.clone()));
    }
    Err(format!(
        "no entertainment area matches \"{name_or_uuid}\" — known areas: [{}]",
        areas
            .iter()
            .map(|a| format!("\"{}\"", a.name))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}


fn pair_once(bridge_ip: &str) -> Result<(String, String), String> {
    let body = r#"{"devicetype":"tv-led-mirror#mac","generateclientkey":true}"#;
    let resp = https_request(bridge_ip, "POST", "/api", None, Some(body))?;
    let v: serde_json::Value = serde_json::from_str(&resp).map_err(|e| format!("json: {e}"))?;
    // Response is an array. Either [{"success":{username, clientkey}}] or
    // [{"error":{type, address, description}}].
    let arr = v.as_array().ok_or("expected array response")?;
    let first = arr.first().ok_or("empty response array")?;
    if let Some(err) = first.get("error") {
        let desc = err
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("?");
        return Err(format!("bridge: {desc}"));
    }
    let success = first
        .get("success")
        .ok_or("response had neither success nor error")?;
    let username = success
        .get("username")
        .and_then(|s| s.as_str())
        .ok_or("missing username")?
        .to_string();
    let clientkey = success
        .get("clientkey")
        .and_then(|s| s.as_str())
        .ok_or("missing clientkey (bridge firmware too old?)")?
        .to_string();
    Ok((username, clientkey))
}

fn list_entertainment_areas(bridge_ip: &str, app_key: &str) -> Result<Vec<AreaSummary>, String> {
    let resp = https_request(
        bridge_ip,
        "GET",
        "/clip/v2/resource/entertainment_configuration",
        Some(app_key),
        None,
    )?;
    let v: serde_json::Value = serde_json::from_str(&resp).map_err(|e| format!("json: {e}"))?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or("no `data` array in entertainment_configuration response")?;
    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let id = item
            .get("id")
            .and_then(|s| s.as_str())
            .ok_or("missing id")?
            .to_string();
        let name = item
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(|s| s.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let channel_count = item
            .get("channels")
            .and_then(|c| c.as_array())
            .map(|a| a.len() as u8)
            .unwrap_or(0);
        out.push(AreaSummary {
            id,
            name,
            channel_count,
        });
    }
    Ok(out)
}

// ─── HTTPS client (just enough for the 4 endpoints we need) ─────────────────

/// Minimal HTTPS GET/POST/PUT against the bridge. Returns the response body
/// as a String. Skips cert verification because the bridge serves a
/// self-signed cert that's a pain to validate; PSK on the streaming socket
/// gives us the actual security boundary.
fn https_request(
    host: &str,
    method: &str,
    path: &str,
    app_key: Option<&str>,
    body: Option<&str>,
) -> Result<String, String> {
    // Build TLS connector with verification disabled.
    let mut builder = SslConnector::builder(SslMethod::tls_client())
        .map_err(|e| format!("ssl ctx: {e}"))?;
    builder.set_verify(SslVerifyMode::NONE);
    let connector = builder.build();

    let tcp = TcpStream::connect((host, 443)).map_err(|e| format!("tcp: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(5))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let mut tls = connector
        .connect(host, tcp)
        .map_err(|e| format!("tls handshake: {e}"))?;

    let body_str = body.unwrap_or("");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Accept: application/json\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n",
        body_str.len()
    );
    if let Some(k) = app_key {
        req.push_str(&format!("hue-application-key: {k}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body_str);
    tls.write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    let mut raw = Vec::with_capacity(4096);
    let mut chunk = [0u8; 2048];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            // openssl signals clean shutdown as an error sometimes; if we
            // already have a complete response, treat it as EOF.
            Err(_) if !raw.is_empty() => break,
            Err(e) => return Err(format!("read: {e}")),
        }
    }

    // Split off headers, return body. Status line check too.
    let text = String::from_utf8_lossy(&raw).into_owned();
    let split_idx = text
        .find("\r\n\r\n")
        .ok_or("malformed http response (no header/body separator)")?;
    let head = &text[..split_idx];
    let body = &text[split_idx + 4..];
    let status = head
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("?");
    if !status.starts_with('2') {
        return Err(format!("HTTP {status}: {body}"));
    }
    Ok(strip_chunked(body))
}

/// Bridge sends `Transfer-Encoding: chunked` for some endpoints. We don't
/// negotiate HTTP/1.0, so we get hex-length lines. Quick-and-dirty unchunk:
/// if the body looks chunked (starts with hex digits + CRLF), parse it;
/// otherwise return as-is.
fn strip_chunked(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut out = String::with_capacity(body.len());
    let mut had_chunk = false;
    while i < bytes.len() {
        // Read hex length up to CRLF.
        let line_end = match find_crlf(&bytes[i..]) {
            Some(p) => p,
            None => break,
        };
        let len_str = std::str::from_utf8(&bytes[i..i + line_end]).unwrap_or("");
        let Ok(len) = usize::from_str_radix(len_str.trim(), 16) else {
            // Not a chunk header — bail and return rest verbatim.
            if !had_chunk {
                return body.to_string();
            }
            break;
        };
        had_chunk = true;
        i += line_end + 2; // past CRLF
        if len == 0 {
            break;
        }
        if i + len > bytes.len() {
            break;
        }
        out.push_str(std::str::from_utf8(&bytes[i..i + len]).unwrap_or(""));
        i += len;
        // Skip trailing CRLF after each chunk.
        if i + 2 <= bytes.len() && &bytes[i..i + 2] == b"\r\n" {
            i += 2;
        }
    }
    if had_chunk { out } else { body.to_string() }
}

fn find_crlf(s: &[u8]) -> Option<usize> {
    s.windows(2).position(|w| w == b"\r\n")
}

// ─── Runtime: a controllable hue thread ──────────────────────────────────────

/// Status the hue thread publishes for the GUI to display. Cheap to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HueStatus {
    /// No config — thread is idle, no DTLS socket.
    Disabled,
    /// Config received, currently doing REST claim + DTLS handshake.
    Connecting { area_name: String },
    /// Streaming colors at 25 Hz. `channel_count` lets the GUI render one
    /// per-channel toggle for the dual-color mask without having to peek
    /// into the runtime's HueConfig.
    Streaming {
        area_name: String,
        bridge_ip: String,
        channel_count: u8,
    },
    /// Last connect attempt failed; thread is sleeping before the next try
    /// (or waiting for a new Apply command, whichever comes first).
    Error { message: String },
}

enum HueCmd {
    /// Swap the active config. `None` = stop streaming and idle.
    Apply(Option<HueConfig>),
}

/// Handle to the long-running hue thread. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct HueRuntime {
    cmd_tx: Sender<HueCmd>,
    status: Arc<RwLock<HueStatus>>,
}

impl HueRuntime {
    /// Push a new config. `None` disconnects and idles.
    pub fn apply(&self, cfg: Option<HueConfig>) {
        // Channel send only fails if the receiver was dropped (i.e. the
        // hue thread crashed or exited). At that point there's nothing to
        // do — the LED strips keep running regardless.
        let _ = self.cmd_tx.send(HueCmd::Apply(cfg));
    }

    pub fn status(&self) -> HueStatus {
        self.status.read().clone()
    }
}

/// Spawn the runtime thread. Returns immediately. If `initial` is Some, the
/// thread will try to connect right away; otherwise it idles waiting for a
/// future `apply()` from the GUI.
pub fn spawn_runtime(state: Arc<SharedState>, initial: Option<HueConfig>) -> HueRuntime {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<HueCmd>();
    let status = Arc::new(RwLock::new(HueStatus::Disabled));

    if let Some(cfg) = initial {
        let _ = cmd_tx.send(HueCmd::Apply(Some(cfg)));
    }

    let st = status.clone();
    std::thread::spawn(move || runtime_loop(state, cmd_rx, st));

    HueRuntime { cmd_tx, status }
}

/// Outer state machine: idle ↔ streaming ↔ error-backoff. Always reactive
/// to Apply commands — switching configs interrupts whatever the loop is
/// doing.
fn runtime_loop(state: Arc<SharedState>, cmd_rx: Receiver<HueCmd>, status: Arc<RwLock<HueStatus>>) {
    let mut current: Option<HueConfig> = None;
    loop {
        match current.clone() {
            None => {
                *status.write() = HueStatus::Disabled;
                // No config → block until one arrives.
                match cmd_rx.recv() {
                    Ok(HueCmd::Apply(cfg)) => current = cfg,
                    Err(_) => return, // GUI dropped; thread exits.
                }
            }
            Some(cfg) => {
                *status.write() = HueStatus::Connecting {
                    area_name: cfg.area_name.clone(),
                };
                match try_stream(&state, &cfg, &cmd_rx, &status) {
                    StreamExit::ConfigChanged(new_cfg) => current = new_cfg,
                    StreamExit::ChannelClosed => return,
                    StreamExit::Error(e) => {
                        log::warn!(
                            "hue: connection to {} failed: {e}; retrying in 5 s",
                            cfg.bridge_ip,
                        );
                        *status.write() = HueStatus::Error { message: e };
                        // Backoff with cancellable wait.
                        match cmd_rx.recv_timeout(Duration::from_secs(5)) {
                            Ok(HueCmd::Apply(new_cfg)) => current = new_cfg,
                            Err(RecvTimeoutError::Disconnected) => return,
                            Err(RecvTimeoutError::Timeout) => {} // retry with same cfg
                        }
                    }
                }
            }
        }
    }
}

enum StreamExit {
    ConfigChanged(Option<HueConfig>),
    ChannelClosed,
    Error(String),
}

/// Inner stream loop: REST claim, DTLS handshake, 25 Hz send tick. Polls the
/// command channel between ticks so the GUI can swap the config / disable
/// streaming without waiting for the next tick boundary.
fn try_stream(
    state: &Arc<SharedState>,
    cfg: &HueConfig,
    cmd_rx: &Receiver<HueCmd>,
    status: &Arc<RwLock<HueStatus>>,
) -> StreamExit {
    if let Err(e) = api_set_action(cfg, "start") {
        return StreamExit::Error(format!("start action: {e}"));
    }
    let mut dtls = match open_dtls(cfg) {
        Ok(s) => s,
        Err(e) => {
            let _ = api_set_action(cfg, "stop");
            return StreamExit::Error(e);
        }
    };

    *status.write() = HueStatus::Streaming {
        area_name: cfg.area_name.clone(),
        bridge_ip: cfg.bridge_ip.clone(),
        channel_count: cfg.channel_count,
    };
    log::info!(
        "hue: streaming to {} (area \"{}\", {} channels)",
        cfg.bridge_ip,
        cfg.area_name,
        cfg.channel_count,
    );

    let w = state.slicer.width();
    let h = state.slicer.height();
    let bytes_per_row = w * 4;
    let total_bytes = (bytes_per_row * h) as u64;
    let readback: GpuBuffer = state.device.create_buffer_shared(total_bytes);

    let interval = Duration::from_millis(40);
    let mut next = Instant::now() + interval;
    let mut consecutive_send_errors = 0u32;
    let mut packet_buf: Vec<u8> = Vec::with_capacity(64 + (cfg.channel_count as usize) * 7);
    // Reused per-tick to avoid an alloc; sized to channel_count.
    let mut channel_colors: Vec<[u8; 3]> = vec![[0; 3]; cfg.channel_count as usize];
    // Per-channel f32 EMA state — smooths color transitions between ticks
    // so secondary-hue bin flips morph rather than snap. Initialized to
    // zero so the first tick fades up from black instead of popping.
    let mut smoothed: Vec<[f32; 3]> = vec![[0.0; 3]; cfg.channel_count as usize];
    // Stateful dual-color reducer. Owns the smoothed histogram bins and
    // the last-chosen primary/secondary bin indices so the selection is
    // stable across frames (no per-frame flipping when two bins are
    // close in weight). Lives across ticks even when single-color mode
    // is active, so toggling back to dual doesn't pop.
    let mut dual_reducer = DualReducer::new();

    loop {
        // Cancellable sleep until next tick. Any incoming command preempts.
        let now = Instant::now();
        if next > now {
            match cmd_rx.recv_timeout(next - now) {
                Ok(HueCmd::Apply(new_cfg)) => {
                    let _ = api_set_action(cfg, "stop");
                    return StreamExit::ConfigChanged(new_cfg);
                }
                Err(RecvTimeoutError::Disconnected) => return StreamExit::ChannelClosed,
                Err(RecvTimeoutError::Timeout) => {} // tick due
            }
        } else {
            // We're already late — drain a pending command if there is one
            // before doing more GPU work, to keep the GUI responsive.
            match cmd_rx.try_recv() {
                Ok(HueCmd::Apply(new_cfg)) => {
                    let _ = api_set_action(cfg, "stop");
                    return StreamExit::ConfigChanged(new_cfg);
                }
                Err(TryRecvError::Disconnected) => return StreamExit::ChannelClosed,
                Err(TryRecvError::Empty) => {}
            }
        }
        next += interval;

        // GPU readback + reduce.
        let mut enc = state.device.create_encoder("tv-led-mirror.hue.readback");
        let out = state.slicer.output();
        enc.copy_texture_to_buffer(out, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();

        // Snapshot dyn knobs + the slicer's gamma in one lock so we can
        // undo the gamma curve when reducing (see `reduce_dominant`).
        let (dyn_cfg, gamma) = {
            let cfg_guard = state.config.read();
            (cfg_guard.hue, cfg_guard.grade.gamma)
        };
        let pixels = unsafe {
            let ptr = readback.mapped_ptr().expect("shared buffer must be mapped");
            std::slice::from_raw_parts(ptr, total_bytes as usize)
        };

        // Dual-color path uses a hue histogram to find two visually distinct
        // dominant hues; single-color path uses the cheaper top-20% reducer.
        if dyn_cfg.dual_color() {
            let (primary, secondary) = dual_reducer.reduce(
                pixels,
                gamma,
                dyn_cfg.smoothing,
                dyn_cfg.saturation,
                dyn_cfg.gain,
            );
            for (i, slot) in channel_colors.iter_mut().enumerate() {
                let bit = 1u32 << i;
                *slot = if dyn_cfg.secondary_channel_mask & bit != 0 {
                    secondary
                } else {
                    primary
                };
            }
        } else {
            let color = reduce_dominant(pixels, gamma, dyn_cfg.saturation, dyn_cfg.gain);
            for slot in channel_colors.iter_mut() {
                *slot = color;
            }
        }

        // Per-channel EMA: each channel's f32 state blends toward the new
        // target. Smoothing 1.0 = pass-through (no inertia); 0.3 = 3 frames
        // of inertia. Done in u8 → f32 → u8 round-trip; the f32 state holds
        // sub-byte precision that compounds across ticks.
        let alpha = dyn_cfg.smoothing.clamp(0.01, 1.0);
        for (i, slot) in channel_colors.iter_mut().enumerate() {
            let target = [slot[0] as f32, slot[1] as f32, slot[2] as f32];
            for c in 0..3 {
                smoothed[i][c] = smoothed[i][c] * (1.0 - alpha) + target[c] * alpha;
            }
            *slot = [
                smoothed[i][0].round().clamp(0.0, 255.0) as u8,
                smoothed[i][1].round().clamp(0.0, 255.0) as u8,
                smoothed[i][2].round().clamp(0.0, 255.0) as u8,
            ];
        }

        pack_packet(&mut packet_buf, &cfg.entertainment_id, &channel_colors);
        match dtls.write_all(&packet_buf) {
            Ok(()) => consecutive_send_errors = 0,
            Err(e) => {
                consecutive_send_errors += 1;
                log::warn!("hue: dtls send failed ({e}); err count = {consecutive_send_errors}");
                if consecutive_send_errors >= 5 {
                    log::warn!("hue: reconnecting after repeated send failures");
                    let _ = api_set_action(cfg, "stop");
                    return StreamExit::Error("repeated DTLS send failures".to_string());
                }
            }
        }
    }
}

/// PUT entertainment_configuration/<id> with `{"action": <action>}`.
fn api_set_action(cfg: &HueConfig, action: &str) -> Result<(), String> {
    let path = format!(
        "/clip/v2/resource/entertainment_configuration/{}",
        cfg.entertainment_id
    );
    let body = format!(r#"{{"action":"{action}"}}"#);
    let _ = https_request(
        &cfg.bridge_ip,
        "PUT",
        &path,
        Some(&cfg.app_key),
        Some(&body),
    )?;
    Ok(())
}

// ─── DTLS-PSK setup ──────────────────────────────────────────────────────────

/// Wrapper to give the openssl SslStream a Read+Write impl over our
/// connected UdpSocket. openssl drives DTLS records over this.
struct UdpStream(UdpSocket);

impl Read for UdpStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.recv(buf)
    }
}
impl Write for UdpStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.send(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn open_dtls(cfg: &HueConfig) -> Result<SslStream<UdpStream>, String> {
    let psk = hex::decode(&cfg.psk_hex).map_err(|e| format!("psk hex decode: {e}"))?;
    if psk.len() != 16 {
        return Err(format!("psk must decode to 16 bytes, got {}", psk.len()));
    }

    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
    sock.connect(format!("{}:2100", cfg.bridge_ip))
        .map_err(|e| format!("udp connect: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
    sock.set_write_timeout(Some(Duration::from_secs(2))).ok();

    let ctx = build_psk_dtls_ctx(&cfg.app_key, &psk)?;
    let mut ssl = openssl::ssl::Ssl::new(&ctx).map_err(|e| format!("ssl new: {e}"))?;
    // Tell openssl this is the client side.
    ssl.set_connect_state();

    let mut stream = SslStream::new(ssl, UdpStream(sock))
        .map_err(|e| format!("dtls stream new: {e}"))?;

    // Drive the handshake to completion. SslStream::do_handshake triggers
    // the ClientHello and runs the round-trips.
    stream
        .do_handshake()
        .map_err(|e| format!("dtls handshake: {e}"))?;
    log::info!("hue: dtls-psk handshake complete");
    Ok(stream)
}

fn build_psk_dtls_ctx(identity: &str, psk: &[u8]) -> Result<SslContext, String> {
    let mut b: SslContextBuilder =
        SslContextBuilder::new(SslMethod::dtls()).map_err(|e| format!("ssl ctx: {e}"))?;
    b.set_verify(SslVerifyMode::NONE);
    // Hue uses TLS_PSK_WITH_AES_128_GCM_SHA256. "PSK" enables all PSK
    // suites; openssl will pick one the bridge supports.
    b.set_cipher_list("PSK").map_err(|e| format!("ciphers: {e}"))?;

    let identity_owned = identity.to_string();
    let psk_owned = psk.to_vec();
    b.set_psk_client_callback(move |_ssl, _hint, id_buf, psk_buf| {
        // openssl wants identity NUL-terminated INSIDE the buffer.
        let id_bytes = identity_owned.as_bytes();
        if id_bytes.len() + 1 > id_buf.len() {
            return Err(openssl::error::ErrorStack::get());
        }
        id_buf[..id_bytes.len()].copy_from_slice(id_bytes);
        id_buf[id_bytes.len()] = 0;
        if psk_owned.len() > psk_buf.len() {
            return Err(openssl::error::ErrorStack::get());
        }
        psk_buf[..psk_owned.len()].copy_from_slice(&psk_owned);
        Ok(psk_owned.len())
    });

    Ok(b.build())
}

// ─── Color reduction ─────────────────────────────────────────────────────────

/// Reduce the slicer's RGBA8 output to one dominant-bright color suitable
/// for ambient Hue lighting.
///
/// Pipeline:
///   1. **Linearize** each pixel by undoing the slicer's luminance-preserving
///      gamma curve. The slicer applies `y' = y^gamma` then scales RGB by
///      `y'/y` (so `RGB' = RGB · y^(gamma-1)`). To recover linear RGB given
///      observed RGB and observed `y_obs = y_orig^gamma`, multiply each
///      channel by `y_obs^(1/gamma - 1)`. This matters because the slicer's
///      gamma is calibrated for SK9822 strips (linear PWM), but Hue bulbs
///      apply their own perceptual response — feeding them gamma-squashed
///      values makes them look dim regardless of `gain`.
///   2. **Weight** each pixel by `luminance · (1 + 4·saturation)`: dim or
///      grey pixels barely contribute; bright saturated regions dominate.
///   3. **Top-20% reduction**: sort pixels by weight, keep the brightest
///      fifth, average those. Critical: a full-frame average of a typical
///      scene (mostly dark + one bright region) lands around 10-20%
///      brightness; the bright region gets diluted by the darkness. Taking
///      only the top fifth gives "the color of the dominant highlight",
///      which is what the user actually wants on the bulbs.
///   4. **Saturation boost** around the result's luma — re-saturates after
///      the inevitable averaging.
///   5. **Gain** as a final master, clamped to 0..1.
fn reduce_dominant(rgba: &[u8], gamma: f32, saturation_boost: f32, gain: f32) -> [u8; 3] {
    if rgba.len() < 4 {
        return [0, 0, 0];
    }

    // Inverse-gamma exponent applied to luminance (see step 1 above).
    // Skip the work entirely when gamma ≈ 1 — exp ≈ 0 ⇒ scale ≈ 1.
    let do_unlinearize = (gamma - 1.0).abs() > 0.01;
    let inv_y_exp = 1.0 / gamma.max(0.0001) - 1.0;

    let mut weighted: Vec<(f32, f32, f32, f32)> = Vec::with_capacity(rgba.len() / 4);
    for px in rgba.chunks_exact(4) {
        let mut r = px[0] as f32 / 255.0;
        let mut g = px[1] as f32 / 255.0;
        let mut b = px[2] as f32 / 255.0;
        if do_unlinearize {
            let y = (0.2126 * r + 0.7152 * g + 0.0722 * b).max(0.0001);
            let scale = y.powf(inv_y_exp);
            r *= scale;
            g *= scale;
            b *= scale;
        }
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        let sat = if mx > 0.001 { (mx - mn) / mx } else { 0.0 };
        let w = lum * (1.0 + 4.0 * sat);
        weighted.push((w, r, g, b));
    }

    // Partial sort would be slightly faster (O(n) via select_nth_unstable +
    // O(k) sum) but n ≈ 1000 at 25 Hz; full sort costs ~0.1 ms. Not worth
    // the complexity.
    weighted.sort_unstable_by(|a, b| {
        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
    });
    let top_n = (weighted.len() / 5).max(1);

    let (mut sr, mut sg, mut sb, mut sw) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for &(w, r, g, b) in &weighted[..top_n] {
        sr += r * w;
        sg += g * w;
        sb += b * w;
        sw += w;
    }
    if sw < 0.0001 {
        return [0, 0, 0];
    }
    let mut r = sr / sw;
    let mut g = sg / sw;
    let mut b = sb / sw;

    // Saturation boost in linear space — pull RGB further from gray. Don't
    // clamp before gain; a saturation boost can push channels above 1.0
    // briefly, and the gain step's clamp catches it.
    if (saturation_boost - 1.0).abs() > 0.001 {
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        r = (y + (r - y) * saturation_boost).max(0.0);
        g = (y + (g - y) * saturation_boost).max(0.0);
        b = (y + (b - y) * saturation_boost).max(0.0);
    }

    let r = (r * gain).clamp(0.0, 1.0);
    let g = (g * gain).clamp(0.0, 1.0);
    let b = (b * gain).clamp(0.0, 1.0);
    [
        (r * 255.0 + 0.5) as u8,
        (g * 255.0 + 0.5) as u8,
        (b * 255.0 + 0.5) as u8,
    ]
}

/// Build a `HueStream` v2 RGB packet with one color per channel. Reuses the
/// caller's buffer to avoid per-tick allocation. Channel ID is the index
/// into `channel_colors` (the bridge keys per-channel state in the order
/// the entertainment_configuration lists them).
fn pack_packet(buf: &mut Vec<u8>, uuid: &str, channel_colors: &[[u8; 3]]) {
    debug_assert_eq!(uuid.len(), 36, "entertainment id must be a 36-char UUID");
    buf.clear();
    buf.extend_from_slice(b"HueStream");
    buf.push(2); // major
    buf.push(0); // minor
    buf.push(0); // sequence id (unused)
    buf.push(0); // reserved
    buf.push(0); // reserved
    buf.push(0); // color space = RGB
    buf.push(0); // reserved
    buf.extend_from_slice(uuid.as_bytes());

    for (i, color) in channel_colors.iter().enumerate() {
        // 8-bit → 16-bit by replication (so 0xFF maps to 0xFFFF, not 0xFF00).
        let r16 = ((color[0] as u16) << 8) | color[0] as u16;
        let g16 = ((color[1] as u16) << 8) | color[1] as u16;
        let b16 = ((color[2] as u16) << 8) | color[2] as u16;
        buf.push(i as u8);
        buf.extend_from_slice(&r16.to_be_bytes());
        buf.extend_from_slice(&g16.to_be_bytes());
        buf.extend_from_slice(&b16.to_be_bytes());
    }
}

// ─── Dual-color extraction (stateful hue histogram + hysteresis) ───────────

/// Number of bins in the hue histogram (10° per bin). Coarse enough to
/// cluster perceptually-similar hues, fine enough to resolve adjacent
/// colors like red vs orange.
const HISTOGRAM_BINS: usize = 36;
/// Pixels below this saturation bypass the hue histogram and feed an
/// achromatic accumulator instead. Avoids near-grey pixels (whose hue is
/// noise) dominating a hue bin.
const ACHROMATIC_SAT: f32 = 0.10;
/// Minimum hue distance (in bins) between primary and secondary so the
/// two outputs look visually distinct. 3 bins = 30°.
const MIN_BIN_DIST: usize = 3;
/// To swap the active primary/secondary bin, a new candidate's smoothed
/// weight must exceed the incumbent's by this factor. Anti-flicker:
/// without hysteresis, two bins hovering at similar weight would swap
/// "winner" every frame on tiny fluctuations.
const HYSTERESIS_MARGIN: f32 = 1.3;

/// Per-frame histogram (rebuilt every tick from raw pixels). Owned briefly
/// before being EMA-blended into [`DualReducer`]'s smoothed state.
///
/// Hand `Default` impl: `[[f32; 3]; 36]` exceeds the array length the
/// stdlib auto-derives Default for (max 32), so we spell it out.
#[derive(Clone, Copy)]
struct FrameHistogram {
    bin_w: [f32; HISTOGRAM_BINS],
    bin_rgb: [[f32; 3]; HISTOGRAM_BINS],
    ach_w: f32,
    ach_rgb: [f32; 3],
}

impl Default for FrameHistogram {
    fn default() -> Self {
        Self {
            bin_w: [0.0; HISTOGRAM_BINS],
            bin_rgb: [[0.0; 3]; HISTOGRAM_BINS],
            ach_w: 0.0,
            ach_rgb: [0.0; 3],
        }
    }
}

/// Stateful reducer that finds two dominant hues over time. Caller owns
/// one of these and calls `reduce()` each tick; the persistent state
/// (smoothed histogram + last-chosen bins) is what makes the output
/// stable in the face of frame-to-frame jitter.
///
/// Two layers of stability:
///   - **Bin-weight EMA**: histogram weights are blended frame-to-frame
///     so transient spikes don't immediately become the winner.
///   - **Selection hysteresis**: the chosen primary/secondary bin only
///     changes when a new candidate's smoothed weight beats the
///     incumbent by [`HYSTERESIS_MARGIN`]. Without this, even smoothed
///     bins flip when two are nearly tied.
pub struct DualReducer {
    smoothed: FrameHistogram,
    last_primary_bin: Option<usize>,
    last_secondary_bin: Option<usize>,
}

impl DualReducer {
    pub fn new() -> Self {
        Self {
            smoothed: FrameHistogram::default(),
            last_primary_bin: None,
            last_secondary_bin: None,
        }
    }

    /// Reduce the current frame's pixels to two dominant hues.
    ///
    /// `smoothing` ∈ (0, 1] — the EMA mix factor for the histogram. 1.0
    /// disables smoothing (per-frame extraction); lower values keep more
    /// inertia. Same knob that smooths the output colors at the channel
    /// level, applied here at the bin level for stability.
    pub fn reduce(
        &mut self,
        rgba: &[u8],
        gamma: f32,
        smoothing: f32,
        saturation_boost: f32,
        gain: f32,
    ) -> ([u8; 3], [u8; 3]) {
        let frame = build_frame_histogram(rgba, gamma);

        // EMA-blend per-bin into persistent state. alpha=1 ⇒ pass-through;
        // smaller alpha ⇒ heavier inertia. Applied to weights AND
        // weighted-RGB so the bin's centre tracks the smoothed colour.
        let alpha = smoothing.clamp(0.01, 1.0);
        for i in 0..HISTOGRAM_BINS {
            self.smoothed.bin_w[i] =
                self.smoothed.bin_w[i] * (1.0 - alpha) + frame.bin_w[i] * alpha;
            for c in 0..3 {
                self.smoothed.bin_rgb[i][c] =
                    self.smoothed.bin_rgb[i][c] * (1.0 - alpha) + frame.bin_rgb[i][c] * alpha;
            }
        }
        self.smoothed.ach_w = self.smoothed.ach_w * (1.0 - alpha) + frame.ach_w * alpha;
        for c in 0..3 {
            self.smoothed.ach_rgb[c] =
                self.smoothed.ach_rgb[c] * (1.0 - alpha) + frame.ach_rgb[c] * alpha;
        }

        // Pick primary with hysteresis.
        let smoothed = &self.smoothed;
        let best_primary = (0..HISTOGRAM_BINS)
            .max_by(|&a, &b| {
                smoothed.bin_w[a]
                    .partial_cmp(&smoothed.bin_w[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        let primary_bin = stick_or_swap(
            self.last_primary_bin,
            best_primary,
            &smoothed.bin_w,
        );
        self.last_primary_bin = Some(primary_bin);

        let primary_rgb = bin_color(smoothed, primary_bin);

        // Pick secondary with hysteresis. The candidate set excludes bins
        // too close to the chosen primary (so the two colors stay
        // visually distinct), then hysteresis stabilises which of those
        // candidates wins.
        let candidate_secondary = (0..HISTOGRAM_BINS)
            .filter(|&i| {
                circular_bin_dist(i, primary_bin, HISTOGRAM_BINS) >= MIN_BIN_DIST
                    && smoothed.bin_w[i] > 0.0001
            })
            .max_by(|&a, &b| {
                smoothed.bin_w[a]
                    .partial_cmp(&smoothed.bin_w[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        // Only keep the previous secondary if it's still far enough from
        // the (possibly new) primary; otherwise the candidate (or
        // fallback) takes over.
        let prev_still_valid = self.last_secondary_bin.is_some_and(|prev| {
            circular_bin_dist(prev, primary_bin, HISTOGRAM_BINS) >= MIN_BIN_DIST
                && smoothed.bin_w[prev] > 0.0001
        });
        let secondary_bin = match (prev_still_valid, candidate_secondary) {
            (true, Some(best)) => Some(stick_or_swap(
                self.last_secondary_bin,
                best,
                &smoothed.bin_w,
            )),
            (false, best) => best,
            (true, None) => self.last_secondary_bin, // shouldn't happen — prev valid implies candidate exists
        };
        self.last_secondary_bin = secondary_bin;

        let secondary_rgb = secondary_bin
            .map(|i| bin_color(smoothed, i))
            .unwrap_or(primary_rgb);

        (
            finalize_color(primary_rgb, saturation_boost, gain),
            finalize_color(secondary_rgb, saturation_boost, gain),
        )
    }
}

impl Default for DualReducer {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a single frame's histogram. Stateless — caller blends into
/// persistent state via EMA.
fn build_frame_histogram(rgba: &[u8], gamma: f32) -> FrameHistogram {
    let mut h = FrameHistogram::default();
    if rgba.len() < 4 {
        return h;
    }
    let do_unlinearize = (gamma - 1.0).abs() > 0.01;
    let inv_y_exp = 1.0 / gamma.max(0.0001) - 1.0;
    for px in rgba.chunks_exact(4) {
        let mut r = px[0] as f32 / 255.0;
        let mut g = px[1] as f32 / 255.0;
        let mut b = px[2] as f32 / 255.0;
        if do_unlinearize {
            let y = (0.2126 * r + 0.7152 * g + 0.0722 * b).max(0.0001);
            let scale = y.powf(inv_y_exp);
            r *= scale;
            g *= scale;
            b *= scale;
        }
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        let sat = if mx > 0.001 { (mx - mn) / mx } else { 0.0 };
        let w = lum * (1.0 + 4.0 * sat);

        if sat < ACHROMATIC_SAT {
            h.ach_w += w;
            h.ach_rgb[0] += r * w;
            h.ach_rgb[1] += g * w;
            h.ach_rgb[2] += b * w;
        } else {
            let hue = rgb_to_hue(r, g, b, mx, mn);
            let bin = ((hue / 360.0 * HISTOGRAM_BINS as f32) as usize).min(HISTOGRAM_BINS - 1);
            h.bin_w[bin] += w;
            h.bin_rgb[bin][0] += r * w;
            h.bin_rgb[bin][1] += g * w;
            h.bin_rgb[bin][2] += b * w;
        }
    }
    h
}

/// Average colour for one bin. Falls back to the achromatic accumulator
/// when the bin is empty (entirely-achromatic scene).
fn bin_color(h: &FrameHistogram, bin: usize) -> [f32; 3] {
    let w = h.bin_w[bin];
    if w > 0.0001 {
        [h.bin_rgb[bin][0] / w, h.bin_rgb[bin][1] / w, h.bin_rgb[bin][2] / w]
    } else if h.ach_w > 0.0001 {
        [h.ach_rgb[0] / h.ach_w, h.ach_rgb[1] / h.ach_w, h.ach_rgb[2] / h.ach_w]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Hysteresis: stick with the previous choice unless the new candidate's
/// smoothed weight beats it by at least [`HYSTERESIS_MARGIN`]. Without
/// this, two bins hovering near equal weight flip-flop every frame even
/// after bin-weight smoothing.
fn stick_or_swap(prev: Option<usize>, best: usize, weights: &[f32; HISTOGRAM_BINS]) -> usize {
    match prev {
        Some(p) if p != best && weights[best] < weights[p] * HYSTERESIS_MARGIN => p,
        _ => best,
    }
}

/// Standard HSV hue formula. `mx` / `mn` are the per-pixel max / min
/// channels, passed in by the caller to avoid recomputing.
fn rgb_to_hue(r: f32, g: f32, b: f32, mx: f32, mn: f32) -> f32 {
    let delta = mx - mn;
    if delta < 0.0001 {
        return 0.0;
    }
    let h = if mx == r {
        // %6.0 keeps the result in [-something, 6) before scaling; the
        // wraparound below normalizes negatives.
        ((g - b) / delta) % 6.0
    } else if mx == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    let h = h * 60.0;
    if h < 0.0 { h + 360.0 } else { h }
}

/// Wraparound-aware distance between two histogram bins. Reds at bin 0 and
/// bin 35 are 1 bin apart, not 35.
fn circular_bin_dist(a: usize, b: usize, n: usize) -> usize {
    let d = (a as i32 - b as i32).unsigned_abs() as usize;
    d.min(n - d)
}

/// Apply saturation boost + gain + clamp + 8-bit quantize. Shared between
/// [`reduce_dominant`]'s single-color path and [`reduce_dual`]'s primary
/// and secondary outputs so all three behave identically post-reduction.
///
/// Gain is applied chroma-preservingly: once the brightest channel hits
/// 1.0, additional gain has no effect (rather than per-channel clamping
/// the others, which would shift the hue toward grey/white). E.g. an
/// orange `(0.8, 0.4, 0.2)` × gain 2 would naïvely clamp to `(1.0, 0.8,
/// 0.4)` — a yellow — instead of staying orange. We compute a
/// "safe" gain capped at `1/max(rgb)` so the ratio between channels is
/// preserved.
fn finalize_color(rgb: [f32; 3], saturation_boost: f32, gain: f32) -> [u8; 3] {
    let [mut r, mut g, mut b] = rgb;
    if (saturation_boost - 1.0).abs() > 0.001 {
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        r = (y + (r - y) * saturation_boost).max(0.0);
        g = (y + (g - y) * saturation_boost).max(0.0);
        b = (y + (b - y) * saturation_boost).max(0.0);
    }
    let mx = r.max(g).max(b);
    let safe_gain = if mx > 0.0001 {
        gain.min(1.0 / mx)
    } else {
        gain
    };
    let r = (r * safe_gain).clamp(0.0, 1.0);
    let g = (g * safe_gain).clamp(0.0, 1.0);
    let b = (b * safe_gain).clamp(0.0, 1.0);
    [
        (r * 255.0 + 0.5) as u8,
        (g * 255.0 + 0.5) as u8,
        (b * 255.0 + 0.5) as u8,
    ]
}

// ─── Background helpers for the GUI pairing wizard ──────────────────────────

/// Polled status for the GUI's pairing flow. Cheap to clone.
#[derive(Clone, Debug)]
pub enum PairProgress {
    /// Currently polling — waiting for the link button or for the bridge
    /// to respond. `seconds_elapsed` lets the GUI show a countdown.
    Waiting { seconds_elapsed: u32 },
    /// Pairing succeeded — the GUI should now move to area selection using
    /// these credentials.
    Done { app_key: String, psk_hex: String },
    /// Bridge gave up, network died, or the user took too long.
    Failed(String),
}

/// Spawn a background pairing poller. The returned receiver yields a single
/// terminal `Done`/`Failed`, optionally preceded by `Waiting` updates every
/// second. The GUI polls this in its `update` loop.
pub fn pair_background(bridge_ip: String) -> Receiver<PairProgress> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let started = Instant::now();
        let timeout = Duration::from_secs(60);
        loop {
            let elapsed = started.elapsed();
            if elapsed > timeout {
                let _ = tx.send(PairProgress::Failed(
                    "timeout — link button not pressed within 60 s".into(),
                ));
                return;
            }
            match pair_once(&bridge_ip) {
                Ok((app_key, psk_hex)) => {
                    let _ = tx.send(PairProgress::Done { app_key, psk_hex });
                    return;
                }
                Err(e) if e.contains("link button not pressed") => {
                    let _ = tx.send(PairProgress::Waiting {
                        seconds_elapsed: elapsed.as_secs() as u32,
                    });
                    std::thread::sleep(Duration::from_secs(1));
                }
                Err(e) => {
                    let _ = tx.send(PairProgress::Failed(e));
                    return;
                }
            }
        }
    });
    rx
}

/// Spawn a background area-list fetch. Returns the list (or an error) once.
pub fn list_areas_background(
    bridge_ip: String,
    app_key: String,
) -> Receiver<Result<Vec<AreaSummary>, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(list_entertainment_areas(&bridge_ip, &app_key));
    });
    rx
}

// ─── flags.conf persistence ──────────────────────────────────────────────────

/// Path of the flags file the GUI should write hue creds to. Mirrors what
/// [`crate::reload`] watches, so a save here gets picked up by the reload
/// thread on the next tick (though the GUI also calls `runtime.apply()`
/// directly so streaming swaps without waiting).
pub fn flags_conf_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push("Library/Application Support/TVLEDMirror/flags.conf");
    Some(p)
}

/// Flags managed by the GUI's debounced auto-save (slider + Hue dynamic).
/// These get stripped from flags.conf before re-appending the current values
/// — anything outside this list (capture flags, comments, custom flags) is
/// preserved verbatim.
const MANAGED_LIVE_FLAGS: &[&str] = &[
    "--brightness",
    "--blur-radius",
    "--luminance-floor",
    "--luminance-knee",
    "--saturation-floor",
    "--saturation-knee",
    "--crop-left",
    "--crop-right",
    "--crop-top",
    "--crop-bottom",
    "--vibrance",
    "--gamma",
    "--saturation-bias",
    "--peak-bias",
    "--wb-r",
    "--wb-g",
    "--wb-b",
    "--max-luminance",
    "--black-floor",
    "--hdr-peak",
    "--smoothing",
    "--hue-gain",
    "--hue-saturation",
    "--hue-secondary-channels",
    "--hue-smoothing",
];

/// Replace all live-tunable `--*` lines in flags.conf with the supplied
/// rendering of the current `DynamicConfig`. Capture flags, hue setup
/// credentials, comments, and any custom flags are preserved.
pub fn write_live_settings_to_flags(lines_to_write: &[String]) -> Result<(), String> {
    rewrite_flags_conf(MANAGED_LIVE_FLAGS, lines_to_write)
}

/// Write (or replace) the four `--hue-*` lines in `flags.conf`. Strips any
/// existing `--hue-bridge`/`--hue-app-key`/`--hue-psk`/`--hue-area`/
/// `--hue-entertainment-id` token from anywhere in the file (inline or on
/// its own line) before appending the fresh values, so re-pairing can't
/// leave stale credentials behind to be duplicated later by another writer.
pub fn write_hue_creds_to_flags(
    bridge: &str,
    app_key: &str,
    psk: &str,
    area: &str,
) -> Result<(), String> {
    rewrite_flags_conf(
        HUE_CRED_FLAGS,
        &[
            format!("--hue-bridge {bridge}"),
            format!("--hue-app-key {app_key}"),
            format!("--hue-psk {psk}"),
            // Area name shell-quoted so values like "Studio Lights" survive.
            format!("--hue-area {}", shell_quote(area)),
        ],
    )
}

/// Flag names this writer owns. Kept as a const so all writers stripping
/// hue creds (now or in future) reference the same set.
const HUE_CRED_FLAGS: &[&str] = &[
    "--hue-bridge",
    "--hue-app-key",
    "--hue-psk",
    "--hue-area",
    "--hue-entertainment-id",
];

/// Replace the `--display <N>` flag in flags.conf with the user's pick.
/// Used by the GUI's display picker. Other flags + comments are preserved.
pub fn write_display_to_flags(display_id: u32) -> Result<(), String> {
    rewrite_flags_conf(&["--display"], &[format!("--display {display_id}")])
}

// ─── Shared flags.conf rewriter ─────────────────────────────────────────────

/// Token-level rewriter that ALL flags.conf writers route through.
///
/// Reads the existing file, tokenizes via shlex (so quoted values like
/// `"Studio Lights"` survive), strips every occurrence of `flags_to_replace`
/// (and its single value-token), then re-emits as:
///   1. Comment lines, preserved verbatim at the top.
///   2. The caller's new lines, in order.
///   3. Any other surviving tokens, joined on a single line.
///
/// Atomic via tmp file + rename, so a crash mid-write can't leave
/// flags.conf truncated.
///
/// **Why all writers must use this**: without unified token-level stripping,
/// writer A appends new lines on top of the existing file; writer B later
/// reads the file and re-emits everything (including A's appended values)
/// on one consolidated line. If A and B touch overlapping flag namespaces,
/// the values end up duplicated, and clap rejects the next launch with
/// "argument cannot be used multiple times".
fn rewrite_flags_conf(
    flags_to_replace: &[&str],
    new_lines: &[String],
) -> Result<(), String> {
    let path = flags_conf_path().ok_or("no $HOME — can't locate flags.conf")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();

    // Pull out standalone comment lines first — we want to preserve them
    // verbatim above the regenerated content. Non-comment text gets
    // tokenized so we can strip individual managed flags.
    let mut comment_lines: Vec<&str> = Vec::new();
    let mut payload = String::with_capacity(raw.len());
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            comment_lines.push(line);
        } else {
            // Strip inline `# ...` comments while keeping the flag content.
            let no_comment = line.split('#').next().unwrap_or("");
            payload.push_str(no_comment);
            payload.push('\n');
        }
    }

    let tokens = shlex::split(&payload).ok_or("flags.conf shell-quote parse error")?;

    // Walk tokens. When we hit a managed `--flag`, swallow it AND the next
    // token (its value, unless the next token starts with `--` — which
    // would mean the value was missing or the flag is boolean).
    let mut kept: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if flags_to_replace.contains(&tokens[i].as_str()) {
            i += 1;
            if i < tokens.len() && !tokens[i].starts_with("--") {
                i += 1;
            }
            continue;
        }
        kept.push(tokens[i].clone());
        i += 1;
    }

    // Re-emit: comments first (preserved order), then our new lines, then
    // any other surviving tokens on a single consolidated line.
    let mut out = String::new();
    for c in &comment_lines {
        out.push_str(c);
        out.push('\n');
    }
    for line in new_lines {
        out.push_str(line);
        out.push('\n');
    }
    if !kept.is_empty() {
        let escaped: Vec<String> = kept.iter().map(|t| shell_quote(t)).collect();
        out.push_str(&escaped.join(" "));
        out.push('\n');
    }

    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, out.as_bytes()).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Quote a value for inclusion in flags.conf. Wraps in double quotes and
/// escapes embedded `"` / `\`. Matches what `shlex::split` will accept.
fn shell_quote(s: &str) -> String {
    if !s.contains([' ', '\t', '"', '\\', '\'']) && !s.is_empty() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_packet_layout() {
        let mut buf = Vec::new();
        let uuid = "12345678-1234-1234-1234-123456789abc";
        let colors = [[0xff, 0x00, 0x80], [0x00, 0xff, 0x00], [0x80, 0x80, 0x80]];
        pack_packet(&mut buf, uuid, &colors);
        assert_eq!(&buf[..9], b"HueStream");
        assert_eq!(buf[9], 2); // major
        assert_eq!(buf[14], 0); // RGB color space
        assert_eq!(&buf[16..52], uuid.as_bytes());
        // 3 channels × (1 id + 6 color) = 21 bytes after the 52-byte header.
        assert_eq!(buf.len(), 52 + 21);
        // Channel 0 with color [0xff, 0x00, 0x80].
        assert_eq!(buf[52], 0);
        assert_eq!(&buf[53..55], &0xffffu16.to_be_bytes());
        assert_eq!(&buf[55..57], &0x0000u16.to_be_bytes());
        assert_eq!(&buf[57..59], &0x8080u16.to_be_bytes());
        // Channel 1 with color [0x00, 0xff, 0x00].
        assert_eq!(buf[59], 1);
        assert_eq!(&buf[60..62], &0x0000u16.to_be_bytes());
        assert_eq!(&buf[62..64], &0xffffu16.to_be_bytes());
    }

    #[test]
    fn dual_reducer_separates_two_distinct_hues() {
        // Half the pixels vivid red, half vivid blue. Histogram should
        // surface BOTH as distinct primary/secondary.
        let mut pixels = Vec::new();
        for _ in 0..50 {
            pixels.extend_from_slice(&[255, 0, 0, 255]); // red
        }
        for _ in 0..50 {
            pixels.extend_from_slice(&[0, 0, 255, 255]); // blue
        }
        let mut r = DualReducer::new();
        // smoothing=1.0 → no inertia, this single call settles immediately.
        let (p, s) = r.reduce(&pixels, 1.0, 1.0, 1.0, 1.0);
        let primary_is_red = p[0] > p[2];
        let secondary_is_red = s[0] > s[2];
        assert_ne!(
            primary_is_red, secondary_is_red,
            "primary {p:?} and secondary {s:?} should be different hues"
        );
    }

    #[test]
    fn dual_reducer_uniform_scene_falls_back_to_primary() {
        let pixels: Vec<u8> = (0..100).flat_map(|_| [255, 0, 0, 255]).collect();
        let mut r = DualReducer::new();
        let (p, s) = r.reduce(&pixels, 1.0, 1.0, 1.0, 1.0);
        assert!(p[0] > p[1] && p[0] > p[2], "primary not red: {p:?}");
        assert_eq!(p, s, "uniform scene: secondary should mirror primary");
    }

    #[test]
    fn dual_reducer_hysteresis_locks_secondary() {
        // Bin weight per pixel: lum × (1 + 4·sat). Pure red lum=0.2126,
        // green lum=0.7152, blue lum=0.0722. So per-pixel weights are
        // approximately: red 1.06, green 3.58, blue 0.36. To make RED the
        // primary, use many more red pixels than green.
        //
        // Frame 1: ~3:1 red:green by weight ⇒ primary=red, secondary=green.
        // Frame 2: add blue with weight only ~10% over green ⇒ challenger
        //   doesn't clear the 1.3× hysteresis margin, green stays.
        let mut r = DualReducer::new();

        let mut frame1 = Vec::new();
        for _ in 0..1000 {
            frame1.extend_from_slice(&[255, 0, 0, 255]); // red weight ≈ 1063
        }
        for _ in 0..100 {
            frame1.extend_from_slice(&[0, 255, 0, 255]); // green weight ≈ 358
        }
        let (p1, s1) = r.reduce(&frame1, 1.0, 1.0, 1.0, 1.0);
        assert!(
            p1[0] > p1[1] && p1[0] > p1[2],
            "frame 1 primary should be red, got {p1:?}"
        );
        assert!(
            s1[1] > s1[0] && s1[1] > s1[2],
            "frame 1 secondary should be green, got {s1:?}"
        );

        // Frame 2: blue ~10% heavier than green (394 vs 358). Hysteresis
        // (1.3×) requires blue ≥ 465 to take over — it doesn't, so green
        // should stay as secondary.
        let mut frame2 = Vec::new();
        for _ in 0..1000 {
            frame2.extend_from_slice(&[255, 0, 0, 255]);
        }
        for _ in 0..100 {
            frame2.extend_from_slice(&[0, 255, 0, 255]); // incumbent
        }
        for _ in 0..1091 {
            frame2.extend_from_slice(&[0, 0, 255, 255]); // blue ≈ 394
        }
        // smoothing=1.0 so the smoothed bins match this frame's raw
        // weights exactly — isolates the hysteresis logic from the EMA.
        let (_, s2) = r.reduce(&frame2, 1.0, 1.0, 1.0, 1.0);
        assert!(
            s2[1] > s2[2],
            "hysteresis should keep green secondary, got {s2:?} (G should beat B)"
        );
    }

    #[test]
    fn reduce_dominant_picks_saturated_over_gray() {
        // 4 gray pixels + 1 vivid orange — top-20% takes ONLY the orange
        // (1/5 of 5 = 1 pixel) so the result should be essentially orange.
        let pixels: Vec<u8> = vec![
            128, 128, 128, 255, // gray
            128, 128, 128, 255, // gray
            128, 128, 128, 255, // gray
            128, 128, 128, 255, // gray
            255, 128, 0, 255, // orange (sat = 1.0)
        ];
        let c = reduce_dominant(&pixels, 1.0, 1.0, 1.0);
        assert!(c[0] > c[1], "R should dominate G ({} vs {})", c[0], c[1]);
        assert!(c[1] > c[2], "G should dominate B ({} vs {})", c[1], c[2]);
    }

    #[test]
    fn reduce_dominant_black_input_stays_black() {
        let pixels = vec![0u8; 16];
        assert_eq!(reduce_dominant(&pixels, 2.2, 1.4, 1.0), [0, 0, 0]);
    }

    #[test]
    fn reduce_dominant_top_n_brightens_dark_scene() {
        // Mostly black with one mid-grey pixel: full-frame average would be
        // near-black; top-20% should pick out the grey region.
        let mut pixels = vec![0u8; 4 * 100];
        pixels[0] = 200;
        pixels[1] = 200;
        pixels[2] = 200;
        pixels[3] = 255;
        let c = reduce_dominant(&pixels, 1.0, 1.0, 1.0);
        assert!(
            c[0] > 100,
            "expected top-20% to surface the bright pixel; got {c:?}"
        );
    }

    #[test]
    fn finalize_color_preserves_hue_under_gain() {
        // Orange (R high, G mid, B low) at high gain. Per-channel clamp
        // would shift toward yellow (R caps but G keeps growing). With
        // chroma-preserving gain, the R:G:B ratio survives.
        let orange = [0.8f32, 0.4, 0.2];
        let lo = finalize_color(orange, 1.0, 1.0);
        let hi = finalize_color(orange, 1.0, 4.0);
        // Both should have the same R:G:B ratio (within rounding).
        let ratio_lo = (lo[1] as f32 / lo[0] as f32, lo[2] as f32 / lo[0] as f32);
        let ratio_hi = (hi[1] as f32 / hi[0] as f32, hi[2] as f32 / hi[0] as f32);
        assert!(
            (ratio_lo.0 - ratio_hi.0).abs() < 0.05,
            "G/R ratio drifted: lo={ratio_lo:?} hi={ratio_hi:?}"
        );
        assert!(
            (ratio_lo.1 - ratio_hi.1).abs() < 0.05,
            "B/R ratio drifted: lo={ratio_lo:?} hi={ratio_hi:?}"
        );
    }

    #[test]
    fn reduce_dominant_inverse_gamma_brightens() {
        // Same pixel, two gamma settings. Higher gamma in the slicer means
        // the OBSERVED value is darker, so inverse-gamma here should
        // brighten more aggressively.
        let pixels = vec![60, 60, 60, 255]; // dim grey
        let no_gamma = reduce_dominant(&pixels, 1.0, 1.0, 1.0);
        let with_gamma = reduce_dominant(&pixels, 2.2, 1.0, 1.0);
        assert!(
            with_gamma[0] > no_gamma[0],
            "inverse-gamma should brighten ({no_gamma:?} → {with_gamma:?})"
        );
    }

    #[test]
    fn shell_quote_handles_spaces_and_quotes() {
        assert_eq!(shell_quote("Studio"), "Studio");
        assert_eq!(shell_quote("Studio Lights"), r#""Studio Lights""#);
        assert_eq!(shell_quote(r#"weird"name"#), r#""weird\"name""#);
        assert_eq!(shell_quote(""), "\"\"");
    }
}
