//! Deterministic, source-aware text for the Code Terminal panes.
//!
//! The terminal stream owns the scheduler and the cell storage.  This module
//! only supplies bounded text: each row selects one of a small set of related
//! shell, source, log, or inspection fragments, and the measured source values
//! are inserted where they make sense.  The writer intentionally uses a fixed
//! scratch-free buffer so changing a line never allocates on the frame path.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Shell,
    Code,
    Logs,
    Inspect,
}

#[derive(Clone, Copy)]
pub(super) struct Context {
    pub row: usize,
    pub left: f32,
    pub right: f32,
    pub light: f32,
    pub edge: f32,
    pub signature: u32,
}

/// Fill `dst` with spaces and append one deterministic terminal row.
///
/// All output is printable ASCII.  Writes are clipped at the supplied
/// destination, so this remains safe for the small buffers used by tiny panes
/// as well as the normal 640-cell line buffer.
pub(super) fn write_line(dst: &mut [u8], role: Role, context: Context) -> usize {
    dst.fill(b' ');
    let mut writer = Writer { dst, at: 0 };
    let row = context.row;
    match role {
        Role::Shell => shell(&mut writer, row, context),
        Role::Code => code(&mut writer, row, context),
        Role::Logs => logs(&mut writer, row, context),
        Role::Inspect => inspect(&mut writer, row, context),
    }
    writer.at
}

struct Writer<'a> {
    dst: &'a mut [u8],
    at: usize,
}

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

    fn row(&mut self, row: usize) {
        self.number(row);
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn source_values(context: Context) -> (f32, f32, f32, f32, u32) {
    let left = finite_unit(context.left);
    let right = finite_unit(context.right);
    let light = finite_unit(context.light);
    let edge = finite_unit(context.edge);
    (left, right, light, edge, context.signature)
}

fn shell(writer: &mut Writer<'_>, row: usize, context: Context) {
    let (_, right, light, edge, signature) = source_values(context);
    match row % 8 {
        0 => {
            writer.bytes(b"$ ssh relay@10.0.0.");
            writer.number(2 + (signature as usize % 240));
        }
        1 => {
            writer.bytes(b"$ find /srv/streams -type f -name '*.raw' | sort");
        }
        2 => {
            writer.bytes(b"$ sha256sum /srv/streams/capture.raw");
        }
        3 => {
            writer.bytes(b"$ ss -tnp | head -n 12");
        }
        4 => {
            writer.bytes(b"$ curl -fsS http://10.0.0.");
            writer.number(2 + (signature as usize % 240));
            writer.bytes(b":8080/health | jq '.streams'");
        }
        5 => {
            writer.bytes(b"ok   packet row=");
            writer.row(row);
            writer.bytes(b" bytes=");
            writer.number(64 + ((right * 512.0) as usize & !7));
            writer.bytes(b" checksum=0x");
            writer.hex(signature ^ (row as u32).rotate_left(5), 8);
        }
        6 => {
            writer.bytes(b"$ journalctl -u stream-relay --no-pager | rg 'connected|decoded|synced' | tail -n 24 > /tmp/relay.log");
        }
        7 => {
            writer.bytes(if edge > 0.2 {
                b"[ok] contour acquired"
            } else if light < 0.12 {
                b"[wait] signal quiet"
            } else {
                b"[ok] signal locked"
            });
        }
        _ => unreachable!(),
    }
}

fn code(writer: &mut Writer<'_>, row: usize, context: Context) {
    let (left, right, _, edge, signature) = source_values(context);
    match row % 8 {
        0 => writer.bytes(b"fn decode_region(frame: &Frame) -> Patch {"),
        1 => {
            writer.bytes(b"    let window = frame.crop(");
            writer.fixed(left, 2);
            writer.bytes(b", ");
            writer.fixed(right, 2);
            writer.bytes(b");");
        }
        2 => {
            writer.bytes(b"    let edges = window.sobel().threshold(");
            writer.fixed(edge, 2);
            writer.bytes(b");");
        }
        3 => writer.bytes(b"    if edges.has_contour() {"),
        4 => {
            writer.bytes(b"        return Patch::tracked(window, 0x");
            writer.hex(signature, 8);
            writer.bytes(b");");
        }
        5 => writer.bytes(b"    }"),
        6 => writer.bytes(b"    Patch::passthrough(window)"),
        7 => writer.bytes(b"}"),
        _ => unreachable!(),
    }
}

fn logs(writer: &mut Writer<'_>, row: usize, context: Context) {
    let (left, right, light, edge, signature) = source_values(context);
    match row % 8 {
        0 => {
            writer.bytes(b"INFO  relay: accepted stream channel=");
            writer.row(row);
            writer.bytes(b" signature=0x");
            writer.hex(signature, 8);
        }
        1 => {
            writer.bytes(b"DEBUG decoder: region [");
            writer.fixed(left, 2);
            writer.bytes(b", ");
            writer.fixed(right, 2);
            writer.bytes(b"] exposure=");
            writer.fixed(light, 2);
        }
        2 => {
            writer.bytes(if edge > 0.2 {
                b"INFO  tracker: contour acquired; following silhouette"
            } else {
                b"INFO  tracker: waiting for structure"
            });
        }
        3 => {
            writer.bytes(b"INFO  transport: packet verified channel=");
            writer.number(row % 3);
            writer.bytes(b" checksum=0x");
            writer.hex(signature.rotate_left(7), 8);
        }
        4 => {
            writer.bytes(b"TRACE scan: channel=");
            writer.number(row % 4);
            writer.bytes(b" region width=");
            writer.fixed((right - left).abs(), 2);
        }
        5 => writer.bytes(b"OK    relay ready"),
        6 => {
            writer.bytes(b"NOTICE archive: samples indexed=");
            writer.number(32 + ((light * 96.0) as usize));
            writer.bytes(b" confidence=");
            writer.fixed(edge, 2);
        }
        7 => {
            writer.bytes(b"INFO  decoder: image block committed index=");
            writer.row(row);
        }
        _ => unreachable!(),
    }
}

fn inspect(writer: &mut Writer<'_>, row: usize, context: Context) {
    let (left, right, light, edge, signature) = source_values(context);
    match row % 8 {
        0 => {
            writer.bytes(b"0000 48 8b 05 ");
            writer.hex(signature, 8);
            writer.bytes(b"  mov rax,[rip+0x");
            writer.hex((right * 65535.0) as u32, 4);
            writer.bytes(b"]");
        }
        1 => {
            writer.bytes(b"0x");
            writer.hex(signature.wrapping_add(row as u32), 8);
            writer.bytes(b" | bounds [");
            writer.fixed(left, 2);
            writer.bytes(b", ");
            writer.fixed(right, 2);
            writer.bytes(b"]");
        }
        2 => {
            writer.bytes(b"field[");
            writer.padded_number(row, 4);
            writer.bytes(b"] = 0x");
            writer.hex(((light * 255.0) as u32) << 8 | (edge * 255.0) as u32, 4);
        }
        3 => {
            writer.bytes(b"packet seq=");
            writer.number(row);
            writer.bytes(b" len=");
            writer.number(24 + ((right * 64.0) as usize));
            writer.bytes(b" crc16=0x");
            writer.hex((signature ^ 0xa55a_5aa5) & 0xffff, 4);
        }
        4 => {
            writer.bytes(b"00000040  7b 22 70 61 6e 65 22 3a  ");
            writer.bytes(b"{ pane: ");
            writer.number(row % 4);
            writer.bytes(b" }");
        }
        5 => {
            writer.bytes(b"disasm: test rdi,rdi ; jne 0x");
            writer.hex(signature.rotate_right(3), 8);
            writer.bytes(b" ; load source profile and return to caller");
        }
        6 => {
            writer.bytes(b"offset +");
            writer.padded_number(((left * 4096.0) as usize) & 0xfff, 3);
            writer.bytes(b"  mask=0x");
            writer.hex(((edge * 65535.0) as u32) & 0xffff, 4);
            writer.bytes(b"  readable");
        }
        7 => writer.bytes(b"reg  r12=0x00000000  r13=0x00000001  flags=NZ"),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(row: usize) -> Context {
        Context {
            row,
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
        let input = context(27);
        assert_eq!(
            write_line(&mut first, Role::Shell, input),
            write_line(&mut second, Role::Shell, input)
        );
        assert_eq!(first, second);
    }

    #[test]
    fn output_is_printable_and_bounded_for_tiny_buffers() {
        for role in [Role::Shell, Role::Code, Role::Logs, Role::Inspect] {
            for size in 0..=7 {
                let mut buffer = [0u8; 7];
                let written = write_line(&mut buffer[..size], role, context(3));
                assert!(written <= size);
                assert!(
                    buffer[..size]
                        .iter()
                        .all(|&byte| (32..=126).contains(&byte))
                );
            }
        }
    }

    #[test]
    fn roles_have_distinct_vocabulary() {
        let mut lines = [[0u8; 128]; 4];
        for (line, role) in
            lines
                .iter_mut()
                .zip([Role::Shell, Role::Code, Role::Logs, Role::Inspect])
        {
            write_line(line, role, context(0));
        }
        assert!(lines.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(lines[0].starts_with(b"$ "));
        assert!(lines[1].starts_with(b"fn "));
        assert!(lines[2].starts_with(b"INFO"));
        assert!(lines[3].starts_with(b"0000"));
    }

    #[test]
    fn rows_have_meaningful_length_variation() {
        for role in [Role::Shell, Role::Code, Role::Logs, Role::Inspect] {
            let mut lengths = [0usize; 8];
            for (row, length) in lengths.iter_mut().enumerate() {
                let mut buffer = [0u8; 640];
                write_line(&mut buffer, role, context(row));
                *length = visible_len(&buffer);
            }
            let min = *lengths.iter().min().expect("templates");
            let max = *lengths.iter().max().expect("templates");
            assert!(max >= min + 25, "{role:?}: {lengths:?}");
            assert!(lengths.iter().any(|&length| (20..=75).contains(&length)));
        }
    }

    #[test]
    fn source_context_changes_meaningful_text() {
        let mut before = [0u8; 640];
        let mut after = [0u8; 640];
        let mut changed = 0;
        let mut altered = context(5);
        altered.left = 0.73;
        altered.right = 0.24;
        altered.light = 0.91;
        altered.edge = 0.04;
        altered.signature = 0xcafe_f00d;
        for role in [Role::Shell, Role::Code, Role::Logs, Role::Inspect] {
            for row in 0..8 {
                altered.row = row;
                write_line(&mut before, role, context(row));
                write_line(&mut after, role, altered);
                if before != after {
                    changed += 1;
                }
            }
        }
        assert!(changed >= 12, "only {changed} templates used source values");
    }
}
