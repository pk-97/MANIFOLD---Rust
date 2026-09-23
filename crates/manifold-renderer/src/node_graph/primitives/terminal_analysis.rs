//! Fixed-grid, asynchronous image analysis for `node.terminal_stream`.
//!
//! This is a small CPU readback bridge rather than a composable GPU primitive.
//! The compute pass writes one record for each cell in the terminal's fixed
//! 64×36 grid, followed by a 256×144 detail grid. Three persistent readback
//! buffers keep the content thread non-blocking while the CPU consumes the
//! newest completed result.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEvent, GpuTexture};

use super::terminal_reaction::SAMPLE_COUNT;

const SHADER: &str = include_str!("shaders/terminal_analysis.wgsl");
const PIPELINE_LABEL: &str = "node.terminal_stream.analysis";
const READBACK_SLOTS: usize = 3;
pub(super) const DETAIL_COLS: usize = 256;
pub(super) const DETAIL_ROWS: usize = 144;
pub(super) const DETAIL_COUNT: usize = DETAIL_COLS * DETAIL_ROWS;
const READBACK_COUNT: usize = SAMPLE_COUNT + DETAIL_COUNT;
const READBACK_BYTES: u64 = (READBACK_COUNT * std::mem::size_of::<[f32; 4]>()) as u64;

pub(crate) fn prewarm_pipeline(device: &GpuDevice) {
    // This CPU readback bridge is outside the pure-GPU atom codegen sweep.
    device.create_compute_pipeline(SHADER, "cs_main", PIPELINE_LABEL);
}

struct ReadbackSlot {
    buffer: GpuBuffer,
    event: GpuEvent,
    signal: u64,
    generation: u64,
    sequence: u64,
}

/// Asynchronous fixed-grid terminal image analysis.
pub(super) struct TerminalAnalysis {
    pipeline: Option<GpuComputePipeline>,
    slots: Option<[ReadbackSlot; READBACK_SLOTS]>,
    samples: Box<[[f32; 4]; SAMPLE_COUNT]>,
    detail_samples: Box<[[f32; 4]; DETAIL_COUNT]>,
    generation: u64,
    next_slot: usize,
    next_sequence: u64,
    latest_sequence: u64,
}

impl TerminalAnalysis {
    pub(super) fn new() -> Self {
        Self {
            pipeline: None,
            slots: None,
            samples: Box::new([[0.0; 4]; SAMPLE_COUNT]),
            detail_samples: vec![[0.0; 4]; DETAIL_COUNT]
                .into_boxed_slice()
                .try_into()
                .expect("fixed detail sample count"),
            generation: 1,
            next_slot: 0,
            next_sequence: 0,
            latest_sequence: 0,
        }
    }

    /// Prepare the persistent pipeline and readback ring. Calling this more
    /// than once is harmless and does not replace in-flight storage.
    pub(super) fn install(&mut self, device: &GpuDevice) {
        if self.pipeline.is_none() {
            self.pipeline = Some(device.create_compute_pipeline(SHADER, "cs_main", PIPELINE_LABEL));
        }
        if self.slots.is_none() {
            self.slots = Some(std::array::from_fn(|_| ReadbackSlot {
                buffer: device.create_buffer_shared(READBACK_BYTES),
                event: device.create_event(),
                signal: 0,
                generation: 0,
                sequence: 0,
            }));
        }
    }

    /// Drop the logical results while retaining GPU resources and respecting
    /// any readback work already in flight.
    pub(super) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.samples.fill([0.0; 4]);
        self.detail_samples.fill([0.0; 4]);
        self.latest_sequence = 0;
    }

    pub(super) fn has_samples(&self) -> bool {
        self.latest_sequence != 0
    }

    pub(super) fn latest_samples(&self) -> &[[f32; 4]; SAMPLE_COUNT] {
        &self.samples
    }

    pub(super) fn latest_detail_samples(&self) -> &[[f32; 4]; DETAIL_COUNT] {
        &self.detail_samples
    }

    fn poll_completed(&mut self) {
        let Some(slots) = self.slots.as_mut() else {
            return;
        };

        let generation = self.generation;
        let samples = &mut *self.samples;
        let detail_samples = &mut *self.detail_samples;
        let latest_sequence = &mut self.latest_sequence;
        for slot in slots {
            if slot.signal == 0 || !slot.event.is_done(slot.signal) {
                continue;
            }

            if slot.generation == generation && slot.sequence > *latest_sequence {
                let ptr = slot
                    .buffer
                    .mapped_ptr()
                    .expect("terminal analysis readback buffer mapped");
                let source = ptr.cast::<[f32; 4]>();
                // The event is the acquire fence for the shared buffer. The
                // GPU has finished writing before this copy is observed.
                unsafe {
                    std::ptr::copy_nonoverlapping(source, samples.as_mut_ptr(), SAMPLE_COUNT);
                    std::ptr::copy_nonoverlapping(
                        source.add(SAMPLE_COUNT),
                        detail_samples.as_mut_ptr(),
                        DETAIL_COUNT,
                    );
                }
                for sample in samples.iter_mut() {
                    for component in sample {
                        if !component.is_finite() {
                            *component = 0.0;
                        }
                    }
                }
                for sample in detail_samples.iter_mut() {
                    for component in sample {
                        if !component.is_finite() {
                            *component = 0.0;
                        }
                    }
                }
                *latest_sequence = slot.sequence;
            }

            // A completed slot is safe to recycle on a later frame. Stale
            // generations are intentionally discarded here without copying.
            slot.signal = 0;
        }
    }

    /// Queue one analysis pass and return the newest completed CPU snapshot.
    /// There is no CPU wait: if every ring slot is busy, the previous snapshot
    /// is returned unchanged.
    pub(super) fn sample(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
        texture: &GpuTexture,
    ) -> &[[f32; 4]; SAMPLE_COUNT] {
        self.install(gpu.device);
        self.poll_completed();

        let pipeline = self
            .pipeline
            .as_ref()
            .expect("terminal analysis pipeline installed");
        let slots = self
            .slots
            .as_mut()
            .expect("terminal analysis readback ring installed");
        let Some(slot_index) = (0..READBACK_SLOTS)
            .map(|step| (self.next_slot + step) % READBACK_SLOTS)
            .find(|&index| slots[index].signal == 0)
        else {
            return &self.samples;
        };

        self.next_slot = (slot_index + 1) % READBACK_SLOTS;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let sequence = self.next_sequence;
        let generation = self.generation;
        let slot = &mut slots[slot_index];

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &slot.buffer,
                    offset: 0,
                },
            ],
            [
                DETAIL_COLS.div_ceil(8) as u32,
                DETAIL_ROWS.div_ceil(8) as u32,
                1,
            ],
            PIPELINE_LABEL,
        );
        gpu.native_enc.signal_event(&slot.event);
        slot.signal = slot.event.current_value();
        slot.generation = generation;
        slot.sequence = sequence;

        &self.samples
    }
}

impl Default for TerminalAnalysis {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use manifold_gpu::{
        GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::TerminalAnalysis;
    use crate::gpu_encoder::GpuEncoder;

    const WIDTH: u32 = 192;
    const HEIGHT: u32 = 72;

    fn source_texture(
        device: &manifold_gpu::GpuDevice,
        label: &str,
        fill: impl Fn(u32, u32) -> [u8; 4],
    ) -> GpuTexture {
        let texture = device.create_texture(&GpuTextureDesc {
            width: WIDTH,
            height: HEIGHT,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label,
            mip_levels: 1,
        });
        let mut pixels = vec![0u8; WIDTH as usize * HEIGHT as usize * 4];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let offset = ((y * WIDTH + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&fill(x, y));
            }
        }
        device.upload_texture(&texture, &pixels);
        texture
    }

    fn sample_and_wait(
        analysis: &mut TerminalAnalysis,
        device: &manifold_gpu::GpuDevice,
        texture: &GpuTexture,
        label: &str,
    ) -> [f32; 4] {
        let mut encoder = device.create_encoder(label);
        let sample = {
            let mut gpu = GpuEncoder::new(&mut encoder, device);
            *analysis
                .sample(&mut gpu, texture)
                .first()
                .expect("fixed sample grid")
        };
        encoder.commit_and_wait_completed();
        sample
    }

    #[test]
    fn constant_rgb_has_expected_mean_and_zero_contrast() {
        let device = crate::test_device();
        let texture = source_texture(&device, "terminal-analysis-constant", |_, _| {
            [64, 128, 192, 255]
        });
        let mut analysis = TerminalAnalysis::new();

        let _ = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-constant-1",
        );
        let sample = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-constant-2",
        );

        assert!(
            (sample[0] - 64.0 / 255.0).abs() < 0.01,
            "red mean: {sample:?}"
        );
        assert!(
            (sample[1] - 128.0 / 255.0).abs() < 0.01,
            "green mean: {sample:?}"
        );
        assert!(
            (sample[2] - 192.0 / 255.0).abs() < 0.01,
            "blue mean: {sample:?}"
        );
        assert!(
            sample[3].abs() < 0.001,
            "constant colour contrast: {sample:?}"
        );
    }

    #[test]
    fn nine_taps_report_contrast_inside_a_split_cell() {
        let device = crate::test_device();
        // Grid cell 32 spans x=96..98. The split at x=97 makes its nine taps
        // observe both sides, proving the analysis is cell-local rather than
        // one representative texel per cell.
        let texture = source_texture(&device, "terminal-analysis-split", |x, _| {
            if x < 97 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            }
        });
        let mut analysis = TerminalAnalysis::new();

        let _ = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-split-1",
        );
        let _ = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-split-2",
        );
        let mut encoder = device.create_encoder("terminal-analysis-split-3");
        let split_sample = {
            let mut gpu = GpuEncoder::new(&mut encoder, &device);
            analysis.sample(&mut gpu, &texture)[32]
        };
        encoder.commit_and_wait_completed();

        assert!(
            split_sample[3] > 0.1,
            "expected local contrast at split: {split_sample:?}"
        );
    }

    #[test]
    fn first_sample_is_zero_then_completed_result_is_visible() {
        let device = crate::test_device();
        let texture = source_texture(&device, "terminal-analysis-latency", |_, _| {
            [255, 32, 16, 255]
        });
        let mut analysis = TerminalAnalysis::new();

        let first = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-latency-1",
        );
        assert_eq!(
            first, [0.0; 4],
            "first call must expose the initial zero snapshot"
        );

        let second = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-latency-2",
        );
        assert!(
            second[0] > 0.95 && second[1] < 0.2 && second[2] < 0.2,
            "completed result: {second:?}"
        );
    }

    #[test]
    fn fine_grid_reports_localized_source_detail() {
        let device = crate::test_device();
        let texture = source_texture(&device, "terminal-analysis-detail", |x, y| {
            if (48..56).contains(&x) && (18..24).contains(&y) {
                [255, 128, 0, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        let mut analysis = TerminalAnalysis::new();

        let _ = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-detail-1",
        );
        let _ = sample_and_wait(
            &mut analysis,
            &device,
            &texture,
            "terminal-analysis-detail-2",
        );

        let mut bright_count = 0;
        for y in 0..super::DETAIL_ROWS {
            for x in 0..super::DETAIL_COLS {
                if analysis.latest_detail_samples()[y * super::DETAIL_COLS + x][0] > 0.9 {
                    assert!((64..75).contains(&x), "detail x escaped source region: {x}");
                    assert!((36..48).contains(&y), "detail y escaped source region: {y}");
                    bright_count += 1;
                }
            }
        }
        assert!(
            bright_count > 0,
            "localized source was absent from detail grid"
        );
    }

    #[test]
    fn reset_discards_pending_result_and_preserves_allocations() {
        let device = crate::test_device();
        let old_texture = source_texture(&device, "terminal-analysis-reset-old", |_, _| {
            [255, 0, 0, 255]
        });
        let new_texture = source_texture(&device, "terminal-analysis-reset-new", |_, _| {
            [0, 0, 255, 255]
        });
        let mut analysis = TerminalAnalysis::new();
        analysis.install(&device);
        let samples_ptr = analysis.samples.as_ptr();
        let buffer_ptr = analysis.slots.as_ref().expect("installed readback ring")[0]
            .buffer
            .mapped_ptr()
            .expect("shared readback buffer") as usize;
        let detail_ptr = analysis.detail_samples.as_ptr();

        let mut pending = device.create_encoder("terminal-analysis-reset-pending");
        {
            let mut gpu = GpuEncoder::new(&mut pending, &device);
            let _ = analysis.sample(&mut gpu, &old_texture);
        }
        analysis.reset();
        assert!(
            analysis
                .latest_detail_samples()
                .iter()
                .all(|sample| *sample == [0.0; 4]),
            "detail snapshot was not cleared"
        );
        pending.commit_and_wait_completed();

        let discarded = sample_and_wait(
            &mut analysis,
            &device,
            &new_texture,
            "terminal-analysis-reset-poll",
        );
        assert_eq!(discarded, [0.0; 4], "pre-reset result must be discarded");
        let current = sample_and_wait(
            &mut analysis,
            &device,
            &new_texture,
            "terminal-analysis-reset-current",
        );
        assert!(
            current[2] > 0.95 && current[0] < 0.2,
            "post-reset result: {current:?}"
        );
        assert_eq!(
            analysis.samples.as_ptr(),
            samples_ptr,
            "CPU sample allocation changed"
        );
        assert_eq!(
            analysis.detail_samples.as_ptr(),
            detail_ptr,
            "CPU detail sample allocation changed"
        );
        assert_eq!(
            analysis.slots.as_ref().expect("installed readback ring")[0]
                .buffer
                .mapped_ptr()
                .expect("shared readback buffer") as usize,
            buffer_ptr,
            "GPU readback allocation changed",
        );
    }
}
