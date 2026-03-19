use std::collections::HashMap;

/// Parsed MIDI note with timing in beats (quarter notes).
/// Port of C# MidiFileParser.cs MidiNote struct.
#[derive(Debug, Clone, Copy)]
pub struct MidiNote {
    pub start_beat: f32,
    pub duration_beats: f32,
    pub pitch: i32,
    pub channel: i32,
}

impl MidiNote {
    pub fn end_beat(&self) -> f32 {
        self.start_beat + self.duration_beats
    }
}

/// Minimal Standard MIDI File (SMF) parser.
/// Extracts note-on/note-off pairs and converts ticks → beats via PPQ.
/// Supports format 0 and format 1 (merged tracks). No external dependencies.
/// Port of C# MidiFileParser.cs.
pub struct MidiFileParser;

const MIN_NOTE_DURATION_BEATS: f32 = 1.0 / 128.0;

impl MidiFileParser {
    /// Parse a .mid file from disk and return all notes as beat-domain MidiNote structs.
    pub fn parse_file(file_path: &str) -> Vec<MidiNote> {
        if file_path.is_empty() {
            return Vec::new();
        }
        match std::fs::read(file_path) {
            Ok(data) => Self::parse(&data),
            Err(_) => Vec::new(),
        }
    }

    /// Parse raw SMF bytes and return all notes as beat-domain MidiNote structs.
    pub fn parse(data: &[u8]) -> Vec<MidiNote> {
        if data.len() < 14 {
            return Vec::new();
        }

        let mut pos = 0usize;

        // --- Header chunk: "MThd" ---
        if !read_chunk_id(data, &mut pos, "MThd") {
            return Vec::new();
        }

        let header_length = read_int32_be(data, &mut pos);
        let header_end = pos + header_length as usize;

        let format = read_int16_be(data, &mut pos);
        let track_count = read_int16_be(data, &mut pos);
        let division = read_int16_be(data, &mut pos);

        // Only support ticks-per-quarter-note division (bit 15 = 0).
        // SMPTE time division (bit 15 = 1) is rare and not needed for Ableton export.
        if (division & 0x8000) != 0 {
            return Vec::new();
        }

        let ppq = division;
        if ppq <= 0 {
            return Vec::new();
        }

        let _ = format; // format 0 and 1 both handled by iterating tracks

        pos = header_end;

        // --- Track chunks ---
        let mut all_notes: Vec<MidiNote> = Vec::new();

        let mut t = 0;
        while t < track_count && pos < data.len() {
            if !read_chunk_id(data, &mut pos, "MTrk") {
                // Skip unknown chunks
                if pos + 4 <= data.len() {
                    let skip_len = read_int32_be(data, &mut pos) as usize;
                    pos += skip_len;
                }
                t += 1;
                continue;
            }

            let track_length = read_int32_be(data, &mut pos) as usize;
            let track_end = pos + track_length;

            parse_track(data, &mut pos, track_end, ppq, &mut all_notes);

            // Ensure we advance to the declared track end
            pos = track_end;

            t += 1;
        }

        // Sort by start beat, then by pitch for stability
        all_notes.sort_by(compare_notes_by_start_beat);
        all_notes
    }
}

fn parse_track(data: &[u8], pos: &mut usize, track_end: usize, ppq: i32, output: &mut Vec<MidiNote>) {
    let mut absolute_tick: i64 = 0;
    let mut running_status: u8 = 0;

    // Active note-on tracking: key = (channel << 8) | pitch
    // Value = absolute tick of note-on
    let mut active_notes: HashMap<i32, i64> = HashMap::new();

    while *pos < track_end {
        // Delta time (variable-length quantity)
        let delta = read_variable_length(data, pos);
        absolute_tick += delta;

        if *pos >= track_end {
            break;
        }

        let status_byte = data[*pos];

        // Meta event
        if status_byte == 0xFF {
            *pos += 1; // skip 0xFF
            if *pos >= track_end {
                break;
            }
            *pos += 1; // skip meta type byte
            let meta_len = read_variable_length(data, pos) as usize;
            *pos += meta_len; // skip meta data
            continue;
        }

        // SysEx event
        if status_byte == 0xF0 || status_byte == 0xF7 {
            *pos += 1; // skip status
            let sysex_len = read_variable_length(data, pos) as usize;
            *pos += sysex_len;
            continue;
        }

        // Channel message
        let status: u8;
        if (status_byte & 0x80) != 0 {
            status = status_byte;
            running_status = status_byte;
            *pos += 1;
        } else {
            // Running status — reuse previous status byte
            status = running_status;
        }

        let message_type = status & 0xF0;
        let channel = (status & 0x0F) as i32;

        match message_type {
            0x80 => {
                // Note Off
                if *pos + 1 >= track_end {
                    *pos = track_end;
                    break;
                }
                let pitch = data[*pos] as i32;
                *pos += 1;
                *pos += 1; // velocity (ignored)

                let key = (channel << 8) | pitch;
                if let Some(on_tick) = active_notes.remove(&key) {
                    emit_note(output, on_tick, absolute_tick, pitch, channel, ppq);
                }
            }

            0x90 => {
                // Note On
                if *pos + 1 >= track_end {
                    *pos = track_end;
                    break;
                }
                let pitch = data[*pos] as i32;
                *pos += 1;
                let velocity = data[*pos] as i32;
                *pos += 1;

                let key = (channel << 8) | pitch;

                if velocity == 0 {
                    // Note-on with velocity 0 = note-off
                    if let Some(on_tick) = active_notes.remove(&key) {
                        emit_note(output, on_tick, absolute_tick, pitch, channel, ppq);
                    }
                } else {
                    // Close any existing note for this pitch+channel before opening new one
                    if let Some(existing_on_tick) = active_notes.remove(&key) {
                        emit_note(output, existing_on_tick, absolute_tick, pitch, channel, ppq);
                    }
                    active_notes.insert(key, absolute_tick);
                }
            }

            0xA0 => {
                // Aftertouch
                *pos += 2;
            }

            0xB0 => {
                // Control Change
                *pos += 2;
            }

            0xC0 => {
                // Program Change
                *pos += 1;
            }

            0xD0 => {
                // Channel Pressure
                *pos += 1;
            }

            0xE0 => {
                // Pitch Bend
                *pos += 2;
            }

            _ => {}
        }
    }

    // Close any orphan notes (note-on without matching note-off).
    // Extend them to the last tick we processed.
    for (key, on_tick) in active_notes {
        let pitch = key & 0xFF;
        let channel = (key >> 8) & 0xFF;
        emit_note(output, on_tick, absolute_tick, pitch, channel, ppq);
    }
}

fn emit_note(output: &mut Vec<MidiNote>, on_tick: i64, off_tick: i64, pitch: i32, channel: i32, ppq: i32) {
    let start_beat = on_tick as f32 / ppq as f32;
    let duration_beats = (off_tick - on_tick) as f32 / ppq as f32;

    if duration_beats < MIN_NOTE_DURATION_BEATS {
        return;
    }

    output.push(MidiNote {
        start_beat,
        duration_beats,
        pitch,
        channel,
    });
}

// ──────────────────────────────────────
// Binary reading helpers (big-endian)
// ──────────────────────────────────────

fn read_chunk_id(data: &[u8], pos: &mut usize, expected: &str) -> bool {
    if *pos + 4 > data.len() {
        return false;
    }
    let expected_bytes = expected.as_bytes();
    for i in 0..4 {
        if data[*pos + i] != expected_bytes[i] {
            return false;
        }
    }
    *pos += 4;
    true
}

fn read_int32_be(data: &[u8], pos: &mut usize) -> i32 {
    if *pos + 4 > data.len() {
        return 0;
    }
    let value = ((data[*pos] as i32) << 24)
        | ((data[*pos + 1] as i32) << 16)
        | ((data[*pos + 2] as i32) << 8)
        | (data[*pos + 3] as i32);
    *pos += 4;
    value
}

fn read_int16_be(data: &[u8], pos: &mut usize) -> i32 {
    if *pos + 2 > data.len() {
        return 0;
    }
    let value = ((data[*pos] as i32) << 8) | (data[*pos + 1] as i32);
    *pos += 2;
    value
}

fn read_variable_length(data: &[u8], pos: &mut usize) -> i64 {
    let mut value: i64 = 0;
    for _ in 0..4 {
        if *pos >= data.len() {
            break;
        }
        let b = data[*pos];
        *pos += 1;
        value = (value << 7) | ((b & 0x7F) as i64);
        if (b & 0x80) == 0 {
            break;
        }
    }
    value
}

fn compare_notes_by_start_beat(a: &MidiNote, b: &MidiNote) -> std::cmp::Ordering {
    a.start_beat
        .partial_cmp(&b.start_beat)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            a.pitch
                .cmp(&b.pitch)
        })
}
