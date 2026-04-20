use manifold_analyzer_dsp::{Analyzer, LoudnessMeter};
use manifold_analyzer_gui::{AnalyzerGuiShared, AnalyzerParams};
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;

// Larger FFT + proportionally higher overlap: same ~8.5 ms hop (time
// resolution) but halves bin width to 2.93 Hz for tighter low-end detail.
// rustfft on Apple Silicon eats this easily (~100 µs/frame at 16384).
const FFT_SIZE: usize = 16384;
const OVERLAP_RATIO: f32 = 0.975;
const AVG_TIME_MS: f32 = 200.0;

struct ManifoldAnalyzer {
    params: Arc<AnalyzerParams>,
    mid_analyzer: Option<Analyzer>,
    side_analyzer: Option<Analyzer>,
    mid_scratch: Vec<f32>,
    side_scratch: Vec<f32>,
    left_scratch: Vec<f32>,
    right_scratch: Vec<f32>,
    loudness: Option<LoudnessMeter>,
    last_loudness_reset_epoch: u32,
    gui_shared: Arc<AnalyzerGuiShared>,
}

impl Default for ManifoldAnalyzer {
    fn default() -> Self {
        Self {
            params: Arc::new(AnalyzerParams::new()),
            mid_analyzer: None,
            side_analyzer: None,
            mid_scratch: Vec::new(),
            side_scratch: Vec::new(),
            left_scratch: Vec::new(),
            right_scratch: Vec::new(),
            loudness: None,
            last_loudness_reset_epoch: 0,
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
        let mut mid = Analyzer::new(buffer_config.sample_rate, FFT_SIZE);
        mid.set_overlap_ratio(OVERLAP_RATIO);
        mid.set_averaging_ms(AVG_TIME_MS);
        let mut side = Analyzer::new(buffer_config.sample_rate, FFT_SIZE);
        side.set_overlap_ratio(OVERLAP_RATIO);
        side.set_averaging_ms(AVG_TIME_MS);
        self.mid_analyzer = Some(mid);
        self.side_analyzer = Some(side);
        let max_block = buffer_config.max_buffer_size as usize;
        self.mid_scratch = vec![0.0; max_block];
        self.side_scratch = vec![0.0; max_block];
        self.left_scratch = vec![0.0; max_block];
        self.right_scratch = vec![0.0; max_block];
        self.loudness = Some(LoudnessMeter::new(buffer_config.sample_rate));
        self.last_loudness_reset_epoch = self.gui_shared.loudness_reset_epoch();
        self.gui_shared.set_sample_rate(buffer_config.sample_rate);
        self.gui_shared
            .set_loudness(manifold_analyzer_dsp::LoudnessSnapshot::EMPTY);
        true
    }

    fn reset(&mut self) {
        if let Some(a) = self.mid_analyzer.as_mut() {
            a.reset();
        }
        if let Some(a) = self.side_analyzer.as_mut() {
            a.reset();
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

        let (Some(mid), Some(side)) = (self.mid_analyzer.as_mut(), self.side_analyzer.as_mut())
        else {
            return ProcessStatus::Normal;
        };

        let num_samples = buffer.samples();
        if num_samples == 0 {
            return ProcessStatus::Normal;
        }
        if self.mid_scratch.len() < num_samples || self.side_scratch.len() < num_samples {
            return ProcessStatus::Normal;
        }

        // M/S decode: Mid = (L+R)/2, Side = (L-R)/2. Falls back to mono
        // (r = l → side = 0) when only one channel is provided.
        // Raw L/R kept in parallel scratch buffers for the BS.1770
        // loudness meter (K-weighting requires pre-M/S signals).
        let mut i = 0;
        for channel_samples in buffer.iter_samples() {
            let mut iter = channel_samples.into_iter();
            let l = iter.next().map(|s| *s).unwrap_or(0.0);
            let r = iter.next().map(|s| *s).unwrap_or(l);
            self.mid_scratch[i] = (l + r) * 0.5;
            self.side_scratch[i] = (l - r) * 0.5;
            self.left_scratch[i] = l;
            self.right_scratch[i] = r;
            i += 1;
        }

        // Loudness: honour a pending GUI reset (edge-triggered via
        // epoch counter), then push raw L/R through the BS.1770
        // meter and publish the fresh snapshot.
        if let Some(meter) = self.loudness.as_mut() {
            let epoch = self.gui_shared.loudness_reset_epoch();
            if epoch != self.last_loudness_reset_epoch {
                meter.reset();
                self.last_loudness_reset_epoch = epoch;
            }
            meter.process(&self.left_scratch[..i], &self.right_scratch[..i]);
            self.gui_shared.set_loudness(meter.snapshot());
        }

        // Averaged curves: push samples into the existing Analyzer pair
        // (16 384 BH FFT on audio thread) and publish newest averaged dB
        // per FFT frame via mailbox.
        if mid.push_mono(&self.mid_scratch[..i]) {
            if let Ok(mut guard) = self.gui_shared.mid_db.try_lock() {
                guard.copy_from_slice(mid.latest_spectrum_db());
            }
        }
        if side.push_mono(&self.side_scratch[..i]) {
            if let Ok(mut guard) = self.gui_shared.side_db.try_lock() {
                guard.copy_from_slice(side.latest_spectrum_db());
            }
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
