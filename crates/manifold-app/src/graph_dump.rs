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
