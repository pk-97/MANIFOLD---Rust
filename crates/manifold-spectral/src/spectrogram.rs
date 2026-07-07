//! GPU sweep-fill renderer for VQT magnitude columns.
//!
//! Consumes per-hop magnitude columns (from [`crate::cqt::CqtTransform`]) into a
//! CPU ring whose width equals the on-screen pixel width, and paints them into a
//! caller-owned [`GpuTexture`] with one fullscreen pass. Oscilloscope-style: the
//! image is stationary and a write head sweeps left→right, overwriting the
//! oldest column in place and wrapping at the right edge — so 1 column maps to
//! exactly 1 pixel (no horizontal resampling, no scrolling judder). The dB
//! conversion, colour ramp, and log-frequency mapping all live in
//! `shaders/spectrogram.wgsl`. Purpose-built for Manifold's Audio Setup scope —
//! no egui/GL coupling.
//!
//! Race-free without locks: the column history is uploaded into one of three
//! rotating GPU buffers per render, so the CPU never writes a buffer the GPU is
//! still reading from a prior in-flight frame.

use crate::scope::{MAX_ONSET_LANES, ScopeColumn, ScopeOnsets};
use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuEncoder, GpuRenderPipeline, GpuTexture, GpuTextureFormat,
};

const SHADER: &str = include_str!("shaders/spectrogram.wgsl");

/// Rotating GPU buffer count — matches the typical in-flight depth so a buffer
/// is never written while a prior frame's GPU read is outstanding.
const BUFFER_ROTATION: usize = 3;

/// [`ScopeOnsets::LANE_COLORS`] padded (alpha'd) to the shader uniform's fixed
/// [`MAX_ONSET_LANES`] capacity; `onset_count` in the params says how many are
/// live. Colours live in `scope.rs` next to the lane fields they belong to.
const ONSET_LANE_COLORS_PADDED: [[f32; 4]; MAX_ONSET_LANES] = {
    let mut out = [[0.0; 4]; MAX_ONSET_LANES];
    let mut i = 0;
    while i < ScopeOnsets::COUNT {
        let [r, g, b] = ScopeOnsets::LANE_COLORS[i];
        out[i] = [r, g, b, 1.0];
        i += 1;
    }
    out
};

/// Uniform params for the shader. `#[repr(C)]`, 16-byte aligned (two `vec4`-
/// sized rows) per the GPU uniform-alignment convention.
#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    num_bins: u32,
    num_cols: u32,
    write_index: u32,
    _pad0: u32,
    db_min: f32,
    db_max: f32,
    band_lo_y: f32,
    band_hi_y: f32,
    /// Pink tilt slope (dB/octave) and the displayed range's octave span
    /// `log2(fmax/fmin)`; together they give the per-bin weighting in the
    /// shader. Slope 0 disables the tilt.
    tilt_slope: f32,
    freq_log_ratio: f32,
    /// Cursor frequency line position (uv.y, 0 top → 1 bottom); negative hides
    /// it. Drawn as a faint horizontal line so the hover readout has a locator.
    cursor_y: f32,
    /// Which band divider the cursor is over (drag affordance): `0` = low/mid,
    /// `1` = mid/high, `< 0` = none. The shader brightens that line's grip.
    hovered_divider: f32,
    /// Overlay-scalar layout, from the one definition in `scope.rs` —
    /// [`ScopeColumn::STRIDE`], [`ScopeColumn::ONSET_BASE`],
    /// [`ScopeOnsets::COUNT`]. The shader indexes `col_scalars` with these, so
    /// the WGSL carries no layout literals of its own.
    scalar_stride: u32,
    onset_base: u32,
    onset_count: u32,
    _pad1: u32,
    /// Onset lane colours (bottom-up), [`ONSET_LANE_COLORS_PADDED`]. Fixed
    /// capacity; only the first `onset_count` entries are live.
    onset_colors: [[f32; 4]; MAX_ONSET_LANES],
}

/// Sweep-fill spectrogram renderer. One per visible scope; sized to the scope's
/// pixel width so each column owns one pixel column.
pub struct Spectrogram {
    num_bins: usize,
    /// Column count = on-screen pixel width. One pixel column per ring column.
    num_cols: usize,
    /// `num_cols * num_bins` magnitudes; a ring of columns. `head` is the next
    /// column to overwrite (also the shader's `write_index` — the sweep line).
    ring: Vec<f32>,
    /// Parallel per-column overlay scalars (one [`ScopeColumn`] per column —
    /// centroid traces + onset tick lanes; see `scope.rs` for the layout). Same
    /// ring layout as `ring`, written at `head`; uploaded as raw bytes.
    col_scalars: Vec<ScopeColumn>,
    head: usize,
    bufs: Vec<GpuBuffer>,
    /// Rotating GPU buffers for `col_scalars`, mirroring `bufs`.
    scalar_bufs: Vec<GpuBuffer>,
    buf_frame: usize,
    pipeline: GpuRenderPipeline,
    /// Colour-ramp bottom (dB) — the FIXED display/amplitude contrast, not the audio
    /// floor. The audio floor zeros the column upstream (a zeroed bin paints black),
    /// so the floor is a gate; this is just how the surviving magnitudes map to
    /// colour. Keeping it fixed means moving the floor never recolours the picture.
    db_min: f32,
    db_max: f32,
    /// Pink-tilt slope (dB/oct) — the one [`SpectrogramConfig::tilt_slope`], passed
    /// at construction so the shader tilts by the same slope the detector does.
    tilt_slope: f32,
}

impl Spectrogram {
    /// Create a renderer for `num_bins`-tall columns across `num_cols` pixel
    /// columns (pass the scope's physical-pixel width). `color_format` must
    /// match the texture passed to [`render`](Self::render). `db_min`/`db_max`
    /// set the magnitude→colour dynamic range (e.g. −59 dB → 0 dB) — fixed contrast,
    /// not the audio floor. `tilt_slope` is [`SpectrogramConfig::tilt_slope`] (dB/oct).
    pub fn new(
        device: &GpuDevice,
        num_bins: usize,
        num_cols: usize,
        color_format: GpuTextureFormat,
        db_min: f32,
        db_max: f32,
        tilt_slope: f32,
    ) -> Self {
        let elems = num_bins * num_cols;
        let bytes = (elems * std::mem::size_of::<f32>()) as u64;
        let bufs = (0..BUFFER_ROTATION)
            .map(|_| {
                let b = device.create_buffer_shared(bytes.max(4));
                b.zero_fill();
                b
            })
            .collect();
        // Overlay scalar ring: one ScopeColumn per column.
        let scalar_bytes = (num_cols * std::mem::size_of::<ScopeColumn>()) as u64;
        let scalar_bufs = (0..BUFFER_ROTATION)
            .map(|_| {
                let b = device.create_buffer_shared(scalar_bytes.max(4));
                b.zero_fill();
                b
            })
            .collect();
        let pipeline = device.create_render_pipeline(
            SHADER,
            "vs_main",
            "fs_main",
            color_format,
            None,
            "Spectrogram",
        );
        Self {
            num_bins,
            num_cols,
            ring: vec![0.0; elems],
            col_scalars: vec![ScopeColumn::EMPTY; num_cols],
            head: 0,
            bufs,
            scalar_bufs,
            buf_frame: 0,
            pipeline,
            db_min,
            db_max,
            tilt_slope,
        }
    }

    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    /// On-screen column count this renderer was built for (== pixel width).
    pub fn num_cols(&self) -> usize {
        self.num_cols
    }

    /// Raw dB at normalised scope position (`ux` 0→1 left→right, `uy` 0→1
    /// top→bottom) — the same column/bin the shader paints, with the same
    /// power-domain 2-tap bin interpolation but WITHOUT the pink tilt, so the
    /// hover readout shows true level. Returns a deep floor for an empty cell.
    pub fn sample_db(&self, ux: f32, uy: f32) -> f32 {
        if self.num_bins == 0 || self.num_cols == 0 {
            return self.db_min;
        }
        let col = ((ux.clamp(0.0, 1.0) * self.num_cols as f32) as usize).min(self.num_cols - 1);
        let top = (self.num_bins - 1) as f32;
        let log_bin_f = ((1.0 - uy).clamp(0.0, 1.0) * top).clamp(0.0, top);
        let lo = log_bin_f.floor() as usize;
        let hi = (lo + 1).min(self.num_bins - 1);
        let frac = log_bin_f - lo as f32;
        let base = col * self.num_bins;
        let m_lo = self.ring[base + lo];
        let m_hi = self.ring[base + hi];
        let power = m_lo * m_lo * (1.0 - frac) + m_hi * m_hi * frac;
        10.0 * (power + 1e-18).log10()
    }

    /// Pink-weighted dB at the cursor: [`sample_db`](Self::sample_db) plus the
    /// exact tilt the shader applies to the colour, so the hover readout matches
    /// what's drawn under the cursor. `freq_log_ratio` is `log2(fmax/fmin)` of
    /// the displayed range (0 → no tilt, raw level). Mirrors the shader's
    /// `tilt = slope · freq_log_ratio · (0.5 − uv.y)`.
    pub fn sample_db_weighted(&self, ux: f32, uy: f32, freq_log_ratio: f32) -> f32 {
        let raw = self.sample_db(ux, uy);
        if freq_log_ratio > 0.0 {
            raw + self.tilt_slope * freq_log_ratio * (0.5 - uy.clamp(0.0, 1.0))
        } else {
            raw
        }
    }

    /// Append one magnitude column at the sweep head (advancing it). Extra
    /// values past `num_bins` are ignored; a short column zero-pads the
    /// remainder. The head wraps at the right edge back to the left.
    ///
    /// `scalars` is the column's overlay record (centroid traces + onset tick
    /// lanes — see `scope.rs`), stored in the parallel scalar ring at the same
    /// slot, so it scrolls with the waterfall.
    pub fn push_column(&mut self, magnitudes: &[f32], scalars: ScopeColumn) {
        let base = self.head * self.num_bins;
        let dst = &mut self.ring[base..base + self.num_bins];
        let n = magnitudes.len().min(self.num_bins);
        dst[..n].copy_from_slice(&magnitudes[..n]);
        for v in &mut dst[n..] {
            *v = 0.0;
        }
        self.col_scalars[self.head] = scalars;
        self.head = (self.head + 1) % self.num_cols;
    }

    /// Render the current history into `target` (cleared first). One fullscreen
    /// pass sampling the rotating buffer this frame writes. `band_ys` are two
    /// band-divider positions, normalised 0..1 from the bottom (low freq);
    /// negative disables a line. `freq_log_ratio` is `log2(fmax/fmin)` of the
    /// displayed range — the octave span the pink tilt is centred and scaled
    /// over; pass `0.0` to disable the tilt (Flat look). `cursor_y` draws a
    /// faint horizontal locator line (uv.y, 0 top → 1 bottom); negative hides it.
    /// `hovered_divider` brightens a divider's grip handle (`0` low/mid, `1`
    /// mid/high, `< 0` none) to signal it's draggable.
    pub fn render(
        &mut self,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        band_ys: [f32; 2],
        freq_log_ratio: f32,
        cursor_y: f32,
        hovered_divider: f32,
    ) {
        let slot = self.buf_frame % BUFFER_ROTATION;
        let buf = &self.bufs[slot];
        let scalar_buf = &self.scalar_bufs[slot];
        self.buf_frame += 1;

        // SAFETY: shared buffers; `ring`/`col_scalars` are exactly each buffer's
        // length and aren't read by an in-flight frame (rotation guarantees it).
        unsafe {
            let bytes = std::slice::from_raw_parts(
                self.ring.as_ptr() as *const u8,
                std::mem::size_of_val(self.ring.as_slice()),
            );
            buf.write(0, bytes);
            let scalar_bytes = std::slice::from_raw_parts(
                self.col_scalars.as_ptr() as *const u8,
                std::mem::size_of_val(self.col_scalars.as_slice()),
            );
            scalar_buf.write(0, scalar_bytes);
        }

        let params = Params {
            num_bins: self.num_bins as u32,
            num_cols: self.num_cols as u32,
            write_index: self.head as u32,
            _pad0: 0,
            db_min: self.db_min,
            db_max: self.db_max,
            band_lo_y: band_ys[0],
            band_hi_y: band_ys[1],
            tilt_slope: if freq_log_ratio > 0.0 { self.tilt_slope } else { 0.0 },
            freq_log_ratio,
            cursor_y,
            hovered_divider,
            scalar_stride: ScopeColumn::STRIDE as u32,
            onset_base: ScopeColumn::ONSET_BASE as u32,
            onset_count: ScopeOnsets::COUNT as u32,
            _pad1: 0,
            onset_colors: ONSET_LANE_COLORS_PADDED,
        };
        // SAFETY: `Params` is `#[repr(C)]` plain-old-data.
        let param_bytes = unsafe {
            std::slice::from_raw_parts(
                &params as *const Params as *const u8,
                std::mem::size_of::<Params>(),
            )
        };

        encoder.draw_fullscreen(
            &self.pipeline,
            target,
            &[
                GpuBinding::Buffer { binding: 0, buffer: buf, offset: 0 },
                GpuBinding::Bytes { binding: 1, data: param_bytes },
                GpuBinding::Buffer { binding: 2, buffer: scalar_buf, offset: 0 },
            ],
            true, // clear
            true, // store
            "Spectrogram",
        );
    }
}

/// GPU-readback proof of the onset-lane path — the real Metal render, not the
/// mod_harness CPU port. Behind `gpu-proofs` (real device; off by default,
/// mirroring manifold-renderer's convention).
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::Spectrogram;
    use crate::scope::{ScopeColumn, ScopeOnsets};
    use manifold_gpu::{
        GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    /// Render 64 silent columns where exactly one column fires each onset
    /// lane, read the pixels back, and assert every lane draws at its own
    /// bottom-up slot in its own colour — the uniform-carried stride, onset
    /// base/count, and colour array doing their job on the actual GPU.
    #[test]
    fn onset_lanes_draw_at_their_slots_in_their_colors() {
        const COLS: u32 = 64;
        const H: u32 = 512;
        let device = GpuDevice::new();
        let mut spec = Spectrogram::new(
            &device,
            64,
            COLS as usize,
            GpuTextureFormat::Rgba8Unorm,
            -59.0,
            0.0,
            0.0,
        );

        // One firing column per lane, in lane (field) order bottom-up.
        let fired_cols: [usize; ScopeOnsets::COUNT] = [10, 20, 30, 40];
        let silence = vec![0.0f32; 64];
        for c in 0..COLS as usize {
            let mut onsets = ScopeOnsets::default();
            let lanes = [&mut onsets.kick, &mut onsets.low, &mut onsets.mid, &mut onsets.high];
            for (lane, col) in lanes.into_iter().zip(fired_cols) {
                if c == col {
                    *lane = 1.0;
                }
            }
            spec.push_column(&silence, ScopeColumn { centroids: [-1.0; 4], onsets });
        }

        let target = device.create_texture(&GpuTextureDesc {
            width: COLS,
            height: H,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::COPY_SRC,
            label: "scope-gpu-proof",
            mip_levels: 1,
        });
        let mut enc = device.create_encoder("scope-gpu-proof");
        // Dividers, tilt, cursor, hover all off — only background + lanes.
        spec.render(&mut enc, &target, [-1.0, -1.0], 0.0, -1.0, -1.0);
        enc.commit_and_wait_completed();

        let bytes_per_row = COLS * 4;
        let readback = device.create_buffer_shared(u64::from(H * bytes_per_row));
        let mut rb = device.create_encoder("scope-gpu-proof-readback");
        rb.copy_texture_to_buffer(&target, &readback, COLS, H, bytes_per_row);
        rb.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared buffer maps");
        let px = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), (COLS * H * 4) as usize) };
        let at = |x: u32, y: u32| -> [u8; 3] {
            let i = ((y * COLS + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };

        // Lane i spans uv.y in (1 - 0.014·(i+1), 1 - 0.014·i]: sample its
        // vertical centre. Expected colour = LANE_COLORS[i] mixed 0.85 over
        // the silent background (colormap(0) = black), so ≈ 0.85·colour·255.
        let lane_center_y = |i: u32| H - 1 - ((0.014 * (i as f32 + 0.5)) * H as f32) as u32;
        for (i, (col, [r, g, b])) in fired_cols.iter().zip(ScopeOnsets::LANE_COLORS).enumerate() {
            let y = lane_center_y(i as u32);
            let got = at(*col as u32, y);
            let want = [r, g, b].map(|c| (c * 0.85 * 255.0) as i32);
            for (ch, (g8, w)) in got.iter().zip(want).enumerate() {
                let diff = (i32::from(*g8) - w).abs();
                assert!(
                    diff <= 12,
                    "lane {i} col {col} channel {ch}: got {got:?}, want ~{want:?}"
                );
            }
            // The same column one lane up must NOT be lit (no bleed).
            let above = at(*col as u32, lane_center_y(i as u32 + 1));
            assert!(
                above.iter().all(|&c| c < 25),
                "lane {i} col {col} bled into the lane above: {above:?}"
            );
        }
        // A column that never fired stays background-dark across every lane.
        for i in 0..ScopeOnsets::COUNT as u32 {
            let got = at(50, lane_center_y(i));
            assert!(got.iter().all(|&c| c < 25), "unfired col 50 lane {i} lit: {got:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Params, SHADER};

    /// The spectrogram WGSL must parse and pass naga's validator — catches a
    /// malformed binding/index/type before it reaches the GPU at runtime (the
    /// shader is otherwise only compiled when the Audio Setup scope opens).
    /// Also asserts the WGSL `Params` size equals the Rust [`Params`] size —
    /// the two are maintained by hand, and a drift garbles every uniform after
    /// the mismatch at runtime with no compile-time signal.
    #[test]
    fn shader_parses_and_validates() {
        let module =
            naga::front::wgsl::parse_str(SHADER).expect("spectrogram WGSL should parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("spectrogram WGSL should validate");

        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).expect("layout should resolve");
        let (handle, _) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("Params"))
            .expect("shader should declare a Params struct");
        assert_eq!(
            layouter[handle].size as usize,
            std::mem::size_of::<Params>(),
            "Rust Params and WGSL Params must have identical sizes"
        );
    }
}
