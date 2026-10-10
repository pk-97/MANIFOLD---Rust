use manifold_analyzer_dsp::{LoudnessMeter, StereoAnalyzer};
use manifold_analyzer_gui::{AnalyzerGuiShared, AnalyzerParams, LoudnessWorker, StereoSample};
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;

/// FFT size used before `initialize` runs and the param-driven size
/// kicks in. Matches the default `FftSize::K4`.
const DEFAULT_FFT_SIZE: usize = 4096;
const FFT_SIZES: [usize; 5] = [2048, 4096, 8192, 16384, 32768];
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
    stereo: Vec<StereoAnalyzer>,
    /// Active analyzer, prepared during initialization.
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
    /// Absolute input position, including frames omitted while closed or full.
    total_pushed_samples: u64,
    visual_was_open: bool,
}

impl Default for ManifoldAnalyzer {
    fn default() -> Self {
        Self {
            params: Arc::new(AnalyzerParams::new()),
            stereo: Vec::new(),
            current_fft_size: DEFAULT_FFT_SIZE,
            left_scratch: Vec::new(),
            right_scratch: Vec::new(),
            loudness: None,
            last_loudness_reset_epoch: 0,
            loudness_worker: None,
            gui_shared: Arc::new(AnalyzerGuiShared::new(44100.0, DEFAULT_FFT_SIZE)),
            total_pushed_samples: 0,
            visual_was_open: false,
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
        self.stereo = FFT_SIZES.iter().map(|&size| {
            let mut stereo = StereoAnalyzer::new(buffer_config.sample_rate, size);
            stereo.set_overlap_ratio(OVERLAP_RATIO);
            stereo.set_attack_release_ms(ATTACK_MS, RELEASE_MS);
            stereo.set_correlation_smoothing_ms(STEREO_CORR_SMOOTH_MS);
            stereo.set_balance_smoothing_ms(STEREO_BALANCE_SMOOTH_MS);
            stereo
        }).collect();
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
        self.gui_shared.request_loudness_reset();
        self.last_loudness_reset_epoch = self.gui_shared.loudness_reset_epoch();
        self.loudness.as_mut().unwrap().set_generation(self.last_loudness_reset_epoch);
        self.gui_shared.advance_audio_generation();
        if self.loudness_worker.is_none() {
            self.loudness_worker = Some(LoudnessWorker::spawn(self.gui_shared.clone()));
        }
        self.gui_shared.set_sample_rate(buffer_config.sample_rate);
        self.gui_shared
            .set_loudness(manifold_analyzer_dsp::LoudnessSnapshot::EMPTY);
        true
    }

    fn reset(&mut self) {
        for s in &mut self.stereo { s.reset(); }
        self.gui_shared.request_loudness_reset();
        self.gui_shared.advance_audio_generation();
        self.last_loudness_reset_epoch = self.gui_shared.loudness_reset_epoch();
        if let Some(m) = self.loudness.as_mut() {
            m.reset();
            m.set_generation(self.last_loudness_reset_epoch);
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
        // UI transport. Queued samples carry their own anchor.
        self.gui_shared.set_transport(
            transport.tempo,
            transport.pos_beats(),
            transport.playing,
            self.total_pushed_samples,
        );

        self.process_audio(buffer, self.params.fft_size.value().samples(),
            self.params.editor_state.is_open(), transport.tempo, transport.pos_beats())
    }
}

impl ManifoldAnalyzer {
    /// The audio callback body, independent of host callbacks for headless proofs.
    fn process_audio(&mut self, buffer: &mut Buffer, desired_fft: usize,
        visual_open: bool, bpm: Option<f64>, beat: Option<f64>) -> ProcessStatus {
        let index = FFT_SIZES.iter().position(|&size| size == desired_fft).unwrap_or(1);
        let Some(stereo) = self.stereo.get_mut(index) else { return ProcessStatus::Normal; };
        if desired_fft != self.current_fft_size || (visual_open && !self.visual_was_open) {
            stereo.reset();
            self.gui_shared.resize_stereo_mailboxes(desired_fft);
            self.current_fft_size = desired_fft;
        }
        self.visual_was_open = visual_open;

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
                meter.set_generation(epoch);
                self.last_loudness_reset_epoch = epoch;
            }
            meter.set_external_integrated_lufs(self.gui_shared.integrated_lufs());
            meter.process(&self.left_scratch[..i], &self.right_scratch[..i]);
            self.gui_shared.set_fast_loudness(meter.snapshot());
        }

        // Single stereo analyser: two FFTs per hop (L + R), then
        // Mid/Side curves, streaming median plus per-bin
        // correlation all derived in one pass. Publish the active mailboxes
        // on a completed frame.
        if visual_open && stereo.push_stereo(&self.left_scratch[..i], &self.right_scratch[..i]) {
            self.gui_shared
                .try_publish_mid_db(stereo.latest_mid_db());
            self.gui_shared
                .try_publish_side_db(stereo.latest_side_db());
            self.gui_shared.try_publish_median_db(stereo.latest_median_db());
            self.gui_shared
                .try_publish_left_balance_db(stereo.latest_left_balance_db());
            self.gui_shared
                .try_publish_right_balance_db(stereo.latest_right_balance_db());
            self.gui_shared
                .try_publish_correlation(stereo.latest_correlation());
        }

        if visual_open {
            let generation = self.gui_shared.audio_generation();
            let sample_rate = self.gui_shared.sample_rate();
            let bpm = bpm.unwrap_or(f64::NAN);
            let beat = beat.unwrap_or(f64::NAN);
            for k in 0..i {
                if !self.gui_shared.sample_ring.push(StereoSample {
                    left: self.left_scratch[k], right: self.right_scratch[k],
                    index: self.total_pushed_samples + k as u64, generation, sample_rate,
                    beat: beat + k as f64 * bpm / (60.0 * sample_rate as f64), bpm,
                }) { break; }
            }
        }
        // Advance across rejected/hidden samples too, exposing discontinuities.
        self.total_pushed_samples = self.total_pushed_samples.saturating_add(i as u64);

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for ManifoldAnalyzer {
    const VST3_CLASS_ID: [u8; 16] = *b"ManifoldAnlyzr01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Analyzer];
}

nih_export_vst3!(ManifoldAnalyzer);

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    thread_local! { static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) }; }
    struct TrackedAllocator;
    #[global_allocator] static ALLOCATOR: TrackedAllocator = TrackedAllocator;
    fn record() { let _ = ALLOCATIONS.try_with(|cell| { if let Some(n) = cell.get() { cell.set(Some(n + 1)); } }); }
    unsafe impl GlobalAlloc for TrackedAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 { record(); unsafe { System.alloc(layout) } }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { record(); unsafe { System.dealloc(ptr, layout) } }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 { record(); unsafe { System.realloc(ptr, layout, size) } }
    }
    struct Init;
    impl InitContext<ManifoldAnalyzer> for Init {
        fn plugin_api(&self) -> PluginApi { PluginApi::Vst3 }
        fn execute(&self, _: ()) {}
        fn set_latency_samples(&self, _: u32) {}
        fn set_current_voice_capacity(&self, _: u32) {}
    }
    fn initialize(plugin: &mut ManifoldAnalyzer, rate: f32) {
        assert!(plugin.initialize(&ManifoldAnalyzer::AUDIO_IO_LAYOUTS[0], &BufferConfig {
            sample_rate: rate, min_buffer_size: Some(32), max_buffer_size: 512,
            process_mode: ProcessMode::Realtime,
        }, &mut Init));
    }
    #[test]
    fn callback_fft_switches_are_allocation_free_and_audio_is_unchanged() {
        let mut plugin = ManifoldAnalyzer::default(); initialize(&mut plugin, 48000.0);
        let left = [0.125;512]; let right = [-0.25;512];
        for &fft in &FFT_SIZES {
            let mut l=left; let mut r=right; let mut buffer=Buffer::default();
            unsafe { buffer.set_slices(512, |v| { v.push(&mut l);v.push(&mut r); }); }
            ALLOCATIONS.with(|a| a.set(Some(0)));
            for _ in 0..8 { plugin.process_audio(&mut buffer, fft, true, Some(120.0), Some(0.0)); }
            let count=ALLOCATIONS.with(|a| a.replace(None)).unwrap();
            assert_eq!(count,0,"alloc/free during FFT switch {fft}");
            drop(buffer); assert_eq!(l,left);assert_eq!(r,right);
        }
    }
    #[test]
    fn spectrum_mailbox_rejects_old_resolution_without_reallocating() {
        let shared = AnalyzerGuiShared::new(48000.0, 2048);
        let small = vec![-10.0; 1025];
        let large = vec![-20.0; 16385];
        let mut display = vec![0.0; 16385];
        ALLOCATIONS.with(|a| a.set(Some(0)));
        assert!(shared.try_publish_mid_db(&small));
        assert!(!shared.try_read_mid_db(&mut display));
        assert!(shared.try_publish_mid_db(&large));
        let allocations = ALLOCATIONS.with(|a| a.replace(None)).unwrap();
        assert_eq!(allocations, 0);
        assert!(display.iter().all(|&v| v == -120.0));
        assert!(shared.try_read_mid_db(&mut display));
        assert_eq!(display, large);
    }

    #[test]
    fn closed_editor_keeps_loudness_and_rate_reset_starts_new_generation() {
        let mut plugin=ManifoldAnalyzer::default();initialize(&mut plugin,48000.0);
        let mut l=[0.25;512];let mut r=l;let mut buffer=Buffer::default();
        unsafe {buffer.set_slices(512,|v|{v.push(&mut l);v.push(&mut r);});}
        plugin.process_audio(&mut buffer,4096,false,None,None);
        assert!(plugin.gui_shared.loudness().elapsed_secs>0.0);
        let mut samples=Vec::new();plugin.gui_shared.sample_ring.drain_into(&mut samples);assert!(samples.is_empty());
        let old=plugin.gui_shared.loudness_reset_epoch();
        plugin.reset();assert_ne!(old,plugin.gui_shared.loudness_reset_epoch());
        assert_eq!(plugin.gui_shared.loudness().elapsed_secs,0.0);
        initialize(&mut plugin,44100.0);
        plugin.process_audio(&mut buffer,4096,true,Some(120.0),Some(4.0));
        plugin.gui_shared.sample_ring.drain_into(&mut samples);
        assert!(samples.iter().all(|s|s.sample_rate==44100.0&&s.generation==plugin.gui_shared.audio_generation()));
    }
}
