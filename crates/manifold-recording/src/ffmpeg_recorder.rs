//! Live recording off the Mac, through FFmpeg. Each pool slot's readback
//! buffer holds the converted sRGB RGBA frame; swscale turns it into
//! limited-range BT.709 4:2:0 for H.264 (NVENC, else libx264 at its fast
//! preset), and the captured audio goes in beside it as AAC or ALAC.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ffmpeg::format::{Pixel, Sample, sample};
use ffmpeg::software::scaling;
use ffmpeg::{ChannelLayout, Rational, codec, encoder, format, frame};
use ffmpeg_next as ffmpeg;
use manifold_gpu::GpuDevice;
use manifold_media::ffmpeg_encoder::{H264, open_h264, write_packets};

use crate::config::{AudioCodec, LiveRecordingConfig};
use crate::texture_pool::PoolSlot;

/// Audio sample-frames that never reached the file, readable from the
/// content thread while the recording thread encodes.
pub(crate) struct AudioDrops(Arc<AtomicU32>);

impl AudioDrops {
    pub(crate) fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
}

/// The audio track: its encoder and the samples waiting for a full frame.
struct AudioTrack {
    encoder: encoder::Audio,
    time_base: Rational,
    stream_time_base: Rational,
    frame: frame::Audio,
    frame_size: usize,
    /// One queue per channel, starting at sample `pending_start`.
    pending: Vec<Vec<f32>>,
    pending_start: i64,
    sample_rate: f64,
}

pub(crate) struct FfmpegRecorder {
    output: format::context::Output,
    video: encoder::Video,
    video_time_base: Rational,
    video_stream_time_base: Rational,
    scaler: scaling::Context,
    frame: frame::Video,
    /// Frames per second, to turn elapsed seconds into frame ticks.
    frame_rate: f64,
    last_pts: Option<i64>,
    width: u32,
    height: u32,
    audio: Option<AudioTrack>,
    audio_drops: Arc<AtomicU32>,
    frames_encoded: u32,
}

// Made on the content thread, then used only on the recording thread.
// FFmpeg's contexts have no thread affinity.
unsafe impl Send for FfmpegRecorder {}

impl FfmpegRecorder {
    /// Open `config.output_path` for `width × height` video at `fps`, with
    /// an audio track when there is audio to record.
    pub(crate) fn new(
        config: &LiveRecordingConfig,
        _device: &GpuDevice,
        width: u32,
        height: u32,
        fps: f32,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, String> {
        if config.hdr {
            return Err("HDR recording is Mac-only so far".into());
        }
        let path = &config.output_path;
        let mut output = format::output(path).map_err(|e| format!("create {path}: {e}"))?;
        let global_header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let H264 {
            name,
            codec,
            encoder: video,
            time_base: video_time_base,
            bitrate,
        } = open_h264(width, height, fps, global_header, true)
            .map_err(|e| format!("open H.264: {e}"))?;
        let mut stream = output
            .add_stream(codec)
            .map_err(|e| format!("video stream: {e}"))?;
        stream.set_parameters(&video);
        stream.set_time_base(video_time_base);

        let audio = (sample_rate > 0 && channels > 0)
            .then(|| {
                open_audio(
                    &mut output,
                    config.audio_codec,
                    sample_rate,
                    channels,
                    global_header,
                )
            })
            .transpose()?;

        output.write_header().map_err(|e| format!("header: {e}"))?;
        let video_stream_time_base = output.stream(0).expect("the video stream").time_base();
        let audio = audio.map(|mut track| {
            track.stream_time_base = output.stream(1).expect("the audio stream").time_base();
            track
        });

        let mut scaler = scaling::Context::get(
            Pixel::RGBA,
            width,
            height,
            Pixel::YUV420P,
            width,
            height,
            scaling::Flags::BILINEAR,
        )
        .map_err(|e| format!("swscale: {e}"))?;
        // The pool holds full-range sRGB; the file is limited-range BT.709.
        // Safety: the context is valid; the coefficient tables are static in
        // libswscale.
        unsafe {
            let bt709 = ffmpeg::ffi::sws_getCoefficients(ffmpeg::ffi::SWS_CS_ITU709);
            ffmpeg::ffi::sws_setColorspaceDetails(
                scaler.as_mut_ptr(),
                bt709,
                1,
                bt709,
                0,
                0,
                1 << 16,
                1 << 16,
            );
        }

        log::info!(
            "[FFmpegRecorder] {name} {width}x{height} @ {fps} fps, {:.0} Mbps, audio {} -> {path}",
            bitrate as f64 / 1e6,
            if audio.is_some() { "on" } else { "off" },
        );
        Ok(Self {
            output,
            video,
            video_time_base,
            video_stream_time_base,
            scaler,
            frame: frame::Video::new(Pixel::YUV420P, width, height),
            frame_rate: f64::from(video_time_base.denominator())
                / f64::from(video_time_base.numerator()),
            last_pts: None,
            width,
            height,
            audio,
            audio_drops: Arc::new(AtomicU32::new(0)),
            frames_encoded: 0,
        })
    }

    pub(crate) fn audio_drops(&self) -> AudioDrops {
        AudioDrops(Arc::clone(&self.audio_drops))
    }

    /// Encode the frame in `slot` at `elapsed` seconds since the start. A
    /// frame that lands on the previous one's tick moves to the next tick.
    pub(crate) fn encode_video(&mut self, slot: &PoolSlot, elapsed: f64) -> Result<(), String> {
        let pts = ((elapsed * self.frame_rate).round() as i64)
            .max(self.last_pts.map_or(0, |last| last + 1));
        // Safety: the frame is a valid, allocated video frame; the encoder
        // may still hold its previous buffers.
        if unsafe { ffmpeg::ffi::av_frame_make_writable(self.frame.as_mut_ptr()) } < 0 {
            return Err("frame not writable".into());
        }
        let src = [slot.pixels];
        let src_stride = [(self.width * 4) as i32];
        // Safety: `slot.pixels` is a mapped `width × height` RGBA buffer the
        // GPU finished writing (the caller waited on its fence), and the
        // frame's planes are allocated for this size.
        let rows = unsafe {
            let dst = self.frame.as_mut_ptr();
            ffmpeg::ffi::sws_scale(
                self.scaler.as_mut_ptr(),
                src.as_ptr(),
                src_stride.as_ptr(),
                0,
                self.height as i32,
                (*dst).data.as_ptr(),
                (*dst).linesize.as_ptr(),
            )
        };
        if rows != self.height as i32 {
            return Err(format!("swscale wrote {rows} of {} rows", self.height));
        }
        self.frame.set_pts(Some(pts));
        self.video
            .send_frame(&self.frame)
            .map_err(|e| format!("send frame: {e}"))?;
        self.last_pts = Some(pts);
        self.write_video_packets()?;
        self.frames_encoded += 1;
        Ok(())
    }

    fn write_video_packets(&mut self) -> Result<(), String> {
        write_packets(
            &mut self.video,
            &mut self.output,
            0,
            self.video_time_base,
            self.video_stream_time_base,
        )
        .map_err(|e| format!("write video: {e}"))
    }

    /// Queue interleaved `samples` that start `elapsed` seconds in, and
    /// encode every full frame. A gap since the last block is filled with
    /// silence so the audio keeps its place against the video.
    pub(crate) fn write_audio(&mut self, samples: &[f32], elapsed: f64) -> Result<(), String> {
        let Some(track) = self.audio.as_mut() else {
            return Ok(());
        };
        let channels = track.pending.len();
        let start = (elapsed * track.sample_rate).round() as i64;
        let end = track.pending_start + track.pending[0].len() as i64;
        let gap = (start - end).max(0) as usize;
        let overlap = (end - start).max(0) as usize;
        for queue in &mut track.pending {
            queue.resize(queue.len() + gap, 0.0);
        }
        for block in samples.chunks_exact(channels).skip(overlap) {
            for (queue, &sample) in track.pending.iter_mut().zip(block) {
                queue.push(sample);
            }
        }
        while track.pending[0].len() >= track.frame_size {
            let n = track.frame_size;
            if let Err(e) = encode_audio(track, &mut self.output, n) {
                self.audio_drops.fetch_add(n as u32, Ordering::Relaxed);
                return Err(e);
            }
        }
        Ok(())
    }

    /// FFmpeg encodes synchronously: nothing drops after a frame is
    /// accepted.
    pub(crate) fn video_frames_append_dropped(&self) -> u32 {
        0
    }

    /// Flush both encoders and finish the file. Returns the frames written.
    pub(crate) fn finalize(mut self) -> Result<u32, String> {
        self.video
            .send_eof()
            .map_err(|e| format!("video eof: {e}"))?;
        self.write_video_packets()?;
        if let Some(track) = self.audio.as_mut() {
            let rest = track.pending[0].len();
            if rest > 0 {
                encode_audio(track, &mut self.output, rest)?;
            }
            track
                .encoder
                .send_eof()
                .map_err(|e| format!("audio eof: {e}"))?;
            write_packets(
                &mut track.encoder,
                &mut self.output,
                1,
                track.time_base,
                track.stream_time_base,
            )
            .map_err(|e| format!("write audio: {e}"))?;
        }
        self.output
            .write_trailer()
            .map_err(|e| format!("trailer: {e}"))?;
        Ok(self.frames_encoded)
    }
}

/// Open the audio encoder and add its stream: AAC at 320 kbps, or ALAC at
/// 24 bits.
fn open_audio(
    output: &mut format::context::Output,
    codec: AudioCodec,
    sample_rate: u32,
    channels: u16,
    global_header: bool,
) -> Result<AudioTrack, String> {
    let (name, sample_format, bit_rate) = match codec {
        AudioCodec::Aac => ("aac", Sample::F32(sample::Type::Planar), Some(320_000)),
        AudioCodec::Alac => ("alac", Sample::I32(sample::Type::Planar), None),
    };
    let codec = encoder::find_by_name(name).ok_or(format!("no {name} encoder"))?;
    let mut audio = codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()
        .map_err(|e| format!("{name}: {e}"))?;
    let time_base = Rational::new(1, sample_rate as i32);
    let layout = ChannelLayout::default(i32::from(channels));
    audio.set_rate(sample_rate as i32);
    audio.set_format(sample_format);
    audio.set_channel_layout(layout);
    audio.set_time_base(time_base);
    if let Some(bit_rate) = bit_rate {
        audio.set_bit_rate(bit_rate);
    }
    if global_header {
        audio.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let encoder = audio
        .open_as(codec)
        .map_err(|e| format!("open {name}: {e}"))?;
    let mut stream = output
        .add_stream(codec)
        .map_err(|e| format!("audio stream: {e}"))?;
    stream.set_parameters(&encoder);
    stream.set_time_base(time_base);
    let frame_size = match encoder.frame_size() {
        0 => 4096,
        n => n as usize,
    };
    let mut frame = frame::Audio::new(sample_format, frame_size, layout);
    frame.set_rate(sample_rate);
    Ok(AudioTrack {
        encoder,
        time_base,
        stream_time_base: time_base,
        frame,
        frame_size,
        pending: vec![Vec::with_capacity(frame_size * 2); usize::from(channels)],
        pending_start: 0,
        sample_rate: f64::from(sample_rate),
    })
}

/// Encode the first `n` queued samples of each channel as one frame.
fn encode_audio(
    track: &mut AudioTrack,
    output: &mut format::context::Output,
    n: usize,
) -> Result<(), String> {
    // Safety: the frame is a valid, allocated audio frame of at least `n`
    // samples; the encoder may still hold its previous buffers.
    if unsafe { ffmpeg::ffi::av_frame_make_writable(track.frame.as_mut_ptr()) } < 0 {
        return Err("audio frame not writable".into());
    }
    track.frame.set_samples(n);
    for (channel, queue) in track.pending.iter_mut().enumerate() {
        match track.frame.format() {
            Sample::I32(_) => {
                for (dst, &src) in track
                    .frame
                    .plane_mut::<i32>(channel)
                    .iter_mut()
                    .zip(&queue[..n])
                {
                    *dst = (f64::from(src.clamp(-1.0, 1.0)) * f64::from(i32::MAX)) as i32;
                }
            }
            _ => track
                .frame
                .plane_mut::<f32>(channel)
                .copy_from_slice(&queue[..n]),
        }
        queue.drain(..n);
    }
    track.frame.set_pts(Some(track.pending_start));
    track.pending_start += n as i64;
    track
        .encoder
        .send_frame(&track.frame)
        .map_err(|e| format!("send audio: {e}"))?;
    write_packets(
        &mut track.encoder,
        output,
        1,
        track.time_base,
        track.stream_time_base,
    )
    .map_err(|e| format!("write audio: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format_converter::POOL_FORMAT;
    use crate::texture_pool::TextureRingPool;
    use ffmpeg::media::Type;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn a_take_keeps_its_frame_times_colour_and_audio_gap() {
        let (w, h) = (64u32, 32u32);
        let dir = std::env::temp_dir().join(format!("manifold-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("take.mov");
        let config = LiveRecordingConfig {
            output_path: path.to_str().unwrap().into(),
            hdr: false,
            audio_device: None,
            audio_codec: AudioCodec::Aac,
        };
        let device = GpuDevice::new();
        let pool = TextureRingPool::new(&device, w, h, POOL_FORMAT, 1);
        let (_, slot) = pool.try_acquire().unwrap();
        let red = [255u8, 0, 0, 255].repeat((w * h) as usize);
        // Safety: the slot's readback buffer is mapped and `w * h` texels long.
        unsafe { std::ptr::copy_nonoverlapping(red.as_ptr(), slot.pixels.cast_mut(), red.len()) };

        let mut recorder = FfmpegRecorder::new(&config, &device, w, h, 10.0, 48_000, 2).unwrap();
        // The second frame at 0.1 s moves to the next free tick; 0.37 s
        // rounds to tick 4.
        for elapsed in [0.0, 0.1, 0.1, 0.37, 0.5] {
            recorder.encode_video(&slot, elapsed).unwrap();
        }
        // 0.2 s of a 440 Hz tone, a 0.2 s capture gap, then 0.2 s more.
        let tone: Vec<f32> = (0..9600)
            .flat_map(|i| {
                let v = 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin();
                [v, v]
            })
            .collect();
        recorder.write_audio(&tone, 0.0).unwrap();
        recorder.write_audio(&tone, 0.4).unwrap();
        assert_eq!(recorder.finalize().unwrap(), 5);

        let mut input = format::input(&path).unwrap();
        let video = input.streams().best(Type::Video).unwrap();
        let (video_index, video_tb) = (video.index(), video.time_base());
        let mut vdec = codec::context::Context::from_parameters(video.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        let audio = input.streams().best(Type::Audio).unwrap();
        let audio_index = audio.index();
        let mut adec = codec::context::Context::from_parameters(audio.parameters())
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        let (mut times, mut ycc, mut left) = (Vec::new(), Vec::new(), Vec::new());
        let mut receive = |vdec: &mut ffmpeg::decoder::Video, adec: &mut ffmpeg::decoder::Audio| {
            let mut frame = frame::Video::empty();
            while vdec.receive_frame(&mut frame).is_ok() {
                times.push(frame.pts().unwrap() as f64 * f64::from(video_tb));
                ycc.push([frame.data(0)[0], frame.data(1)[0], frame.data(2)[0]]);
            }
            let mut frame = frame::Audio::empty();
            while adec.receive_frame(&mut frame).is_ok() {
                left.extend_from_slice(frame.plane::<f32>(0));
            }
        };
        for (stream, packet) in input.packets() {
            if stream.index() == video_index {
                vdec.send_packet(&packet).unwrap();
            } else if stream.index() == audio_index {
                adec.send_packet(&packet).unwrap();
            }
            receive(&mut vdec, &mut adec);
        }
        vdec.send_eof().unwrap();
        adec.send_eof().unwrap();
        receive(&mut vdec, &mut adec);

        let want = [0.0, 0.1, 0.2, 0.4, 0.5];
        assert!(
            times.len() == 5 && times.iter().zip(want).all(|(t, w)| (t - w).abs() < 1e-3),
            "{times:?}"
        );
        // sRGB red in limited-range BT.709: Y 63, Cb 102, Cr 240.
        for got in &ycc {
            let near = got
                .iter()
                .zip([63u8, 102, 240])
                .all(|(&g, w)| g.abs_diff(w) <= 3);
            assert!(near, "{got:?}");
        }
        // 0.6 s of audio; the gap is silent and the tone either side keeps
        // its place. Windows leave room for the AAC frame edges.
        assert!((27_776..=31_872).contains(&left.len()), "{}", left.len());
        assert!(rms(&left[2_400..7_200]) > 0.25);
        assert!(rms(&left[12_000..16_800]) < 0.02);
        assert!(rms(&left[21_600..26_400]) > 0.25);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
