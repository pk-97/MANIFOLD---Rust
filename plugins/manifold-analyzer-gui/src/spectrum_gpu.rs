//! Spectrum lines + scrolling spectrogram rendered by manifold-gpu into an
//! IOSurface-backed texture.
//!
//! One shader, one draw, two regions. Top region draws Mid + Side curves;
//! bottom region samples a ring-buffer history of Mid frames for the
//! spectrogram.
//!
//! Pattern:
//! 1. WGSL fragment shader reads `mid_spectrum[]` + `side_spectrum[]` (current
//!    frame) and `history[]` (ring buffer of past Mid frames) from storage
//!    buffers, maps each pixel (x → log-freq → bin), and produces either a
//!    curve+fill composite (top) or a colourmapped history sample (bottom).
//! 2. `device.draw_fullscreen` renders into the IOSurface-backed `GpuTexture`.
//! 3. `commit_and_wait_completed` syncs; egui's PaintCallback samples the same
//!    IOSurface via GL_TEXTURE_RECTANGLE (see `gl_paint.rs`).
//!
//! The uniform + spectrum + history storage buffers are allocated once and
//! rewritten via the shared-memory mapped pointer each frame — zero audio-
//! thread cost, small memcpys on the GUI thread.

use crate::cqt::{CqtComplex, CqtTransform};
use crate::gpu_bridge::IoSurfaceMtlTexture;
use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuRenderPipeline, GpuTextureFormat,
};
use std::ffi::c_void;

const SHADER: &str = include_str!("../shaders/spectrum_line.wgsl");

/// Number of historical Mid frames kept for the spectrogram. One column is
/// written per VQT hop; at 5.33 ms/hop, 2048 cols ≈ 10.9 s of scroll-back,
/// enough to fill a wide window.
const HISTORY_COLS: u32 = 2048;

/// VQT parameters. Variable-Q transform: each bin's bandwidth is
/// `bandwidth(f) = α·f + γ(f)`. At high freq `α·f` dominates (constant-Q
/// behavior, tight pitch); at low freq `γ(f)` floors the bandwidth so
/// bass windows stay tractable instead of growing to seconds. `γ = 0`
/// reduces to classical CQT.
///
/// Here `γ(f)` is itself frequency-dependent — see the γ curve constants
/// below. The longest window lands around ~500 ms at the very bottom of
/// the range (enough to fit several sub-bass cycles and flatten the 2f
/// ripple), so an N_fft of 65 536 (≈ 1.37 s) covers everything down to
/// ~10 Hz comfortably.
const CQT_N_FFT: usize = 65_536;
const CQT_FMIN_HZ: f32 = 10.0;
const CQT_FMAX_HZ: f32 = 22_000.0;
const CQT_BINS_PER_OCTAVE: usize = 24;
/// Frequency-dependent bandwidth floor γ(f): ramps from `CQT_GAMMA_LO_HZ`
/// at 0 Hz up to `CQT_GAMMA_HI_HZ` at `CQT_GAMMA_TRANSITION_HZ` and above.
/// A small γ at the very bottom (≤ 30 Hz) grows the bass kernel long enough
/// to fit several cycles, killing the 2f amplitude-modulation ripple that
/// makes sub-bass sines look wavy; the normal γ above the knee keeps mid
/// and high bins short enough for crisp transients. Below 30 Hz at γ = 2
/// the kernel reaches ~500 ms; around 200 Hz it's back to the familiar
/// ~40 ms. Setting lo == hi reproduces the earlier constant-floor
/// behaviour.
const CQT_GAMMA_LO_HZ: f32 = 10.0;
const CQT_GAMMA_HI_HZ: f32 = 20.0;
const CQT_GAMMA_TRANSITION_HZ: f32 = 200.0;
/// Prune kernel entries below this fraction of each row's peak.
/// 1e-4 ≈ −80 dB floor — below the synchrosqueezing scatter gate so
/// kernel-truncation bleed can't ghost a harmonic on a pure sine. Larger
/// kernels (more FFT-weight entries retained) but still sparse.
const CQT_THRESHOLD_REL: f32 = 1e-4;
/// Samples between consecutive VQT columns. 256 at 48 kHz = 5.33 ms per
/// column, 187.5 columns per second. Short enough that phase advance
/// per hop stays below π for all in-range bins, which keeps
/// synchrosqueezing's instantaneous-frequency estimate unambiguous
/// (no 2f-style ghost lines from phase wrap).
const CQT_HOP_SAMPLES: usize = 256;
/// Kernel length floor in samples. Clamps HF bandwidth so high-frequency
/// partials don't smear across hundreds of Hz. 4 × hop guarantees 75 %
/// overlap between consecutive frames, which keeps synchrosqueezing's
/// phase-diff coherent at the top end. Costs more per-column CPU (HF
/// kernels retain more FFT bins after sparsification), but the VQT is
/// already cheap relative to the shared FFT.
const CQT_MIN_KERNEL_LEN: usize = 4 * CQT_HOP_SAMPLES;
/// Use asymmetric (causal) Blackman-Harris windows. Each kernel peaks
/// at the newest sample and tapers back in time — the display column
/// you see "now" reflects audio "now", not audio centered n_k/2 samples
/// ago. Matches how iZotope RX / Vision 4X handle real-time display.
/// Trade-off: ~2× wider main lobe per bin than a symmetric window of
/// the same length. The IF-consistency gate accounts for this.
const CQT_CAUSAL_WINDOW: bool = false;

/// Noise-floor gate. Log bins whose final dB sits below this cut off
/// to the black floor so quiet background stays fully black instead of
/// speckling with sub-audible bin-level dithering.
const NOISE_GATE_DB: f32 = -90.0;
const FLOOR_DB: f32 = -140.0;

/// Fallback scatter-side gate used when the GUI hasn't supplied one yet.
/// Source bins whose current or previous-frame power sits below the
/// active gate don't contribute to synchrosqueezed scatter — their
/// phases are noise. Too high → more "vertical rain"; too low → thicker
/// tonal lines and kick attacks drop out. Exposed to the UI as a param.
const SYNCHRO_SCATTER_GATE_DEFAULT_DB: f32 = -75.0;

/// SPAN-style display options driving the fragment shader.
#[derive(Copy, Clone, Debug)]
pub struct DisplayConfig {
    pub slope_db_per_oct: f32,
    pub slope_ref_freq: f32,
    pub align_offset_db: f32,
    /// Half-bandwidth of frequency smoothing in log2(octave). Set to
    /// `1.0 / 24.0` for a 1/12-oct smoothing (±1/24 oct either side of the
    /// centre frequency). `0.0` disables smoothing.
    pub smooth_half_oct_log2: f32,
    /// Alpha of the fill below each curve. `0.0` disables fill.
    pub fill_alpha: f32,
    /// Fraction of total render-target height devoted to the spectrum curves.
    /// Remainder is spectrogram. `1.0` disables the spectrogram region.
    pub spectrum_fraction: f32,
    /// dB range that maps onto the spectrogram colourmap (independent of
    /// the spectrum curve's `db_min`/`db_max` axis). Vision 4X's Heatmap
    /// default is `-59 … 0`.
    pub spectrogram_db_min: f32,
    pub spectrogram_db_max: f32,
    /// If true, synchrosqueeze each VQT column — reassign energy in
    /// frequency (not time) toward the instantaneous-frequency implied
    /// by the phase advance between consecutive frames. Tonal content
    /// (sustained notes, harmonic stacks) collapses to razor-thin lines
    /// without the striation artifact of full reassignment; transient
    /// content is unchanged. Invertible, so no visual lying.
    pub enable_synchrosqueezing: bool,
    /// 3-frame coherence gate on top of synchrosqueezing. Requires
    /// `if_now ≈ if_prev` to accept a scatter; rejects transient /
    /// noise scatter but can strip sustained notes on brief amplitude
    /// dips.
    pub enable_coherence_check: bool,
    /// Power threshold (dB) below which a source bin is rejected from
    /// synchrosqueezed scatter. Lower (−85..−90) keeps kick attacks /
    /// transients continuous; higher (−65..−60) is cleaner on noise.
    pub synchro_gate_db: f32,
    /// Weighting mode id consumed by the shader's `weighting_db`
    /// function. 0 = Flat, 1 = Pink (+3), 2 = Tilted (+4.5),
    /// 3 = LUFS (BS.1770 K-weighting), 4 = LUFS sub-adjusted
    /// (HF shelf only, no HPF).
    pub weighting_mode: f32,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            slope_db_per_oct: 0.0,
            slope_ref_freq: 1000.0,
            align_offset_db: 0.0,
            smooth_half_oct_log2: 0.0,
            fill_alpha: 0.0,
            spectrum_fraction: 1.0,
            spectrogram_db_min: -59.0,
            spectrogram_db_max: 0.0,
            enable_synchrosqueezing: false,
            enable_coherence_check: false,
            synchro_gate_db: SYNCHRO_SCATTER_GATE_DEFAULT_DB,
            weighting_mode: 2.0,
        }
    }
}

/// BPM-locked spectrogram state. When `enabled`, each emitted column is
/// written at `col = (beat_pos / beats_per_window) · HISTORY_COLS mod
/// HISTORY_COLS` instead of being appended. The display then fills left
/// → right and wraps to overwrite the oldest pixels, matching Vision 4X.
///
/// `beat_pos` is the host's absolute musical position in quarter notes.
/// Modulo `beats_per_window` gives the position inside the currently
/// visible grid cycle. `bpm` is only needed to detect tempo changes
/// (which invalidate history since column→beat scale changes).
#[derive(Copy, Clone, Debug)]
pub struct SyncConfig {
    pub enabled: bool,
    pub bpm: f32,
    pub beat_pos: f64,
    pub beats_per_window: f32,
}

impl SyncConfig {
    pub const OFF: Self = Self {
        enabled: false,
        bpm: 0.0,
        beat_pos: 0.0,
        beats_per_window: 0.0,
    };
}

/// Matches the `Uniforms` struct in `spectrum_line.wgsl`.
#[repr(C)]
#[derive(Copy, Clone)]
struct SpectrumUniforms {
    resolution: [f32; 2],
    sample_rate: f32,
    fft_size: f32,
    freq_min: f32,
    freq_max: f32,
    db_min: f32,
    db_max: f32,
    line_color: [f32; 4],
    bg_color: [f32; 4],
    side_color: [f32; 4],
    line_thickness: f32,
    slope_db_per_oct: f32,
    slope_ref_freq: f32,
    align_offset_db: f32,
    smooth_half_oct_log2: f32,
    fill_alpha: f32,
    spectrum_height: f32,
    history_cols: f32,
    write_col: f32,
    spectrogram_db_min: f32,
    spectrogram_db_max: f32,
    log_bins: f32,
    /// 1.0 when sync mode is active; 0.0 for free-scroll. In sync mode
    /// the shader treats x → col as a linear map over `history_cols`
    /// (left = col 0, right = last col) and draws a 1-pixel playhead
    /// line at `write_col`.
    sync_mode: f32,
    /// Bottom edge of the CQT's log-bin axis, needed so the shader can
    /// map `freq_min`..`freq_max` onto the stored log bins for the
    /// spectrogram Y axis.
    cqt_fmin_hz: f32,
    cqt_bins_per_octave: f32,
    /// Weighting mode (see `DisplayConfig::weighting_mode`).
    weighting_mode: f32,
}

pub struct SpectrumGpuRenderer {
    target: IoSurfaceMtlTexture,
    mid_buf: GpuBuffer,
    side_buf: GpuBuffer,
    history_buf: GpuBuffer,
    pipeline: GpuRenderPipeline,
    num_bins: usize,
    display: DisplayConfig,
    sync: SyncConfig,
    /// Last-applied sync state. When `enabled`, `bpm`, or
    /// `beats_per_window` changes, we clear the history to `FLOOR_DB` so
    /// stale pixels at the new column positions don't show through.
    last_sync_applied: SyncConfig,
    /// Internal beat clock. The host only publishes `beat_pos` once per
    /// process block, but we need a beat position for every emitted
    /// column (many per block). Advancing an internal accumulator by
    /// `beats_per_hop` each hop gives a continuous clock that matches
    /// the audio we're actually processing. Re-anchored to the host
    /// only on big jumps (seek, tempo change) — small steady drift is
    /// invisible over a few seconds and keeps us from sprinkling gaps
    /// on every block boundary.
    internal_beat_pos: f64,
    have_internal_beat: bool,
    write_col: u32,
    // Spectrogram pipeline. Audio samples arrive in variable-sized chunks
    // via `ingest_samples`; we keep an N_fft-long rolling window and run
    // the CQT every CQT_HOP_SAMPLES samples. Each CQT emits one column
    // into `history_buf`, which is sized `cqt.num_bins() × HISTORY_COLS`.
    cqt: CqtTransform,
    cqt_num_bins: usize,
    sample_rate: f32,
    // Rolling window: latest CQT_N_FFT samples, newest at `rolling_head`.
    rolling: Vec<f32>,
    rolling_head: usize,
    samples_since_last_hop: usize,
    fft_scratch_audio: Vec<f32>,
    cqt_complex: Vec<CqtComplex<f32>>,
    // Synchrosqueezing state. We keep the two previous columns so the
    // IF can be computed at two consecutive frame boundaries — a
    // coherence check (IF_now ≈ IF_prev) rejects transient/noise
    // columns whose phase-diff happens to look tonal for one frame
    // but can't stay consistent across three. `synchro_power_scratch`
    // is the scatter accumulator — cleared per column, then summed.
    prev_cqt_complex: Vec<CqtComplex<f32>>,
    prev2_cqt_complex: Vec<CqtComplex<f32>>,
    have_prev_cqt: bool,
    have_prev2_cqt: bool,
    synchro_power_scratch: Vec<f32>,
    cqt_out: Vec<f32>,
    /// dB output of the previous emitted hop. When one hop's write
    /// span covers many screen columns (sync mode, tight zoom), we
    /// lerp per-column between this and the current `cqt_out` so the
    /// display shows a smooth transition instead of N identical
    /// staircase pixels.
    prev_cqt_out: Vec<f32>,
    have_prev_cqt_out: bool,
    /// Scratch buffer for per-column lerp writes.
    lerp_scratch: Vec<f32>,
}

impl SpectrumGpuRenderer {
    /// Create the renderer with a fixed-size render target. `num_bins` is
    /// the top-curve FFT's half-size (for the averaged Mid/Side storage
    /// buffers); the spectrogram CQT has its own count derived from
    /// `sample_rate` + `CQT_FMIN_HZ/FMAX_HZ/BINS_PER_OCTAVE`.
    pub fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        num_bins: usize,
        sample_rate: f32,
    ) -> Option<Self> {
        let target = IoSurfaceMtlTexture::new(device, width, height)?;

        let mid_buf = device.create_buffer_shared((num_bins * 4) as u64);
        let side_buf = device.create_buffer_shared((num_bins * 4) as u64);

        let fmax = CQT_FMAX_HZ.min(sample_rate * 0.5);
        let cqt = CqtTransform::new(
            sample_rate,
            CQT_N_FFT,
            CQT_FMIN_HZ,
            fmax,
            CQT_BINS_PER_OCTAVE,
            CQT_GAMMA_LO_HZ,
            CQT_GAMMA_HI_HZ,
            CQT_GAMMA_TRANSITION_HZ,
            CQT_MIN_KERNEL_LEN,
            CQT_CAUSAL_WINDOW,
            CQT_THRESHOLD_REL,
        );
        let cqt_num_bins = cqt.num_bins();

        let history_buf =
            device.create_buffer_shared((cqt_num_bins as u64) * (HISTORY_COLS as u64) * 4);

        // Shared buffers are zero-initialised. 0 dB would hit the top of
        // the colourmap (solid red) on every unwritten column — fill with
        // a low floor so unseen history reads as silence (black) until
        // real frames overwrite it.
        if let Some(ptr) = history_buf.mapped_ptr() {
            let num_elems = cqt_num_bins * HISTORY_COLS as usize;
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, num_elems) };
            slice.fill(-140.0);
        }
        let pipeline = device.create_render_pipeline(
            SHADER,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Bgra8Unorm,
            None,
            "spectrum_line",
        );

        Some(Self {
            target,
            mid_buf,
            side_buf,
            history_buf,
            pipeline,
            num_bins,
            display: DisplayConfig::default(),
            sync: SyncConfig::OFF,
            last_sync_applied: SyncConfig::OFF,
            internal_beat_pos: 0.0,
            have_internal_beat: false,
            write_col: 0,
            cqt,
            cqt_num_bins,
            sample_rate,
            rolling: vec![0.0; CQT_N_FFT],
            rolling_head: 0,
            samples_since_last_hop: 0,
            fft_scratch_audio: vec![0.0; CQT_N_FFT],
            cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev2_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            have_prev_cqt: false,
            have_prev2_cqt: false,
            synchro_power_scratch: vec![0.0; cqt_num_bins],
            cqt_out: vec![FLOOR_DB; cqt_num_bins],
            prev_cqt_out: vec![FLOOR_DB; cqt_num_bins],
            have_prev_cqt_out: false,
            lerp_scratch: vec![FLOOR_DB; cqt_num_bins],
        })
    }

    pub fn set_display(&mut self, config: DisplayConfig) {
        self.display = config;
    }

    /// Update the BPM-sync configuration. The current config is remembered
    /// and consulted inside `emit_column` — we don't apply it eagerly
    /// because the audio thread's beat position advances between renders
    /// and each emitted column should snap to its own moment. When the
    /// mode toggles or the tempo/window width changes, clear the history
    /// so old columns don't bleed into the new grid.
    pub fn set_sync(&mut self, config: SyncConfig) {
        let changed = config.enabled != self.last_sync_applied.enabled
            || (config.enabled
                && ((config.bpm - self.last_sync_applied.bpm).abs() > 0.05
                    || (config.beats_per_window - self.last_sync_applied.beats_per_window).abs()
                        > 1e-3));
        if changed {
            self.clear_history();
            self.write_col = 0;
            self.have_internal_beat = false;
            self.have_prev_cqt_out = false;
        }
        // Re-anchor the internal beat clock to the host whenever we
        // don't have one yet (first enable, mode change) or the host's
        // position has jumped far from ours (seek, loop point). Small
        // drift from steady playback is ignored so per-block snapshot
        // quantisation doesn't retrigger the re-anchor every paint.
        if config.enabled {
            let anchor_threshold = (config.beats_per_window as f64 * 0.5).max(0.1);
            if !self.have_internal_beat
                || (self.internal_beat_pos - config.beat_pos).abs() > anchor_threshold
            {
                self.internal_beat_pos = config.beat_pos;
                self.have_internal_beat = true;
            }
        } else {
            self.have_internal_beat = false;
        }
        self.sync = config;
        self.last_sync_applied = config;
    }

    fn clear_history(&mut self) {
        if let Some(ptr) = self.history_buf.mapped_ptr() {
            let num_elems = self.cqt_num_bins * HISTORY_COLS as usize;
            let slice =
                unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, num_elems) };
            slice.fill(FLOOR_DB);
        }
    }

    pub fn iosurface(&self) -> *mut c_void {
        self.target.iosurface_raw()
    }

    pub fn width(&self) -> u32 {
        self.target.width
    }

    pub fn height(&self) -> u32 {
        self.target.height
    }

    /// Resize the IOSurface-backed texture to `new_w × new_h` if different
    /// from the current size. Returns `true` if a rebuild happened — the
    /// caller must invalidate the GL-side `QuadPainter` because it bound
    /// to the now-dropped IOSurface.
    pub fn ensure_size(&mut self, device: &GpuDevice, new_w: u32, new_h: u32) -> bool {
        if new_w == self.target.width && new_h == self.target.height {
            return false;
        }
        if let Some(new_target) = IoSurfaceMtlTexture::new(device, new_w, new_h) {
            self.target = new_target;
            true
        } else {
            false
        }
    }

    /// Number of log bins in the spectrogram's vertical axis (== number
    /// of CQT output bins).
    #[allow(dead_code)] // diagnostic accessor
    pub fn cqt_num_bins(&self) -> usize {
        self.cqt_num_bins
    }

    /// Consume a block of mono audio samples. Appends them to the rolling
    /// N_fft window and, every `CQT_HOP_SAMPLES`, runs one CQT and writes
    /// a column to the history buffer. Samples beyond the rolling window
    /// naturally fall off the back (ring overwrite).
    pub fn ingest_samples(&mut self, samples: &[f32]) {
        let beats_per_hop = if self.sync.bpm > 0.0 {
            (CQT_HOP_SAMPLES as f64) / (self.sample_rate as f64)
                * (self.sync.bpm as f64)
                / 60.0
        } else {
            0.0
        };
        for &s in samples {
            self.rolling[self.rolling_head] = s;
            self.rolling_head = (self.rolling_head + 1) % CQT_N_FFT;
            self.samples_since_last_hop += 1;
            if self.samples_since_last_hop >= CQT_HOP_SAMPLES {
                self.samples_since_last_hop = 0;
                let beat_at_hop = self.internal_beat_pos;
                self.emit_column(beat_at_hop);
                // Advance the internal clock regardless of sync mode so
                // that re-entering sync later keeps a plausible
                // position — it'll still be re-anchored on the next
                // set_sync anyway.
                self.internal_beat_pos += beats_per_hop;
            }
        }
    }

    fn emit_column(&mut self, beat_at_hop: f64) {
        // Copy the rolling ring into linear order (oldest → newest).
        // `rolling_head` is the next write position, so oldest sample is
        // at `rolling_head` and the ring reads out as
        // [head..N_fft) ++ [0..head).
        let head = self.rolling_head;
        let (tail, front) = self.rolling.split_at(head);
        self.fft_scratch_audio[..front.len()].copy_from_slice(front);
        self.fft_scratch_audio[front.len()..].copy_from_slice(tail);

        self.cqt
            .process_complex(&self.fft_scratch_audio, &mut self.cqt_complex);

        if self.display.enable_synchrosqueezing && self.have_prev_cqt {
            self.synchrosqueeze_into_cqt_out();
        } else {
            for (dst, c) in self.cqt_out.iter_mut().zip(self.cqt_complex.iter()) {
                let db = 10.0 * (c.norm_sqr() + 1e-24).log10();
                *dst = if db < NOISE_GATE_DB { FLOOR_DB } else { db };
            }
        }

        // Shift the phase-history buffers: prev2 ← prev, prev ← curr.
        // Done unconditionally so SS toggled on later immediately has
        // valid history.
        if self.have_prev_cqt {
            self.prev2_cqt_complex
                .copy_from_slice(&self.prev_cqt_complex);
            self.have_prev2_cqt = true;
        }
        self.prev_cqt_complex.copy_from_slice(&self.cqt_complex);
        self.have_prev_cqt = true;

        let prev_col = self.write_col;
        let new_col = if self.sync.enabled && self.sync.beats_per_window > 0.0 {
            let t = beat_at_hop / self.sync.beats_per_window as f64;
            let frac = t - t.floor();
            let col = (frac * HISTORY_COLS as f64) as u32;
            col.min(HISTORY_COLS - 1)
        } else {
            (self.write_col + 1) % HISTORY_COLS
        };
        self.write_col = new_col;

        // Distance from prev_col+1 forward to new_col (inclusive).
        // Wrap-aware so the 1→0 seam across the history ring fills
        // correctly; capped to 3 hops' worth of cols so a discontinuity
        // (startup, scrub, tempo change, host stall) paints a single
        // hop-wide stripe at the new position instead of smearing
        // across the ring.
        let forward = (new_col + HISTORY_COLS - prev_col) % HISTORY_COLS;
        let max_forward =
            if self.sync.enabled && self.sync.beats_per_window > 0.0 && self.sync.bpm > 0.0 {
                let hop_in_beats = (CQT_HOP_SAMPLES as f64) / (self.sample_rate as f64)
                    * (self.sync.bpm as f64)
                    / 60.0;
                let cph = (hop_in_beats / self.sync.beats_per_window as f64
                    * HISTORY_COLS as f64)
                    .ceil() as u32;
                (cph * 3).max(4)
            } else {
                1
            };
        let (start_col, fill_count) = if forward == 0 || forward > max_forward {
            (new_col, 1)
        } else {
            ((prev_col + 1) % HISTORY_COLS, forward)
        };

        if let Some(ptr) = self.history_buf.mapped_ptr() {
            let bins = self.cqt_num_bins;
            let stride = bins * 4;
            // Single-column path (free mode, discontinuities) — plain
            // memcpy of the current frame. Avoid touching the lerp
            // scratch so the fast path stays a single bulk write.
            if fill_count == 1 || !self.have_prev_cqt_out {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.cqt_out.as_ptr() as *const u8,
                        ptr.add(start_col as usize * stride),
                        stride,
                    );
                    if fill_count > 1 {
                        // No previous frame — hold current across the
                        // span (first hop after a mode change).
                        let mut c = (start_col + 1) % HISTORY_COLS;
                        for _ in 1..fill_count {
                            std::ptr::copy_nonoverlapping(
                                self.cqt_out.as_ptr() as *const u8,
                                ptr.add(c as usize * stride),
                                stride,
                            );
                            c = (c + 1) % HISTORY_COLS;
                        }
                    }
                }
            } else {
                // Multi-column fill: lerp dB values between the previous
                // frame and the current one so the hop's span shows a
                // smooth gradient instead of `fill_count` identical
                // pixel columns.
                let mut c = start_col;
                let denom = fill_count as f32;
                for i in 0..fill_count {
                    let t = (i + 1) as f32 / denom;
                    let one_minus_t = 1.0 - t;
                    for (dst, (&prev, &curr)) in self
                        .lerp_scratch
                        .iter_mut()
                        .zip(self.prev_cqt_out.iter().zip(self.cqt_out.iter()))
                    {
                        *dst = prev * one_minus_t + curr * t;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            self.lerp_scratch.as_ptr() as *const u8,
                            ptr.add(c as usize * stride),
                            stride,
                        );
                    }
                    c = (c + 1) % HISTORY_COLS;
                }
            }
        }

        self.prev_cqt_out.copy_from_slice(&self.cqt_out);
        self.have_prev_cqt_out = true;
    }

    /// Synchrosqueezing: for each source bin k, compute its instantaneous
    /// frequency from phase advance between consecutive columns, then
    /// scatter its power to the log bin at that IF. Time position is
    /// preserved (we're rebuilding this column's output in place). Pure
    /// tones whose main lobes report consistent IFs collapse to near-delta
    /// lines; noise bins scatter diffusely but stay in this column, not
    /// across adjacent columns.
    ///
    /// We require three consecutive frames of coherent phase before
    /// accepting a scatter. A transient or noise spike produces one
    /// anomalous frame — IF_now and IF_prev disagree — and is rejected
    /// before it can pile energy into a single-pixel vertical streak.
    /// Sustained tones pass trivially (IF stays put frame-to-frame).
    fn synchrosqueeze_into_cqt_out(&mut self) {
        let center_freqs = self.cqt.center_freqs();
        let bandwidths = self.cqt.bandwidths_hz();
        let num_bins = self.cqt_num_bins;
        let hop = CQT_HOP_SAMPLES as f32;
        let two_pi = std::f32::consts::TAU;
        // rad/hop → Hz: sr / (2π · hop).
        let freq_per_rad_per_hop = self.sample_rate / (two_pi * hop);
        let hop_over_sr = hop / self.sample_rate;
        // Reassignment only makes sense for bins with enough power in
        // BOTH current and previous frames — noise-floor bins have
        // random phase whose difference is random IF, piling rain onto
        // random log bins if not gated.
        let mag_gate_power = 10.0_f32.powf(self.display.synchro_gate_db * 0.1);
        // Discrete-time phase diff across one hop can only unambiguously
        // resolve IF deviations up to ±sr/(2·hop). Beyond that, the phase
        // wraps and produces an aliased IF — a 50 Hz signal leaked into
        // the 100 Hz bin (only −30 dB down the main-lobe skirt) would
        // otherwise scatter a ghost line at ~143 Hz. Cap the gate below
        // this wrap limit, with an 80 % safety margin.
        let unambiguous_hz = self.sample_rate / (2.0 * hop) * 0.8;

        // Map from freq → log-bin index. VQT bins are geometric:
        //   f_k = fmin · 2^(k/bpo)   ⇒   k(f) = bpo · log2(f / fmin).
        // So we need fmin and bpo; derive from center_freqs[0..1].
        let fmin = center_freqs[0];
        let log2_ratio = if num_bins > 1 {
            (center_freqs[1] / center_freqs[0]).log2()
        } else {
            1.0 / 24.0
        };
        let inv_log2_ratio = 1.0 / log2_ratio;

        self.synchro_power_scratch.fill(0.0);

        for k in 0..num_bins {
            let x_curr = self.cqt_complex[k];
            let x_prev = self.prev_cqt_complex[k];
            let x_prev2 = self.prev2_cqt_complex[k];
            let power = x_curr.norm_sqr();
            let prev_power = x_prev.norm_sqr();
            let prev2_power = x_prev2.norm_sqr();
            // 2-frame power gate is mandatory — phase difference is
            // only meaningful when both endpoints have real energy.
            if power < mag_gate_power || prev_power < mag_gate_power {
                continue;
            }
            let f_k = center_freqs[k];
            let expected = two_pi * f_k * hop_over_sr;
            // IF from the most recent frame boundary (curr, prev).
            let raw_dev_now = x_curr.arg() - x_prev.arg() - expected;
            let wrapped_now = raw_dev_now - two_pi * (raw_dev_now / two_pi).round();
            let if_now = f_k + wrapped_now * freq_per_rad_per_hop;

            if if_now <= 0.0 {
                continue;
            }

            // IF-consistency gate: reject scatter when the estimated
            // IF deviates farther than either the bin's bandwidth
            // (main-lobe cutoff) or the discrete-time unambiguous
            // wrap limit.
            let if_deviation = (if_now - f_k).abs();
            let gate_hz = bandwidths[k].min(unambiguous_hz);
            if if_deviation > gate_hz {
                continue;
            }

            // Optional 3-frame coherence: when prev2 also carries real
            // energy, require if_now ≈ if_prev. Transients move through
            // the window and produce disagreeing IFs; sustained tones
            // pass trivially. We DON'T require prev2 power — without it
            // a brief amplitude dip would reject the same bin three
            // output frames in a row (striping), so fall back to the
            // 2-frame gate above when prev2 is silent.
            if self.display.enable_coherence_check && prev2_power >= mag_gate_power {
                let raw_dev_prev = x_prev.arg() - x_prev2.arg() - expected;
                let wrapped_prev = raw_dev_prev - two_pi * (raw_dev_prev / two_pi).round();
                let if_prev = f_k + wrapped_prev * freq_per_rad_per_hop;
                let coherence_threshold = bandwidths[k] * 0.5;
                if (if_now - if_prev).abs() > coherence_threshold {
                    continue;
                }
            }

            let log_bin_f = (if_now / fmin).log2() * inv_log2_ratio;
            if !(log_bin_f >= 0.0) {
                continue;
            }
            let lo = log_bin_f as usize;
            if lo >= num_bins {
                continue;
            }
            let frac = log_bin_f - lo as f32;
            self.synchro_power_scratch[lo] += power * (1.0 - frac);
            let hi = lo + 1;
            if hi < num_bins {
                self.synchro_power_scratch[hi] += power * frac;
            }
        }

        for (dst, &p) in self.cqt_out.iter_mut().zip(self.synchro_power_scratch.iter()) {
            let db = if p > 1e-20 {
                10.0 * p.log10()
            } else {
                FLOOR_DB
            };
            *dst = if db < NOISE_GATE_DB { FLOOR_DB } else { db };
        }
    }

    /// Render one frame. `mid_db` / `side_db` feed the Mid + Side curves
    /// (averaged); the spectrogram is drawn from history already populated
    /// via `push_spectrogram_frame`.
    pub fn render(
        &mut self,
        device: &GpuDevice,
        mid_db: &[f32],
        side_db: &[f32],
        sample_rate: f32,
        freq_min: f32,
        freq_max: f32,
        db_min: f32,
        db_max: f32,
    ) {
        debug_assert_eq!(mid_db.len(), self.num_bins);
        debug_assert_eq!(side_db.len(), self.num_bins);

        // Zero-copy upload into the shared-storage spectrum buffers.
        if let Some(ptr) = self.mid_buf.mapped_ptr() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    mid_db.as_ptr() as *const u8,
                    ptr,
                    mid_db.len() * 4,
                );
            }
        }
        if let Some(ptr) = self.side_buf.mapped_ptr() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    side_db.as_ptr() as *const u8,
                    ptr,
                    side_db.len() * 4,
                );
            }
        }

        let spectrum_fraction = self.display.spectrum_fraction.clamp(0.1, 1.0);
        let spectrum_height = (self.target.height as f32 * spectrum_fraction).round();

        let uniforms = SpectrumUniforms {
            resolution: [self.target.width as f32, self.target.height as f32],
            sample_rate,
            fft_size: (self.num_bins * 2) as f32,
            freq_min,
            freq_max,
            db_min,
            db_max,
            // Mid — bright green outline, darker green fill handled in shader via fill_alpha.
            line_color: [0.72, 0.98, 0.38, 1.0],
            bg_color: [0.031, 0.039, 0.055, 1.0], // panel bg (8,10,14)/255
            // Side — red/orange outline, same fill-alpha strategy.
            side_color: [0.95, 0.30, 0.15, 1.0],
            line_thickness: 1.2,
            slope_db_per_oct: self.display.slope_db_per_oct,
            slope_ref_freq: self.display.slope_ref_freq,
            align_offset_db: self.display.align_offset_db,
            smooth_half_oct_log2: self.display.smooth_half_oct_log2,
            fill_alpha: self.display.fill_alpha,
            spectrum_height,
            history_cols: HISTORY_COLS as f32,
            write_col: self.write_col as f32,
            spectrogram_db_min: self.display.spectrogram_db_min,
            spectrogram_db_max: self.display.spectrogram_db_max,
            log_bins: self.cqt_num_bins as f32,
            sync_mode: if self.sync.enabled { 1.0 } else { 0.0 },
            cqt_fmin_hz: CQT_FMIN_HZ,
            cqt_bins_per_octave: CQT_BINS_PER_OCTAVE as f32,
            weighting_mode: self.display.weighting_mode,
        };
        let uniform_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                &uniforms as *const SpectrumUniforms as *const u8,
                std::mem::size_of::<SpectrumUniforms>(),
            )
        };

        let bindings = &[
            GpuBinding::Bytes {
                binding: 0,
                data: uniform_bytes,
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &self.mid_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &self.side_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &self.history_buf,
                offset: 0,
            },
        ];

        let mut enc = device.create_encoder("spectrum line");
        enc.draw_fullscreen(
            &self.pipeline,
            self.target.gpu_texture(),
            bindings,
            true, // clear
            true, // store
            "spectrum_line",
        );
        enc.commit_and_wait_completed();
    }
}
