//! Asynchronous connected-component analysis of an image-space foreground mask.
//! The C detector owns OpenCV state on a single background worker. The graph
//! publishes its labels, region records, and validity as one sample.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use manifold_core::Seconds;
use manifold_gpu::{
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_native::region_detector::{
    Region as NativeRegion, RegionDetector, RegionError, RegionOptions,
};

use super::region_types::{MAX_REGIONS, Region};

use crate::background_worker::BackgroundWorker;
use crate::gpu_readback::ReadbackRequest;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// The performance proof enables this fixed-capacity probe only around its
// measured frames. Normal playback uses relaxed atomic loads and allocates nothing.
const PERF_SAMPLES: usize = 1024;
static PERF_ENABLED: AtomicBool = AtomicBool::new(false);
static PERF_RUNS: AtomicUsize = AtomicUsize::new(0);
static PERF_MISSING_MASK: AtomicUsize = AtomicUsize::new(0);
static PERF_MISSING_REGIONS: AtomicUsize = AtomicUsize::new(0);
static PERF_INVALID_DIMS: AtomicUsize = AtomicUsize::new(0);
struct PerfBank {
    count: AtomicUsize,
    values: [AtomicU64; PERF_SAMPLES],
}
impl PerfBank {
    const fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
            values: [const { AtomicU64::new(0) }; PERF_SAMPLES],
        }
    }
    fn record(&self, value: u64) {
        if PERF_ENABLED.load(Ordering::Relaxed) {
            let index = self.count.fetch_add(1, Ordering::Relaxed);
            self.values[index % PERF_SAMPLES].store(value, Ordering::Relaxed);
        }
    }
    fn take(&self) -> Vec<u64> {
        let count = self.count.swap(0, Ordering::Relaxed).min(PERF_SAMPLES);
        self.values[..count]
            .iter()
            .map(|value| value.load(Ordering::Relaxed))
            .collect()
    }
}
static WORKER_NS: PerfBank = PerfBank::new();
static READBACK_AGE_FRAMES: PerfBank = PerfBank::new();
static FIXED_LAG_WAIT_NS: PerfBank = PerfBank::new();
static CAPTURE_TO_OUTPUT_FRAMES: PerfBank = PerfBank::new();

/// Samples from one isolated 600-frame performance case. The fixed-size banks
/// retain the newest 1024 observations; this proof produces fewer than that.
pub struct RegionPerfSamples {
    pub runs: usize,
    pub missing_mask: usize,
    pub missing_regions: usize,
    pub invalid_dims: usize,
    pub worker_ns: Vec<u64>,
    pub readback_age_frames: Vec<u64>,
    pub fixed_lag_wait_ns: Vec<u64>,
    pub capture_to_output_frames: Vec<u64>,
}

pub fn start_region_perf_samples() {
    PERF_ENABLED.store(false, Ordering::SeqCst);
    for count in [
        &PERF_RUNS,
        &PERF_MISSING_MASK,
        &PERF_MISSING_REGIONS,
        &PERF_INVALID_DIMS,
    ] {
        count.store(0, Ordering::Relaxed);
    }
    for bank in [
        &WORKER_NS,
        &READBACK_AGE_FRAMES,
        &FIXED_LAG_WAIT_NS,
        &CAPTURE_TO_OUTPUT_FRAMES,
    ] {
        bank.count.store(0, Ordering::Relaxed);
    }
    PERF_ENABLED.store(true, Ordering::SeqCst);
}

pub fn take_region_perf_samples() -> RegionPerfSamples {
    PERF_ENABLED.store(false, Ordering::SeqCst);
    RegionPerfSamples {
        runs: PERF_RUNS.swap(0, Ordering::Relaxed),
        missing_mask: PERF_MISSING_MASK.swap(0, Ordering::Relaxed),
        missing_regions: PERF_MISSING_REGIONS.swap(0, Ordering::Relaxed),
        invalid_dims: PERF_INVALID_DIMS.swap(0, Ordering::Relaxed),
        worker_ns: WORKER_NS.take(),
        readback_age_frames: READBACK_AGE_FRAMES.take(),
        fixed_lag_wait_ns: FIXED_LAG_WAIT_NS.take(),
        capture_to_output_frames: CAPTURE_TO_OUTPUT_FRAMES.take(),
    }
}

pub struct RegionPacket {
    rgba: Vec<u8>,
    labels: Vec<u8>,
    regions: [NativeRegion; MAX_REGIONS],
    options: RegionOptions,
    max_box_area: f32,
    width: u32,
    height: u32,
    generation: u64,
    serial: u64,
    capture: Seconds,
    capture_frame: i64,
}

pub struct RegionResponse {
    packet: RegionPacket,
    result: Result<usize, RegionError>,
}

pub struct RegionState {
    width: u32,
    height: u32,
    staging: GpuTexture,
    label_texture: GpuTexture,
    readback: ReadbackRequest,
    readback_pending: bool,
    packet: Option<RegionPacket>,
    published: [Region; MAX_REGIONS],
    rgba_labels: Vec<u8>,
    valid: bool,
    previous_capture: Option<Seconds>,
    readback_capture: Seconds,
    readback_frame: i64,
    sample_dt: f32,
    generation: u64,
    serial: u64,
    last_request_frame: i64,
    frame_counter: i64,
    warmup_complete: bool,
    clear_pending: bool,
    previous_run: Option<Seconds>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegionSchedule {
    consume_before_submit: bool,
    schedule_readback: bool,
}

fn region_schedule(
    worker_busy: bool,
    readback_pending: bool,
    due: bool,
    packet_free: bool,
) -> RegionSchedule {
    RegionSchedule {
        consume_before_submit: worker_busy,
        schedule_readback: due && !worker_busy && !readback_pending && packet_free,
    }
}

fn finite_clamped(value: f32, fallback: f32, lo: f32, hi: f32) -> f32 {
    if value.is_finite() {
        value.clamp(lo, hi)
    } else {
        fallback
    }
}

fn packet_for(width: u32, height: u32) -> RegionPacket {
    let pixels = width as usize * height as usize;
    RegionPacket {
        rgba: vec![0; pixels * 4],
        labels: vec![0; pixels],
        regions: [NativeRegion::default(); MAX_REGIONS],
        options: RegionOptions {
            threshold: 0.5,
            min_area: 0.001,
            max_area: 0.8,
            min_aspect: 0.05,
            max_aspect: 20.0,
            max_regions: 8,
        },
        max_box_area: 1.0,
        width,
        height,
        generation: 0,
        serial: 0,
        capture: Seconds(0.0),
        capture_frame: 0,
    }
}

/// Commit every CPU-visible output from one response together. A stale
/// generation leaves the currently published sample untouched.
fn publish_response(
    response: &RegionResponse,
    generation: u64,
    width: u32,
    height: u32,
    published: &mut [Region; MAX_REGIONS],
    rgba_labels: &mut [u8],
    valid: &mut bool,
    previous_capture: &mut Option<Seconds>,
    sample_dt: &mut f32,
) -> bool {
    let packet = &response.packet;
    if packet.generation != generation || packet.width != width || packet.height != height {
        return false;
    }
    *sample_dt = previous_capture
        .map(|previous| (packet.capture.0 - previous.0) as f32)
        .unwrap_or(0.0);
    *previous_capture = Some(packet.capture);
    published.fill(Region::default());
    if let Ok(count) = response.result {
        *valid = true;
        for (destination, source) in published
            .iter_mut()
            .zip(&packet.regions[..count.min(MAX_REGIONS)])
        {
            *destination = Region {
                label: source.label,
                x: source.x,
                y: source.y,
                width: source.width,
                height: source.height,
                area: source.area,
                cx: source.cx,
                cy: source.cy,
            };
        }
        for (pixel, &label) in rgba_labels.chunks_exact_mut(4).zip(&packet.labels) {
            pixel.copy_from_slice(&[label, label, label, 255]);
        }
    } else {
        *valid = false;
        for pixel in rgba_labels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0, 0, 0, 255]);
        }
    }
    true
}

crate::primitive! {
    name: DetectRegions,
    type_id: "node.detect_regions",
    purpose: "Detect 8-connected filled foreground components in a mask, preserving a categorical label image and bounded region records for independent tracking and mask rendering.",
    inputs: {
        mask: Texture2D required,
        reset: ScalarF32 optional,
    },
    outputs: {
        labels: Texture2D,
        regions: Channels[LABEL: U32, X: F32, Y: F32, WIDTH: F32, HEIGHT: F32, AREA: F32, CX: F32, CY: F32],
        updated: ScalarF32,
        sample_dt: ScalarF32,
        valid: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("threshold"), label: "Threshold", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("min_area"), label: "Min Area", ty: ParamType::Float, default: ParamValue::Float(0.001), range: Some((0.0, 0.25)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_area"), label: "Max Area", ty: ParamType::Float, default: ParamValue::Float(0.8), range: Some((0.01, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_box_area"), label: "Max Box Area", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("min_aspect"), label: "Min Aspect", ty: ParamType::Float, default: ParamValue::Float(0.05), range: Some((0.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_aspect"), label: "Max Aspect", ty: ParamType::Float, default: ParamValue::Float(20.0), range: Some((0.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_regions"), label: "Max Blobs", ty: ParamType::Int, default: ParamValue::Float(8.0), range: Some((1.0, 32.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("update_interval"), label: "Update Interval", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((1.0, 8.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Resize and denoise the mask before this node. Min/Max Area measure foreground pixels; Max Box Area measures width*height of the enclosing rectangle as a fraction of the image. All filters run before Max Blobs selection, and rejected regions have no label pixels. A Max Box Area of 1 preserves unrestricted bounds. Its Rgba8Unorm labels are categorical: read exact texels and round red*255. `updated` is a one-run pulse when a native sample is consumed; `valid` remains published between samples. Wire regions, updated, sample_dt and valid to node.track_regions. A successful empty result is valid; an error or reset is invalid.",
    examples: [],
    picker: { label: "Detect Regions", category: Atom },
    summary: "Finds filled regions and keeps their real pixel shapes for blob masks and tracking.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["regions", "connected components", "blob labels"],
    boundary_reason: IoBridge,
    extra_fields: {
        worker: Option<BackgroundWorker<RegionPacket, RegionResponse>> = None,
        worker_tried: bool = false,
        state: Option<RegionState> = None,
        generation: u64 = 0,
        reset_was_active: bool = false,
    },
}

impl DetectRegions {
    fn ensure_worker(&mut self) {
        if self.worker.is_some() || self.worker_tried {
            return;
        }
        self.worker_tried = true;
        self.worker = BackgroundWorker::try_new(|| {
            let mut detector = match manifold_native::ffi::region_ffi::FfiRegionDetector::new() {
                Ok(detector) => detector,
                Err(error) => {
                    log::error!("[node.detect_regions] {error}");
                    return None;
                }
            };
            Some(move |mut packet: RegionPacket| {
                let start = PERF_ENABLED.load(Ordering::Relaxed).then(Instant::now);
                let result = detector.process_bounded(
                    &packet.rgba,
                    packet.width,
                    packet.height,
                    packet.options,
                    packet.max_box_area,
                    &mut packet.labels,
                    &mut packet.regions,
                );
                if let Some(start) = start {
                    WORKER_NS.record(start.elapsed().as_nanos() as u64);
                }
                RegionResponse { packet, result }
            })
        });
    }

    fn ensure_state(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
        width: u32,
        height: u32,
    ) {
        if self
            .state
            .as_ref()
            .is_some_and(|state| state.width == width && state.height == height)
        {
            return;
        }
        if let Some(state) = self.state.as_mut() {
            state.readback.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        let pixels = width as usize * height as usize;
        let staging = gpu.device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "node.detect_regions.staging",
            mip_levels: 1,
        });
        let label_texture = gpu.device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "node.detect_regions.labels",
            mip_levels: 1,
        });
        gpu.clear_texture(&label_texture, 0.0, 0.0, 0.0, 1.0);
        let packet = self
            .state
            .as_mut()
            .and_then(|state| state.packet.take())
            .filter(|packet| {
                packet.rgba.capacity() >= pixels * 4 && packet.labels.capacity() >= pixels
            })
            .unwrap_or_else(|| packet_for(width, height));
        self.state = Some(RegionState {
            width,
            height,
            staging,
            label_texture,
            readback: ReadbackRequest::new(),
            readback_pending: false,
            packet: Some(packet),
            published: [Region::default(); MAX_REGIONS],
            rgba_labels: vec![0; pixels * 4],
            valid: false,
            previous_capture: None,
            readback_capture: Seconds(0.0),
            readback_frame: 0,
            sample_dt: 0.0,
            generation: self.generation,
            serial: 0,
            last_request_frame: -1024,
            frame_counter: 0,
            warmup_complete: self.worker.is_none(),
            clear_pending: false,
            previous_run: None,
        });
    }

    fn reset_state(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(state) = self.state.as_mut() {
            state.readback.cancel();
            state.readback_pending = false;
            state.generation = self.generation;
            state.published.fill(Region::default());
            state.rgba_labels.fill(0);
            for alpha in state.rgba_labels[3..].iter_mut().step_by(4) {
                *alpha = 255;
            }
            state.valid = false;
            state.previous_capture = None;
            state.sample_dt = 0.0;
            state.last_request_frame = -1024;
            state.frame_counter = 0;
            state.clear_pending = true;
            state.warmup_complete = self.worker.is_none();
        }
    }
}

impl Primitive for DetectRegions {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        input_dims: &[(&str, (u32, u32))],
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        (port == "labels")
            .then(|| {
                input_dims
                    .iter()
                    .find(|(name, _)| *name == "mask")
                    .map(|(_, dims)| *dims)
            })
            .flatten()
    }

    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        (port == "labels").then_some(GpuTextureFormat::Rgba8Unorm)
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "regions").then_some(MAX_REGIONS as u32)
    }

    fn warmup_pending(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| !state.warmup_complete)
    }

    fn clear_state(&mut self) {
        self.reset_state();
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if PERF_ENABLED.load(Ordering::Relaxed) {
            PERF_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        let reset_active = ctx
            .inputs
            .scalar("reset")
            .and_then(|value| value.as_scalar())
            .unwrap_or(0.0)
            > 0.5;
        if reset_active && !self.reset_was_active {
            self.reset_state();
        }
        self.reset_was_active = reset_active;
        if self
            .state
            .as_ref()
            .and_then(|state| state.previous_run)
            .is_some_and(|previous| {
                ctx.time.seconds.0 < previous.0 || ctx.time.seconds.0 - previous.0 > 1.0
            })
        {
            self.reset_state();
        }

        ctx.outputs.set_scalar("updated", ParamValue::Float(0.0));
        ctx.outputs.set_scalar("sample_dt", ParamValue::Float(0.0));
        ctx.outputs.set_scalar("valid", ParamValue::Float(0.0));

        let Some(mask) = ctx.inputs.texture_2d("mask") else {
            if PERF_ENABLED.load(Ordering::Relaxed) {
                PERF_MISSING_MASK.fetch_add(1, Ordering::Relaxed);
            }
            return;
        };
        let labels = ctx.outputs.texture_2d("labels");
        let Some(regions) = ctx.outputs.array("regions") else {
            if PERF_ENABLED.load(Ordering::Relaxed) {
                PERF_MISSING_REGIONS.fetch_add(1, Ordering::Relaxed);
            }
            return;
        };
        if mask.width == 0 || mask.height == 0 || mask.width > 1024 || mask.height > 1024 {
            if PERF_ENABLED.load(Ordering::Relaxed) {
                PERF_INVALID_DIMS.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if labels.is_some_and(|labels| labels.width != mask.width || labels.height != mask.height) {
            return;
        }
        let update_interval =
            finite_clamped(ctx.param_f32("update_interval", 2.0), 2.0, 1.0, 8.0).round() as i64;
        let options = RegionOptions {
            threshold: finite_clamped(ctx.param_f32("threshold", 0.5), 0.5, 0.0, 1.0),
            min_area: finite_clamped(ctx.param_f32("min_area", 0.001), 0.001, 0.0, 0.25),
            max_area: finite_clamped(ctx.param_f32("max_area", 0.8), 0.8, 0.01, 1.0),
            min_aspect: finite_clamped(ctx.param_f32("min_aspect", 0.05), 0.05, 0.0, 100.0),
            max_aspect: finite_clamped(ctx.param_f32("max_aspect", 20.0), 20.0, 0.0, 100.0),
            max_regions: finite_clamped(ctx.param_f32("max_regions", 8.0), 8.0, 1.0, 32.0).round()
                as u32,
        };
        let capture_time = ctx.time.seconds;
        let max_box_area = finite_clamped(ctx.param_f32("max_box_area", 1.0), 1.0, 0.0, 1.0);
        let gpu = ctx.gpu_encoder();
        self.ensure_worker();
        self.ensure_state(gpu, mask.width, mask.height);
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.previous_run = Some(capture_time);
        if state.clear_pending {
            gpu.clear_texture(&state.label_texture, 0.0, 0.0, 0.0, 1.0);
            state.clear_pending = false;
        }

        let mut updated = false;
        if let Some(worker) = self.worker.as_mut() {
            let response =
                if region_schedule(worker.is_busy(), false, false, false).consume_before_submit {
                    let start = PERF_ENABLED.load(Ordering::Relaxed).then(Instant::now);
                    let response = worker.recv_blocking();
                    if let Some(start) = start {
                        FIXED_LAG_WAIT_NS.record(start.elapsed().as_nanos() as u64);
                    }
                    response
                } else {
                    FIXED_LAG_WAIT_NS.record(0);
                    worker.try_recv()
                };
            if let Some(mut response) = response {
                updated = publish_response(
                    &response,
                    state.generation,
                    state.width,
                    state.height,
                    &mut state.published,
                    &mut state.rgba_labels,
                    &mut state.valid,
                    &mut state.previous_capture,
                    &mut state.sample_dt,
                );
                if updated {
                    CAPTURE_TO_OUTPUT_FRAMES.record(
                        (state.frame_counter - response.packet.capture_frame).max(0) as u64,
                    );
                    state.warmup_complete = true;
                    gpu.native_enc.upload_texture(
                        &state.label_texture,
                        state.width,
                        state.height,
                        1,
                        &state.rgba_labels,
                    );
                }
                response
                    .packet
                    .rgba
                    .resize(state.width as usize * state.height as usize * 4, 0);
                response
                    .packet
                    .labels
                    .resize(state.width as usize * state.height as usize, 0);
                state.packet = Some(response.packet);
            }

            if state.readback_pending
                && !worker.is_busy()
                && let Some(packet) = state.packet.as_mut()
                && state.readback.try_read_into(&mut packet.rgba)
            {
                READBACK_AGE_FRAMES
                    .record((state.frame_counter - state.readback_frame).max(0) as u64);
                state.readback_pending = false;
                let mut packet = state.packet.take().expect("packet exists");
                packet.width = state.width;
                packet.height = state.height;
                packet.options = options;
                packet.max_box_area = max_box_area;
                packet.generation = state.generation;
                state.serial = state.serial.wrapping_add(1);
                packet.serial = state.serial;
                packet.capture = state.readback_capture;
                packet.capture_frame = state.readback_frame;
                worker.submit(packet);
            }

            let due = state.frame_counter - state.last_request_frame >= update_interval;
            if region_schedule(
                worker.is_busy(),
                state.readback_pending,
                due,
                state.packet.is_some(),
            )
            .schedule_readback
            {
                gpu.resize_sample(mask, &state.staging);
                state
                    .readback
                    .submit(gpu, &state.staging, state.width, state.height);
                state.readback_pending = true;
                state.readback_capture = capture_time;
                state.readback_frame = state.frame_counter;
                state.last_request_frame = state.frame_counter;
            }
        }
        state.frame_counter += 1;

        if let Some(ptr) = regions.mapped_ptr() {
            let capacity = (regions.size as usize / std::mem::size_of::<Region>()).min(MAX_REGIONS);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    state.published.as_ptr(),
                    ptr as *mut Region,
                    capacity,
                );
            }
        } else {
            log::error!("node.detect_regions requires shared-memory region output");
        }
        if let Some(labels) = labels {
            if state.label_texture.format == labels.format {
                gpu.copy_texture_to_texture(
                    &state.label_texture,
                    labels,
                    state.width,
                    state.height,
                );
            } else {
                // The effect runtime may prebind a host-format output texture.
                // Equal dimensions sample texel centres, retaining categorical
                // label values through the Rgba8Unorm → Rgba16Float conversion.
                gpu.resize_sample(&state.label_texture, labels);
            }
        }
        ctx.outputs.set_scalar(
            "updated",
            ParamValue::Float(if updated { 1.0 } else { 0.0 }),
        );
        ctx.outputs.set_scalar(
            "sample_dt",
            ParamValue::Float(if updated { state.sample_dt } else { 0.0 }),
        );
        ctx.outputs.set_scalar(
            "valid",
            ParamValue::Float(if state.valid { 1.0 } else { 0.0 }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_v2_schedule_has_one_packet_and_a_next_run_deadline() {
        assert!(region_schedule(true, false, true, false).consume_before_submit);
        assert!(!region_schedule(true, false, true, false).schedule_readback);
        assert!(!region_schedule(false, true, true, true).schedule_readback);
        assert!(region_schedule(false, false, true, true).schedule_readback);
    }

    #[test]
    fn blob_v2_packet_storage_reused() {
        let mut packet = packet_for(320, 180);
        let image = packet.rgba.as_ptr();
        let labels = packet.labels.as_ptr();
        packet.rgba.resize(320 * 180 * 4, 0);
        packet.labels.resize(320 * 180, 0);
        assert_eq!(image, packet.rgba.as_ptr());
        assert_eq!(labels, packet.labels.as_ptr());
    }

    #[test]
    fn blob_v2_sample_publication_is_atomic() {
        let mut packet = packet_for(3, 1);
        packet.generation = 4;
        packet.capture = Seconds(1.0);
        packet.labels.copy_from_slice(&[1, 0, 32]);
        packet.regions[0] = NativeRegion {
            label: 1,
            x: 0.0,
            y: 0.0,
            width: 1.0 / 3.0,
            height: 1.0,
            area: 1.0 / 3.0,
            cx: 1.0 / 6.0,
            cy: 0.5,
        };
        let mut response = RegionResponse {
            packet,
            result: Ok(1),
        };
        let mut published = [Region::default(); MAX_REGIONS];
        let mut rgba = [0u8; 12];
        let mut valid = false;
        let mut previous = None;
        let mut dt = 0.0;
        assert!(publish_response(
            &response,
            4,
            3,
            1,
            &mut published,
            &mut rgba,
            &mut valid,
            &mut previous,
            &mut dt,
        ));
        assert!(valid);
        assert_eq!(published[0].label, 1);
        assert_eq!(rgba, [1, 1, 1, 255, 0, 0, 0, 255, 32, 32, 32, 255]);
        assert_eq!(dt, 0.0);

        // A successful empty sample clears both representations while
        // retaining validity; an error clears them and invalidates the mask.
        response.packet.capture = Seconds(1.1);
        response.packet.labels.fill(0);
        response.result = Ok(0);
        assert!(publish_response(
            &response,
            4,
            3,
            1,
            &mut published,
            &mut rgba,
            &mut valid,
            &mut previous,
            &mut dt,
        ));
        assert!(valid);
        assert!(published.iter().all(|region| region.label == 0));
        assert!(rgba.chunks_exact(4).all(|pixel| pixel == [0, 0, 0, 255]));
        assert!((dt - 0.1).abs() < 1.0e-5);

        response.result = Err(RegionError::NativeFailure);
        response.packet.labels[0] = 1;
        assert!(publish_response(
            &response,
            4,
            3,
            1,
            &mut published,
            &mut rgba,
            &mut valid,
            &mut previous,
            &mut dt,
        ));
        assert!(!valid);
        assert!(published.iter().all(|region| region.label == 0));
        assert!(rgba.chunks_exact(4).all(|pixel| pixel == [0, 0, 0, 255]));
    }

    #[test]
    fn blob_v2_lifecycle_discards_stale_samples() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let mut worker = BackgroundWorker::new(move || {
            move |mut packet: RegionPacket| {
                release_rx.recv().expect("test releases delayed worker");
                packet.labels[0] = 7;
                RegionResponse {
                    packet,
                    result: Ok(1),
                }
            }
        });
        let mut old = packet_for(2, 2);
        old.generation = 1;
        worker.submit(old);
        assert!(worker.is_busy());

        // A reset or resize advances the generation and clears publication
        // before the old worker result becomes available.
        let mut published = [Region::default(); MAX_REGIONS];
        let mut rgba = [0u8; 16];
        let mut valid = false;
        let mut previous = None;
        let mut dt = 0.0;
        release_tx.send(()).expect("release worker");
        let stale = worker.recv_blocking().expect("delayed response");
        assert!(!publish_response(
            &stale,
            2,
            2,
            2,
            &mut published,
            &mut rgba,
            &mut valid,
            &mut previous,
            &mut dt,
        ));
        assert!(!valid);
        assert_eq!(rgba, [0; 16]);
        assert!(previous.is_none());
        assert!(!publish_response(
            &stale,
            1,
            3,
            2,
            &mut published,
            &mut rgba,
            &mut valid,
            &mut previous,
            &mut dt,
        ));
        let image_ptr = stale.packet.rgba.as_ptr();
        let label_ptr = stale.packet.labels.as_ptr();
        let mut recycled = stale.packet;
        recycled.rgba.resize(8, 0);
        recycled.labels.resize(2, 0);
        assert_eq!(recycled.rgba.as_ptr(), image_ptr);
        assert_eq!(recycled.labels.as_ptr(), label_ptr);
    }
}
