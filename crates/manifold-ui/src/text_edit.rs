//! One text-editing model for the whole app (UI_WIDGET_UNIFICATION P5a, D16).
//!
//! Sibling of `slider.rs`/`stepper.rs`/`drag.rs` — a widget-layer primitive,
//! no deps beyond the crate (satisfies ui-depends-only-on-foundation). Owns
//! the editing MODEL only: text + caret + selection, and the pure mutations
//! on it (insert/delete/move/select). Everything host-specific — rendering,
//! session lifecycle (begin/commit/cancel), pointer↔byte mapping via a
//! concrete measurer, clipboard, undo-stack integration — stays with the
//! host (P5b/P5c). `manifold-app/src/text_input.rs`'s `TextInputState` and
//! `graph_canvas/mapping_popover.rs`'s `MappingPopover` both embed one
//! instead of hand-rolling their own caret/selection mechanics (I7).
//!
//! Selection model (Peter, 2026-07-13): click places the caret, click-drag
//! selects a range, shift-click extends, double-click selects the word,
//! Cmd+A selects all; typing replaces the selection. Clipboard and the
//! session commit/cancel/undo policy are host concerns (D16), not modeled
//! here.
//!
//! No IME: this model only ever receives committed characters (`insert_char`/
//! `insert_str`) — there is no marked-text / composition state. Consequence,
//! stated in the design doc (D16): composition-based scripts and macOS
//! dead-key accents are a host-layer concern, not this model's — see the
//! host's own doc comments for what it does with a raw `Key::Character`
//! before it reaches here.

use std::ops::Range;

/// One text-editing session's state: the text plus a caret/anchor pair, both
/// byte offsets always on a UTF-8 char boundary. `anchor == caret` means no
/// selection. Pointer x ↔ byte offset stays OUTSIDE the model — see
/// [`byte_offset_for_x`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEditModel {
    text: String,
    caret: usize,
    anchor: usize,
}

/// A word character for word-motion purposes: alphanumeric or `_`, matching
/// the common "identifier word" convention (Rust identifiers, param names,
/// clip/marker names — the actual content typed into MANIFOLD text fields).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextEditModel {
    /// A fresh session seeded with `text`; caret and anchor both start at the
    /// end (no selection). Hosts that want the "first keystroke replaces
    /// everything" convention call [`Self::select_all`] right after (P5b).
    pub fn new(text: &str) -> Self {
        let len = text.len();
        Self { text: text.to_string(), caret: len, anchor: len }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The normalized selection range (`start <= end`), always on char
    /// boundaries. Empty (`start == end`) when there's no selection.
    pub fn selection(&self) -> Range<usize> {
        if self.caret <= self.anchor {
            self.caret..self.anchor
        } else {
            self.anchor..self.caret
        }
    }

    /// Byte offset of the caret (the live editing position — where the next
    /// typed character lands).
    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    /// True when there's a non-empty selection.
    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    /// Empties the model, returning the text it held. Caret/anchor reset to
    /// 0. Used by a host tearing down a session it's about to discard (not
    /// "get text" — for that, use [`Self::text`]).
    pub fn take_text(&mut self) -> String {
        self.caret = 0;
        self.anchor = 0;
        std::mem::take(&mut self.text)
    }

    // ── Char-boundary walking ────────────────────────────────────────

    fn prev_boundary(&self, idx: usize) -> usize {
        if idx == 0 {
            return 0;
        }
        let mut i = idx - 1;
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_boundary(&self, idx: usize) -> usize {
        if idx >= self.text.len() {
            return self.text.len();
        }
        let mut i = idx + 1;
        while i < self.text.len() && !self.text.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    fn word_left(&self, mut i: usize) -> usize {
        // Skip whitespace immediately to the left, then the word run.
        while i > 0 {
            let p = self.prev_boundary(i);
            if self.text[p..i].chars().next().is_some_and(char::is_whitespace) {
                i = p;
            } else {
                break;
            }
        }
        while i > 0 {
            let p = self.prev_boundary(i);
            if self.text[p..i].chars().next().is_some_and(is_word_char) {
                i = p;
            } else {
                break;
            }
        }
        i
    }

    /// If the caret is inside a word, moves to the end of THAT word (mac
    /// Option+Right from mid-word doesn't jump the trailing space too). If
    /// the caret is already past a word (on whitespace, or sitting exactly
    /// at a word's end), skips forward to the end of the NEXT word instead.
    fn word_right(&self, i: usize) -> usize {
        let len = self.text.len();
        let mut j = i;
        let mut moved = false;
        while j < len {
            let n = self.next_boundary(j);
            if self.text[j..n].chars().next().is_some_and(is_word_char) {
                j = n;
                moved = true;
            } else {
                break;
            }
        }
        if moved {
            return j;
        }
        while j < len {
            let n = self.next_boundary(j);
            if self.text[j..n].chars().next().is_some_and(char::is_whitespace) {
                j = n;
            } else {
                break;
            }
        }
        while j < len {
            let n = self.next_boundary(j);
            if self.text[j..n].chars().next().is_some_and(is_word_char) {
                j = n;
            } else {
                break;
            }
        }
        j
    }

    // ── Mutation ─────────────────────────────────────────────────────

    /// Deletes the current selection (if any) and returns `true` if it did.
    /// The shared first step of every insert/backspace/delete — typing (or
    /// backspacing, or forward-deleting) with an active selection replaces
    /// it, never edits around it.
    fn delete_selection_if_any(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let r = self.selection();
        self.text.replace_range(r.clone(), "");
        self.caret = r.start;
        self.anchor = r.start;
        true
    }

    pub fn insert_char(&mut self, c: char) {
        self.delete_selection_if_any();
        self.text.insert(self.caret, c);
        self.caret += c.len_utf8();
        self.anchor = self.caret;
    }

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.delete_selection_if_any();
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.anchor = self.caret;
    }

    /// Deletes the selection, or (no selection) the char before the caret.
    pub fn backspace(&mut self) {
        if self.delete_selection_if_any() {
            return;
        }
        if self.caret == 0 {
            return;
        }
        let p = self.prev_boundary(self.caret);
        self.text.replace_range(p..self.caret, "");
        self.caret = p;
        self.anchor = p;
    }

    /// Deletes the selection, or (no selection) the char after the caret
    /// (forward delete).
    pub fn delete(&mut self) {
        if self.delete_selection_if_any() {
            return;
        }
        if self.caret >= self.text.len() {
            return;
        }
        let n = self.next_boundary(self.caret);
        self.text.replace_range(self.caret..n, "");
        // caret/anchor unchanged — deleting forward doesn't move the caret.
    }

    // ── Keyboard motion (select: Shift extends; word: Option/Ctrl word-step) ──

    pub fn move_left(&mut self, select: bool, word: bool) {
        if self.has_selection() && !select {
            let start = self.selection().start;
            self.caret = start;
            self.anchor = start;
            return;
        }
        let target = if word { self.word_left(self.caret) } else { self.prev_boundary(self.caret) };
        self.caret = target;
        if !select {
            self.anchor = self.caret;
        }
    }

    pub fn move_right(&mut self, select: bool, word: bool) {
        if self.has_selection() && !select {
            let end = self.selection().end;
            self.caret = end;
            self.anchor = end;
            return;
        }
        let target = if word { self.word_right(self.caret) } else { self.next_boundary(self.caret) };
        self.caret = target;
        if !select {
            self.anchor = self.caret;
        }
    }

    pub fn move_home(&mut self, select: bool) {
        self.caret = 0;
        if !select {
            self.anchor = 0;
        }
    }

    pub fn move_end(&mut self, select: bool) {
        self.caret = self.text.len();
        if !select {
            self.anchor = self.text.len();
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Selects the word (per [`is_word_char`]) touching byte offset `byte`.
    /// If `byte` lands on a non-word char (whitespace/punctuation), the
    /// selection collapses to a caret at `byte` (clamped to a boundary) —
    /// no word to select.
    pub fn select_word_at(&mut self, byte: usize) {
        let byte = byte.min(self.text.len());
        let byte = if self.text.is_char_boundary(byte) { byte } else { self.prev_boundary(byte + 1) };
        let on_word_char = self.text[byte..]
            .chars()
            .next()
            .is_some_and(is_word_char);
        if !on_word_char {
            self.caret = byte;
            self.anchor = byte;
            return;
        }
        let mut start = byte;
        while start > 0 {
            let p = self.prev_boundary(start);
            if self.text[p..start].chars().next().is_some_and(is_word_char) {
                start = p;
            } else {
                break;
            }
        }
        let mut end = byte;
        let len = self.text.len();
        while end < len {
            let n = self.next_boundary(end);
            if self.text[end..n].chars().next().is_some_and(is_word_char) {
                end = n;
            } else {
                break;
            }
        }
        self.anchor = start;
        self.caret = end;
    }

    /// Places the caret at `byte` (clamped to a char boundary); extends the
    /// current selection when `extend` (shift-click), else collapses to a
    /// fresh caret (plain click) — the anchor moves to `byte` too.
    pub fn caret_to(&mut self, byte: usize, extend: bool) {
        let byte = byte.min(self.text.len());
        let byte = if self.text.is_char_boundary(byte) { byte } else { self.prev_boundary(byte + 1) };
        self.caret = byte;
        if !extend {
            self.anchor = byte;
        }
    }

    /// Moves the caret to `byte` during a mouse drag, anchor held fixed at
    /// wherever the press started — the live-selection-growing counterpart
    /// to `caret_to`'s single-shot click/shift-click.
    pub fn drag_to(&mut self, byte: usize) {
        let byte = byte.min(self.text.len());
        let byte = if self.text.is_char_boundary(byte) { byte } else { self.prev_boundary(byte + 1) };
        self.caret = byte;
    }
}

/// Maps a pointer's x position (relative to the text's left edge) to the
/// nearest byte offset, via `measure` (the host's own text-width function —
/// `Painter::text_width` on the canvas, `UIRenderer`'s measurer in chrome).
/// Pure, allocation-free beyond what `measure` itself does. Midpoint
/// rounding: a click past a glyph's horizontal midpoint lands after it, not
/// before — the standard text-editor convention.
pub fn byte_offset_for_x(text: &str, rel_x: f32, measure: &mut dyn FnMut(&str) -> f32) -> usize {
    if rel_x <= 0.0 || text.is_empty() {
        return 0;
    }
    let mut prev_w = 0.0_f32;
    let mut prev_idx = 0usize;
    for (idx, ch) in text.char_indices() {
        let next_idx = idx + ch.len_utf8();
        let w = measure(&text[..next_idx]);
        let char_w = w - prev_w;
        let midpoint = prev_w + char_w * 0.5;
        if rel_x < midpoint {
            return prev_idx;
        }
        prev_w = w;
        prev_idx = next_idx;
    }
    prev_idx
}

/// Inverse of [`byte_offset_for_x`]: the x position (relative to the text's
/// left edge) of byte offset `byte` — the prefix width up to it. Used to
/// place the caret/selection-highlight glyph (P5c).
pub fn x_for_byte_offset(text: &str, byte: usize, measure: &mut dyn FnMut(&str) -> f32) -> f32 {
    let byte = byte.min(text.len());
    measure(&text[..byte])
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction / basic state ──────────────────────────────────

    #[test]
    fn new_seeds_caret_and_anchor_at_the_end_no_selection() {
        let m = TextEditModel::new("hello");
        assert_eq!(m.text(), "hello");
        assert_eq!(m.caret(), 5);
        assert_eq!(m.selection(), 5..5);
        assert!(!m.has_selection());
    }

    #[test]
    fn select_all_selects_whole_text() {
        let mut m = TextEditModel::new("hello");
        m.select_all();
        assert_eq!(m.selection(), 0..5);
        assert_eq!(m.selected_text(), "hello");
    }

    // ── Insert / delete, plain ──────────────────────────────────────

    #[test]
    fn insert_char_at_caret_advances_caret() {
        let mut m = TextEditModel::new("");
        m.insert_char('h');
        m.insert_char('i');
        assert_eq!(m.text(), "hi");
        assert_eq!(m.caret(), 2);
        assert!(!m.has_selection());
    }

    #[test]
    fn insert_str_inserts_at_caret() {
        let mut m = TextEditModel::new("ac");
        m.caret_to(1, false);
        m.insert_str("b");
        assert_eq!(m.text(), "abc");
        assert_eq!(m.caret(), 2);
    }

    #[test]
    fn backspace_deletes_char_before_caret() {
        let mut m = TextEditModel::new("abc");
        m.backspace();
        assert_eq!(m.text(), "ab");
        assert_eq!(m.caret(), 2);
    }

    #[test]
    fn backspace_at_start_is_a_noop() {
        let mut m = TextEditModel::new("abc");
        m.caret_to(0, false);
        m.backspace();
        assert_eq!(m.text(), "abc");
        assert_eq!(m.caret(), 0);
    }

    #[test]
    fn delete_removes_char_after_caret_without_moving_it() {
        let mut m = TextEditModel::new("abc");
        m.caret_to(0, false);
        m.delete();
        assert_eq!(m.text(), "bc");
        assert_eq!(m.caret(), 0);
    }

    #[test]
    fn delete_at_end_is_a_noop() {
        let mut m = TextEditModel::new("abc");
        m.delete();
        assert_eq!(m.text(), "abc");
    }

    // ── Selection replace-on-type (D16: "typing replaces the selection") ──

    #[test]
    fn typing_with_a_selection_replaces_it() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(0, false);
        m.caret_to(5, true); // select "hello"
        assert_eq!(m.selected_text(), "hello");
        m.insert_str("goodbye");
        assert_eq!(m.text(), "goodbye world");
        assert!(!m.has_selection());
    }

    #[test]
    fn backspace_with_a_selection_deletes_the_whole_selection() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(0, false);
        m.caret_to(5, true);
        m.backspace();
        assert_eq!(m.text(), " world");
    }

    #[test]
    fn delete_key_with_a_selection_deletes_the_whole_selection() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(0, false);
        m.caret_to(5, true);
        m.delete();
        assert_eq!(m.text(), " world");
    }

    // ── Multi-byte UTF-8 boundaries ─────────────────────────────────

    #[test]
    fn backspace_deletes_a_whole_multibyte_char_not_a_byte() {
        let mut m = TextEditModel::new("caf\u{e9}"); // "café" (é = 2 bytes)
        assert_eq!(m.text().len(), 5);
        m.backspace();
        assert_eq!(m.text(), "caf");
    }

    #[test]
    fn caret_motion_steps_by_char_not_byte_across_cjk() {
        // Each CJK char below is 3 bytes in UTF-8.
        let mut m = TextEditModel::new("\u{4f60}\u{597d}"); // "你好"
        m.caret_to(0, false);
        m.move_right(false, false);
        assert_eq!(m.caret(), 3, "one char = 3 bytes, not 1");
        m.move_right(false, false);
        assert_eq!(m.caret(), 6);
        m.move_left(false, false);
        assert_eq!(m.caret(), 3);
    }

    #[test]
    fn insert_and_delete_around_an_emoji_stay_on_char_boundaries() {
        let mut m = TextEditModel::new("a\u{1f600}b"); // "a😀b"
        // 😀 is a 4-byte scalar; total len = 1 + 4 + 1 = 6.
        assert_eq!(m.text().len(), 6);
        m.caret_to(5, false); // right after the emoji, before 'b'
        m.backspace();
        assert_eq!(m.text(), "ab", "backspace removed the whole emoji, not a byte of it");
    }

    #[test]
    fn move_left_from_middle_of_a_would_be_split_char_snaps_to_boundary() {
        // caret_to with a raw byte offset that lands mid-char (byte 2 is
        // inside "é", which spans bytes 1..3) must clamp to the nearest
        // boundary rather than panicking or corrupting state.
        let mut m = TextEditModel::new("caf\u{e9}");
        m.caret_to(2, false); // mid-é
        assert!(m.text().is_char_boundary(m.caret()));
    }

    // ── Word motion ──────────────────────────────────────────────────

    #[test]
    fn word_right_stops_at_the_end_of_the_next_word() {
        let mut m = TextEditModel::new("hello world foo");
        m.caret_to(0, false);
        m.move_right(false, true);
        assert_eq!(m.caret(), 5, "end of 'hello'");
        m.move_right(false, true);
        assert_eq!(m.caret(), 11, "end of 'world' (skips the space, then the word)");
    }

    #[test]
    fn word_left_stops_at_the_start_of_the_previous_word() {
        let mut m = TextEditModel::new("hello world foo");
        m.move_left(false, true);
        assert_eq!(m.caret(), 12, "start of 'foo'");
        m.move_left(false, true);
        assert_eq!(m.caret(), 6, "start of 'world'");
    }

    #[test]
    fn shift_word_right_extends_the_selection() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(0, false);
        m.move_right(true, true);
        assert_eq!(m.selection(), 0..5);
        assert_eq!(m.selected_text(), "hello");
    }

    // ── Arrow keys collapse an active selection (standard OS behavior) ──

    #[test]
    fn left_arrow_without_shift_collapses_selection_to_its_start() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(0, false);
        m.caret_to(5, true); // select "hello"
        m.move_left(false, false);
        assert_eq!(m.caret(), 0);
        assert!(!m.has_selection());
    }

    #[test]
    fn right_arrow_without_shift_collapses_selection_to_its_end() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(0, false);
        m.caret_to(5, true);
        m.move_right(false, false);
        assert_eq!(m.caret(), 5);
        assert!(!m.has_selection());
    }

    // ── Home/End ─────────────────────────────────────────────────────

    #[test]
    fn home_and_end_move_to_text_boundaries() {
        let mut m = TextEditModel::new("hello");
        m.caret_to(2, false);
        m.move_home(false);
        assert_eq!(m.caret(), 0);
        m.move_end(false);
        assert_eq!(m.caret(), 5);
    }

    #[test]
    fn shift_end_selects_to_the_end() {
        let mut m = TextEditModel::new("hello");
        m.caret_to(0, false);
        m.move_end(true);
        assert_eq!(m.selection(), 0..5);
    }

    // ── select_word_at ───────────────────────────────────────────────

    #[test]
    fn select_word_at_selects_the_containing_word() {
        let mut m = TextEditModel::new("hello world");
        m.select_word_at(7); // inside "world"
        assert_eq!(m.selected_text(), "world");
    }

    #[test]
    fn select_word_at_on_whitespace_collapses_to_a_caret() {
        let mut m = TextEditModel::new("hello world");
        m.select_word_at(5); // the space
        assert!(!m.has_selection());
        assert_eq!(m.caret(), 5);
    }

    #[test]
    fn select_word_at_a_multibyte_word_selects_the_whole_word() {
        let mut m = TextEditModel::new("caf\u{e9} time"); // "café time"
        m.select_word_at(1); // inside "café"
        assert_eq!(m.selected_text(), "caf\u{e9}");
    }

    // ── caret_to / drag_to ───────────────────────────────────────────

    #[test]
    fn caret_to_without_extend_collapses_selection() {
        let mut m = TextEditModel::new("hello world");
        m.select_all();
        m.caret_to(3, false);
        assert_eq!(m.caret(), 3);
        assert!(!m.has_selection());
    }

    #[test]
    fn caret_to_with_extend_grows_from_the_existing_anchor() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(2, false);
        m.caret_to(7, true);
        assert_eq!(m.selection(), 2..7);
    }

    #[test]
    fn drag_to_moves_caret_keeping_the_press_anchor_fixed() {
        let mut m = TextEditModel::new("hello world");
        m.caret_to(2, false); // press at 2 — anchor = caret = 2
        m.drag_to(8);
        assert_eq!(m.selection(), 2..8);
        m.drag_to(0);
        assert_eq!(m.selection(), 0..2, "dragging back past the anchor flips the range");
    }

    // ── take_text ────────────────────────────────────────────────────

    #[test]
    fn take_text_empties_the_model_and_returns_the_text() {
        let mut m = TextEditModel::new("hello");
        let out = m.take_text();
        assert_eq!(out, "hello");
        assert_eq!(m.text(), "");
        assert_eq!(m.caret(), 0);
    }

    // ── byte_offset_for_x / x_for_byte_offset (fake monospace measurer) ──

    /// 10px per ASCII char — deterministic, boundary math is exact.
    fn monospace(s: &str) -> f32 {
        s.chars().count() as f32 * 10.0
    }

    #[test]
    fn byte_offset_for_x_at_zero_or_negative_is_start() {
        let mut measure = monospace;
        assert_eq!(byte_offset_for_x("hello", -5.0, &mut measure), 0);
        assert_eq!(byte_offset_for_x("hello", 0.0, &mut measure), 0);
    }

    #[test]
    fn byte_offset_for_x_past_the_end_clamps_to_len() {
        let mut measure = monospace;
        assert_eq!(byte_offset_for_x("hi", 1000.0, &mut measure), 2);
    }

    #[test]
    fn byte_offset_for_x_rounds_to_the_nearer_glyph_edge() {
        let mut measure = monospace;
        // "hi": h occupies [0,10), i occupies [10,20).
        assert_eq!(byte_offset_for_x("hi", 4.0, &mut measure), 0, "before h's midpoint (5) -> before h");
        assert_eq!(byte_offset_for_x("hi", 6.0, &mut measure), 1, "past h's midpoint -> after h");
        assert_eq!(byte_offset_for_x("hi", 14.0, &mut measure), 1, "before i's midpoint (15) -> before i");
        assert_eq!(byte_offset_for_x("hi", 16.0, &mut measure), 2, "past i's midpoint -> after i");
    }

    #[test]
    fn byte_offset_for_x_on_empty_text_is_zero() {
        let mut measure = monospace;
        assert_eq!(byte_offset_for_x("", 50.0, &mut measure), 0);
    }

    #[test]
    fn x_for_byte_offset_is_the_inverse_of_byte_offset_for_x() {
        let mut measure = monospace;
        assert_eq!(x_for_byte_offset("hello", 0, &mut measure), 0.0);
        assert_eq!(x_for_byte_offset("hello", 3, &mut measure), 30.0);
        assert_eq!(x_for_byte_offset("hello", 5, &mut measure), 50.0);
    }
}
