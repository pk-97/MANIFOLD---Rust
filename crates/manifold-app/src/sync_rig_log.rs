//! Sync rig log writer (FRAME_EXACT_TRANSPORT_DESIGN P3, commit C0).
//!
//! Always on. Writes `sync-<unix ms>-<pid>-pNNNN.log` beside the session logs in
//! `~/Library/Logs/com.latentspace.manifold` and prints the path once at
//! startup. Hot threads only `try_send` records (`manifold_playback::rig_log`);
//! this thread formats and writes them, so no file I/O ever happens on the
//! content, MIDI, OSC or Metal threads. Files rotate at [`PART_BYTES`] and the
//! newest [`PARTS_KEPT`] are kept, so the log can't grow past about 512 MB.
//!
//! Every 5 s this thread also writes a clock pair (the sink's `Instant` epoch
//! against Core Animation host time, the base of `presentedTime`), the drop
//! count, and, when the set of active displays changes, what macOS reports
//! about each display's latency. `scripts/sync_rig_report.py` turns the file
//! into the numbers D7, D19 and D20 need.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use manifold_core::types::ClockAuthority;
use manifold_playback::rig_log::{self, RigRecord};

pub const FILE_PREFIX: &str = "sync-";
pub const PART_BYTES: u64 = 32 << 20;
pub const PARTS_KEPT: usize = 16;
const STATUS_EVERY: Duration = Duration::from_secs(5);
const FLUSH_EVERY: Duration = Duration::from_secs(1);
const PHASE_EVERY_NS: u64 = 5_000_000_000;

/// Start the writer. Called once from `main` after the session log.
pub fn start() {
    let Some(dir) = crate::session_log::log_dir() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        log::warn!("sync rig log disabled: cannot create {dir:?}: {err}");
        return;
    }
    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let stem = format!("{FILE_PREFIX}{unix_ms}-{}", std::process::id());
    let writer = match RigWriter::open(dir, stem, PART_BYTES, PARTS_KEPT) {
        Ok(w) => w,
        Err(err) => {
            log::warn!("sync rig log disabled: {err}");
            return;
        }
    };
    let Some(rx) = rig_log::install(rig_log::CHANNEL_CAPACITY) else {
        return;
    };
    log::info!("sync rig log: {:?}", writer.path());
    let spawned = std::thread::Builder::new()
        .name("sync-rig-log".into())
        .spawn(move || run(rx, writer));
    if let Err(err) = spawned {
        log::warn!("sync rig log writer thread failed to start: {err}");
    }
}

/// A transport command from the user (UI, keys, MIDI mapping), as handled on
/// the content thread. Sync-source transport is logged in `SyncArbiter`.
pub fn user_transport(kind: rig_log::TransportKind, value: f64) {
    rig_log::emit(RigRecord::Transport {
        at_ns: rig_log::now_ns(),
        source: rig_log::TransportSource::User,
        kind,
        value,
    });
}

/// Appends records to a size-capped, rotating set of files.
pub struct RigWriter {
    dir: PathBuf,
    stem: String,
    part: u32,
    part_bytes: u64,
    keep: usize,
    file: BufWriter<File>,
    bytes: u64,
    line: String,
}

impl RigWriter {
    pub fn open(dir: PathBuf, stem: String, part_bytes: u64, keep: usize) -> io::Result<Self> {
        let file = Self::create(&dir, &stem, 0)?;
        let mut writer = Self {
            dir,
            stem,
            part: 0,
            part_bytes,
            keep,
            file,
            bytes: 0,
            line: String::with_capacity(512),
        };
        // Earlier sessions' parts count toward the cap too.
        crate::session_log::prune_prefix(&writer.dir, FILE_PREFIX, writer.keep);
        writer.header()?;
        Ok(writer)
    }

    fn part_path(dir: &Path, stem: &str, part: u32) -> PathBuf {
        dir.join(format!("{stem}-p{part:04}.log"))
    }

    fn create(dir: &Path, stem: &str, part: u32) -> io::Result<BufWriter<File>> {
        let path = Self::part_path(dir, stem, part);
        let file = File::create(&path)
            .map_err(|e| io::Error::other(format!("cannot open {path:?}: {e}")))?;
        Ok(BufWriter::with_capacity(64 * 1024, file))
    }

    pub fn path(&self) -> PathBuf {
        Self::part_path(&self.dir, &self.stem, self.part)
    }

    fn header(&mut self) -> io::Result<()> {
        self.line.clear();
        self.line.push_str(&format!(
            "# manifold sync rig log v1 part={} pid={} times: t=ns since the rig epoch (Instant)\n",
            self.part,
            std::process::id()
        ));
        self.emit_line()
    }

    fn emit_line(&mut self) -> io::Result<()> {
        self.file.write_all(self.line.as_bytes())?;
        self.bytes += self.line.len() as u64;
        if self.bytes >= self.part_bytes {
            self.rotate()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.part += 1;
        self.file = Self::create(&self.dir, &self.stem, self.part)?;
        self.bytes = 0;
        crate::session_log::prune_prefix(&self.dir, FILE_PREFIX, self.keep);
        self.header()
    }

    pub fn record(&mut self, record: &RigRecord) -> io::Result<()> {
        self.line.clear();
        rig_log::format_line(record, &mut self.line);
        self.emit_line()
    }

    /// One free-form line (status, clock, display); `text` ends without '\n'.
    pub fn text(&mut self, text: &str) -> io::Result<()> {
        self.line.clear();
        self.line.push_str(text);
        self.line.push('\n');
        self.emit_line()
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Drain until every sender is gone. Write errors are reported once and the
/// thread keeps draining, so senders never back up.
fn run(rx: Receiver<RigRecord>, writer: RigWriter) {
    run_with(rx, writer, true);
}

/// `probe_displays` is false only in tests, which must not shell out to ioreg.
pub fn run_with(rx: Receiver<RigRecord>, mut writer: RigWriter, probe_displays: bool) {
    let mut failed = false;
    let mut report = |r: io::Result<()>| {
        if let Err(err) = r
            && !failed
        {
            failed = true;
            log::warn!("sync rig log write failed, further lines may be lost: {err}");
        }
    };
    let mut next_status = Instant::now();
    let mut last_flush = Instant::now();
    let mut displays = DisplayWatch::default();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(record) => {
                report(writer.record(&record));
                while let Ok(record) = rx.try_recv() {
                    report(writer.record(&record));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        if now >= next_status {
            next_status = now + STATUS_EVERY;
            report(write_status(&mut writer));
            if probe_displays {
                report(displays.poll(&mut writer));
            }
        }
        if now.duration_since(last_flush) >= FLUSH_EVERY {
            last_flush = now;
            report(writer.flush());
        }
    }
    report(writer.flush());
}

fn write_status(writer: &mut RigWriter) -> io::Result<()> {
    let line = match host_time_pair() {
        Some((t, host_s, err_ns)) => format!(
            "status t={t} dropped={} host_s={host_s:?} pair_err_ns={err_ns}",
            rig_log::dropped()
        ),
        None => format!("status t={} dropped={}", rig_log::now_ns(), rig_log::dropped()),
    };
    writer.text(&line)
}

/// `Instant` (as rig ns) and Core Animation host seconds read back to back:
/// `(midpoint ns, host seconds, ns between the two Instant reads)`.
#[cfg(target_os = "macos")]
fn host_time_pair() -> Option<(u64, f64, u64)> {
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {
        fn CACurrentMediaTime() -> f64;
    }
    let before = rig_log::now_ns();
    // SAFETY: a pure clock read with no arguments.
    let host_s = unsafe { CACurrentMediaTime() };
    let after = rig_log::now_ns();
    Some((before + (after - before) / 2, host_s, after - before))
}

#[cfg(not(target_os = "macos"))]
fn host_time_pair() -> Option<(u64, f64, u64)> {
    None
}

/// Re-probes displays when the set of active displays changes.
#[derive(Default)]
struct DisplayWatch {
    last: Vec<u32>,
    probed: bool,
}

impl DisplayWatch {
    fn poll(&mut self, writer: &mut RigWriter) -> io::Result<()> {
        let ids = display::active_ids();
        if self.probed && ids == self.last {
            return Ok(());
        }
        self.probed = true;
        self.last = ids;
        for line in display::describe(&self.last) {
            writer.text(&format!("display t={} {line}", rig_log::now_ns()))?;
        }
        for line in display::edid_lines() {
            writer.text(&format!("edid t={} {line}", rig_log::now_ns()))?;
        }
        Ok(())
    }
}

/// What macOS says about each display's latency (D19). Off the hot path: this
/// runs on the writer thread, and only when the display set changes.
pub(crate) mod display {
    #[cfg(target_os = "macos")]
    mod ffi {
        use std::ffi::c_void;

        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        pub struct CVTime {
            pub time_value: i64,
            pub time_scale: i32,
            pub flags: i32,
        }

        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            pub fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
            pub fn CGDisplayIsBuiltin(display: u32) -> u32;
            pub fn CGDisplayVendorNumber(display: u32) -> u32;
            pub fn CGDisplayModelNumber(display: u32) -> u32;
            pub fn CGDisplayPixelsWide(display: u32) -> usize;
            pub fn CGDisplayPixelsHigh(display: u32) -> usize;
        }

        #[link(name = "CoreVideo", kind = "framework")]
        unsafe extern "C" {
            pub fn CVDisplayLinkCreateWithCGDisplay(display: u32, out: *mut *mut c_void) -> i32;
            pub fn CVDisplayLinkGetOutputVideoLatency(link: *mut c_void) -> CVTime;
            pub fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link: *mut c_void) -> CVTime;
            pub fn CVDisplayLinkRelease(link: *mut c_void);
        }
    }

    #[cfg(target_os = "macos")]
    pub fn active_ids() -> Vec<u32> {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        // SAFETY: the buffer holds 16 ids and the call writes at most `max`.
        let err = unsafe { ffi::CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) };
        if err != 0 {
            return Vec::new();
        }
        ids[..count.min(16) as usize].to_vec()
    }

    #[cfg(target_os = "macos")]
    pub fn describe(ids: &[u32]) -> Vec<String> {
        ids.iter()
            .map(|&id| {
                // SAFETY: plain queries on an id CoreGraphics just returned.
                let (builtin, vendor, model, w, h) = unsafe {
                    (
                        ffi::CGDisplayIsBuiltin(id) != 0,
                        ffi::CGDisplayVendorNumber(id),
                        ffi::CGDisplayModelNumber(id),
                        ffi::CGDisplayPixelsWide(id),
                        ffi::CGDisplayPixelsHigh(id),
                    )
                };
                let mut link = std::ptr::null_mut();
                // SAFETY: `link` receives a retained display link, released below.
                let (latency, period) = if unsafe { ffi::CVDisplayLinkCreateWithCGDisplay(id, &mut link) } == 0
                    && !link.is_null()
                {
                    // SAFETY: `link` is valid until released.
                    let pair = unsafe {
                        (
                            ffi::CVDisplayLinkGetOutputVideoLatency(link),
                            ffi::CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link),
                        )
                    };
                    // SAFETY: balances the create above.
                    unsafe { ffi::CVDisplayLinkRelease(link) };
                    pair
                } else {
                    (ffi::CVTime::default(), ffi::CVTime::default())
                };
                format!(
                    "id={id} builtin={} vendor={vendor} model={model} px={w}x{h} cv_latency={}/{} cv_latency_flags={} cv_period={}/{}",
                    u8::from(builtin),
                    latency.time_value,
                    latency.time_scale,
                    latency.flags,
                    period.time_value,
                    period.time_scale
                )
            })
            .collect()
    }

    /// Every raw EDID the IORegistry exposes, with its HDMI latency fields.
    /// Runs `ioreg` once per display change; nothing private is called.
    #[cfg(target_os = "macos")]
    pub fn edid_lines() -> Vec<String> {
        let out = match std::process::Command::new("/usr/sbin/ioreg").args(["-l", "-w0"]).output() {
            Ok(out) => out,
            Err(err) => return vec![format!("ioreg_error={err}")],
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let edids = super::edid::find_hex_blobs(&text);
        if edids.is_empty() {
            return vec!["none_in_ioregistry=1".to_string()];
        }
        edids.iter().map(|bytes| super::edid::describe(bytes)).collect()
    }

    /// The display's own output latency in seconds, when CoreVideo reports a
    /// valid one (D19's `L_display`); `None` means unknown.
    #[cfg(target_os = "macos")]
    pub fn output_latency_seconds(id: u32) -> Option<f64> {
        const INDEFINITE: i32 = 1;
        let mut link = std::ptr::null_mut();
        // SAFETY: `link` receives a retained display link, released below.
        if unsafe { ffi::CVDisplayLinkCreateWithCGDisplay(id, &mut link) } != 0 || link.is_null() {
            return None;
        }
        // SAFETY: `link` is valid until released.
        let latency = unsafe { ffi::CVDisplayLinkGetOutputVideoLatency(link) };
        // SAFETY: balances the create above.
        unsafe { ffi::CVDisplayLinkRelease(link) };
        (latency.flags & INDEFINITE == 0 && latency.time_scale > 0 && latency.time_value > 0)
            .then(|| latency.time_value as f64 / f64::from(latency.time_scale))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn output_latency_seconds(_id: u32) -> Option<f64> {
        None
    }

    #[cfg(not(target_os = "macos"))]
    pub fn active_ids() -> Vec<u32> {
        Vec::new()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn describe(_ids: &[u32]) -> Vec<String> {
        Vec::new()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn edid_lines() -> Vec<String> {
        vec!["unsupported_platform=1".to_string()]
    }
}

/// EDID parsing for the HDMI vendor block's latency fields (HDMI 1.4 VSDB).
pub mod edid {
    /// Hex blobs after `"EDID" = <` or `"IODisplayEDID" = <` (with or without
    /// spaces around `=`), as `ioreg -l` prints them.
    pub fn find_hex_blobs(text: &str) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for key in ["\"EDID\"", "\"IODisplayEDID\""] {
            let mut rest = text;
            while let Some(pos) = rest.find(key) {
                rest = &rest[pos + key.len()..];
                let trimmed = rest.trim_start_matches([' ', '=']);
                let Some(body) = trimmed.strip_prefix('<') else {
                    continue;
                };
                let Some(end) = body.find('>') else {
                    break;
                };
                if let Some(bytes) = parse_hex(&body[..end])
                    && bytes.len() >= 128
                    && !out.contains(&bytes)
                {
                    out.push(bytes);
                }
            }
        }
        out
    }

    fn parse_hex(s: &str) -> Option<Vec<u8>> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        if !s.len().is_multiple_of(2) {
            return None;
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect()
    }

    /// The HDMI vendor block's video and audio latency in ms, `None` per field
    /// when the block or the field is absent or says "unknown".
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct HdmiLatency {
        pub vsdb_present: bool,
        pub video_ms: Option<u16>,
        pub audio_ms: Option<u16>,
    }

    /// Latency byte: 0 = not given, 255 = that path unsupported, else (v − 1) × 2 ms.
    fn latency_ms(v: u8) -> Option<u16> {
        match v {
            0 | 255 => None,
            v => Some((u16::from(v) - 1) * 2),
        }
    }

    pub fn hdmi_latency(edid: &[u8]) -> HdmiLatency {
        let mut found = HdmiLatency::default();
        for block in edid.chunks_exact(128).skip(1) {
            // CTA-861 extension: tag 0x02; data blocks run from byte 4 to the
            // detailed-timing offset in byte 2.
            if block[0] != 0x02 {
                continue;
            }
            let end = usize::from(block[2]).clamp(4, 127);
            let mut i = 4;
            while i < end {
                let tag = block[i] >> 5;
                let len = usize::from(block[i] & 0x1f);
                let data = &block[i + 1..(i + 1 + len).min(128)];
                if tag == 3 && data.len() >= 3 && data[..3] == [0x03, 0x0c, 0x00] {
                    found.vsdb_present = true;
                    // data[7] carries the Latency_Fields_Present flag (bit 7).
                    if data.len() >= 10 && data[7] & 0x80 != 0 {
                        found.video_ms = latency_ms(data[8]);
                        found.audio_ms = latency_ms(data[9]);
                    }
                }
                i += 1 + len;
            }
        }
        found
    }

    pub fn describe(edid: &[u8]) -> String {
        let mfg = u16::from_be_bytes([edid[8], edid[9]]);
        let letter = |shift: u16| char::from(b'@' + ((mfg >> shift) & 0x1f) as u8);
        let product = u16::from_le_bytes([edid[10], edid[11]]);
        let lat = hdmi_latency(edid);
        let ms = |v: Option<u16>| v.map_or_else(|| "none".to_string(), |v| v.to_string());
        format!(
            "mfg={}{}{} product={product} bytes={} hdmi_vsdb={} video_latency_ms={} audio_latency_ms={}",
            letter(10),
            letter(5),
            letter(0),
            edid.len(),
            u8::from(lat.vsdb_present),
            ms(lat.video_ms),
            ms(lat.audio_ms)
        )
    }
}

/// Content-thread state for the per-tick and 5-second records. `Copy` so the
/// content thread can hand it out and take it back without borrowing itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct TickProbe {
    authority: Option<ClockAuthority>,
    next_phase_ns: u64,
}

/// What the content thread knows after an engine tick.
pub struct TickFacts {
    pub frame: u64,
    pub wake: Instant,
    pub deadline: Option<Instant>,
    pub frames: u32,
    pub index: i64,
    pub beat: f64,
    pub authority: ClockAuthority,
    pub playing: bool,
    /// Link's beat at this tick's deadline, its tempo and peer count.
    pub link: Option<(f64, f64, i32)>,
    pub link_offset: f64,
    /// MIDI clock beat and tempo while clock arrives.
    pub clk: Option<(f32, f32)>,
    /// AbletonOSC: last song time, its age in seconds, Live's tempo.
    pub osc: Option<(f32, f64, f32)>,
}

impl TickProbe {
    /// Emit this tick's records. No allocation: records are `Copy` and the
    /// sink only `try_send`s.
    pub fn after_tick(&mut self, facts: &TickFacts) {
        if rig_log::enabled() {
            self.after_tick_into(facts, rig_log::emit);
        }
    }

    pub fn after_tick_into(&mut self, facts: &TickFacts, mut emit: impl FnMut(RigRecord)) {
        let wake_ns = rig_log::ns_since_epoch(facts.wake);
        if self.authority != Some(facts.authority) {
            emit(RigRecord::Authority {
                at_ns: wake_ns,
                from: self.authority.unwrap_or(facts.authority),
                to: facts.authority,
            });
            self.authority = Some(facts.authority);
        }
        let (link_beat, link_bpm, link_peers) = facts.link.unwrap_or((f64::NAN, f64::NAN, 0));
        emit(RigRecord::Tick {
            frame: facts.frame,
            wake_ns,
            deadline_ns: facts.deadline.map_or(0, rig_log::ns_since_epoch),
            frames: facts.frames,
            index: facts.index,
            beat: facts.beat,
            authority: facts.authority,
            playing: facts.playing,
            link_beat,
            link_bpm,
            link_peers,
        });
        if wake_ns >= self.next_phase_ns {
            self.next_phase_ns = wake_ns + PHASE_EVERY_NS;
            let (clk_beat, clk_bpm) = facts.clk.unwrap_or((f32::NAN, f32::NAN));
            let (osc_beat, osc_age_s, osc_bpm) = facts.osc.unwrap_or((f32::NAN, f64::NAN, f32::NAN));
            emit(RigRecord::Phase {
                at_ns: wake_ns,
                engine_beat: facts.beat,
                link_beat,
                link_offset: facts.link_offset,
                clk_beat,
                clk_bpm,
                osc_beat,
                osc_age_s,
                osc_bpm,
            });
        }
    }
}

#[cfg(test)]
mod tests;
