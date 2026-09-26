//! Bounded audio handoff with source frame positions and explicit gaps.
//!
//! Sample and descriptor rings are published in that order. The reader only
//! consumes samples described by a published block. Neither end allocates after
//! construction. Frame positions count source frames, including dropped frames;
//! they are not transport or hardware timestamps.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioBlockStamp {
    pub first_frame: u64,
    pub sample_rate: u32,
    /// Changes when the producer's sample rate changes. A replaced stream has
    /// its own lifetime; its owner supplies the source identity.
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioStreamRead {
    Samples {
        stamp: AudioBlockStamp,
        samples: usize,
    },
    /// Missing source frames in this stream's coordinate system, [first, end).
    Gap { first_frame: u64, end_frame: u64 },
    /// Invalid source format or counter overflow. Replace the stream to recover.
    InvalidInput,
}

#[derive(Clone, Copy)]
struct Block {
    stamp: AudioBlockStamp,
    frames: usize,
}

pub struct AudioStreamProducer {
    samples: ringbuf::HeapProd<f32>,
    blocks: ringbuf::HeapProd<Block>,
    channels: usize,
    rate: u32,
    generation: u64,
    next_frame: u64,
    published_end: Arc<AtomicU64>,
    invalid: Arc<AtomicBool>,
}

pub struct AudioStreamConsumer {
    samples: ringbuf::HeapCons<f32>,
    blocks: ringbuf::HeapCons<Block>,
    channels: usize,
    current: Option<Block>,
    next_frame: u64,
    published_end: Arc<AtomicU64>,
    invalid: Arc<AtomicBool>,
}

/// Allocate one SPSC stream. Configuration is checked before audio starts.
/// A zero initial rate is allowed for a tap awaiting its device initialization;
/// it must call `set_sample_rate` before writing samples.
pub fn audio_stream(
    channels: usize,
    frame_capacity: usize,
    block_capacity: usize,
    sample_rate: u32,
) -> (AudioStreamProducer, AudioStreamConsumer) {
    assert!(channels > 0 && frame_capacity > 0 && block_capacity > 0);
    let capacity = frame_capacity
        .checked_mul(channels)
        .expect("audio capacity overflow");
    let (samples, sample_reader) = HeapRb::new(capacity).split();
    let (blocks, block_reader) = HeapRb::new(block_capacity).split();
    let published_end = Arc::new(AtomicU64::new(0));
    let invalid = Arc::new(AtomicBool::new(false));
    (
        AudioStreamProducer {
            samples,
            blocks,
            channels,
            rate: sample_rate,
            generation: 0,
            next_frame: 0,
            published_end: published_end.clone(),
            invalid: invalid.clone(),
        },
        AudioStreamConsumer {
            samples: sample_reader,
            blocks: block_reader,
            channels,
            current: None,
            next_frame: 0,
            published_end,
            invalid,
        },
    )
}

impl AudioStreamProducer {
    /// Available whole frames for synthetic producers that can defer generating
    /// audio. Live callbacks still submit every source frame to report loss.
    pub fn vacant_frames(&self) -> usize {
        if self.invalid.load(Ordering::Relaxed) || self.blocks.vacant_len() == 0 {
            0
        } else {
            self.samples.vacant_len() / self.channels
        }
    }

    /// Publish a whole-frame prefix without blocking. Returns accepted sample
    /// count; all unaccepted source frames still advance the source position.
    pub fn push_interleaved(&mut self, input: &[f32]) -> usize {
        if self.invalid.load(Ordering::Relaxed) {
            return 0;
        }
        if self.rate == 0 || !input.len().is_multiple_of(self.channels) {
            self.invalidate();
            return 0;
        }
        let frames = input.len() / self.channels;
        let Some(end) = self.next_frame.checked_add(frames as u64) else {
            self.invalidate();
            return 0;
        };
        let accepted = if self.blocks.vacant_len() == 0 {
            0
        } else {
            frames.min(self.samples.vacant_len() / self.channels)
        };
        if accepted > 0 {
            // The sole producer reserves both capacities above. Samples become
            // visible before the descriptor; its release publishes both.
            self.samples.push_slice(&input[..accepted * self.channels]);
            let block = Block {
                stamp: AudioBlockStamp {
                    first_frame: self.next_frame,
                    sample_rate: self.rate,
                    generation: self.generation,
                },
                frames: accepted,
            };
            if self.blocks.try_push(block).is_err() {
                self.invalidate();
                return 0;
            }
        }
        self.next_frame = end;
        self.published_end.store(end, Ordering::Release);
        accepted * self.channels
    }

    /// Propagate a gap from an upstream stream (for example capture → downmix).
    pub fn skip_frames(&mut self, count: u64) {
        if let Some(end) = self.next_frame.checked_add(count) {
            self.next_frame = end;
            self.published_end.store(end, Ordering::Release);
        } else {
            self.invalidate();
        }
    }

    pub fn set_sample_rate(&mut self, rate: u32) {
        if rate == 0 {
            self.invalidate();
            return;
        }
        if rate != self.rate {
            let Some(generation) = self.generation.checked_add(1) else {
                self.invalidate();
                return;
            };
            self.generation = generation;
            self.rate = rate;
        }
    }

    pub fn invalidate(&mut self) {
        self.invalid.store(true, Ordering::Release);
    }
}

impl AudioStreamConsumer {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Copy a whole-frame part of the next contiguous block. Gaps are returned
    /// before any post-gap audio, including a dropped tail with no later block.
    /// The caller must stop on `InvalidInput` and replace the source stream.
    pub fn read(&mut self, output: &mut [f32]) -> Option<AudioStreamRead> {
        assert!(
            output.len() >= self.channels,
            "audio scratch must hold a frame"
        );
        if self.invalid.load(Ordering::Acquire) {
            return Some(AudioStreamRead::InvalidInput);
        }
        // Read the watermark BEFORE testing the descriptor queue. Otherwise a
        // newly published block could be misclassified as missing audio.
        let published_end = self.published_end.load(Ordering::Acquire);
        if self.current.is_none() {
            self.current = self.blocks.try_pop();
        }
        if let Some(block) = self.current.as_mut() {
            if self.next_frame < block.stamp.first_frame {
                let first_frame = self.next_frame;
                self.next_frame = block.stamp.first_frame;
                return Some(AudioStreamRead::Gap {
                    first_frame,
                    end_frame: self.next_frame,
                });
            }
            let frames = block.frames.min(output.len() / self.channels);
            let count = frames * self.channels;
            let got = self.samples.pop_slice(&mut output[..count]);
            if got != count {
                self.invalid.store(true, Ordering::Release);
                return Some(AudioStreamRead::InvalidInput);
            }
            let stamp = block.stamp;
            block.frames -= frames;
            block.stamp.first_frame += frames as u64;
            self.next_frame = block.stamp.first_frame;
            if block.frames == 0 {
                self.current = None;
            }
            Some(AudioStreamRead::Samples {
                stamp,
                samples: count,
            })
        } else if self.next_frame < published_end {
            let first_frame = self.next_frame;
            self.next_frame = published_end;
            Some(AudioStreamRead::Gap {
                first_frame,
                end_frame: published_end,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_reads_preserve_interleaving_and_source_positions() {
        let (mut writer, mut reader) = audio_stream(2, 8, 4, 48_000);
        assert_eq!(writer.push_interleaved(&[1., 2., 3., 4., 5., 6.]), 6);
        let mut scratch = [0.; 5];
        assert_eq!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Samples {
                stamp: AudioBlockStamp {
                    first_frame: 0,
                    sample_rate: 48_000,
                    generation: 0
                },
                samples: 4,
            })
        );
        assert_eq!(&scratch[..4], &[1., 2., 3., 4.]);
        assert_eq!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Samples {
                stamp: AudioBlockStamp {
                    first_frame: 2,
                    sample_rate: 48_000,
                    generation: 0
                },
                samples: 2,
            })
        );
        assert_eq!(&scratch[..2], &[5., 6.]);
        assert_eq!(reader.read(&mut scratch), None);
    }

    #[test]
    fn full_sample_ring_reports_missing_tail_without_a_later_block() {
        let (mut writer, mut reader) = audio_stream(2, 2, 4, 48_000);
        assert_eq!(writer.push_interleaved(&[1.; 8]), 4);
        let mut scratch = [0.; 8];
        assert!(matches!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Samples { samples: 4, .. })
        ));
        assert_eq!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Gap {
                first_frame: 2,
                end_frame: 4
            })
        );
        assert_eq!(reader.read(&mut scratch), None);
        writer.push_interleaved(&[7., 8.]);
        assert!(matches!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Samples {
                stamp: AudioBlockStamp { first_frame: 4, .. },
                samples: 2,
            })
        ));
    }

    #[test]
    fn descriptor_exhaustion_and_upstream_loss_preserve_order() {
        let (mut writer, mut reader) = audio_stream(1, 16, 1, 44_100);
        assert_eq!(writer.push_interleaved(&[1., 2.]), 2);
        assert_eq!(writer.push_interleaved(&[3., 4., 5.]), 0);
        let mut scratch = [0.; 8];
        reader.read(&mut scratch).unwrap();
        writer.skip_frames(2);
        writer.push_interleaved(&[8.]);
        assert_eq!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Gap {
                first_frame: 2,
                end_frame: 7
            })
        );
        assert!(matches!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Samples {
                stamp: AudioBlockStamp { first_frame: 7, .. },
                samples: 1,
            })
        ));
        assert_eq!(scratch[0], 8.);
    }

    #[test]
    fn rate_changes_keep_queued_block_format_and_advance_generation() {
        let (mut writer, mut reader) = audio_stream(1, 8, 4, 0);
        writer.set_sample_rate(44_100);
        writer.push_interleaved(&[1., 2.]);
        writer.set_sample_rate(48_000);
        writer.push_interleaved(&[3.]);
        let mut scratch = [0.; 8];
        assert!(matches!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Samples {
                stamp: AudioBlockStamp {
                    first_frame: 0,
                    sample_rate: 44_100,
                    generation: 1
                },
                ..
            })
        ));
        assert!(matches!(
            reader.read(&mut scratch),
            Some(AudioStreamRead::Samples {
                stamp: AudioBlockStamp {
                    first_frame: 2,
                    sample_rate: 48_000,
                    generation: 2
                },
                ..
            })
        ));
    }

    #[test]
    fn concurrent_publication_never_mistakes_queued_audio_for_a_gap() {
        let (mut writer, mut reader) = audio_stream(2, 31, 7, 48_000);
        let producer = std::thread::spawn(move || {
            for frame in 0..10_000 {
                writer.push_interleaved(&[frame as f32, -(frame as f32)]);
                if frame % 19 == 0 {
                    std::thread::yield_now();
                }
            }
        });
        let mut scratch = [0.; 8];
        let mut next_frame = 0;
        loop {
            match reader.read(&mut scratch) {
                Some(AudioStreamRead::Samples { stamp, samples }) => {
                    assert_eq!(stamp.first_frame, next_frame);
                    for frame in scratch[..samples].chunks_exact(2) {
                        assert_eq!(frame, &[next_frame as f32, -(next_frame as f32)]);
                        next_frame += 1;
                    }
                }
                Some(AudioStreamRead::Gap {
                    first_frame,
                    end_frame,
                }) => {
                    assert_eq!(first_frame, next_frame);
                    next_frame = end_frame;
                }
                Some(AudioStreamRead::InvalidInput) => panic!("valid whole-frame stream"),
                None if producer.is_finished() => break,
                None => std::thread::yield_now(),
            }
        }
        producer.join().unwrap();
        // A producer completion can race the empty observation; one final drain
        // after join observes its final published watermark.
        while let Some(read) = reader.read(&mut scratch) {
            match read {
                AudioStreamRead::Samples { stamp, samples } => {
                    assert_eq!(stamp.first_frame, next_frame);
                    for frame in scratch[..samples].chunks_exact(2) {
                        assert_eq!(frame, &[next_frame as f32, -(next_frame as f32)]);
                        next_frame += 1;
                    }
                }
                AudioStreamRead::Gap {
                    first_frame,
                    end_frame,
                } => {
                    assert_eq!(first_frame, next_frame);
                    next_frame = end_frame;
                }
                AudioStreamRead::InvalidInput => panic!("valid whole-frame stream"),
            }
        }
        assert_eq!(next_frame, 10_000);
    }

    #[test]
    fn malformed_input_latches_instead_of_shifting_channel_alignment() {
        let (mut writer, mut reader) = audio_stream(2, 4, 2, 48_000);
        assert_eq!(writer.push_interleaved(&[1., 2., 3.]), 0);
        assert_eq!(writer.push_interleaved(&[4., 5.]), 0);
        assert_eq!(
            reader.read(&mut [0.; 8]),
            Some(AudioStreamRead::InvalidInput)
        );
    }
}
