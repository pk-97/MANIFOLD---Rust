//! Encode replay, the backend-neutral half (`docs/ENCODE_REPLAY_DESIGN.md`):
//! the recorded command list, the comparison every dispatch in a span runs
//! against it, and the stats. The backend store keeps the GPU side (Metal:
//! indirect command buffers). A recording is only ever a cache of what
//! direct encoding would issue: a dispatch that doesn't match it exactly is
//! recorded again, never replayed.

use std::sync::OnceLock;

/// Recordings kept per span. An entry is only written while nothing pending
/// executed it, so a span needs one more entry than there are frames in
/// flight.
pub const REPLAY_RING: usize = 3;

/// Commands per recording chunk.
pub(crate) const CHUNK_COMMANDS: usize = 512;
/// Bytes per uniform arena.
pub(crate) const ARENA_BYTES: usize = 256 * 1024;
/// Uniform slot alignment: the constant-buffer offset rule on non-Apple GPUs.
pub(crate) const BYTES_ALIGN: usize = 256;
/// Most bindings one recordable dispatch carries, the sizes buffer included.
pub(crate) const MAX_KEY_BINDINGS: usize = 32;

/// Bytes per entry of a gated segment's range buffer: `{location: u32,
/// length: u32}`, Metal's indirect execution range. A live segment holds
/// `{0, commands}`, a dead one `{0, 0}`; the GPU writes it, the replayed
/// segment reads it (`GpuEncoder::begin_gated_segment`).
pub const GATED_RANGE_BYTES: u64 = 8;

/// What replay did, summed over a span cache's life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuReplayStats {
    /// Dispatches that matched a recording and ran from it.
    pub replayed: u64,
    /// Dispatches written into a recording: first use, a changed tail, or
    /// running past the end.
    pub recorded: u64,
    /// Dispatches in a span that ran directly: not recordable, replay off,
    /// no room left in the recording, or no idle entry.
    pub direct: u64,
    /// Execute calls: one per replayed stretch (per chunk it spans).
    pub executes: u64,
    /// Span visits that found no idle entry.
    pub ring_busy: u64,
    /// Recording chunks and uniform arenas created. Flat once warm.
    pub store_allocations: u64,
    /// Gated segments run from a recording, the GPU deciding each one's
    /// length (dead segments included).
    pub segments_replayed: u64,
    /// Gated dispatches that ran directly, as an indirect dispatch.
    pub segments_direct: u64,
    /// Gated templates (`GpuEncoder::repeat_gated_template`) committed by a
    /// walk that recorded at least one command.
    pub templates_recorded: u64,
    /// Gated templates committed by a walk that only validated.
    pub templates_replayed: u64,
    /// Gated templates that ran whole as direct dispatches: no entry, a
    /// full span, a count mismatch, an unrecordable dispatch or no room.
    pub templates_direct: u64,
}

impl std::ops::AddAssign for GpuReplayStats {
    fn add_assign(&mut self, other: Self) {
        self.replayed += other.replayed;
        self.recorded += other.recorded;
        self.direct += other.direct;
        self.executes += other.executes;
        self.ring_busy += other.ring_busy;
        self.store_allocations += other.store_allocations;
        self.segments_replayed += other.segments_replayed;
        self.segments_direct += other.segments_direct;
        self.templates_recorded += other.templates_recorded;
        self.templates_replayed += other.templates_replayed;
        self.templates_direct += other.templates_direct;
    }
}

/// A dispatch's place in a gated segment: the range buffer the GPU writes
/// the segment's length into (by identity and byte offset), the length the
/// segment declared, and this dispatch's slot inside it. Slot 0 opens the
/// segment, so a recording's segment structure is part of every key. A
/// template's segment also carries how many copies execute it and the
/// range entries between them; a plain segment has both 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GateKey {
    pub ranges: usize,
    pub offset: u64,
    pub commands: u32,
    pub slot: u32,
    pub copies: u32,
    pub stride: u32,
}

/// `MANIFOLD_ENCODE_REPLAY=0` turns replay off for the process, which brings
/// per-dispatch signposts back for fault hunts and GPU captures.
pub(crate) fn replay_allowed_by_env() -> bool {
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(|| std::env::var("MANIFOLD_ENCODE_REPLAY").map_or(true, |v| v != "0"))
}

/// One binding as the GPU sees it: a buffer by identity and offset, or
/// inline bytes by content. `slot` is the backend's argument index.
#[derive(Clone, Copy, Debug)]
pub(crate) enum KeyBinding<'a> {
    Buffer { slot: u32, id: usize, offset: u64 },
    Bytes { slot: u32, data: &'a [u8] },
}

/// A dispatch reduced to what decides the GPU's work.
pub(crate) struct DispatchKey<'a> {
    pub pipeline: usize,
    pub groups: [u32; 3],
    pub gate: Option<GateKey>,
    bindings: [KeyBinding<'a>; MAX_KEY_BINDINGS],
    len: usize,
}

impl<'a> DispatchKey<'a> {
    pub fn new(pipeline: usize, groups: [u32; 3]) -> Self {
        Self {
            pipeline,
            groups,
            gate: None,
            bindings: [KeyBinding::Bytes { slot: 0, data: &[] }; MAX_KEY_BINDINGS],
            len: 0,
        }
    }

    /// False when the key is full: the dispatch is then not recordable.
    #[must_use]
    pub fn push(&mut self, binding: KeyBinding<'a>) -> bool {
        if self.len == MAX_KEY_BINDINGS {
            return false;
        }
        self.bindings[self.len] = binding;
        self.len += 1;
        true
    }

    pub fn bindings(&self) -> &[KeyBinding<'a>] {
        &self.bindings[..self.len]
    }

    /// Arena bytes this dispatch's inline bindings take, alignment included.
    pub fn arena_bytes(&self) -> usize {
        self.bindings()
            .iter()
            .map(|b| match b {
                KeyBinding::Bytes { data, .. } => data.len().next_multiple_of(BYTES_ALIGN),
                KeyBinding::Buffer { .. } => 0,
            })
            .sum()
    }
}

/// Where one recorded bytes binding lives in the backend store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BytesSlot {
    pub arena: u32,
    pub offset: u32,
}

#[derive(Clone, Copy)]
struct RecordedCommand {
    pipeline: usize,
    groups: [u32; 3],
    gate: Option<GateKey>,
    bindings_start: u32,
    bindings_end: u32,
}

#[derive(Clone, Copy)]
enum RecordedBinding {
    Buffer { slot: u32, id: usize, offset: u64 },
    Bytes { slot: u32, len: u32, shadow: u32, store: BytesSlot },
}

/// The recorded command list of one ring entry, in encode order.
#[derive(Default)]
pub(crate) struct Recording {
    commands: Vec<RecordedCommand>,
    bindings: Vec<RecordedBinding>,
    /// The bytes each inline slot holds, as last written to the store.
    shadow: Vec<u8>,
}

impl Recording {
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether command `index` continues a gated segment opened before it.
    pub fn continues_segment(&self, index: usize) -> bool {
        self.commands.get(index).and_then(|c| c.gate).is_some_and(|gate| gate.slot > 0)
    }

    /// Drop every command from `len` on.
    pub fn truncate(&mut self, len: usize) {
        let Some(first) = self.commands.get(len) else {
            return;
        };
        let start = first.bindings_start as usize;
        if let Some(shadow) = self.bindings[start..].iter().find_map(|b| match b {
            RecordedBinding::Bytes { shadow, .. } => Some(*shadow as usize),
            RecordedBinding::Buffer { .. } => None,
        }) {
            self.shadow.truncate(shadow);
        }
        self.bindings.truncate(start);
        self.commands.truncate(len);
    }

    /// Whether `key` matches command `index` in everything but the content
    /// of its inline bytes.
    pub fn matches(&self, index: usize, key: &DispatchKey) -> bool {
        let Some(command) = self.commands.get(index) else {
            return false;
        };
        let recorded = &self.bindings[command.bindings_start as usize..command.bindings_end as usize];
        command.pipeline == key.pipeline
            && command.groups == key.groups
            && command.gate == key.gate
            && recorded.len() == key.bindings().len()
            && recorded.iter().zip(key.bindings()).all(|(recorded, incoming)| match (recorded, incoming) {
                (
                    RecordedBinding::Buffer { slot, id, offset },
                    KeyBinding::Buffer { slot: s, id: i, offset: o },
                ) => slot == s && id == i && offset == o,
                (RecordedBinding::Bytes { slot, len, .. }, KeyBinding::Bytes { slot: s, data }) => {
                    slot == s && *len as usize == data.len()
                }
                _ => false,
            })
    }

    /// For a command `matches` accepted: hand every inline slot whose
    /// content changed to `write`, and remember the new content.
    pub fn refresh_bytes(&mut self, index: usize, key: &DispatchKey, mut write: impl FnMut(BytesSlot, &[u8])) {
        let command = self.commands[index];
        let recorded = &self.bindings[command.bindings_start as usize..command.bindings_end as usize];
        for (recorded, incoming) in recorded.iter().zip(key.bindings()) {
            if let (RecordedBinding::Bytes { len, shadow, store, .. }, KeyBinding::Bytes { data, .. }) =
                (recorded, incoming)
            {
                let held = &mut self.shadow[*shadow as usize..*shadow as usize + *len as usize];
                if held != *data {
                    held.copy_from_slice(data);
                    write(*store, data);
                }
            }
        }
    }

    /// Append `key` as the next command. `slots` holds one store slot per
    /// inline binding, in binding order.
    pub fn push(&mut self, key: &DispatchKey, slots: &[BytesSlot]) {
        let bindings_start = self.bindings.len() as u32;
        let mut slots = slots.iter();
        for binding in key.bindings() {
            self.bindings.push(match *binding {
                KeyBinding::Buffer { slot, id, offset } => RecordedBinding::Buffer { slot, id, offset },
                KeyBinding::Bytes { slot, data } => {
                    let shadow = self.shadow.len() as u32;
                    self.shadow.extend_from_slice(data);
                    RecordedBinding::Bytes {
                        slot,
                        len: data.len() as u32,
                        shadow,
                        store: *slots.next().expect("one store slot per inline binding"),
                    }
                }
            });
        }
        self.commands.push(RecordedCommand {
            pipeline: key.pipeline,
            groups: key.groups,
            gate: key.gate,
            bindings_start,
            bindings_end: self.bindings.len() as u32,
        });
    }
}

/// The entry a span visit uses: the most recently used one if idle, else
/// another idle one, else a new one while the ring has room, else none.
pub(crate) fn pick_entry(len: usize, mru: Option<usize>, is_idle: impl Fn(usize) -> bool) -> Option<usize> {
    if let Some(mru) = mru
        && is_idle(mru)
    {
        return Some(mru);
    }
    (0..len)
        .find(|&i| Some(i) != mru && is_idle(i))
        .or((len < REPLAY_RING).then_some(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_bytes() -> [u8; 8] {
        [1, 2, 3, 4, 5, 6, 7, 8]
    }

    fn key<'a>(bytes: &'a [u8]) -> DispatchKey<'a> {
        let mut key = DispatchKey::new(0x1000, [4, 2, 1]);
        assert!(key.push(KeyBinding::Buffer { slot: 0, id: 0x2000, offset: 0 }));
        assert!(key.push(KeyBinding::Buffer { slot: 1, id: 0x3000, offset: 64 }));
        assert!(key.push(KeyBinding::Bytes { slot: 2, data: bytes }));
        key
    }

    fn recorded(bytes: &[u8]) -> Recording {
        let mut recording = Recording::default();
        recording.push(&key(bytes), &[BytesSlot { arena: 0, offset: 0 }]);
        recording
    }

    /// Every field that changes what the GPU runs must miss; only the
    /// content of inline bytes may differ, and then it is written through.
    #[test]
    fn replay_key_detects_every_field() {
        let bytes = base_bytes();
        let recording = recorded(&bytes);
        assert!(recording.matches(0, &key(&bytes)));

        let mut variants: Vec<(&str, DispatchKey)> = Vec::new();
        let mut k = key(&bytes);
        k.pipeline = 0x1001;
        variants.push(("pipeline", k));
        for axis in 0..3 {
            let mut k = key(&bytes);
            k.groups[axis] += 1;
            variants.push(("groups", k));
        }
        let mut k = key(&bytes);
        k.bindings[0] = KeyBinding::Buffer { slot: 0, id: 0x2008, offset: 0 };
        variants.push(("buffer identity", k));
        let mut k = key(&bytes);
        k.bindings[1] = KeyBinding::Buffer { slot: 1, id: 0x3000, offset: 128 };
        variants.push(("buffer offset", k));
        let mut k = key(&bytes);
        k.bindings[1] = KeyBinding::Buffer { slot: 4, id: 0x3000, offset: 64 };
        variants.push(("buffer slot", k));
        let mut k = key(&bytes);
        k.bindings[2] = KeyBinding::Bytes { slot: 3, data: &bytes };
        variants.push(("bytes slot", k));
        let mut k = key(&bytes);
        k.bindings[2] = KeyBinding::Bytes { slot: 2, data: &bytes[..4] };
        variants.push(("bytes length", k));
        let mut k = key(&bytes);
        k.bindings[1] = KeyBinding::Bytes { slot: 1, data: &bytes };
        variants.push(("binding kind", k));
        let mut k = key(&bytes);
        k.len = 2;
        variants.push(("binding count (fewer)", k));
        let mut k = key(&bytes);
        assert!(k.push(KeyBinding::Buffer { slot: 5, id: 0x4000, offset: 0 }));
        variants.push(("binding count (more)", k));

        let mut k = key(&bytes);
        k.gate = Some(GateKey { ranges: 0x5000, offset: 0, commands: 4, slot: 0, copies: 0, stride: 0 });
        variants.push(("gate (added)", k));

        for (field, variant) in &variants {
            assert!(!recording.matches(0, variant), "a changed {field} must miss");
        }
        assert!(!recording.matches(1, &key(&bytes)), "past the end must miss");

        // A gated command: every gate field decides the match too.
        let gate = GateKey { ranges: 0x5000, offset: 8, commands: 4, slot: 1, copies: 0, stride: 0 };
        let mut gated = Recording::default();
        let mut k = key(&bytes);
        k.gate = Some(gate);
        gated.push(&k, &[BytesSlot { arena: 0, offset: 0 }]);
        assert!(gated.matches(0, &k));
        assert!(gated.continues_segment(0));
        for (field, changed) in [
            ("ranges", GateKey { ranges: 0x5008, ..gate }),
            ("offset", GateKey { offset: 16, ..gate }),
            ("commands", GateKey { commands: 5, ..gate }),
            ("slot", GateKey { slot: 0, ..gate }),
            ("copies", GateKey { copies: 3, ..gate }),
            ("stride", GateKey { stride: 2, ..gate }),
        ] {
            let mut k = key(&bytes);
            k.gate = Some(changed);
            assert!(!gated.matches(0, &k), "a changed gate {field} must miss");
        }
        assert!(!gated.matches(0, &key(&bytes)), "a gate removed must miss");
        assert!(!recording.continues_segment(0), "an ungated command opens no segment");

        let mut recording = recording;
        let changed = [9, 2, 3, 4, 5, 6, 7, 8];
        let changed_key = key(&changed);
        assert!(recording.matches(0, &changed_key), "new bytes content is not a miss");
        let mut writes = Vec::new();
        recording.refresh_bytes(0, &changed_key, |slot, data| writes.push((slot, data.to_vec())));
        assert_eq!(writes, vec![(BytesSlot { arena: 0, offset: 0 }, changed.to_vec())]);
        writes.clear();
        recording.refresh_bytes(0, &changed_key, |slot, data| writes.push((slot, data.to_vec())));
        assert!(writes.is_empty(), "unchanged bytes are not written again");
    }

    #[test]
    fn truncate_keeps_the_prefix_and_its_bytes() {
        let a = base_bytes();
        let b = [8u8; 8];
        let mut recording = Recording::default();
        recording.push(&key(&a), &[BytesSlot { arena: 0, offset: 0 }]);
        recording.push(&key(&b), &[BytesSlot { arena: 0, offset: 256 }]);
        recording.truncate(1);
        assert_eq!(recording.len(), 1);
        assert!(recording.matches(0, &key(&a)));
        let mut writes = 0;
        recording.refresh_bytes(0, &key(&a), |_, _| writes += 1);
        assert_eq!(writes, 0, "the kept command's bytes survive the cut");
        recording.push(&key(&b), &[BytesSlot { arena: 0, offset: 256 }]);
        assert!(recording.matches(1, &key(&b)));
    }

    #[test]
    fn entries_are_picked_most_recent_first_and_never_busy() {
        assert_eq!(pick_entry(0, None, |_| true), Some(0));
        assert_eq!(pick_entry(2, Some(1), |_| true), Some(1));
        assert_eq!(pick_entry(2, Some(1), |i| i != 1), Some(0));
        assert_eq!(pick_entry(2, Some(1), |_| false), Some(2));
        assert_eq!(pick_entry(REPLAY_RING, Some(0), |_| false), None);
    }
}
