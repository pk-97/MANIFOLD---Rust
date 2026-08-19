//! `bridge-probe` — headless tear detector for `SharedTextureBridge`
//! (BUG-xaw4, presentation-transport audit).
//!
//! Replicates the production topology with no app: TWO GpuDevices (writer =
//! content, reader = UI — MTLSharedEvent cannot cross MTLDevice instances,
//! which is exactly why the bridge's read side needs CPU-side fencing), one
//! real `SharedTextureBridge`, a writer thread writing per-frame solid
//! colours into surfaces as EIGHT band render passes (the production shape:
//! a frame is many passes, the surface is "written" across the whole frame
//! interval), and a reader thread sampling `front_index()` surfaces through
//! a render pass — production reads the bridge texture in a render pass
//! (`composite_main_ui_frame`), and render passes from two queues co-execute
//! on Apple silicon where compute dispatches and blits serialize. Earlier
//! probe iterations measured nothing for exactly that reason (compute fill +
//! blit copy never overlapped; a params-buffer race then faked tears — see
//! the module history in git).
//!
//! A readback of a fully-written surface is ONE exact colour (texel-exact
//! `textureLoad`, no filtering). A readback with >1 distinct pixel value
//! means the sample raced a write of the same surface — a torn frame.
//! `--policy legacy` reproduces today's contract (content waits only for its
//! own write completion before reusing a slot); `--policy fenced` uses the
//! bridge's read-fence API (`is_reusable` / `acquire_read` / `retire_read`)
//! and must report ZERO torn reads. Red before the fix, green after.
//!
//! Run: `cargo run --features perf-soak --bin manifold -- bridge-probe [--policy legacy|fenced] [--frames N]`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::shared_texture::{SURFACE_COUNT, SharedTextureBridge};

const W: u32 = 1024;
const H: u32 = 640;
const BYTES_PER_ROW: u32 = W * 8;
/// Bands per frame write — each its own render pass, so a reader sampling
/// mid-frame catches a partial frame (3/8-band splits measured).
/// Pinned at 8 for BUG-4qob probabilistic gate (legacy ≥1/7 tears, fenced 0/7).
const BANDS: u32 = 8;

const VS: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
"#;

const FILL_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read> params: array<f32, 6>;
@group(0) @binding(1) var<storage, read_write> sink: array<f32>;
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Delay loop so the band pass stays in flight ~ms; the result MUST reach
    // a real store (sink) or the compiler deletes the loop (measured —
    // `colour + x*0` turned an earlier probe into a no-backlog run).
    var x = params[0] + in.uv.x;
    for (var i = 0; i < 200; i++) {
        x = sin(x) * cos(x) + 0.1;
    }
    if (in.position.x < 1.0 && in.position.y < 1.0) {
        sink[0] = x;
    }
    return vec4<f32>(params[0], params[1], params[2], params[3]);
}
"#;

const SAMPLE_WGSL: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> sink: array<f32>;
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Delay keeps the sample pass in flight (wider collision window — a
    // 0.1ms sample almost never straddles a write). Sink store keeps the loop.
    var x = in.uv.x + in.uv.y;
    for (var i = 0; i < 600; i++) {
        x = sin(x) * cos(x) + 0.1;
    }
    if (in.position.x < 1.0 && in.position.y < 1.0) {
        sink[0] = x;
    }
    return textureLoad(t_source, vec2<i32>(in.position.xy), 0);
}
"#;

const HEAVY_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read_write> sink: array<f32>;
@compute @workgroup_size(256)
fn heavy(@builtin(global_invocation_id) gid: vec3<u32>) {
    var x = f32(gid.x) * 1.01 + 0.3;
    for (var i = 0; i < 400; i++) {
        x = sin(x) * cos(x) + 0.1;
    }
    sink[gid.x % arrayLength(&sink)] = x;
}
"#;

/// Per-frame solid colour, distinct across frames, exact in f16 (small ints).
fn frame_colour(i: u64) -> [f32; 4] {
    [
        ((i * 37 % 251) + 1) as f32,
        ((i * 91 % 239) + 1) as f32,
        ((i * 53 % 241) + 1) as f32,
        1.0,
    ]
}

fn fill_wgsl() -> String {
    format!("{VS}{FILL_WGSL}")
}

fn sample_wgsl() -> String {
    format!("{VS}{SAMPLE_WGSL}")
}

pub fn run(args: &[String]) -> ! {
    let fenced = args
        .windows(2)
        .find(|w| w[0] == "--policy")
        .is_some_and(|w| w[1] == "fenced");
    let frames: u64 = args
        .windows(2)
        .find(|w| w[0] == "--frames")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(600);
    println!(
        "=== BRIDGE PROBE policy={} frames={frames} surfaces={W}x{H}x{SURFACE_COUNT} bands={BANDS} ===",
        if fenced { "fenced" } else { "legacy" }
    );

    let writer_dev = Arc::new(manifold_gpu::GpuDevice::new());
    let reader_dev = Arc::new(manifold_gpu::GpuDevice::new());
    let bridge = Arc::new(SharedTextureBridge::new(W, H));

    if args.iter().any(|a| a == "--selftest") {
        let fill = writer_dev.create_render_pipeline(
            &fill_wgsl(),
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "selftest-fill",
        );
        let tex = unsafe { bridge.import_texture_native(&writer_dev, 0) };
        let params_buf = writer_dev.create_buffer_shared(24);
        let colour = frame_colour(7);
        let params = [colour[0], colour[1], colour[2], colour[3], 0.0, 1.0];
        unsafe {
            let ptr = params_buf.mapped_ptr().expect("mapped");
            std::ptr::copy_nonoverlapping(params.as_ptr() as *const u8, ptr, 24);
        }
        let sink = writer_dev.create_buffer_shared(4096);
        let buf = writer_dev.create_buffer_shared(u64::from(BYTES_PER_ROW) * u64::from(H));
        let t0 = std::time::Instant::now();
        {
            let mut enc = writer_dev.create_encoder("selftest");
            enc.draw_fullscreen(
                &fill,
                &tex,
                &[
                    manifold_gpu::GpuBinding::Buffer { binding: 0, buffer: &params_buf, offset: 0 },
                    manifold_gpu::GpuBinding::Buffer { binding: 1, buffer: &sink, offset: 0 },
                ],
                false,
                true,
                "fill",
            );
            enc.copy_texture_to_buffer(&tex, &buf, W, H, BYTES_PER_ROW);
            enc.commit_and_wait_completed();
        }
        let gpu_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let ptr = buf.mapped_ptr().expect("readback mapped");
        let px = unsafe { std::slice::from_raw_parts(ptr as *const u16, 4) };
        println!("[selftest] gpu={gpu_ms:.1}ms expected={colour:?} pixel0_f16bits={px:?}");
        std::process::exit(0);
    }

    let fill_pipeline = writer_dev.create_render_pipeline(
        &fill_wgsl(),
        "vs_main",
        "fs_main",
        manifold_gpu::GpuTextureFormat::Rgba16Float,
        None,
        "probe-fill",
    );
    let sample_pipeline = reader_dev.create_render_pipeline(
        &sample_wgsl(),
        "vs_main",
        "fs_main",
        manifold_gpu::GpuTextureFormat::Rgba16Float,
        None,
        "probe-sample",
    );
    let writer_heavy = writer_dev.create_compute_pipeline(HEAVY_WGSL, "heavy", "probe-heavy-w");

    // SAFETY: bridge outlives all imported textures (Arc held to the end).
    let writer_tex: Vec<manifold_gpu::GpuTexture> = (0..SURFACE_COUNT)
        .map(|i| unsafe { bridge.import_texture_native(&writer_dev, i) })
        .collect();
    let reader_tex: Vec<manifold_gpu::GpuTexture> = (0..SURFACE_COUNT)
        .map(|i| unsafe { bridge.import_texture_native(&reader_dev, i) })
        .collect();

    let stop = Arc::new(AtomicBool::new(false));
    let reads_done = Arc::new(AtomicU64::new(0));
    let torn_reads = Arc::new(AtomicU64::new(0));
    // Per-slot last-written frame + completed frame (the legacy contract:
    // reuse a slot only once the frame that last wrote it has completed).
    let write_issued = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
    let write_done = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);

    let writer = {
        let (bridge, stop) = (bridge.clone(), stop.clone());
        let (write_issued, write_done) = (write_issued.clone(), write_done.clone());
        let writer_dev = writer_dev.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let sink_buf = writer_dev.create_buffer_shared(4096);
            for i in 1..=frames {
                objc2::rc::autoreleasepool(|_| {
                    let slot = ((i - 1) % SURFACE_COUNT as u64) as usize;
                    // Contract wait: the frame that last wrote this slot must
                    // be GPU-complete. Legacy = only this; fenced = plus the
                    // bridge's read-fence (front moved off + reads retired).
                    loop {
                        let issued = write_issued[slot].load(Ordering::Acquire);
                        if issued == 0 || write_done[slot].load(Ordering::Acquire) >= issued {
                            break;
                        }
                        std::thread::yield_now();
                    }
                    if fenced {
                        let spin_start = std::time::Instant::now();
                        while !bridge.is_reusable(slot) {
                            std::thread::yield_now();
                            if spin_start.elapsed() > std::time::Duration::from_secs(3) {
                                eprintln!(
                                    "[probe-w] STUCK frame {i} slot={slot} front={} in_flight={} \
                                     issued={} done={}",
                                    bridge.front_index(),
                                    bridge.debug_reads_in_flight(slot),
                                    write_issued[slot].load(Ordering::Acquire),
                                    write_done[slot].load(Ordering::Acquire),
                                );
                                break;
                            }
                        }
                    }
                    let colour = frame_colour(i);
                    let mut enc = writer_dev.create_encoder("probe-w");
                    enc.dispatch_compute(
                        &writer_heavy,
                        &[manifold_gpu::GpuBinding::Buffer {
                            binding: 0,
                            buffer: &sink_buf,
                            offset: 0,
                        }],
                        [1024, 1, 1],
                        "pad",
                    );
                    for band in 0..BANDS {
                        // Per-band params buffer, written once and never
                        // reused — a shared params buffer raced late passes
                        // of the PREVIOUS frame (measured: slot surfaces
                        // holding two frames' colours from that alone).
                        let params_buf = writer_dev.create_buffer_shared(24);
                        let params = [
                            colour[0], colour[1], colour[2], colour[3],
                            band as f32, BANDS as f32,
                        ];
                        unsafe {
                            let ptr = params_buf.mapped_ptr().expect("params mapped");
                            std::ptr::copy_nonoverlapping(params.as_ptr() as *const u8, ptr, 24);
                        }
                        enc.draw_fullscreen_viewport(
                            &fill_pipeline,
                            &writer_tex[slot],
                            &[
                                manifold_gpu::GpuBinding::Buffer {
                                    binding: 0,
                                    buffer: &params_buf,
                                    offset: 0,
                                },
                                manifold_gpu::GpuBinding::Buffer {
                                    binding: 1,
                                    buffer: &sink_buf,
                                    offset: 0,
                                },
                            ],
                            (0.0, band as f32 * (H / BANDS) as f32, W as f32, (H / BANDS) as f32),
                            manifold_gpu::GpuLoadAction::Load,
                            "fill-band",
                        );
                    }
                    write_issued[slot].store(i, Ordering::Release);
                    let bridge = bridge.clone();
                    let write_done = write_done.clone();
                    enc.add_completed_handler(move || {
                        // Publish-then-flag, mirroring production's handler.
                        bridge.publish_front(slot as u32, i);
                        write_done[slot].store(i, Ordering::Release);
                    });
                    enc.commit();
                });
            }
            stop.store(true, Ordering::Release);
            eprintln!("[bridge-probe] writer done: {frames} frames in {:.1?}", t0.elapsed());
        })
    };

    let reader = {
        let (bridge, stop) = (bridge.clone(), stop.clone());
        let (reads_done, torn_reads) = (reads_done.clone(), torn_reads.clone());
        std::thread::spawn(move || {
            // Three rotating offscreens + readback buffers, retired by their
            // own completion handlers — the reader paces itself to GPU
            // retirement instead of flooding allocations behind the writer's
            // command-buffer stream (first probe version starved: 2 reads).
            let sink_buf = reader_dev.create_buffer_shared(4096);
            let offscreens = Arc::new(
                (0..3)
                    .map(|_| {
                        reader_dev.create_texture(&manifold_gpu::GpuTextureDesc {
                            width: W,
                            height: H,
                            depth: 1,
                            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                            dimension: manifold_gpu::GpuTextureDimension::D2,
                            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                            label: "probe-offscreen",
                            mip_levels: 1,
                        })
                    })
                    .collect::<Vec<_>>(),
            );
            let read_bufs = Arc::new(
                (0..3)
                    .map(|_| reader_dev.create_buffer_shared(u64::from(BYTES_PER_ROW) * u64::from(H)))
                    .collect::<Vec<manifold_gpu::GpuBuffer>>(),
            );
            let read_issued = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
            let read_retired = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
            let mut rng: u64 = 0x9e3779b97f4a7c15;
            let mut next_rand = move || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };
            let mut n = 0u64;
            while !stop.load(Ordering::Acquire) {
                objc2::rc::autoreleasepool(|_| {
                    n += 1;
                    let rslot = ((n - 1) % 3) as usize;
                    loop {
                        let issued = read_issued[rslot].load(Ordering::Acquire);
                        if issued == 0 || read_retired[rslot].load(Ordering::Acquire) >= issued {
                            break;
                        }
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::yield_now();
                    }
                    // 60-160Hz with jitter, plus a random 0-2ms CPU delay
                    // between the front read and the encode — the live window.
                    std::thread::sleep(std::time::Duration::from_micros(
                        4_000 + next_rand() % 8_000,
                    ));
                    let lease = if fenced {
                        Some(bridge.acquire_read())
                    } else {
                        None
                    };
                    let slot = lease.map_or_else(|| bridge.front_index() as usize, |l| l.slot());
                    std::thread::sleep(std::time::Duration::from_micros(next_rand() % 2_000));
                    let mut enc = reader_dev.create_encoder("probe-r");
                    enc.draw_fullscreen(
                        &sample_pipeline,
                        &offscreens[rslot],
                        &[
                            manifold_gpu::GpuBinding::Texture {
                                binding: 0,
                                texture: &reader_tex[slot],
                            },
                            manifold_gpu::GpuBinding::Buffer {
                                binding: 1,
                                buffer: &sink_buf,
                                offset: 0,
                            },
                        ],
                        false,
                        true,
                        "sample",
                    );
                    enc.copy_texture_to_buffer(
                        &offscreens[rslot],
                        &read_bufs[rslot],
                        W,
                        H,
                        BYTES_PER_ROW,
                    );
                    read_issued[rslot].store(n, Ordering::Release);
                    let (reads_done, torn_reads) = (reads_done.clone(), torn_reads.clone());
                    let bridge = bridge.clone();
                    let (read_bufs, read_retired) = (read_bufs.clone(), read_retired.clone());
                    enc.add_completed_handler(move || {
                        let buf = &read_bufs[rslot];
                        let mut mismatch = 0u64;
                        if let Some(ptr) = buf.mapped_ptr() {
                            let px = unsafe {
                                std::slice::from_raw_parts(ptr as *const u64, (W * H) as usize)
                            };
                            let first = px[0];
                            mismatch = px.iter().filter(|&&p| p != first).count() as u64;
                        }
                        reads_done.fetch_add(1, Ordering::Relaxed);
                        if mismatch > 0 {
                            torn_reads.fetch_add(1, Ordering::Relaxed);
                            eprintln!(
                                "[bridge-probe] TORN read: slot={slot} mismatch_px={mismatch}/{}",
                                W * H
                            );
                        }
                        // Forensics on torn reads (MANIFOLD_PROBE_TORN): the
                        // per-band colour pair answers "overwrite mid-read"
                        // (slot's adjacent occupants, k and k+3) vs anything
                        // else (coherency, probe bug).
                        #[cfg(debug_assertions)]
                        if mismatch > 0 && std::env::var_os("MANIFOLD_PROBE_TORN").is_some()
                            && let Some(ptr) = buf.mapped_ptr()
                        {
                            let px = unsafe {
                                std::slice::from_raw_parts(ptr as *const u16, (W * H) as usize * 4)
                            };
                            let band_h = H as usize / BANDS as usize;
                            let mut report = String::new();
                            for b in 0..BANDS as usize {
                                let o = b * band_h * W as usize * 4;
                                let r = half::f16::from_bits(px[o]).to_f32() as u64;
                                let g = half::f16::from_bits(px[o + 1]).to_f32() as u64;
                                report.push_str(&format!(" b{b}=({r},{g})"));
                            }
                            eprintln!("[torn-forensics] slot={slot} bands:{report}");
                        }
                        if let Some(l) = lease {
                            bridge.retire_read(l);
                        }
                        read_retired[rslot].store(n, Ordering::Release);
                    });
                    enc.commit();
                });
            }
        })
    };

    writer.join().expect("writer panicked");
    // Drain: let the queued reads finish before joining the reader.
    std::thread::sleep(std::time::Duration::from_secs(2));
    stop.store(true, Ordering::Release);
    reader.join().expect("reader panicked");

    let reads = reads_done.load(Ordering::Relaxed);
    let torn = torn_reads.load(Ordering::Relaxed);
    println!(
        "[bridge-probe] reads={reads} torn={torn} ({:.1}%)",
        if reads > 0 { torn as f64 * 100.0 / reads as f64 } else { 0.0 }
    );
    if torn > 0 {
        println!("[bridge-probe] VERDICT: RACE PRESENT — reader sampled a surface mid-write");
        std::process::exit(1);
    }
    println!("[bridge-probe] VERDICT: CLEAN");
    std::process::exit(0);
}
