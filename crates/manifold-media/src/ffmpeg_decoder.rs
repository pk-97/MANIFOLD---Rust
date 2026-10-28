//! Video decode off the Mac: FFmpeg decodes on the worker's CPU and swscale
//! converts each frame to sRGB-encoded RGBA fitted inside the clip's canvas.
//! The content thread uploads it into an `Rgba8UnormSrgb` texture, so
//! sampling returns linear light as the Mac's float texture does.

use std::sync::atomic::{AtomicU64, Ordering};

use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;
use ffmpeg::util::color;
use ffmpeg::{Packet, Rational, codec, frame, media};
use ffmpeg_next as ffmpeg;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, GpuTextureUsage};

use crate::decoder::{DecodeStatus, DecoderBackend, DecoderError, VideoDecoder, VideoMetadata};

/// Opens files through FFmpeg.
pub struct FfmpegBackend;

impl FfmpegBackend {
    pub fn new() -> Result<Self, DecoderError> {
        ffmpeg::init().map_err(|e| {
            log::error!("[FFmpeg] init: {e}");
            DecoderError::Unavailable
        })?;
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
        Ok(Self)
    }
}

impl DecoderBackend for FfmpegBackend {
    fn open(
        &self,
        path: &str,
        width: u32,
        height: u32,
    ) -> Result<Box<dyn VideoDecoder>, DecoderError> {
        Ok(Box::new(FfmpegDecoder::open(path, width, height)?))
    }

    fn target_format(&self) -> GpuTextureFormat {
        GpuTextureFormat::Rgba8UnormSrgb
    }

    fn target_usage(&self) -> GpuTextureUsage {
        GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD
    }
}

fn pack(width: u32, height: u32) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}

fn seconds(r: Rational) -> f64 {
    f64::from(r.numerator()) / f64::from(r.denominator().max(1))
}

/// Where a `src_w × src_h` frame sits inside a `dst_w × dst_h` canvas,
/// aspect kept: `(width, height, x, y)`.
fn fit_inside(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> (u32, u32, u32, u32) {
    let src_aspect = src_w as f64 / src_h as f64;
    let dst_aspect = dst_w as f64 / dst_h as f64;
    let (w, h) = if src_aspect > dst_aspect {
        (
            dst_w,
            ((dst_w as f64 / src_aspect).round() as u32).clamp(1, dst_h),
        )
    } else {
        (
            ((dst_h as f64 * src_aspect).round() as u32).clamp(1, dst_w),
            dst_h,
        )
    };
    (w, h, (dst_w - w) / 2, (dst_h - h) / 2)
}

/// The swscale matrix for a frame: its tag, else BT.601 up to 576 lines
/// and BT.709 above, as the Mac decides.
fn sws_matrix(space: color::Space, height: u32) -> i32 {
    match space {
        color::Space::BT709 => ffmpeg::ffi::SWS_CS_ITU709,
        color::Space::BT470BG | color::Space::SMPTE170M => ffmpeg::ffi::SWS_CS_ITU601,
        color::Space::BT2020NCL | color::Space::BT2020CL => ffmpeg::ffi::SWS_CS_BT2020,
        _ if height <= 576 => ffmpeg::ffi::SWS_CS_ITU601,
        _ => ffmpeg::ffi::SWS_CS_ITU709,
    }
}

/// The swscale context for one source format and size into one fitted size.
struct Scaler {
    context: scaling::Context,
    key: (Pixel, u32, u32, u32, u32),
}

/// One file: its demuxer, decoder and the current frame fitted into the
/// canvas.
struct FfmpegDecoder {
    input: ffmpeg::format::context::Input,
    stream: usize,
    /// Seconds per timestamp unit, and the stream's first timestamp.
    time_base: f64,
    start: i64,
    decoder: ffmpeg::decoder::Video,
    duration: f32,
    frame_rate: f32,
    eof_sent: bool,
    decoded: frame::Video,
    fitted: frame::Video,
    scaler: Option<Scaler>,
    frame_time: f32,
    /// The current frame, RGBA8 at `canvas_size`, transparent around it.
    canvas: Vec<u8>,
    canvas_size: (u32, u32),
    /// The canvas size the content thread last copied into, packed by
    /// `pack`; the next conversion uses it.
    wanted: AtomicU64,
}

// Every FFmpeg object here belongs to this decoder and one thread uses it
// at a time: its worker, or the content thread between jobs, which reads
// only `canvas` and `wanted`.
unsafe impl Send for FfmpegDecoder {}

impl FfmpegDecoder {
    fn open(path: &str, width: u32, height: u32) -> Result<Self, DecoderError> {
        let input = ffmpeg::format::input(path).map_err(|e| {
            log::warn!("[FFmpeg] open {path}: {e}");
            DecoderError::OpenFailed
        })?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or(DecoderError::NoVideoTrack)?;
        let mut context = codec::context::Context::from_parameters(stream.parameters())
            .map_err(|_| DecoderError::OpenFailed)?;
        context.set_threading(codec::threading::Config {
            kind: codec::threading::Type::Frame,
            count: 0,
        });
        let decoder = context
            .decoder()
            .video()
            .map_err(|_| DecoderError::OpenFailed)?;
        let time_base = seconds(stream.time_base());
        let start = match stream.start_time() {
            ffmpeg::ffi::AV_NOPTS_VALUE => 0,
            start => start,
        };
        let duration = if stream.duration() > 0 {
            stream.duration() as f64 * time_base
        } else {
            input.duration().max(0) as f64 / ffmpeg::ffi::AV_TIME_BASE as f64
        };
        let rate = match seconds(stream.avg_frame_rate()) {
            r if r > 0.0 => r,
            _ => seconds(stream.rate()),
        };
        let index = stream.index();
        Ok(Self {
            input,
            stream: index,
            time_base,
            start,
            decoder,
            duration: duration as f32,
            frame_rate: if rate > 0.0 { rate as f32 } else { 30.0 },
            eof_sent: false,
            decoded: frame::Video::empty(),
            fitted: frame::Video::empty(),
            scaler: None,
            frame_time: -1.0,
            canvas: Vec::new(),
            canvas_size: (0, 0),
            wanted: AtomicU64::new(pack(width, height)),
        })
    }

    /// Decode the next frame into `decoded`; `false` at the end of the file.
    fn decode_one(&mut self) -> Result<bool, DecoderError> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => return Ok(true),
                Err(ffmpeg::Error::Eof) => return Ok(false),
                Err(ffmpeg::Error::Other {
                    errno: ffmpeg::util::error::EAGAIN,
                }) => {}
                Err(e) => {
                    log::warn!("[FFmpeg] decode: {e}");
                    return Err(DecoderError::DecodeFailed);
                }
            }
            let mut packet = Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) if packet.stream() == self.stream => {
                    match self.decoder.send_packet(&packet) {
                        // A damaged packet costs its frame, not the clip.
                        Ok(()) | Err(ffmpeg::Error::InvalidData) => {}
                        Err(e) => {
                            log::warn!("[FFmpeg] decode: {e}");
                            return Err(DecoderError::DecodeFailed);
                        }
                    }
                }
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) if !self.eof_sent => {
                    self.eof_sent = true;
                    self.decoder
                        .send_eof()
                        .map_err(|_| DecoderError::DecodeFailed)?;
                }
                Err(ffmpeg::Error::Eof) => return Ok(false),
                Err(e) => {
                    log::warn!("[FFmpeg] read: {e}");
                    return Err(DecoderError::ReaderFailed);
                }
            }
        }
    }

    /// Seconds of `decoded` from the stream's start.
    fn decoded_time(&self) -> f32 {
        let ts = self
            .decoded
            .timestamp()
            .or(self.decoded.pts())
            .unwrap_or(self.start);
        ((ts - self.start) as f64 * self.time_base) as f32
    }

    /// Convert `decoded` into `canvas`, fitted inside the wanted size.
    fn convert(&mut self) -> Result<(), DecoderError> {
        let wanted = self.wanted.load(Ordering::Relaxed);
        let (cw, ch) = ((wanted >> 32) as u32, wanted as u32);
        let (sw, sh) = (self.decoded.width(), self.decoded.height());
        if cw == 0 || ch == 0 || sw == 0 || sh == 0 {
            return Err(DecoderError::DecodeFailed);
        }
        let (fw, fh, ox, oy) = fit_inside(sw, sh, cw, ch);
        let key = (self.decoded.format(), sw, sh, fw, fh);
        if self.scaler.as_ref().is_none_or(|s| s.key != key) {
            let mut context =
                scaling::Context::get(key.0, sw, sh, Pixel::RGBA, fw, fh, scaling::Flags::BILINEAR)
                    .map_err(|_| DecoderError::ComputeFailed)?;
            let full_range = self.decoded.color_range() == color::Range::JPEG;
            // Safety: the context is valid; the coefficient tables are
            // static in libswscale.
            unsafe {
                ffmpeg::ffi::sws_setColorspaceDetails(
                    context.as_mut_ptr(),
                    ffmpeg::ffi::sws_getCoefficients(sws_matrix(self.decoded.color_space(), sh)),
                    i32::from(full_range),
                    ffmpeg::ffi::sws_getCoefficients(ffmpeg::ffi::SWS_CS_DEFAULT),
                    1,
                    0,
                    1 << 16,
                    1 << 16,
                );
            }
            self.scaler = Some(Scaler { context, key });
            self.fitted = frame::Video::empty();
        }
        let scaler = self.scaler.as_mut().expect("scaler built above");
        scaler
            .context
            .run(&self.decoded, &mut self.fitted)
            .map_err(|_| DecoderError::ComputeFailed)?;
        if self.canvas_size != (cw, ch) {
            self.canvas.clear();
            self.canvas.resize((cw * ch * 4) as usize, 0);
            self.canvas_size = (cw, ch);
        }
        let stride = self.fitted.stride(0);
        let src = self.fitted.data(0);
        let row = (fw * 4) as usize;
        for y in 0..fh as usize {
            let dst = ((oy as usize + y) * cw as usize + ox as usize) * 4;
            self.canvas[dst..dst + row].copy_from_slice(&src[y * stride..y * stride + row]);
        }
        Ok(())
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn prepare(&mut self) -> Result<(), DecoderError> {
        if !self.decode_one()? {
            return Err(DecoderError::DecodeFailed);
        }
        self.frame_time = self.decoded_time();
        self.convert()
    }

    fn seek_to(&mut self, seconds: f32) -> Result<(), DecoderError> {
        // In AV_TIME_BASE units for a seek over every stream; it lands on
        // the keyframe at or before the target.
        let target = f64::from(seconds.max(0.0)) + self.start as f64 * self.time_base;
        let ts = (target * ffmpeg::ffi::AV_TIME_BASE as f64) as i64;
        self.input
            .seek(ts, ..ts)
            .map_err(|_| DecoderError::SeekFailed)?;
        self.decoder.flush();
        self.eof_sent = false;
        // Decode forward to the frame on screen at the target.
        let frame_len = 1.0 / self.frame_rate;
        let mut decoded_any = false;
        loop {
            if !self.decode_one()? {
                if decoded_any {
                    break;
                }
                return Err(DecoderError::SeekFailed);
            }
            decoded_any = true;
            if self.decoded_time() + frame_len > seconds {
                break;
            }
        }
        // The reported time is the target inside that frame, as a reader
        // trimmed to start there reports it.
        self.frame_time = self.decoded_time().max(seconds);
        self.convert()
    }

    fn decode_next_frame(&mut self) -> Result<DecodeStatus, DecoderError> {
        if !self.decode_one()? {
            return Ok(DecodeStatus::EndOfFile);
        }
        self.frame_time = self.decoded_time();
        self.convert()?;
        Ok(DecodeStatus::FrameReady)
    }

    fn frame_time(&self) -> f32 {
        self.frame_time
    }

    fn duration(&self) -> f32 {
        self.duration
    }

    fn width(&self) -> i32 {
        self.decoder.width() as i32
    }

    fn height(&self) -> i32 {
        self.decoder.height() as i32
    }

    fn frame_rate(&self) -> f32 {
        self.frame_rate
    }

    fn copy_frame_to(&self, device: &GpuDevice, target: &GpuTexture) -> Result<(), DecoderError> {
        let size = (target.width, target.height);
        if size != self.canvas_size {
            // The clip's texture changed size: the next frame converts at
            // the new one.
            self.wanted.store(pack(size.0, size.1), Ordering::Relaxed);
            return Err(DecoderError::Resized);
        }
        let mut enc = device.create_encoder("Video frame upload");
        enc.upload_texture(target, size.0, size.1, 1, &self.canvas);
        enc.commit();
        Ok(())
    }
}

/// Duration and size of the video at `path`.
pub(crate) fn probe_metadata(path: &str) -> Option<VideoMetadata> {
    let decoder = FfmpegDecoder::open(path, 1, 1).ok()?;
    Some(VideoMetadata {
        duration: decoder.duration,
        width: decoder.width(),
        height: decoder.height(),
    })
}

#[cfg(test)]
mod tests {
    use super::fit_inside;

    #[test]
    fn frames_fit_inside_the_canvas_centred() {
        // 16:9 into a square: full width, centred rows.
        assert_eq!(fit_inside(64, 36, 64, 64), (64, 36, 0, 14));
        // 4:3 into 16:9: full height, centred columns.
        assert_eq!(fit_inside(640, 480, 1920, 1080), (1440, 1080, 240, 0));
        // A tall phone clip into 16:9.
        assert_eq!(fit_inside(1080, 1920, 1920, 1080), (608, 1080, 656, 0));
    }
}
