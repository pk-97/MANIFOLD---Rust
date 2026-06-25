//! Small numeric formatting helpers shared across panels.

/// Format `v` to `decimals` places, then strip trailing zeros and any trailing
/// decimal point: `1.50 → "1.5"`, `2.000 → "2"`, `0.010 → "0.01"`.
///
/// The single home for the
/// `format!("{:.N}").trim_end_matches('0').trim_end_matches('.')` idiom that was
/// copy-pasted across the graph live-readout (`trim_num`), the table cell
/// (`fmt_table_cell`), and the inspector value (`fmt_value`).
pub fn fmt_trimmed(v: f32, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_zeros_and_point() {
        assert_eq!(fmt_trimmed(1.5, 2), "1.5");
        assert_eq!(fmt_trimmed(2.0, 3), "2");
        assert_eq!(fmt_trimmed(0.01, 2), "0.01");
        assert_eq!(fmt_trimmed(0.125, 2), "0.12"); // rounds to 2 places
        assert_eq!(fmt_trimmed(64.0, 2), "64");
    }
}
