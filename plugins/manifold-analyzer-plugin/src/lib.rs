use manifold_analyzer_dsp::{LoudnessMeter, StereoAnalyzer};
use manifold_analyzer_gui::{AnalyzerGuiShared, AnalyzerParams, LoudnessWorker};
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;

// Larger FFT + proportionally higher overlap: same ~8.5 ms hop (time
// resolution) but halves bin width to 2.93 Hz for tighter low-end detail.
// rustfft on Apple Silicon eats this easily (~100 µs/frame at 16384).
const FFT_SIZE: usize = 16384;
const OVERLAP_RATIO: f32 = 0.975;
/// SPAN-style peak response: instant attack so transients register on
/// the rising edge, 200 ms release so the curve decays smoothly instead
/// of chattering on every frame.
const ATTACK_MS: f32 = 0.0;
const RELEASE_MS: f32 = 200.0;
/// 500 ms power-domain smoothing on the stereo cross-correlation per
/// bin — steady enough that the colour strip doesn't flicker, fast
/// enough that a polarity flip reads within half a second.
const STEREO_CORR_SMOOTH_MS: f32 = 500.0;

struct ManifoldAnalyzer {
    params: Arc<AnalyzerParams>,
    /// One analyser for everything: two FFTs per hop (L + R), which
    /// then feed Mid/Side/L/R dB curves and the per-bin correlation
    /// used by the centreline strip. Replaces the four mono analysers
    /// plus the standalone stereo cross analyser.
    stereo: Option<StereoAnalyzer>,
    /// Time-domain Mid samples for the CQT spectrogram worker. Kept
    /// around because the spectrogram pulls raw audio from the sample
    /// ring, not spectrum output.
    mid_scratch: Vec<f32>,
    /// Raw L / R kept around for the BS.1770 loudness meter (K-weighting
    /// wants pre-M/S signals) — the stereo analyser consumes the same
    /// slices for its FFTs.
    left_scratch: Vec<f32>,
    right_scratch: Vec<f32>,
    loudness: Option<LoudnessMeter>,
    last_loudness_reset_epoch: u32,
    /// Off-thread BS.1770 integrated / LRA recompute. Spawned once in
    /// `initialize` and joined on `Drop` (plugin teardown). Holds only a
    /// clone of `gui_shared`; the meter feeds it via the shared queue.
    loudness_worker: Option<LoudnessWorker>,
    gui_shared: Arc<AnalyzerGuiShared>,
}

impl Default for ManifoldAnalyzer {
    fn default() -> Self {
        Self {
            params: Arc::new(AnalyzerParams::new()),
            stereo: None,
            mid_scratch: Vec::new(),
            left_scratch: Vec::new(),
            right_scratch: Vec::new(),
            loudness: None,
            last_loudness_reset_epoch: 0,
            loudness_worker: None,
            gui_shared: Arc::new(AnalyzerGuiShared::new(44100.0, FFT_SIZE)),
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
        let mut stereo = StereoAnalyzer::new(buffer_config.sample_rate, FFT_SIZE);
        stereo.set_overlap_ratio(OVERLAP_RATIO);
        stereo.set_attack_release_ms(ATTACK_MS, RELEASE_MS);
        stereo.set_correlation_smoothing_ms(STEREO_CORR_SMOOTH_MS);
        self.stereo = Some(stereo);
        let max_block = buffer_config.max_buffer_size as usize;
        self.mid_scratch = vec![0.0; max_block];
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
        self.gui_shared
            .set_transport(transport.tempo, transport.pos_beats(), transport.playing);

        let Some(stereo) = self.stereo.as_mut() else {
            return ProcessStatus::Normal;
        };

        let num_samples = buffer.samples();
        if num_samples == 0 {
            return ProcessStatus::Normal;
        }
        if self.mid_scratch.len() < num_samples
            || self.left_scratch.len() < num_samples
            || self.right_scratch.len() < num_samples
        {
            return ProcessStatus::Normal;
        }

        // De-interleave L/R (falling back to mono when only one channel
        // is provided). Mid is derived in-line for the CQT sample ring;
        // Side no longer needs its own scratch buffer since M/S is
        // recovered inside the stereo analyser from the L and R FFTs.
        let mut i = 0;
        for channel_samples in buffer.iter_samples() {
            let mut iter = channel_samples.into_iter();
            let l = iter.next().map(|s| *s).unwrap_or(0.0);
            let r = iter.next().map(|s| *s).unwrap_or(l);
            self.left_scratch[i] = l;
            self.right_scratch[i] = r;
            self.mid_scratch[i] = (l + r) * 0.5;
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
                .try_publish_correlation(stereo.latest_correlation());
        }

        // Spectrogram: push raw Mid audio samples into the lock-free
        // sample ring; the GUI thread runs the CQT. No FFT work here for
        // the spectrogram path.
        self.gui_shared
            .mid_sample_ring
            .push(&self.mid_scratch[..i]);

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for ManifoldAnalyzer {
    const VST3_CLASS_ID: [u8; 16] = *b"ManifoldAnlyzr01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Analyzer];
}

nih_export_vst3!(ManifoldAnalyzer);
