use manifold_analyzer_dsp::{LoudnessMeter, StereoAnalyzer};
use manifold_analyzer_gui::{AnalyzerGuiShared, AnalyzerParams, LoudnessWorker};
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;

/// FFT size used before `initialize` runs and the param-driven size
/// kicks in. Matches the default `FftSize::K4`.
const DEFAULT_FFT_SIZE: usize = 4096;
/// Overlap ratio for the StereoAnalyser's sliding window. 0.9 keeps the
/// audio-publish rate at ~117 Hz for the default FFT=4096 @ 48 kHz — well
/// above 60 Hz display refresh so the MS curve + L/R column update every
/// frame — while running the FFT ~4× less than the old 0.975 value, which
/// re-ran at ~470 Hz for no visible benefit on any monitor.
const OVERLAP_RATIO: f32 = 0.9;
/// Peak-meter style response: instant attack so transients register on
/// the rising edge, 200 ms release so the curve decays smoothly instead
/// of chattering on every frame.
const ATTACK_MS: f32 = 0.0;
const RELEASE_MS: f32 = 200.0;
/// 500 ms power-domain smoothing on the stereo cross-correlation per
/// bin — steady enough that the colour strip doesn't flicker, fast
/// enough that a polarity flip reads within half a second.
const STEREO_CORR_SMOOTH_MS: f32 = 500.0;
/// 150 ms symmetric smoothing on the per-bin L and R magnitudes
/// specifically feeding the L/R balance line. Faster than correlation
/// so balance feels live, slower than the instant-attack asym EMA so
/// it doesn't chatter.
const STEREO_BALANCE_SMOOTH_MS: f32 = 150.0;

struct ManifoldAnalyzer {
    params: Arc<AnalyzerParams>,
    /// One analyser for everything: two FFTs per hop (L + R), which
    /// then feed Mid/Side/L/R dB curves and the per-bin correlation
    /// used by the centreline strip. Replaces the four mono analysers
    /// plus the standalone stereo cross analyser.
    stereo: Option<StereoAnalyzer>,
    /// FFT length the current `stereo` was built with. If the GUI's
    /// `fft_size` param flips mid-session we rebuild once and resize
    /// the shared mailboxes to match. Avoids re-creating the analyser
    /// (and the ~100 ms rustfft plan bake) every process call.
    current_fft_size: usize,
    /// Raw L / R for the BS.1770 loudness meter (K-weighting wants the
    /// pre-M/S signals), the stereo analyser's two FFTs, and the
    /// spectrogram's L/R sample rings. The CQT worker derives Mid/Side
    /// from the L/R rings on demand based on the GUI's spectrogram
    /// source mode — no per-channel Mid scratch is computed here.
    left_scratch: Vec<f32>,
    right_scratch: Vec<f32>,
    loudness: Option<LoudnessMeter>,
    last_loudness_reset_epoch: u32,
    /// Off-thread BS.1770 integrated / LRA recompute. Spawned once in
    /// `initialize` and joined on `Drop` (plugin teardown). Holds only a
    /// clone of `gui_shared`; the meter feeds it via the shared queue.
    loudness_worker: Option<LoudnessWorker>,
    gui_shared: Arc<AnalyzerGuiShared>,
    /// Running total of audio samples successfully pushed into the
    /// shared L/R sample rings since plugin instantiation. Published
    /// alongside the host beat position so the spectrogram worker can
    /// derive the host beat for any sample it later drains — the basis
    /// for sample-accurate Beat-Sync placement.
    total_pushed_samples: u64,
}

impl Default for ManifoldAnalyzer {
    fn default() -> Self {
        Self {
            params: Arc::new(AnalyzerParams::new()),
            stereo: None,
            current_fft_size: DEFAULT_FFT_SIZE,
            left_scratch: Vec::new(),
            right_scratch: Vec::new(),
            loudness: None,
            last_loudness_reset_epoch: 0,
            loudness_worker: None,
            gui_shared: Arc::new(AnalyzerGuiShared::new(44100.0, DEFAULT_FFT_SIZE)),
            total_pushed_samples: 0,
        }
    }
}

impl Plugin for ManifoldAnalyzer {
    const NAME: &'static str = "Manifold Analyzer";
    const VENDOR: &'static str = "Latent Space";
    const URL: &'static str = "https://latentspace.studio";
    const EMAIL: &'static str = "peter.kiemann97@gmail.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::None;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        manifold_analyzer_gui::create_editor(self.params.clone(), self.gui_shared.clone())
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        let fft_size = self.params.fft_size.value().samples();
        self.current_fft_size = fft_size;
        self.gui_shared.resize_stereo_mailboxes(fft_size);
        let mut stereo = StereoAnalyzer::new(buffer_config.sample_rate, fft_size);
        stereo.set_overlap_ratio(OVERLAP_RATIO);
        stereo.set_attack_release_ms(ATTACK_MS, RELEASE_MS);
        stereo.set_correlation_smoothing_ms(STEREO_CORR_SMOOTH_MS);
        stereo.set_balance_smoothing_ms(STEREO_BALANCE_SMOOTH_MS);
        self.stereo = Some(stereo);
        let max_block = buffer_config.max_buffer_size as usize;
        self.left_scratch = vec![0.0; max_block];
        self.right_scratch = vec![0.0; max_block];
        let mut meter = LoudnessMeter::new(buffer_config.sample_rate);
        // Attach the shared block queue so closed-block z values flow to
        // the worker thread instead of the audio thread running O(N)
        // gating in-line. Spawn the worker on first initialize — it
        // survives further initialize/reset calls for this plugin
        // instance and joins on plugin drop.
        meter.attach_block_sink(self.gui_shared.loudness_block_queue.clone());
        self.loudness = Some(meter);
        self.last_loudness_reset_epoch = self.gui_shared.loudness_reset_epoch();
        if self.loudness_worker.is_none() {
            self.loudness_worker = Some(LoudnessWorker::spawn(self.gui_shared.clone()));
        }
        self.gui_shared.set_sample_rate(buffer_config.sample_rate);
        self.gui_shared
            .set_loudness(manifold_analyzer_dsp::LoudnessSnapshot::EMPTY);
        true
    }

    fn reset(&mut self) {
        if let Some(s) = self.stereo.as_mut() {
            s.reset();
        }
        if let Some(m) = self.loudness.as_mut() {
            m.reset();
            self.gui_shared.set_loudness(m.snapshot());
        }
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let transport = context.transport();
        // Publish (bpm, beat, pushed) BEFORE pushing this buffer's
        // audio. The published `pushed` is the index in the worker's
        // drain stream of the *first* sample we're about to enqueue —
        // which is the sample that corresponds to `beat_pos`. The
        // worker reads all three coherently and derives the host beat
        // for any later sample it processes.
        self.gui_shared.set_transport(
            transport.tempo,
            transport.pos_beats(),
            transport.playing,
            self.total_pushed_samples,
        );

        // Honour a user-driven FFT-size change. Rebuild once, resize
        // the shared mailboxes, and carry on. Uses the current sample
        // rate from `gui_shared` since `BufferConfig` isn't available
        // here. Brief audio-thread glitch is acceptable — this fires
        // only when the user changes the dropdown.
        let desired_fft = self.params.fft_size.value().samples();
        if desired_fft != self.current_fft_size {
            let sr = self.gui_shared.sample_rate();
            let mut stereo = StereoAnalyzer::new(sr, desired_fft);
            stereo.set_overlap_ratio(OVERLAP_RATIO);
            stereo.set_attack_release_ms(ATTACK_MS, RELEASE_MS);
            stereo.set_correlation_smoothing_ms(STEREO_CORR_SMOOTH_MS);
        stereo.set_balance_smoothing_ms(STEREO_BALANCE_SMOOTH_MS);
            self.gui_shared.resize_stereo_mailboxes(desired_fft);
            self.stereo = Some(stereo);
            self.current_fft_size = desired_fft;
        }

        let Some(stereo) = self.stereo.as_mut() else {
            return ProcessStatus::Normal;
        };

        let num_samples = buffer.samples();
        if num_samples == 0 {
            return ProcessStatus::Normal;
        }
        if self.left_scratch.len() < num_samples || self.right_scratch.len() < num_samples {
            return ProcessStatus::Normal;
        }

        // De-interleave L/R (falling back to mono when only one channel
        // is provided). Mid/Side are recovered downstream — the stereo
        // analyser derives M/S from its L and R FFTs, and the CQT worker
        // mixes M or S sample-by-sample from these L/R rings on demand.
        let mut i = 0;
        for channel_samples in buffer.iter_samples() {
            let mut iter = channel_samples.into_iter();
            let l = iter.next().map(|s| *s).unwrap_or(0.0);
            let r = iter.next().map(|s| *s).unwrap_or(l);
            self.left_scratch[i] = l;
            self.right_scratch[i] = r;
            i += 1;
        }

        // Loudness: honour a pending GUI reset (edge-triggered via
        // epoch counter), inject the worker's latest integrated value
        // so the meter's in-line DR/PLR derivation stays current, push
        // L/R through the BS.1770 meter, and publish only the fast-
        // moving fields — the worker owns integrated + LRA.
        if let Some(meter) = self.loudness.as_mut() {
            let epoch = self.gui_shared.loudness_reset_epoch();
            if epoch != self.last_loudness_reset_epoch {
                meter.reset();
                self.last_loudness_reset_epoch = epoch;
            }
            meter.set_external_integrated_lufs(self.gui_shared.integrated_lufs());
            meter.process(&self.left_scratch[..i], &self.right_scratch[..i]);
            self.gui_shared.set_fast_loudness(meter.snapshot());
        }

        // Single stereo analyser: two FFTs per hop (L + R), then
        // Mid/Side/L/R averaged magnitude curves plus per-bin
        // correlation all derived in one pass. Publish all five mailboxes
        // on a completed frame.
        if stereo.push_stereo(&self.left_scratch[..i], &self.right_scratch[..i]) {
            self.gui_shared
                .try_publish_mid_db(stereo.latest_mid_db());
            self.gui_shared
                .try_publish_side_db(stereo.latest_side_db());
            self.gui_shared
                .try_publish_left_db(stereo.latest_left_db());
            self.gui_shared
                .try_publish_right_db(stereo.latest_right_db());
            self.gui_shared
                .try_publish_left_balance_db(stereo.latest_left_balance_db());
            self.gui_shared
                .try_publish_right_balance_db(stereo.latest_right_balance_db());
            self.gui_shared
                .try_publish_correlation(stereo.latest_correlation());
        }

        // Spectrogram: push raw L and R audio into the lock-free sample
        // rings; the worker derives M/S (or runs both channels for L+R
        // mode) and runs the CQT. No FFT work here for the spectrogram
        // path. Both rings are pushed in lockstep so the worker can
        // assume they always advance together. `total_pushed_samples`
        // tracks what *actually* landed (the rings drop on overflow);
        // the worker's drain counter stays aligned because dropped
        // samples never enter the ring.
        let pushed = self
            .gui_shared
            .left_sample_ring
            .push(&self.left_scratch[..i]);
        self.gui_shared
            .right_sample_ring
            .push(&self.right_scratch[..i]);
        self.total_pushed_samples = self.total_pushed_samples.saturating_add(pushed as u64);

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for ManifoldAnalyzer {
    const VST3_CLASS_ID: [u8; 16] = *b"ManifoldAnlyzr01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Analyzer];
}

nih_export_vst3!(ManifoldAnalyzer);
