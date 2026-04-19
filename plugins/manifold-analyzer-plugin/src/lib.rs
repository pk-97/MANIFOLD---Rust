use manifold_analyzer_dsp::Analyzer;
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;

const FFT_SIZE: usize = 4096;

struct ManifoldAnalyzer {
    params: Arc<ManifoldAnalyzerParams>,
    analyzer: Option<Analyzer>,
    mono_scratch: Vec<f32>,
}

#[derive(Params)]
struct ManifoldAnalyzerParams {}

impl Default for ManifoldAnalyzer {
    fn default() -> Self {
        Self {
            params: Arc::new(ManifoldAnalyzerParams::default()),
            analyzer: None,
            mono_scratch: Vec::new(),
        }
    }
}

impl Default for ManifoldAnalyzerParams {
    fn default() -> Self {
        Self {}
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

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.analyzer = Some(Analyzer::new(buffer_config.sample_rate, FFT_SIZE));
        self.mono_scratch = vec![0.0; buffer_config.max_buffer_size as usize];
        true
    }

    fn reset(&mut self) {
        if let Some(a) = self.analyzer.as_mut() {
            a.reset();
        }
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let Some(analyzer) = self.analyzer.as_mut() else {
            return ProcessStatus::Normal;
        };

        let num_samples = buffer.samples();
        if num_samples == 0 {
            return ProcessStatus::Normal;
        }

        // Sum channels to mono in a pre-allocated scratch buffer, then
        // feed the analyzer once. Audio stays untouched (pass-through).
        if self.mono_scratch.len() < num_samples {
            // Host gave us more samples than advertised in initialize(). Skip this block.
            return ProcessStatus::Normal;
        }

        let mut i = 0;
        for channel_samples in buffer.iter_samples() {
            let mut sum = 0.0f32;
            let mut n = 0usize;
            for sample in channel_samples {
                sum += *sample;
                n += 1;
            }
            self.mono_scratch[i] = if n > 0 { sum / n as f32 } else { 0.0 };
            i += 1;
        }

        analyzer.push_mono(&self.mono_scratch[..i]);

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for ManifoldAnalyzer {
    const VST3_CLASS_ID: [u8; 16] = *b"ManifoldAnlyzr01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Analyzer];
}

nih_export_vst3!(ManifoldAnalyzer);
