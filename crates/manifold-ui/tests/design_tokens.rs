//! Design-token enforcement guard (design system §16).
//!
//! Blocks NEW raw colour and radius literals so the design tokens can't silently
//! re-drift — the exact bug class the dedup audit (§22) keeps surfacing. The rule
//! (§16.1): colours and radii are defined once in `color.rs`; call sites reference
//! tokens, never raw literals.
//!
//! Existing, not-yet-cleaned violations are grandfathered by a per-category
//! BASELINE. This is a **ratchet**:
//!   - Add a raw literal  → count goes up  → test fails (use a token, or exempt it).
//!   - Clean one up        → count goes down → test fails until you LOWER the baseline.
//!
//! So the number can only go down, and each future cleanup phase (§14 B′ radii,
//! §15 colour ramp) tightens it toward zero.
//!
//! Escape hatch for the rare honest exception (a sub-pixel hairline, a test
//! fixture colour): put `// design-token-exempt: <reason>` on the same line.

use std::fs;
use std::path::{Path, PathBuf};

// ── Baselines (high-water marks; lower these as phases clean up) ──────
//
// `color.rs` (the token home) and `node.rs` (the `Color32` type + its own
// `WHITE`/`BLACK` consts) are excluded from the scan.
//
// Every raw `corner_radius`/`.radius()`
// literal now references a radius token (`BUTTON`/`CARD`/`SMALL`/`POPUP`/
// `HAIRLINE_RADIUS`). The one survivor is a `// design-token-exempt:` circular
// status dot. From here the radius guard is absolute — any raw literal fails.
// COLOR is still grandfathered pending the §15 ramp.
// DEBT: tokenize the graph-editor redesign's 69 raw literals before that
// redesign closes, then ratchet the baseline back down toward the §15 ramp.
const COLOR_BASELINE: usize = 214;
const RADIUS_BASELINE: usize = 0;

#[test]
fn no_new_raw_color_literals() {
    let counts = scan();
    eprintln!(
        "design-token guard: Color32::new={} raw_radius={}",
        counts.color, counts.radius
    );
    assert!(
        counts.color <= COLOR_BASELINE,
        "Raw `Color32::new(` count rose to {} (baseline {COLOR_BASELINE}). Use a `color::` token, \
         or add `// design-token-exempt: <reason>` if it's a genuine one-off.",
        counts.color,
    );
    assert_eq!(
        counts.color, COLOR_BASELINE,
        "Raw `Color32::new(` count dropped to {} — nice. Lower COLOR_BASELINE to {} to ratchet it in.",
        counts.color, counts.color,
    );
}

#[test]
fn no_new_raw_radius_literals() {
    let counts = scan();
    // Radius is fully tokenised (baseline 0), so a single equality catches both
    // directions: a new raw literal pushes the count up; an intentional further
    // cleanup that somehow lowered it would also trip (there's nothing left to
    // clean, so that can't happen — the message covers the realistic case).
    assert_eq!(
        counts.radius, RADIUS_BASELINE,
        "Raw `corner_radius:`/`.radius(` literal count is {} (baseline {RADIUS_BASELINE}). \
         Radius is fully tokenised — use a `color::*_RADIUS` token, or add \
         `// design-token-exempt: <reason>` for a genuine one-off (e.g. a circular dot).",
        counts.radius,
    );
}

struct Counts {
    color: usize,
    radius: usize,
}

fn scan() -> Counts {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);

    let mut counts = Counts { color: 0, radius: 0 };
    for path in &files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // `color.rs` is the token home; `node.rs` defines `Color32` and its consts.
        let token_home = name == "color.rs" || name == "node.rs";
        let text = fs::read_to_string(path).unwrap();
        for line in text.lines() {
            let (is_color, is_radius) = classify(line);
            counts.color += usize::from(!token_home && is_color);
            counts.radius += usize::from(!token_home && is_radius);
        }
    }
    counts
}

/// Classify one source line: `(raw_color, raw_radius)`. A `// design-token-exempt:`
/// comment on the line suppresses both.
fn classify(line: &str) -> (bool, bool) {
    if line.contains("// design-token-exempt:") {
        return (false, false);
    }
    (line.contains("Color32::new("), raw_radius(line))
}

/// A raw radius literal is `corner_radius:` or `.radius(` immediately followed by
/// a numeric literal. A token (`color::CARD_RADIUS`) or an expression (`dot * 0.5`)
/// starts with a letter, so it isn't flagged.
fn raw_radius(line: &str) -> bool {
    raw_literal_after(line, "corner_radius:") || raw_literal_after(line, ".radius(")
}

fn raw_literal_after(line: &str, needle: &str) -> bool {
    line.find(needle).is_some_and(|i| {
        line[i + needle.len()..]
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    })
}

#[test]
fn classifier_detects_and_exempts() {
    // Raw colour construction is flagged; an exempt comment clears it.
    assert_eq!(classify("    bg: Color32::new(1, 2, 3, 4),"), (true, false));
    assert_eq!(
        classify("    bg: Color32::new(1, 2, 3, 4), // design-token-exempt: dynamic alpha blend"),
        (false, false),
    );
    // Raw radius literals flagged; tokens and computed expressions are not.
    assert_eq!(classify("    corner_radius: 2.0,"), (false, true));
    assert_eq!(classify("        .radius(6.0)"), (false, true));
    assert_eq!(classify("    corner_radius: color::CARD_RADIUS,"), (false, false));
    assert_eq!(classify("        .radius(SECTION_RADIUS - 1.0)"), (false, false));
    assert_eq!(classify("    corner_radius: dot * 0.5,"), (false, false));
    // A plain line is clean.
    assert_eq!(classify("    let x = 5;"), (false, false));
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
