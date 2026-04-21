use manifold_analyzer_dsp::LoudnessMeter;
use manifold_analyzer_gui::{AnalyzerGuiShared, AnalyzerParams, LoudnessWorker};
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;

// FFT size is no longer used on the audio thread — all frequency
// analysis runs off-thread in the CQT worker. Kept here only to
// satisfy `AnalyzerGuiShared::new` (the field still exists for legacy
// reasons; the worker overrides bin sizing via `resize_cqt_mailboxes`
// on spawn).
const FFT_SIZE: usize = 16384;

struct ManifoldAnalyzer {
    params: Arc<AnalyzerParams>,
    /// Raw L / R kept around for the BS.1770 loudness meter (K-weighting
    /// wants pre-M/S signals) and for pushing into the sample rings
    /// that feed the off-thread CQT worker.
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

        let num_samples = buffer.samples();
        if num_samples == 0 {
            return ProcessStatus::Normal;
        }
        if self.left_scratch.len() < num_samples || self.right_scratch.len() < num_samples {
            return ProcessStatus::Normal;
        }

        // De-interleave L/R (falling back to mono when only one channel
        // is provided). Everything downstream — spectra, spectrogram,
        // correlation — runs off-thread in the CQT worker. The audio
        // thread just ships samples.
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

        // All frequency analysis (Mid/Side curves, L/R balance, per-bin
        // correlation, spectrogram) runs off-thread. Audio thread just
        // pushes raw L / R samples into the lock-free sample rings and
        // lets the CQT worker take it from here.
        self.gui_shared
            .left_sample_ring
            .push(&self.left_scratch[..i]);
        self.gui_shared
            .right_sample_ring
            .push(&self.right_scratch[..i]);

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for ManifoldAnalyzer {
    const VST3_CLASS_ID: [u8; 16] = *b"ManifoldAnlyzr01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Analyzer];
}

nih_export_vst3!(ManifoldAnalyzer);
