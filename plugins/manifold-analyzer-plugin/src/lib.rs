use manifold_analyzer_dsp::Analyzer;
use manifold_analyzer_gui::AnalyzerGuiShared;
use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::num::NonZeroU32;
use std::sync::Arc;

const FFT_SIZE: usize = 4096;
const INITIAL_WINDOW_SIZE: (u32, u32) = (900, 450);

struct ManifoldAnalyzer {
    params: Arc<ManifoldAnalyzerParams>,
    analyzer: Option<Analyzer>,
    mono_scratch: Vec<f32>,
    gui_shared: Arc<AnalyzerGuiShared>,
    egui_state: Arc<EguiState>,
}

#[derive(Params)]
struct ManifoldAnalyzerParams {
    #[persist = "editor-state"]
    editor_state: Arc<EguiState>,
}

impl Default for ManifoldAnalyzer {
    fn default() -> Self {
        let egui_state = EguiState::from_size(INITIAL_WINDOW_SIZE.0, INITIAL_WINDOW_SIZE.1);
        Self {
            params: Arc::new(ManifoldAnalyzerParams {
                editor_state: egui_state.clone(),
            }),
            analyzer: None,
            mono_scratch: Vec::new(),
            gui_shared: Arc::new(AnalyzerGuiShared::new(44100.0, FFT_SIZE)),
            egui_state,
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
        manifold_analyzer_gui::create_editor(self.egui_state.clone(), self.gui_shared.clone())
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.analyzer = Some(Analyzer::new(buffer_config.sample_rate, FFT_SIZE));
        self.mono_scratch = vec![0.0; buffer_config.max_buffer_size as usize];
        self.gui_shared.set_sample_rate(buffer_config.sample_rate);
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
        if self.mono_scratch.len() < num_samples {
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

        // Publish latest spectrum to GUI via try_lock — skip if GUI is reading,
        // no audio-thread allocations.
        if analyzer.push_mono(&self.mono_scratch[..i]) {
            if let Ok(mut guard) = self.gui_shared.spectrum_db.try_lock() {
                guard.copy_from_slice(analyzer.latest_spectrum_db());
            }
        }

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for ManifoldAnalyzer {
    const VST3_CLASS_ID: [u8; 16] = *b"ManifoldAnlyzr01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Analyzer];
}

nih_export_vst3!(ManifoldAnalyzer);
