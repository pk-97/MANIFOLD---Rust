/// Lightweight inline text input system.
///
/// When active, keyboard events are intercepted for text editing.
/// The app layer renders a small text field overlay at the specified position.
/// Enter commits, Escape cancels.

/// What kind of field is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputField {
    Bpm,
    Fps,
    LayerName(usize),
    ClipBpm,
}

/// Active text input session.
pub struct TextInputState {
    pub active: bool,
    pub field: TextInputField,
    pub text: String,
    pub cursor: usize,
}

impl TextInputState {
    pub fn new() -> Self {
        Self {
            active: false,
            field: TextInputField::Bpm,
            text: String::new(),
            cursor: 0,
        }
    }

    /// Begin editing a field with an initial value.
    pub fn begin(&mut self, field: TextInputField, initial: &str) {
        self.active = true;
        self.field = field;
        self.text = initial.to_string();
        self.cursor = self.text.len();
    }

    /// Cancel editing without committing.
    pub fn cancel(&mut self) {
        self.active = false;
        self.text.clear();
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.remove(prev);
            self.cursor = prev;
        }
    }

    /// Delete the character after the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    /// Commit the current text. Returns the field and final text.
    pub fn commit(&mut self) -> (TextInputField, String) {
        self.active = false;
        let field = self.field;
        let text = std::mem::take(&mut self.text);
        (field, text)
    }

    /// Move cursor left.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    /// Move cursor right.
    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += self.text[self.cursor..].chars().next().map_or(0, |c| c.len_utf8());
        }
    }
}
