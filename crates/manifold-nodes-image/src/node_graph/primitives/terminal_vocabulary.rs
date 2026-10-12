//! Deterministic, source-aware text for the Code Terminal panes.
//!
//! The terminal stream owns the scheduler and the cell storage. This module
//! turns a passage seed plus a line index into one row of text: the seed picks
//! a passage from `terminal_passages`, the line index walks down it, and the
//! measured source values and a seeded number stream fill its placeholders.
//! A new seed shows a different passage, so a group that is rewritten reads
//! as fresh output rather than the same lines with new numbers. Writes go into
//! the caller's fixed buffer, so changing a line never allocates.

use super::terminal_passages::{CODE, INSPECT, LOGS, Passage, SHELL};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Shell,
    Code,
    Logs,
    Inspect,
}

#[derive(Clone, Copy)]
pub(super) struct Context {
    /// Picks the passage. Neighbouring lines that share it read as one block.
    pub stream: u32,
    /// Line index down the passage; past its end the next passage continues.
    pub line: usize,
    pub left: f32,
    pub right: f32,
    pub light: f32,
    pub edge: f32,
    pub signature: u32,
}

/// Fill `dst` with spaces and write one deterministic terminal row.
///
/// All output is printable ASCII. Writes are clipped at the supplied
/// destination, so this stays safe for the small buffers of tiny panes as
/// well as the normal 640-cell line buffer.
pub(super) fn write_line(dst: &mut [u8], role: Role, context: Context) -> usize {
    dst.fill(b' ');
    let template = template(passages(role), context.stream, context.line);
    let mut writer = Writer {
        dst,
        at: 0,
        rng: Rng::new(hash3(context.stream, context.line as u32, context.signature)),
    };
    writer.template(template, context);
    writer.at
}

fn passages(role: Role) -> &'static [Passage] {
    match role {
        Role::Shell => SHELL,
        Role::Code => CODE,
        Role::Logs => LOGS,
        Role::Inspect => INSPECT,
    }
}

fn template(passages: &'static [Passage], stream: u32, line: usize) -> &'static [u8] {
    let mut passage = stream as usize % passages.len();
    let mut line = line;
    loop {
        let lines = passages[passage];
        if line < lines.len() {
            return lines[line];
        }
        line -= lines.len();
        passage = (passage + 1) % passages.len();
    }
}

pub(super) fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

pub(super) fn hash3(a: u32, b: u32, c: u32) -> u32 {
    hash(a ^ hash(b ^ hash(c)))
}

struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9e37_79b9);
        hash(self.0)
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound.max(1)
    }
}

struct Writer<'a> {
    dst: &'a mut [u8],
    at: usize,
    rng: Rng,
}

const REGISTERS: [&[u8]; 14] = [
    b"rax", b"rbx", b"rcx", b"rdx", b"rsi", b"rdi", b"rbp", b"r8", b"r9", b"r10", b"r11", b"r12",
    b"r13", b"r14",
];
const BINARY: [&[u8]; 12] = [
    b"mov", b"add", b"sub", b"xor", b"and", b"or", b"cmp", b"test", b"lea", b"imul", b"shl",
    b"shr",
];
const UNARY: [&[u8]; 5] = [b"push", b"pop", b"inc", b"dec", b"neg"];
const BRANCH: [&[u8]; 5] = [b"call", b"jmp", b"jne", b"je", b"jg"];
const VECTOR: [&[u8]; 4] = [b"vmulps", b"vaddps", b"vfmadd231ps", b"vxorps"];

impl Writer<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        let available = self.dst.len().saturating_sub(self.at);
        let count = bytes.len().min(available);
        self.dst[self.at..self.at + count].copy_from_slice(&bytes[..count]);
        self.at += count;
    }

    fn byte(&mut self, value: u8) {
        if self.at < self.dst.len() {
            self.dst[self.at] = value.clamp(32, 126);
            self.at += 1;
        }
    }

    fn number(&mut self, mut value: usize) {
        let mut digits = [b'0'; 20];
        let mut count = digits.len();
        while value >= 10 {
            count -= 1;
            digits[count] = b'0' + (value % 10) as u8;
            value /= 10;
        }
        count -= 1;
        digits[count] = b'0' + value as u8;
        self.bytes(&digits[count..]);
    }

    fn padded_number(&mut self, mut value: usize, width: usize) {
        let mut digits = [b'0'; 20];
        let width = width.min(digits.len());
        for digit in digits[..width].iter_mut().rev() {
            *digit += (value % 10) as u8;
            value /= 10;
        }
        self.bytes(&digits[..width]);
    }

    fn hex(&mut self, mut value: u32, width: usize) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let width = width.min(8);
        let mut digits = [b'0'; 8];
        for digit in digits[..width].iter_mut().rev() {
            *digit = HEX[(value & 0xf) as usize];
            value >>= 4;
        }
        self.bytes(&digits[..width]);
    }

    fn fixed(&mut self, value: f32, places: usize) {
        let value = finite_unit(value);
        let scale = match places {
            0 => 1_u32,
            1 => 10,
            2 => 100,
            _ => 1000,
        };
        let scaled = (value * scale as f32).round() as u32;
        self.number((scaled / scale) as usize);
        if places != 0 {
            self.byte(b'.');
            let fraction = scaled % scale;
            self.padded_number(fraction as usize, places.min(3));
        }
    }

    fn template(&mut self, template: &[u8], context: Context) {
        let mut i = 0;
        while i < template.len() {
            let ch = template[i];
            if ch == b'%' && i + 1 < template.len() {
                self.token(template[i + 1], context);
                i += 2;
            } else {
                self.byte(ch);
                i += 1;
            }
        }
    }

    fn token(&mut self, token: u8, context: Context) {
        let passage = hash(context.stream);
        let line = context.line;
        match token {
            b'L' => self.fixed(context.left, 2),
            b'R' => self.fixed(context.right, 2),
            b'B' => self.fixed(context.light, 2),
            b'E' => self.fixed(context.edge, 2),
            b'C' => self.bytes(contour(context)),
            b'H' => {
                let value = self.rng.next() ^ context.signature;
                self.hex(value, 8);
            }
            b'h' => {
                let value = self.rng.next();
                self.hex(value, 4);
            }
            b'b' => {
                let value = self.rng.next();
                self.hex(value, 2);
            }
            b'G' => {
                let value = self.rng.next();
                self.hex(value, 7);
            }
            b'N' => {
                let value = self.rng.below(10_000);
                self.number(value as usize);
            }
            b'n' => {
                let value = self.rng.below(10);
                self.number(value as usize);
            }
            b'P' => self.number(2 + (passage % 250) as usize),
            b'W' => {
                let shift = 6 + self.rng.below(7);
                self.number(1 << shift);
            }
            b'M' => {
                let value = self.rng.below(1000) as usize;
                self.number(value / 10);
                self.byte(b'.');
                self.number(value % 10);
            }
            b'K' => {
                let value = 1 + self.rng.below(999) as usize;
                self.number(value);
                if value < 100 {
                    self.byte(b'.');
                    let tenth = self.rng.below(10);
                    self.number(tenth as usize);
                }
                let unit = [b'K', b'M', b'G'][self.rng.below(3) as usize];
                self.byte(unit);
            }
            b'T' => {
                // One clock per passage that ticks forward down its lines.
                let seconds = passage as usize % 86_400 + line * 3;
                self.padded_number(seconds / 3600 % 24, 2);
                self.byte(b':');
                self.padded_number(seconds / 60 % 60, 2);
                self.byte(b':');
                self.padded_number(seconds % 60, 2);
                self.byte(b'.');
                let millis = self.rng.below(1000);
                self.padded_number(millis as usize, 3);
            }
            b'S' => self.number((passage % 50_000) as usize + line),
            b'A' => self.hex((passage & 0x00ff_fff0).wrapping_add(line as u32 * 16), 8),
            b'X' => self.hexdump(),
            b'D' => self.instruction(),
            b'%' => self.byte(b'%'),
            other => {
                self.byte(b'%');
                self.byte(other);
            }
        }
    }

    fn hexdump(&mut self) {
        let mut ascii = [b'.'; 16];
        for (i, cell) in ascii.iter_mut().enumerate() {
            let value = (self.rng.next() & 0xff) as u8;
            self.hex(u32::from(value), 2);
            self.byte(b' ');
            if i == 7 {
                self.byte(b' ');
            }
            if (32..127).contains(&value) {
                *cell = value;
            }
        }
        self.bytes(b" |");
        self.bytes(&ascii);
        self.byte(b'|');
    }

    fn instruction(&mut self) {
        let count = 2 + self.rng.below(4) as usize;
        for i in 0..6 {
            if i < count {
                let value = self.rng.next();
                self.hex(value, 2);
                self.byte(b' ');
            } else {
                self.bytes(b"   ");
            }
        }
        self.byte(b' ');
        match self.rng.below(10) {
            0..=4 => {
                let op = BINARY[self.rng.below(BINARY.len() as u32) as usize];
                self.bytes(op);
                self.byte(b' ');
                self.register();
                self.bytes(b", ");
                self.operand();
            }
            5 => {
                let op = UNARY[self.rng.below(UNARY.len() as u32) as usize];
                self.bytes(op);
                self.byte(b' ');
                self.register();
            }
            6 | 7 => {
                let op = BRANCH[self.rng.below(BRANCH.len() as u32) as usize];
                self.bytes(op);
                self.bytes(b" 0x");
                let target = self.rng.next();
                self.hex(target, 8);
            }
            8 => {
                let op = VECTOR[self.rng.below(VECTOR.len() as u32) as usize];
                self.bytes(op);
                for i in 0..3 {
                    self.bytes(if i == 0 { b" ymm" } else { b", ymm" });
                    let reg = self.rng.below(16);
                    self.number(reg as usize);
                }
            }
            _ => {
                let op: &[u8] = if self.rng.below(2) == 0 { b"ret" } else { b"nop" };
                self.bytes(op);
            }
        }
    }

    fn register(&mut self) {
        let reg = REGISTERS[self.rng.below(REGISTERS.len() as u32) as usize];
        self.bytes(reg);
    }

    fn operand(&mut self) {
        match self.rng.below(4) {
            0 => self.register(),
            1 => {
                self.bytes(b"[rbp-0x");
                let offset = self.rng.next();
                self.hex(offset, 2);
                self.byte(b']');
            }
            2 => {
                self.bytes(b"[rip+0x");
                let offset = self.rng.next();
                self.hex(offset, 4);
                self.byte(b']');
            }
            _ => {
                self.bytes(b"0x");
                let value = self.rng.next();
                self.hex(value, 2);
            }
        }
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn contour(context: Context) -> &'static [u8] {
    if finite_unit(context.edge) > 0.2 {
        b"contour acquired; following silhouette"
    } else if finite_unit(context.light) < 0.12 {
        b"signal quiet; waiting for structure"
    } else {
        b"signal locked"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLES: [Role; 4] = [Role::Shell, Role::Code, Role::Logs, Role::Inspect];

    fn context(stream: u32, line: usize) -> Context {
        Context {
            stream,
            line,
            left: 0.13,
            right: 0.87,
            light: 0.42,
            edge: 0.31,
            signature: 0x1a2b_3c4d,
        }
    }

    fn visible_len(buffer: &[u8]) -> usize {
        buffer
            .iter()
            .rposition(|&byte| byte != b' ')
            .map_or(0, |i| i + 1)
    }

    #[test]
    fn repeated_input_is_byte_for_byte_deterministic() {
        let mut first = [0u8; 640];
        let mut second = [0u8; 640];
        for role in ROLES {
            let input = context(27, 3);
            assert_eq!(
                write_line(&mut first, role, input),
                write_line(&mut second, role, input)
            );
            assert_eq!(first, second);
        }
    }

    #[test]
    fn output_is_printable_and_bounded_for_tiny_buffers() {
        for role in ROLES {
            for stream in 0..40 {
                for size in 0..=7 {
                    let mut buffer = [0u8; 7];
                    let written = write_line(&mut buffer[..size], role, context(stream, 3));
                    assert!(written <= size);
                    assert!(buffer[..size].iter().all(|&byte| (32..=126).contains(&byte)));
                }
            }
        }
    }

    #[test]
    fn every_template_uses_only_known_placeholders() {
        const KNOWN: &[u8] = b"LRBECHhbGNnPWMKTSAXD%";
        for role in ROLES {
            for passage in passages(role) {
                assert!(!passage.is_empty());
                for line in *passage {
                    let mut i = 0;
                    while i < line.len() {
                        if line[i] == b'%' {
                            let token = line.get(i + 1).copied().unwrap_or(b' ');
                            assert!(
                                KNOWN.contains(&token),
                                "{role:?}: unknown placeholder in {}",
                                String::from_utf8_lossy(line)
                            );
                            i += 2;
                        } else {
                            assert!((32..=126).contains(&line[i]) || line[i] == b'\t');
                            i += 1;
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_line_renders_printable_text() {
        for role in ROLES {
            let total: usize = passages(role).iter().map(|p| p.len()).sum();
            for line in 0..total {
                let mut buffer = [0u8; 640];
                let written = write_line(&mut buffer, role, context(0, line));
                assert!(written > 0);
                assert!(buffer.iter().all(|&byte| (32..=126).contains(&byte)));
            }
        }
    }

    #[test]
    fn each_role_has_a_large_pool_of_distinct_lines() {
        for role in ROLES {
            let passages = passages(role);
            let mut lines: Vec<&[u8]> = passages.iter().flat_map(|p| p.iter().copied()).collect();
            lines.sort_unstable();
            lines.dedup();
            let floor = if role == Role::Inspect { 25 } else { 45 };
            assert!(passages.len() >= 8, "{role:?}: {} passages", passages.len());
            assert!(lines.len() >= floor, "{role:?}: {} distinct lines", lines.len());
        }
    }

    #[test]
    fn a_new_stream_shows_a_different_passage() {
        for role in ROLES {
            let count = passages(role).len() as u32;
            let mut firsts = Vec::new();
            for stream in 0..count {
                let mut buffer = [0u8; 640];
                write_line(&mut buffer, role, context(stream, 1));
                firsts.push(buffer);
            }
            firsts.sort_unstable();
            firsts.dedup();
            assert!(firsts.len() as u32 >= count - 1, "{role:?}");
        }
    }

    #[test]
    fn lines_walk_down_one_passage_then_continue_into_the_next() {
        for role in ROLES {
            let passages = passages(role);
            let stream = 5_u32;
            let first = stream as usize % passages.len();
            let len = passages[first].len();
            assert_eq!(template(passages, stream, 0), passages[first][0]);
            assert_eq!(template(passages, stream, len - 1), passages[first][len - 1]);
            assert_eq!(
                template(passages, stream, len),
                passages[(first + 1) % passages.len()][0]
            );
        }
    }

    #[test]
    fn group_lines_have_meaningful_length_variation() {
        for role in [Role::Shell, Role::Code, Role::Logs] {
            for stream in 0..8 {
                let mut lengths = [0usize; 8];
                for (line, length) in lengths.iter_mut().enumerate() {
                    let mut buffer = [0u8; 640];
                    write_line(&mut buffer, role, context(stream, line));
                    *length = visible_len(&buffer);
                }
                let min = *lengths.iter().min().expect("lines");
                let max = *lengths.iter().max().expect("lines");
                assert!(max >= min + 10, "{role:?} stream {stream}: {lengths:?}");
            }
        }
    }

    #[test]
    fn source_context_changes_meaningful_text() {
        let mut before = [0u8; 640];
        let mut after = [0u8; 640];
        let mut changed = 0;
        let mut total = 0;
        for role in ROLES {
            for stream in 0..8 {
                for line in 0..8 {
                    let mut altered = context(stream, line);
                    altered.left = 0.73;
                    altered.right = 0.24;
                    altered.light = 0.91;
                    altered.edge = 0.04;
                    altered.signature = 0xcafe_f00d;
                    write_line(&mut before, role, context(stream, line));
                    write_line(&mut after, role, altered);
                    total += 1;
                    if before != after {
                        changed += 1;
                    }
                }
            }
        }
        assert!(
            changed * 2 >= total,
            "only {changed} of {total} lines used source values"
        );
    }
}
