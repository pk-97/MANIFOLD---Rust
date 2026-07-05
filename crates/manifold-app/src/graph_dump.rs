//! Graph output dump — read every node's output texture back to the CPU and
//! write it as a 16-bit **linear** PNG (no tonemap, no gamma, just clamp
//! [0,1]→16-bit) plus a `manifest.json`, so the raw GPU output of each node can
//! be inspected as images. Authoring-only, one-shot (user clicks a button).
//!
//! PNG is the only lossless format that renders in the inspection tooling, so
//! out-of-[0,1] values (HDR, depth, flow) clip in the image — the manifest
//! records each texture's native format and per-channel min/max/mean so the
//! clipped values aren't lost. See the design discussion in the session notes.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};

/// One texture to dump: a display name (node id), the source port, the node's
/// type id, and the live texture (preserved past the frame by dump mode).
pub struct DumpTexture<'a> {
    pub name: String,
    pub port: String,
    pub type_id: String,
    pub texture: &'a GpuTexture,
}

/// Decode one pixel's bytes to linear f32 RGBA for `fmt`. `None` for formats
/// we don't dump (e.g. integer / depth-stencil) — the manifest notes those.
fn decode_pixel(fmt: GpuTextureFormat, px: &[u8]) -> Option<[f32; 4]> {
    use GpuTextureFormat::*;
    let h = |i: usize| half::f16::from_le_bytes([px[i], px[i + 1]]).to_f32();
    let f = |i: usize| f32::from_le_bytes([px[i], px[i + 1], px[i + 2], px[i + 3]]);
    let u = |i: usize| px[i] as f32 / 255.0;
    Some(match fmt {
        Rgba16Float => [h(0), h(2), h(4), h(6)],
        Rgba32Float => [f(0), f(4), f(8), f(12)],
        Rgba8Unorm | Rgba8UnormSrgb => [u(0), u(1), u(2), u(3)],
        Bgra8Unorm | Bgra8UnormSrgb => [u(2), u(1), u(0), u(3)],
        Rg16Float => [h(0), h(2), 0.0, 1.0],
        R16Float => {
            let v = h(0);
            [v, v, v, 1.0]
        }
        R32Float => {
            let v = f(0);
            [v, v, v, 1.0]
        }
        R8Unorm => {
            let v = u(0);
            [v, v, v, 1.0]
        }
        _ => return None,
    })
}

/// Read a texture back to CPU as row-major linear f32 RGBA. `None` if the
/// format isn't decodable. The blit uses a 256-byte-aligned row stride (Metal
/// requirement); we step over the padding when decoding.
fn readback_rgba(device: &GpuDevice, tex: &GpuTexture) -> Option<Vec<[f32; 4]>> {
    let bpp = tex.format.bytes_per_pixel();
    // Cheap format-support probe before we allocate/copy.
    decode_pixel(tex.format, &[0u8; 16])?;

    let tight = tex.width * bpp;
    let aligned = tight.div_ceil(256) * 256;
    let buf = device.create_buffer_shared(u64::from(aligned) * u64::from(tex.height));
    let mut enc = device.create_encoder("Graph Dump Readback");
    enc.copy_texture_to_buffer(tex, &buf, tex.width, tex.height, aligned);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr()?;
    let bytes =
        unsafe { std::slice::from_raw_parts(ptr, (aligned as usize) * (tex.height as usize)) };
    let mut out = Vec::with_capacity((tex.width * tex.height) as usize);
    for y in 0..tex.height {
        let row = &bytes[(y * aligned) as usize..];
        for x in 0..tex.width {
            let off = (x * bpp) as usize;
            out.push(decode_pixel(tex.format, &row[off..])?);
        }
    }
    Some(out)
}

/// Per-channel statistics over a decoded image.
struct Stats {
    min: [f32; 4],
    max: [f32; 4],
    mean: [f32; 4],
}

fn compute_stats(pixels: &[[f32; 4]]) -> Stats {
    let mut min = [f32::INFINITY; 4];
    let mut max = [f32::NEG_INFINITY; 4];
    let mut sum = [0f64; 4];
    for p in pixels {
        for c in 0..4 {
            min[c] = min[c].min(p[c]);
            max[c] = max[c].max(p[c]);
            sum[c] += f64::from(p[c]);
        }
    }
    let n = pixels.len().max(1) as f64;
    Stats {
        min,
        max,
        mean: std::array::from_fn(|c| (sum[c] / n) as f32),
    }
}

/// Write a 16-bit RGBA PNG. `data` is row-major RGBA u16 (length w*h*4).
fn write_png16(path: &Path, w: u32, h: u32, data: &[u16]) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Sixteen);
    let mut writer = encoder
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    // PNG stores 16-bit samples big-endian.
    let mut bytes = Vec::with_capacity(data.len() * 2);
    for &v in data {
        bytes.extend_from_slice(&v.to_be_bytes());
    }
    writer
        .write_image_data(&bytes)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Make a filesystem-safe slug from a node name.
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// Dump every texture in `textures` to `dir` as a 16-bit linear PNG, plus a
/// `manifest.json` mapping each file to its node / port / format / stats.
/// Returns the directory written. Errors are best-effort per texture (a
/// failed one is noted in the manifest, not fatal).
pub fn write_graph_dump(
    device: &GpuDevice,
    textures: &[DumpTexture<'_>],
    dir: &Path,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let mut entries: Vec<serde_json::Value> = Vec::with_capacity(textures.len());

    for (i, t) in textures.iter().enumerate() {
        let fmt = format!("{:?}", t.texture.format);
        let (w, h) = (t.texture.width, t.texture.height);
        let Some(pixels) = readback_rgba(device, t.texture) else {
            entries.push(serde_json::json!({
                "node": t.name, "port": t.port, "type_id": t.type_id,
                "format": fmt, "width": w, "height": h,
                "note": "format not decodable to RGBA — not dumped",
            }));
            continue;
        };
        let stats = compute_stats(&pixels);

        let file = format!("{:02}_{}__{}.png", i, slug(&t.name), slug(&t.port));
        let mut u16buf = Vec::with_capacity(pixels.len() * 4);
        for p in &pixels {
            for &v in p {
                u16buf.push((v.clamp(0.0, 1.0) * 65535.0).round() as u16);
            }
        }
        let entry = serde_json::json!({
            "file": file, "node": t.name, "port": t.port, "type_id": t.type_id,
            "format": fmt, "width": w, "height": h,
            // Linear values, before the [0,1] clamp the PNG applies. So a max
            // above 1.0 means the PNG clipped that channel to white.
            "min": stats.min, "max": stats.max, "mean": stats.mean,
        });
        match write_png16(&dir.join(&file), w, h, &u16buf) {
            Ok(()) => entries.push(entry),
            Err(e) => {
                log::warn!("[graph-dump] failed to write {file}: {e}");
                entries.push(serde_json::json!({
                    "node": t.name, "port": t.port, "format": fmt,
                    "note": format!("png write failed: {e}"),
                }));
            }
        }
    }

    let manifest = serde_json::json!({
        "note": "16-bit linear PNGs (no tonemap/gamma); values clamped to [0,1]. \
                 min/max/mean are the raw linear values before clamping.",
        "count": entries.len(),
        "textures": entries,
    });
    let manifest_path = dir.join("manifest.json");
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    log::info!(
        "[graph-dump] wrote {} textures + manifest to {}",
        textures.len(),
        dir.display()
    );
    Ok(dir.to_path_buf())
}

/// Read one array (storage-buffer) output back to CPU and write a JSON with
/// the channel schema, per-field stats over every item, and a sample of the
/// first rows — for inspecting particle / instance / edge buffers that have no
/// image representation. Returns the per-array manifest entry.
fn dump_one_array(
    device: &GpuDevice,
    a: &manifold_renderer::compositor::ArrayDump<'_>,
) -> serde_json::Value {
    let size = a.buffer.size();
    let item_count = if a.item_size == 0 {
        0
    } else {
        (size / u64::from(a.item_size)) as usize
    };
    // Read the buffer back via a shared staging buffer.
    let staging = device.create_buffer_shared(size);
    let mut enc = device.create_encoder("Array Dump Readback");
    enc.copy_buffer_to_buffer(a.buffer, &staging, size);
    enc.commit_and_wait_completed();
    let Some(ptr) = staging.mapped_ptr() else {
        return serde_json::json!({ "node": a.name, "port": a.port, "note": "readback failed" });
    };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size as usize) };

    let rd_f32 = |o: usize| f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let rd_u32 = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let rd_i32 = |o: usize| i32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);

    // Per-field stats over all items, plus a sample of the first rows.
    let stride = a.item_size as usize;
    let comps = |kind: &str| match kind {
        "vec2f" => 2,
        "vec3f" => 3,
        "vec4f" => 4,
        _ => 1,
    };
    let mut field_stats: Vec<serde_json::Value> = Vec::new();
    for (fname, kind, off) in &a.fields {
        let n = comps(kind);
        let mut mn = vec![f64::INFINITY; n];
        let mut mx = vec![f64::NEG_INFINITY; n];
        let mut sum = vec![0f64; n];
        for i in 0..item_count {
            let base = i * stride + *off as usize;
            for (c, (mnc, mxc)) in mn.iter_mut().zip(mx.iter_mut()).enumerate() {
                let o = base + c * 4;
                if o + 4 > bytes.len() {
                    break;
                }
                let v = match *kind {
                    "u32" => f64::from(rd_u32(o)),
                    "i32" => f64::from(rd_i32(o)),
                    _ => f64::from(rd_f32(o)),
                };
                *mnc = mnc.min(v);
                *mxc = mxc.max(v);
                sum[c] += v;
            }
        }
        let denom = item_count.max(1) as f64;
        let mean: Vec<f64> = sum.iter().map(|s| s / denom).collect();
        field_stats.push(serde_json::json!({
            "name": fname, "kind": kind, "offset": off,
            "min": mn, "max": mx, "mean": mean,
        }));
    }

    // Sample the first rows (decoded per field) for spot-checking.
    let sample_n = item_count.min(16);
    let mut sample: Vec<serde_json::Value> = Vec::with_capacity(sample_n);
    for i in 0..sample_n {
        let mut row = serde_json::Map::new();
        for (fname, kind, off) in &a.fields {
            let base = i * stride + *off as usize;
            let n = comps(kind);
            let vals: Vec<f64> = (0..n)
                .map(|c| {
                    let o = base + c * 4;
                    match *kind {
                        "u32" => f64::from(rd_u32(o)),
                        "i32" => f64::from(rd_i32(o)),
                        _ => f64::from(rd_f32(o)),
                    }
                })
                .collect();
            row.insert(
                fname.clone(),
                if n == 1 {
                    serde_json::json!(vals[0])
                } else {
                    serde_json::json!(vals)
                },
            );
        }
        sample.push(serde_json::Value::Object(row));
    }

    serde_json::json!({
        "node": a.name, "port": a.port, "type_id": a.type_id,
        "item_size": a.item_size, "item_count": item_count,
        "fields": field_stats, "sample": sample,
    })
}

/// Dump every array output to `dir/arrays.json` (schema + stats + samples).
pub fn write_array_dump(
    device: &GpuDevice,
    arrays: &[manifold_renderer::compositor::ArrayDump<'_>],
    dir: &Path,
) -> std::io::Result<()> {
    if arrays.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let entries: Vec<serde_json::Value> =
        arrays.iter().map(|a| dump_one_array(device, a)).collect();
    let doc = serde_json::json!({
        "note": "Array (storage-buffer) outputs decoded against their channel \
                 layout. Per-field min/max/mean are over ALL items; sample is \
                 the first rows. Components for vecNf are listed in order.",
        "count": entries.len(),
        "arrays": entries,
    });
    std::fs::write(dir.join("arrays.json"), serde_json::to_vec_pretty(&doc)?)?;
    log::info!("[graph-dump] wrote {} array(s) to arrays.json", arrays.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_rgba16f_reads_half_floats() {
        // [1.0, 0.5, 0.0, 1.0] as little-endian f16 pairs.
        let mut px = Vec::new();
        for v in [1.0f32, 0.5, 0.0, 1.0] {
            px.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
        }
        let p = decode_pixel(GpuTextureFormat::Rgba16Float, &px).unwrap();
        assert!((p[0] - 1.0).abs() < 1e-3);
        assert!((p[1] - 0.5).abs() < 1e-3);
        assert!((p[2] - 0.0).abs() < 1e-3);
    }

    #[test]
    fn decode_bgra8_swaps_channels() {
        // Stored BGRA = [b=10, g=20, r=30, a=255] → RGBA [30,20,10,255]/255.
        let p = decode_pixel(GpuTextureFormat::Bgra8Unorm, &[10, 20, 30, 255]).unwrap();
        assert!((p[0] - 30.0 / 255.0).abs() < 1e-4, "r");
        assert!((p[2] - 10.0 / 255.0).abs() < 1e-4, "b");
    }

    #[test]
    fn stats_track_range_including_hdr() {
        let pixels = vec![[0.0, 0.0, 0.0, 1.0], [2.0, 0.5, 0.0, 1.0]];
        let s = compute_stats(&pixels);
        assert_eq!(s.max[0], 2.0); // HDR value preserved in stats
        assert_eq!(s.min[0], 0.0);
        assert!((s.mean[0] - 1.0).abs() < 1e-6);
    }

    /// Headless end-to-end dump of the bundled BlackHole generator. Builds it
    /// on a real device, warms up the particle sim, then dumps every node
    /// output to /tmp for inspection. GPU + slow, so `#[ignore]` — run with:
    /// `cargo test -p manifold-app --bin manifold dump_blackhole_headless \
    ///   --release -- --ignored --nocapture`
    #[test]
    #[ignore = "headless GPU dump; run manually with --ignored"]
    fn dump_blackhole_headless() {
        use manifold_gpu::{GpuDevice, GpuTextureFormat};
        use manifold_renderer::preset_runtime::PresetRuntime;
        use manifold_renderer::gpu_encoder::GpuEncoder as RGpuEncoder;
        use manifold_renderer::preset_context::PresetContext;
        use manifold_renderer::node_graph::PrimitiveRegistry;
        use manifold_renderer::render_target::RenderTarget;
        use manifold_core::params::ParamManifest;

        const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
        let (w, h) = (1280u32, 720u32);
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../manifold-renderer/assets/generator-presets/BlackHole.json"
        ))
        .expect("read BlackHole.json");

        let device = GpuDevice::new();
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_json_str_with_device(&json, &registry, &device, w, h, FMT)
                .expect("build BlackHole generator");
        let target = RenderTarget::new(&device, w, h, FMT, "dump-target");
        let params = ParamManifest::default();

        let mk_ctx = |t: f64| PresetContext {
            time: t,
            beat: t * 2.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: w as f32 / h as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // Warm up so the 800k-particle sim populates the disk (particles
        // self-seed frame 1, then spiral into the visible structure).
        for i in 0..90 {
            let mut enc = device.create_encoder("dump-warmup");
            {
                let mut gpu = RGpuEncoder::new(&mut enc, &device);
                generator.render(
                    &mut gpu,
                    &target.texture,
                    &mk_ctx(f64::from(i) / 60.0),
                    &params,
                );
            }
            enc.commit_and_wait_completed();
        }

        // Final frame with dump mode on, then read every node output back.
        generator.set_dump_all(true);
        {
            let mut enc = device.create_encoder("dump-frame");
            {
                let mut gpu = RGpuEncoder::new(&mut enc, &device);
                generator.render(&mut gpu, &target.texture, &mk_ctx(90.0 / 60.0), &params);
            }
            enc.commit_and_wait_completed();
        }

        let textures: Vec<DumpTexture> = generator
            .dump_textures_all()
            .into_iter()
            .map(|(name, port, type_id, texture)| DumpTexture {
                name,
                port,
                type_id,
                texture,
            })
            .collect();
        let arrays = generator.dump_arrays_all();
        let dir = std::path::PathBuf::from("/tmp/manifold-blackhole-dump");
        let _ = std::fs::remove_dir_all(&dir);
        write_graph_dump(&device, &textures, &dir).expect("dump textures");
        write_array_dump(&device, &arrays, &dir).expect("dump arrays");
        eprintln!(
            "DUMP_DONE: {} textures + {} arrays -> {}",
            textures.len(),
            arrays.len(),
            dir.display()
        );
    }

    /// Sweep BlackHole disk-density knobs (polar-blur `step`/`kernel_size` and
    /// splat `scaled_energy`) headless and dump the final display of each
    /// variant, so we can see which reads as a dense cloud. Run with:
    /// `cargo test -p manifold-app --bin manifold sweep_blackhole_cloud \
    ///   --release -- --ignored --nocapture`
    #[test]
    #[ignore = "headless GPU sweep; run manually with --ignored"]
    fn sweep_blackhole_cloud() {
        use manifold_gpu::{GpuDevice, GpuTextureFormat};
        use manifold_renderer::preset_runtime::PresetRuntime;
        use manifold_renderer::gpu_encoder::GpuEncoder as RGpuEncoder;
        use manifold_renderer::preset_context::PresetContext;
        use manifold_renderer::node_graph::PrimitiveRegistry;
        use manifold_renderer::render_target::RenderTarget;
        use manifold_core::params::ParamManifest;

        // Set a node param's `value` (preserving its type tag) by numeric id.
        fn set_val(doc: &mut serde_json::Value, id: u64, key: &str, v: serde_json::Value) {
            for n in doc["nodes"].as_array_mut().unwrap() {
                if n["id"].as_u64() == Some(id)
                    && let Some(p) = n["params"].get_mut(key)
                {
                    p["value"] = v.clone();
                }
            }
        }

        const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
        let (w, h) = (1280u32, 720u32);
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../manifold-renderer/assets/generator-presets/BlackHole.json"
        ))
        .unwrap();
        let base: serde_json::Value = serde_json::from_str(&json).unwrap();

        let device = GpuDevice::new();
        let registry = PrimitiveRegistry::with_builtin();
        let dir = std::path::PathBuf::from("/tmp/manifold-blackhole-sweep");
        let _ = std::fs::remove_dir_all(&dir);

        // Widen the hardcoded disk-thickness consts in the shader sources:
        // sim confinement, scatter vertical cull, deflection volume profile.
        fn thicken(doc: &mut serde_json::Value) {
            for n in doc["nodes"].as_array_mut().unwrap() {
                if let Some(src) = n["wgslSource"].as_str() {
                    let s = src
                        .replace("-pos.y * 1.5", "-pos.y * 0.3") // looser plane pull
                        .replace("0.24 * r", "0.7 * r") // wider scatter cull
                        .replace("0.12 * disk_r_xz", "0.45 * disk_r_xz"); // thicker volume
                    n["wgslSource"] = serde_json::json!(s);
                }
            }
        }

        let pd = [15u64, 16, 17, 18]; // polar-density blur nodes
        for label in ["a_baseline", "e_thick", "f_thick_blur", "g_thick_energy"] {
            let mut doc = base.clone();
            let thick = matches!(label, "e_thick" | "f_thick_blur" | "g_thick_energy");
            let wide = matches!(label, "f_thick_blur");
            let energy = matches!(label, "g_thick_energy");
            if thick {
                thicken(&mut doc);
            }
            if wide {
                for id in pd {
                    set_val(&mut doc, id, "step", serde_json::json!(6.0));
                    set_val(&mut doc, id, "kernel_size", serde_json::json!(2));
                }
            }
            if energy {
                set_val(&mut doc, 4, "scaled_energy", serde_json::json!(8192.0));
            }

            let mut generator = PresetRuntime::from_json_str_with_device(
                &doc.to_string(),
                &registry,
                &device,
                w,
                h,
                FMT,
            )
            .expect("build variant");
            let target = RenderTarget::new(&device, w, h, FMT, "sweep-target");
            let params = ParamManifest::default();
            let mk = |t: f64| PresetContext {
                time: t,
                beat: t * 2.0,
                dt: 1.0 / 60.0,
                width: w,
                height: h,
                output_width: w,
                output_height: h,
                aspect: w as f32 / h as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress: 0.0,
                trigger_count: 0,
            };
            for i in 0..90 {
                let mut enc = device.create_encoder("sweep");
                {
                    let mut gpu = RGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &mk(f64::from(i) / 60.0),
                        &params,
                    );
                }
                enc.commit_and_wait_completed();
            }
            // The render target holds the final display composite.
            let tex = [DumpTexture {
                name: label.to_string(),
                port: "display".to_string(),
                type_id: String::new(),
                texture: &target.texture,
            }];
            write_graph_dump(&device, &tex, &dir.join(label)).expect("dump variant");
            eprintln!("VARIANT_DONE: {label}");
        }
        eprintln!("SWEEP_DONE -> {}", dir.display());
    }

    /// Write a known gradient (with an HDR region > 1) so the produced PNG can
    /// be opened and visually confirmed. Linear, clamp-only.
    #[test]
    fn write_png16_produces_readable_file() {
        let (w, h) = (128u32, 64u32);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let t = (x as f32 / w as f32 * 1.5).clamp(0.0, 1.0); // right third clips
                let chan = |v: f32| (v * 65535.0).round() as u16;
                let (r, g, b) = if y < h / 2 { (t, 0.0, 0.0) } else { (t, t, t) };
                data.extend_from_slice(&[chan(r), chan(g), chan(b), 65535]);
            }
        }
        let path = std::path::Path::new("/tmp/graphdump_probe.png");
        write_png16(path, w, h, &data).expect("png write");
        assert!(path.exists());
    }
}
