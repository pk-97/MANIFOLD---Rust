//! Export encoding off the Mac, through FFmpeg. Each frame is read back
//! from the GPU, then converted on the CPU exactly as the Mac's SDR kernel
//! does it: highlight rolloff, the sRGB OETF, limited-range BT.709 Y'CbCr
//! with each 2x2 block's chroma averaged. H.264 (NVENC when the driver has
//! it, else libx264) goes into the MP4.

use ffmpeg::format::Pixel;
use ffmpeg::util::color;
use ffmpeg::{Dictionary, Packet, Rational, codec, encoder, format, frame};
use ffmpeg_next as ffmpeg;
use manifold_gpu::{BufferUse, GpuBuffer, GpuDevice, GpuTexture, GpuTextureFormat};

use crate::encoder::EncoderError;
use crate::frame_rate::fps_to_rational;
use crate::still_exporter::{f16_to_f32, highlight_rolloff, linear_to_srgb};

/// H.264 encoders to try, best first.
const CODECS: [&str; 2] = ["h264_nvenc", "libx264"];

/// Limited-range BT.709 Y'CbCr of encoded R'G'B' in [0, 1], as 8-bit code
/// values before rounding: `manifold_bt709_limited_ycbcr` in the Mac's
/// `ColorTransferFunctions.h`.
fn bt709_limited_ycbcr([r, g, b]: [f32; 3]) -> [f32; 3] {
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    [
        16.0 + 219.0 * y,
        128.0 + 224.0 * (b - y) / 1.8556,
        128.0 + 224.0 * (r - y) / 1.5748,
    ]
}

fn code(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Encoded R'G'B' for every f16 bit pattern: negative and NaN to black,
/// highlights rolled off into white, then the sRGB OETF.
fn encoded_lut() -> Vec<f32> {
    (0..=u16::MAX)
        .map(|bits| linear_to_srgb(highlight_rolloff(f16_to_f32(bits).max(0.0))))
        .collect()
}

/// Convert packed linear `Rgba16Float` pixels into 4:2:0 planes: luma into
/// `y` (row stride `y_stride`), chroma into `cb` and `cr` (tight, half size
/// rounded up). Edge blocks of an odd size repeat the last row or column.
fn write_ycbcr420(
    src: &[u16],
    width: usize,
    height: usize,
    lut: &[f32],
    y: &mut [u8],
    y_stride: usize,
    cb: &mut [u8],
    cr: &mut [u8],
) {
    let chroma_w = width.div_ceil(2);
    for cy in 0..height.div_ceil(2) {
        for cx in 0..chroma_w {
            let (mut sum_cb, mut sum_cr) = (0.0, 0.0);
            for i in 0..4 {
                let (px, py) = (cx * 2 + (i & 1), cy * 2 + (i >> 1));
                let o = (py.min(height - 1) * width + px.min(width - 1)) * 4;
                let ycc = bt709_limited_ycbcr([
                    lut[usize::from(src[o])],
                    lut[usize::from(src[o + 1])],
                    lut[usize::from(src[o + 2])],
                ]);
                sum_cb += ycc[1];
                sum_cr += ycc[2];
                if px < width && py < height {
                    y[py * y_stride + px] = code(ycc[0]);
                }
            }
            cb[cy * chroma_w + cx] = code(sum_cb / 4.0);
            cr[cy * chroma_w + cx] = code(sum_cr / 4.0);
        }
    }
}

/// An opened H.264 encoder.
pub struct H264 {
    pub name: &'static str,
    pub codec: ffmpeg::Codec,
    pub encoder: encoder::Video,
    /// One tick per frame at the requested rate.
    pub time_base: Rational,
    pub bitrate: usize,
}

/// Open the best H.264 encoder for `width × height` frames at `fps`:
/// NVENC when the driver has it, else libx264, high profile, BT.709 limited
/// range, at the Mac's rate of 0.6 bits per pixel per frame within 20 to
/// 400 Mbps. `live` picks libx264's fast preset, for encoding while a show
/// renders. `global_header` comes from the output format.
pub fn open_h264(
    width: u32,
    height: u32,
    fps: f32,
    global_header: bool,
    live: bool,
) -> Result<H264, EncoderError> {
    crate::ffmpeg_decoder::init().map_err(|_| EncoderError::CodecUnavailable)?;
    let (num, den) = fps_to_rational(fps);
    let time_base = Rational::new(den, num);
    let fps_approx = (f64::from(num) / f64::from(den)).round().max(1.0);
    let bitrate =
        (f64::from(width) * f64::from(height) * fps_approx * 0.6).clamp(20e6, 400e6) as usize;
    for name in CODECS {
        let Some(codec) = encoder::find_by_name(name) else {
            continue;
        };
        let mut video = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|_| EncoderError::CodecUnavailable)?;
        video.set_width(width);
        video.set_height(height);
        video.set_format(Pixel::YUV420P);
        video.set_time_base(time_base);
        video.set_frame_rate(Some(Rational::new(num, den)));
        video.set_bit_rate(bitrate);
        video.set_gop(fps_approx as u32);
        video.set_max_b_frames(0);
        video.set_colorspace(color::Space::BT709);
        video.set_color_range(color::Range::MPEG);
        // Safety: the context is valid; these two fields have no setter.
        unsafe {
            let ctx = video.as_mut_ptr();
            (*ctx).color_primaries = ffmpeg::ffi::AVColorPrimaries::AVCOL_PRI_BT709;
            (*ctx).color_trc = ffmpeg::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        }
        if global_header {
            video.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let mut options = Dictionary::new();
        options.set("profile", "high");
        if live && name == "libx264" {
            options.set("preset", "veryfast");
        }
        match video.open_as_with(codec, options) {
            Ok(encoder) => {
                return Ok(H264 {
                    name,
                    codec,
                    encoder,
                    time_base,
                    bitrate,
                });
            }
            Err(e) => log::info!("[FFmpegEncoder] {name} unavailable: {e}"),
        }
    }
    Err(EncoderError::CodecUnavailable)
}

/// Write every packet `encoder` has ready to `stream` of `output`. Video
/// packets without a duration last one tick, so a file's length counts its
/// last frame.
pub fn write_packets(
    encoder: &mut encoder::Encoder,
    output: &mut format::context::Output,
    stream: usize,
    encoder_time_base: Rational,
    stream_time_base: Rational,
) -> Result<(), ffmpeg::Error> {
    let mut packet = Packet::empty();
    loop {
        match encoder.receive_packet(&mut packet) {
            Ok(()) => {
                packet.set_stream(stream);
                if packet.duration() == 0 {
                    packet.set_duration(1);
                }
                packet.rescale_ts(encoder_time_base, stream_time_base);
                packet.write_interleaved(output)?;
            }
            Err(ffmpeg::Error::Eof)
            | Err(ffmpeg::Error::Other {
                errno: ffmpeg::util::error::EAGAIN,
            }) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// An export in progress: the MP4 being written and its H.264 encoder.
pub struct FfmpegEncoder {
    output: format::context::Output,
    encoder: encoder::Video,
    encoder_time_base: Rational,
    stream_time_base: Rational,
    frame: frame::Video,
    /// Chroma planes before they are copied into `frame`.
    cb: Vec<u8>,
    cr: Vec<u8>,
    readback: Option<GpuBuffer>,
    lut: Vec<f32>,
    width: u32,
    height: u32,
    frames_encoded: u32,
}

impl FfmpegEncoder {
    /// Open `output_path` for `width × height` frames at `fps`. HDR export
    /// is Mac-only so far.
    pub fn new(
        width: u32,
        height: u32,
        fps: f32,
        output_path: &str,
        hdr: bool,
        _device: Option<&GpuDevice>,
    ) -> Result<Self, EncoderError> {
        if hdr {
            return Err(EncoderError::HdrUnavailable);
        }
        crate::ffmpeg_decoder::init().map_err(|_| EncoderError::CodecUnavailable)?;
        let mut output = format::output(output_path).map_err(|e| {
            log::error!("[FFmpegEncoder] create {output_path}: {e}");
            EncoderError::FileFailed
        })?;
        let global_header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let H264 {
            name,
            codec,
            encoder,
            time_base,
            bitrate,
        } = open_h264(width, height, fps, global_header, false)?;

        let mut stream = output
            .add_stream(codec)
            .map_err(|_| EncoderError::FileFailed)?;
        stream.set_parameters(&encoder);
        stream.set_time_base(time_base);
        output.write_header().map_err(|e| {
            log::error!("[FFmpegEncoder] header: {e}");
            EncoderError::FileFailed
        })?;
        let stream_time_base = output
            .stream(0)
            .expect("the stream added above")
            .time_base();

        log::info!(
            "[FFmpegEncoder] {name} {width}x{height} @ {fps} fps, {:.0} Mbps -> {output_path}",
            bitrate as f64 / 1e6
        );
        let chroma = (width.div_ceil(2) * height.div_ceil(2)) as usize;
        Ok(Self {
            output,
            encoder,
            encoder_time_base: time_base,
            stream_time_base,
            frame: frame::Video::new(Pixel::YUV420P, width, height),
            cb: vec![0; chroma],
            cr: vec![0; chroma],
            readback: None,
            lut: encoded_lut(),
            width,
            height,
            frames_encoded: 0,
        })
    }

    /// Encode `texture`, a linear `Rgba16Float` frame of this export's size
    /// rendered on `device`, as the next frame.
    pub fn encode_frame(
        &mut self,
        device: &GpuDevice,
        texture: &GpuTexture,
    ) -> Result<(), EncoderError> {
        let (w, h) = (self.width, self.height);
        if (texture.width, texture.height, texture.format) != (w, h, GpuTextureFormat::Rgba16Float)
        {
            log::error!(
                "[FFmpegEncoder] frame is {}x{} {:?}, the export is {w}x{h} Rgba16Float",
                texture.width,
                texture.height,
                texture.format
            );
            return Err(EncoderError::NullTexture);
        }
        let row = w * 8;
        let buffer = self
            .readback
            .get_or_insert_with(|| device.create_buffer(u64::from(row * h), BufferUse::Readback));
        let mut enc = device.create_encoder("Export frame readback");
        enc.copy_texture_to_buffer(texture, buffer, w, h, row);
        enc.commit_and_wait().map_err(|e| {
            log::error!("[FFmpegEncoder] readback: {e}");
            EncoderError::ReadbackFailed
        })?;
        let ptr = buffer.mapped_ptr().ok_or(EncoderError::ReadbackFailed)?;
        // Safety: the readback buffer is mapped, `w * h` texels long, and the
        // copy into it has completed.
        let src = unsafe {
            std::slice::from_raw_parts(ptr.cast_const().cast::<u16>(), (w * h * 4) as usize)
        };

        // The encoder may still hold the previous frame's buffers.
        // Safety: the frame is a valid, allocated video frame.
        if unsafe { ffmpeg::ffi::av_frame_make_writable(self.frame.as_mut_ptr()) } < 0 {
            return Err(EncoderError::EncodeFailed);
        }
        let y_stride = self.frame.stride(0);
        write_ycbcr420(
            src,
            w as usize,
            h as usize,
            &self.lut,
            self.frame.data_mut(0),
            y_stride,
            &mut self.cb,
            &mut self.cr,
        );
        let chroma_w = w.div_ceil(2) as usize;
        for (plane, src) in [(1, &self.cb), (2, &self.cr)] {
            let stride = self.frame.stride(plane);
            let dst = self.frame.data_mut(plane);
            for (row, line) in src.chunks_exact(chroma_w).enumerate() {
                dst[row * stride..row * stride + chroma_w].copy_from_slice(line);
            }
        }
        self.frame.set_pts(Some(i64::from(self.frames_encoded)));
        self.encoder.send_frame(&self.frame).map_err(|e| {
            log::error!("[FFmpegEncoder] frame {}: {e}", self.frames_encoded);
            EncoderError::EncodeFailed
        })?;
        self.write_packets()?;
        self.frames_encoded += 1;
        Ok(())
    }

    /// Write every packet the encoder has ready.
    fn write_packets(&mut self) -> Result<(), EncoderError> {
        write_packets(
            &mut self.encoder,
            &mut self.output,
            0,
            self.encoder_time_base,
            self.stream_time_base,
        )
        .map_err(|e| {
            log::error!("[FFmpegEncoder] write: {e}");
            EncoderError::FileFailed
        })
    }

    /// Flush the encoder and finish the file. Returns the frames encoded.
    pub fn end_session(mut self) -> Result<u32, EncoderError> {
        self.encoder
            .send_eof()
            .map_err(|_| EncoderError::EncodeFailed)?;
        self.write_packets()?;
        self.output.write_trailer().map_err(|e| {
            log::error!("[FFmpegEncoder] trailer: {e}");
            EncoderError::FileFailed
        })?;
        log::info!(
            "[FFmpegEncoder] Session complete, {} frames encoded",
            self.frames_encoded
        );
        Ok(self.frames_encoded)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frames_encoded(&self) -> u32 {
        self.frames_encoded
    }

    pub fn is_hdr(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    #[test]
    fn primaries_and_neutrals_match_the_mac_kernel() {
        // 2x2 blocks below the rolloff knee and above white, and a 3x1 strip
        // whose odd column repeats. Linear 0.5 encodes to sRGB 187.5/255.
        let lut = encoded_lut();
        let (half, one, two) = (0x3800u16, 0x3C00u16, 0x4000u16);
        let px = |r: u16, g: u16, b: u16| [r, g, b, one];
        let convert = |pixels: &[[u16; 4]], w: usize, h: usize| {
            let src: Vec<u16> = pixels.iter().flatten().copied().collect();
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let (mut y, mut cb, mut cr) = (vec![0; w * h], vec![0; cw * ch], vec![0; cw * ch]);
            write_ycbcr420(&src, w, h, &lut, &mut y, w, &mut cb, &mut cr);
            (y, cb, cr)
        };
        assert_eq!(
            convert(&[px(0, 0, 0); 4], 2, 2),
            (vec![16; 4], vec![128], vec![128])
        );
        assert_eq!(
            convert(&[px(half, half, half); 4], 2, 2),
            (vec![177; 4], vec![128], vec![128])
        );
        // Red: luma from 0.2126 of it, chroma from the BT.709 matrix.
        assert_eq!(
            convert(&[px(half, 0, 0); 4], 2, 2),
            (vec![50; 4], vec![109], vec![210])
        );
        // Linear 1.0 sits past the knee and rolls off to 230; brighter
        // light lands on white, never past it.
        assert_eq!(convert(&[px(one, one, one); 4], 2, 2).0, vec![230; 4]);
        assert_eq!(convert(&[px(two, two, two); 4], 2, 2).0, vec![235; 4]);
        let (y, cb, _) = convert(&[px(two, two, two), px(0, 0, 0), px(two, two, two)], 3, 1);
        assert_eq!(y, vec![235, 16, 235]);
        // The second block is the white column twice over.
        assert_eq!(cb, vec![128, 128]);
    }

    #[test]
    fn an_export_decodes_back_with_its_colours_and_timing() {
        let dir =
            std::env::temp_dir().join(format!("manifold_ffmpeg_export_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("red_blue.mp4");
        let path = path.to_str().unwrap();

        // 64x36, linear 0.5 red on the left and blue on the right: below
        // the rolloff knee, so each decodes to sRGB 187.5.
        let (w, h) = (64u32, 36u32);
        let (half, one) = (0x3800u16.to_le_bytes(), 0x3C00u16.to_le_bytes());
        let mut pixels = Vec::with_capacity((w * h * 8) as usize);
        for _ in 0..h {
            for x in 0..w {
                let [r, b] = if x < w / 2 {
                    [half, [0; 2]]
                } else {
                    [[0; 2], half]
                };
                pixels.extend_from_slice(&[r, [0; 2], b, one].concat());
            }
        }
        let device = GpuDevice::new();
        let texture = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "export test",
            mip_levels: 1,
        });
        device.upload_texture(&texture, &pixels);

        let mut encoder = FfmpegEncoder::new(w, h, 10.0, path, false, Some(&device)).unwrap();
        for _ in 0..5 {
            encoder.encode_frame(&device, &texture).unwrap();
        }
        assert_eq!(encoder.end_session().unwrap(), 5);

        use crate::decoder::DecoderBackend;
        let backend = crate::ffmpeg_decoder::FfmpegBackend::new().unwrap();
        let mut decoder = backend.open(path, w, h).unwrap();
        assert!(
            (decoder.duration() - 0.5).abs() < 0.02,
            "{}",
            decoder.duration()
        );
        assert!((decoder.frame_rate() - 10.0).abs() < 0.01);
        decoder.prepare().unwrap();
        // The decoded frame, sRGB-encoded: red and blue survive the trip
        // through limited-range BT.709 within a few code values.
        let target = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: backend.target_format(),
            dimension: GpuTextureDimension::D2,
            usage: backend.target_usage(),
            label: "export test decode",
            mip_levels: 1,
        });
        decoder.copy_frame_to(&device, &target).unwrap();
        let buffer = device.create_buffer(u64::from(w * h * 4), BufferUse::Readback);
        let mut enc = device.create_encoder("export test readback");
        enc.copy_texture_to_buffer(&target, &buffer, w, h, w * 4);
        enc.commit_and_wait().unwrap();
        let ptr = buffer.mapped_ptr().unwrap();
        let rgba = unsafe { std::slice::from_raw_parts(ptr.cast_const(), (w * h * 4) as usize) };
        let at = |x: u32, y: u32| &rgba[((y * w + x) * 4) as usize..][..4];
        let near =
            |got: &[u8], want: [u8; 4]| got.iter().zip(want).all(|(&g, w)| g.abs_diff(w) <= 4);
        assert!(near(at(16, 18), [188, 0, 0, 255]), "{:?}", at(16, 18));
        assert!(near(at(48, 18), [0, 0, 188, 255]), "{:?}", at(48, 18));
        let mut frames = 1;
        while decoder.decode_next_frame().unwrap() == crate::decoder::DecodeStatus::FrameReady {
            frames += 1;
        }
        assert_eq!(frames, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
