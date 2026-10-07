use crate::runtime::preset_context::ProjectTempo;
use manifold_core::Seconds;
use manifold_core::tempo::TempoMapConverter;
use std::iter::Peekable;

use super::{Batch, Hash, Reader, TakeTime, batch_path, interpolate_time, read_record};

const TEMPO_TOLERANCE_ULPS: f64 = 8.0;

/// Validate the recorded beat-versus-transport curve against the current
/// project tempo. Only one authenticated batch is resident at a time.
pub(super) fn validate_project_tempo(
    reader: &Reader,
    project_tempo: &ProjectTempo,
) -> Result<(), String> {
    if reader.header.version < 4 || !reader.header.project_timing {
        return Err("Physics take: recorded project tempo provenance is unavailable".into());
    }
    let range = reader
        .clock_range
        .ok_or("Physics take: recorded project tempo has no completed clock range")?;

    compare_point(project_tempo, range.start)?;
    let mut clipped_previous = range.start;
    let mut reached_end = range.start == range.end;
    let mut clock_previous = reader.header.clock_origin;
    let mut previous_hash = reader.progress.header_hash;
    let mut clock_index = 0;
    let mut breakpoints = project_tempo
        .map()
        .points()
        .iter()
        .filter(|point| point.beat.0 > 0.0)
        .map(|point| {
            TempoMapConverter::beat_to_seconds_immut(
                project_tempo.map(),
                point.beat,
                project_tempo.fallback_bpm(),
            )
            .0
        })
        .peekable();

    for record in 0..reader.progress.records {
        let (batch, digest): (Batch, Hash) = read_record(&batch_path(&reader.directory, record))?;
        if batch.header_hash != reader.progress.header_hash || batch.previous_hash != previous_hash
        {
            return Err("Physics take: project timing hash chain changed after preflight".into());
        }
        if batch.clock.is_empty() {
            if reader
                .clock_index
                .get(clock_index)
                .is_some_and(|entry| entry.record == record)
            {
                return Err("Physics take: project timing index changed after preflight".into());
            }
        } else {
            let Some(indexed) = reader.clock_index.get(clock_index) else {
                return Err("Physics take: project timing index changed after preflight".into());
            };
            if indexed.record != record || indexed.digest != digest {
                return Err("Physics take: project timing changed after clock preflight".into());
            }
            clock_index += 1;
        }

        let next_clock_previous = super::validate_clock(&batch.clock, clock_previous)?;
        for point in batch.clock.iter().copied() {
            if reached_end {
                continue;
            }
            let clipped = if point.simulation.0 <= range.end.simulation.0 {
                point
            } else if clipped_previous.simulation.0 < range.end.simulation.0 {
                let width = point.simulation.0 - clipped_previous.simulation.0;
                if width <= 0.0 {
                    continue;
                }
                interpolate_time(
                    clipped_previous,
                    point,
                    (range.end.simulation.0 - clipped_previous.simulation.0) / width,
                )
            } else {
                continue;
            };
            compare_segment(project_tempo, clipped_previous, clipped, &mut breakpoints)?;
            clipped_previous = clipped;
            if clipped == range.end {
                reached_end = true;
            }
        }
        clock_previous = next_clock_previous;
        previous_hash = digest;
    }

    if previous_hash != reader.progress.last_batch_hash
        || clock_previous != reader.progress.clock_end
        || clock_index != reader.clock_index.len()
        || clipped_previous != range.end
    {
        return Err("Physics take: project timing changed after clock preflight".into());
    }
    Ok(())
}

fn compare_segment<I: Iterator<Item = f64>>(
    project_tempo: &ProjectTempo,
    previous: TakeTime,
    point: TakeTime,
    breakpoints: &mut Peekable<I>,
) -> Result<(), String> {
    compare_point(project_tempo, point)?;
    let width = point.transport.0 - previous.transport.0;
    if width <= 0.0 {
        return Ok(());
    }

    // TempoMapConverter is piecewise linear in beat space. Sampling every
    // positive current-map breakpoint catches a pulse between two recorded
    // observations even when the endpoint values happen to agree.
    while breakpoints
        .peek()
        .is_some_and(|&transport| transport <= previous.transport.0)
    {
        breakpoints.next();
    }
    while let Some(&transport) = breakpoints.peek() {
        if transport >= point.transport.0 {
            break;
        }
        let fraction = (transport - previous.transport.0) / width;
        let recorded_beat = previous.beat.0 + (point.beat.0 - previous.beat.0) * fraction;
        let current_beat = TempoMapConverter::seconds_to_beat_immut(
            project_tempo.map(),
            Seconds(transport),
            project_tempo.fallback_bpm(),
        )
        .0;
        compare_value(Seconds(transport), recorded_beat, current_beat)?;
        breakpoints.next();
    }
    Ok(())
}

fn compare_point(project_tempo: &ProjectTempo, point: TakeTime) -> Result<(), String> {
    let current_beat = TempoMapConverter::seconds_to_beat_immut(
        project_tempo.map(),
        point.transport,
        project_tempo.fallback_bpm(),
    )
    .0;
    compare_value(point.transport, point.beat.0, current_beat)
}

fn compare_value(transport: Seconds, recorded_beat: f64, current_beat: f64) -> Result<(), String> {
    let tolerance = TEMPO_TOLERANCE_ULPS
        * f64::EPSILON
        * 1.0_f64.max(recorded_beat.abs()).max(current_beat.abs());
    if (recorded_beat - current_beat).abs() <= tolerance {
        Ok(())
    } else {
        Err(format!(
            "Physics take: project tempo changed at transport {} (recorded beat {}, current beat {})",
            transport.0, recorded_beat, current_beat
        ))
    }
}
