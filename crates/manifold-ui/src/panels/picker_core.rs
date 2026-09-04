//! `PickerCore` — the reusable pick-from-a-list model shared by every
//! search + category-chip + filtered-grid + keyboard-nav surface.
//!
//! `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` section 4, D3. Owns items, categories,
//! filter text, the filtered index list, the keyboard cursor, and scroll —
//! plus the interaction rules (typing filters, chips filter, arrows move,
//! Enter picks, Escape dismisses). Deliberately does NOT render: the browser
//! popup draws a grid, a future list-style picker would draw rows — drawing
//! stays per-surface, only the model + interaction is shared.
//!
//! The browser popup (`browser_popup.rs`) is the first consumer, migrated in
//! P2 of the design doc; a second consumer is the library browser
//! (`PRESET_LIBRARY_DESIGN.md` P3).

use crate::input::Key;
use crate::scroll_container::ScrollContainer;

/// One selectable item. Replaces the parallel per-field `Vec<String>`s
/// (name / type id / category / search-alias, one vec each) a picker
/// request used to carry — D5.
#[derive(Debug, Clone)]
pub struct PickerItem {
    pub label: String,
    pub type_id: String,
    pub category: Option<String>,
    /// Extra haystack (aliases etc.); filter matches label + this.
    pub search_text: Option<String>,
    /// Origin badge for library surfaces (PRESET_LIBRARY_DESIGN P5, D6):
    /// display text only ("Factory" / "My Library" / "Project" / "missing
    /// from library") — filtering uses [`Self::source`], not this string.
    pub badge: Option<String>,
    /// Source-filter dimension (PRESET_LIBRARY_DESIGN P5, D6): `None` for
    /// pickers with no source concept (the graph-editor node picker).
    pub source: Option<Source>,
    /// True for a project-embedded `Snapshot` entry surfaced only because its
    /// library file is gone (PRESET_LIBRARY_DESIGN section 3/D6: "listed only when
    /// their source file is gone, badged 'missing from library'"). Distinct
    /// from `source`/`badge` because it also gates the browser's right-click
    /// management menu off (an auto-captured cache isn't user-manageable the
    /// way a `Saved` project preset is).
    pub missing_from_library: bool,
    /// Absolute path to a save-time-rendered thumbnail PNG (PRESET_LIBRARY_DESIGN
    /// P6, D7) — `Some` for a Factory/My-Library entry that has one, `None`
    /// otherwise (This-Project entries never do; browse time never renders one
    /// to fill the gap). Doubles as the cache key the app decodes+registers
    /// once per distinct path (`manifold_ui::node::texture_handle_for_key`).
    pub thumbnail: Option<String>,
}

/// Which of the three library places an item's def lives in — the browser's
/// filter row (PRESET_LIBRARY_DESIGN P5, D6: "All · Factory · My Library ·
/// This Project"). `None` (no filter dimension) is for pickers that aren't
/// preset browsers (e.g. the graph-editor node picker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Ships with the app; read-only.
    Factory,
    /// A file under the user's library folder.
    MyLibrary,
    /// A project-embedded preset (`origin: Saved`, or a `Snapshot` whose
    /// library file is gone — see [`PickerItem::missing_from_library`]).
    Project,
}

/// Result of a keyboard-nav step ([`PickerCore::key_nav`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerNav {
    /// The cursor moved; no selection yet.
    Moved,
    /// An item was picked — carries its index into the original `items` list
    /// passed to [`PickerCore::new`] (not a filtered-list position).
    Picked(usize),
    /// Escape — the caller should close the picker.
    Dismissed,
    /// The key wasn't a nav key, or there was nothing to act on (e.g. Enter
    /// with no cursor and an empty filter).
    Ignored,
}

/// The pick-from-a-list model: items, categories, filter, filtered indices,
/// keyboard cursor, and scroll. Rendering (grid cells, list rows, chips)
/// stays on the consuming surface.
pub struct PickerCore {
    items: Vec<PickerItem>,
    categories: Vec<String>,
    active_category: Option<String>,
    /// Source-filter dimension (PRESET_LIBRARY_DESIGN P5, D6) — `None` = "All".
    active_source: Option<Source>,
    filter: String,
    /// Indices into `items` that pass the current category + source + filter.
    filtered: Vec<usize>,
    /// Keyboard position *within `filtered`* (not an `items` index).
    cursor: Option<usize>,
    pub scroll: ScrollContainer,
}

impl PickerCore {
    pub fn new(items: Vec<PickerItem>, categories: Vec<String>) -> Self {
        let mut me = Self {
            items,
            categories,
            active_category: None,
            active_source: None,
            filter: String::new(),
            filtered: Vec::new(),
            cursor: None,
            scroll: ScrollContainer::new(),
        };
        me.rebuild_filtered();
        me
    }

    /// Category chip labels (the picker's full set, before the "Generators"-
    /// style surface-specific exclusions the browser applies when drawing
    /// chips).
    pub fn categories(&self) -> &[String] {
        &self.categories
    }

    /// The active category chip, if any.
    pub fn active_category(&self) -> Option<&str> {
        self.active_category.as_deref()
    }

    /// Set the search filter. Resets scroll + keyboard cursor (a changed
    /// result set invalidates any prior cursor position). A no-op when the
    /// text is unchanged — callers that reapply the live text on every
    /// consumed keystroke (cursor-move keys, re-committing the same string)
    /// must not silently wipe an in-progress keyboard-nav cursor.
    pub fn set_filter(&mut self, filter: String) {
        if filter == self.filter {
            return;
        }
        self.filter = filter;
        self.scroll.reset();
        self.rebuild_filtered();
    }

    /// Set the active category chip (`None` = "All"). Resets scroll + cursor.
    pub fn set_category(&mut self, cat: Option<String>) {
        self.active_category = cat;
        self.scroll.reset();
        self.rebuild_filtered();
    }

    /// Set the active source chip (`None` = "All" — PRESET_LIBRARY_DESIGN P5,
    /// D6). Resets scroll + cursor, mirroring [`Self::set_category`].
    pub fn set_source(&mut self, source: Option<Source>) {
        self.active_source = source;
        self.scroll.reset();
        self.rebuild_filtered();
    }

    /// The active source chip, if any.
    pub fn active_source(&self) -> Option<Source> {
        self.active_source
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The items passing the current category + filter, as `(items index,
    /// &PickerItem)` pairs in filtered order. Callers that need the keyboard
    /// cursor's position should `.enumerate()` this — `cursor()` is a position
    /// within this sequence, not an `items` index.
    pub fn filtered(&self) -> impl Iterator<Item = (usize, &PickerItem)> {
        self.filtered.iter().map(move |&i| (i, &self.items[i]))
    }

    /// Keyboard cursor position within [`Self::filtered`] (`None` = no
    /// keyboard selection yet — the mouse-hover-only state).
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Count of items passing the current filter/category — avoids
    /// materializing [`Self::filtered`] just to measure it (the grid's
    /// row-count math).
    pub fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    /// The full item at an `items` index (e.g. resolving a
    /// [`PickerNav::Picked`] payload, which carries an `items` index, not a
    /// filtered-list position). `None` if `idx` is out of range.
    pub fn item(&self, idx: usize) -> Option<&PickerItem> {
        self.items.get(idx)
    }

    /// Every item, regardless of the current filter/category/source
    /// (PRESET_LIBRARY_DESIGN P6) — the app decodes+registers every open
    /// item's thumbnail up front (a bounded corpus, tens of presets) rather
    /// than tracking which ones are currently filtered into view.
    pub fn all_items(&self) -> impl Iterator<Item = &PickerItem> {
        self.items.iter()
    }

    /// Handle keyboard nav in grid geometry (PRESET_BROWSER_AUDITION_DESIGN
    /// D14): Left/Right move within the cursor's row (clamped at the row
    /// edges — they never cross rows), Up/Down move a full row with
    /// wraparound, Home/End jump to the first/last item, PageUp/PageDown
    /// move one screenful (`page` rows × `columns`, clamped, no wrap).
    /// Enter picks the cursor's item; with no cursor and a non-empty filter
    /// it picks `filtered[0]` — the type-and-enter fast path. Escape
    /// dismisses. `columns`/`page` describe the rendered grid (`columns`
    /// ≥ 1; `page` ≥ 1 row). Any other key is `Ignored`.
    pub fn key_nav(&mut self, key: Key, columns: usize, page: usize) -> PickerNav {
        let columns = columns.max(1);
        if key == Key::Escape {
            return PickerNav::Dismissed;
        }
        if self.filtered.is_empty() {
            return PickerNav::Ignored;
        }
        let len = self.filtered.len();
        let len_i = len as i64;
        match key {
            Key::Left => {
                let c = self.cursor.unwrap_or(0);
                let row_start = c / columns * columns;
                self.cursor = Some(c.saturating_sub(1).max(row_start));
                PickerNav::Moved
            }
            Key::Right => {
                let c = self.cursor.unwrap_or(0);
                let row_end = ((c / columns + 1) * columns).min(len) - 1;
                self.cursor = Some((c + 1).min(row_end));
                PickerNav::Moved
            }
            Key::Up => {
                // From no cursor, enter at the last item (list semantics);
                // otherwise move a full row up, wrapping into the last row.
                self.cursor = Some(match self.cursor {
                    None => len - 1,
                    Some(c) => (c as i64 - columns as i64).rem_euclid(len_i) as usize,
                });
                PickerNav::Moved
            }
            Key::Down => {
                // From no cursor, enter at the first item; otherwise a full
                // row down, wrapping into the first row.
                self.cursor = Some(match self.cursor {
                    None => 0,
                    Some(c) => (c as i64 + columns as i64).rem_euclid(len_i) as usize,
                });
                PickerNav::Moved
            }
            Key::Home => {
                self.cursor = Some(0);
                PickerNav::Moved
            }
            Key::End => {
                self.cursor = Some(len - 1);
                PickerNav::Moved
            }
            Key::PageUp => {
                let c = self.cursor.unwrap_or(0);
                self.cursor = Some(c.saturating_sub(page.max(1) * columns));
                PickerNav::Moved
            }
            Key::PageDown => {
                let c = self.cursor.unwrap_or(0);
                self.cursor = Some((c + page.max(1) * columns).min(len - 1));
                PickerNav::Moved
            }
            Key::Enter => match self.cursor {
                Some(pos) if pos < len => PickerNav::Picked(self.filtered[pos]),
                None if !self.filter.is_empty() => PickerNav::Picked(self.filtered[0]),
                _ => PickerNav::Ignored,
            },
            _ => PickerNav::Ignored,
        }
    }

    /// Verbatim move of `BrowserPopupPanel::rebuild_filtered_list`:
    /// case-insensitive substring over `search_text.unwrap_or(label)`, with a
    /// category pre-filter, plus the source pre-filter (PRESET_LIBRARY_DESIGN
    /// P5, D6). Resets the keyboard cursor — a changed filtered set
    /// invalidates any prior position.
    fn rebuild_filtered(&mut self) {
        self.filtered.clear();
        let filter_lower = self.filter.to_lowercase();
        for (i, item) in self.items.iter().enumerate() {
            if let Some(ref cat) = self.active_category
                && item.category.as_deref() != Some(cat.as_str())
            {
                continue;
            }
            if let Some(src) = self.active_source
                && item.source != Some(src)
            {
                continue;
            }
            if !filter_lower.is_empty() {
                let haystack = item.search_text.as_deref().unwrap_or(&item.label);
                if !haystack.to_lowercase().contains(&filter_lower) {
                    continue;
                }
            }
            self.filtered.push(i);
        }
        self.cursor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, category: Option<&str>, search: Option<&str>) -> PickerItem {
        item_with_source(label, category, search, None)
    }

    fn item_with_source(
        label: &str,
        category: Option<&str>,
        search: Option<&str>,
        source: Option<Source>,
    ) -> PickerItem {
        PickerItem {
            label: label.to_string(),
            type_id: label.to_lowercase().replace(' ', "_"),
            category: category.map(str::to_string),
            search_text: search.map(str::to_string),
            badge: None,
            source,
            missing_from_library: false,
            thumbnail: None,
        }
    }

    fn sample() -> PickerCore {
        PickerCore::new(
            vec![
                item("Gaussian Blur", Some("Spatial"), None),
                item("Chromatic Aberration", Some("Filmic"), None),
                item("Blur TOP", Some("Spatial"), Some("gaussian blur legacy")),
                item("Noise Field", None, None),
            ],
            vec!["Spatial".to_string(), "Filmic".to_string()],
        )
    }

    /// Sample mirroring a real preset browser: one Factory, one My Library,
    /// one Project entry, spread across two categories so a source-alone
    /// filter can be told apart from a category-alone filter.
    fn source_sample() -> PickerCore {
        PickerCore::new(
            vec![
                item_with_source("Bloom", Some("Post-Process"), None, Some(Source::Factory)),
                item_with_source("Bloom 2", Some("Post-Process"), None, Some(Source::MyLibrary)),
                item_with_source("Sunset Glow", Some("Filmic"), None, Some(Source::Project)),
                item_with_source("Chromatic Aberration", Some("Filmic"), None, Some(Source::Factory)),
            ],
            vec!["Post-Process".to_string(), "Filmic".to_string()],
        )
    }

    #[test]
    fn filter_matches_label_substring_case_insensitive() {
        let mut p = sample();
        p.set_filter("blur".to_string());
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        // "Gaussian Blur" (label) and "Blur TOP" (search_text alias) both
        // match; "Chromatic Aberration" and "Noise Field" don't.
        assert_eq!(labels, vec!["Gaussian Blur", "Blur TOP"]);
    }

    #[test]
    fn filter_matches_search_text_haystack_over_label() {
        let mut p = sample();
        // "legacy" only appears in Blur TOP's search_text, not its label.
        p.set_filter("legacy".to_string());
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert_eq!(labels, vec!["Blur TOP"]);
    }

    #[test]
    fn category_prefilter_excludes_other_categories_and_uncategorized() {
        let mut p = sample();
        p.set_category(Some("Spatial".to_string()));
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        // Filmic ("Chromatic Aberration") and uncategorized ("Noise Field")
        // are excluded even though neither has an active filter string.
        assert_eq!(labels, vec!["Gaussian Blur", "Blur TOP"]);
    }

    /// Generator items re-tagged `category: "LED"` (LED_STRIPS_DESIGN
    /// MVP-P3c) filter through `set_category` exactly like effect items —
    /// the lane-scoped browser open is a plain category prefilter.
    #[test]
    fn led_category_prefilter_selects_only_led_items() {
        let mut p = PickerCore::new(
            vec![
                item_with_source("LED Fill", Some("LED"), None, Some(Source::Factory)),
                item_with_source("LED Pulse", Some("LED"), None, Some(Source::Factory)),
                item_with_source("Plasma", Some("Pattern"), None, Some(Source::Factory)),
                item("Wave", None, None),
            ],
            vec!["LED".to_string(), "Pattern".to_string()],
        );
        p.set_category(Some("LED".to_string()));
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert_eq!(labels, vec!["LED Fill", "LED Pulse"]);

        // Widening back to "All" returns the full list — the scoped open is
        // a starting chip, not a hard filter.
        p.set_category(None);
        assert_eq!(p.filtered_len(), 4);
    }

    // ── Source filter (PRESET_LIBRARY_DESIGN P5, D6) ────────────────────

    #[test]
    fn source_filter_alone_selects_only_that_source() {
        let mut p = source_sample();
        assert_eq!(p.filtered_len(), 4, "no filter active yet — all four items");

        p.set_source(Some(Source::Factory));
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert_eq!(labels, vec!["Bloom", "Chromatic Aberration"]);

        p.set_source(Some(Source::MyLibrary));
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert_eq!(labels, vec!["Bloom 2"]);

        p.set_source(Some(Source::Project));
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert_eq!(labels, vec!["Sunset Glow"]);

        // Back to "All".
        p.set_source(None);
        assert_eq!(p.filtered_len(), 4);
    }

    #[test]
    fn source_and_category_combine_as_an_and() {
        let mut p = source_sample();
        // Factory ∩ Filmic = "Chromatic Aberration" only ("Bloom" is Factory
        // but Post-Process; "Sunset Glow" is Filmic but Project).
        p.set_source(Some(Source::Factory));
        p.set_category(Some("Filmic".to_string()));
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert_eq!(labels, vec!["Chromatic Aberration"]);
    }

    #[test]
    fn source_and_text_filter_combine_as_an_and() {
        let mut p = source_sample();
        // Both "Bloom" entries match the text filter; restricting to
        // MyLibrary must leave only "Bloom 2".
        p.set_filter("bloom".to_string());
        p.set_source(Some(Source::MyLibrary));
        let labels: Vec<&str> = p.filtered().map(|(_, it)| it.label.as_str()).collect();
        assert_eq!(labels, vec!["Bloom 2"]);
    }

    #[test]
    fn nav_up_from_first_wraps_to_last() {
        let mut p = sample();
        // 4 items, no filter/category → all four in filtered order. A single
        // column reduces grid nav to list nav.
        assert_eq!(p.key_nav(Key::Down, 1, 1), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
        assert_eq!(p.key_nav(Key::Up, 1, 1), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(3));
    }

    #[test]
    fn nav_down_from_last_wraps_to_first() {
        let mut p = sample();
        for _ in 0..4 {
            p.key_nav(Key::Down, 1, 1);
        }
        assert_eq!(p.cursor(), Some(3));
        assert_eq!(p.key_nav(Key::Down, 1, 1), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
    }

    // ── Grid geometry (PRESET_BROWSER_AUDITION_DESIGN D14) ─────────────

    /// A 3-column grid over 10 items: Down moves a full row (0→3→6→9); the
    /// wraparound lands a full row ahead, modulo the item count (9 → 2, the
    /// last row's raggedness shifts the column), and Up returns the same
    /// way (2 → 9).
    #[test]
    fn grid_nav_down_moves_a_full_row_and_wraps() {
        let mut p = PickerCore::new(
            (0..10).map(|i| item(&format!("Item {i}"), None, None)).collect(),
            vec![],
        );
        assert_eq!(p.key_nav(Key::Down, 3, 2), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
        p.key_nav(Key::Down, 3, 2);
        p.key_nav(Key::Down, 3, 2);
        assert_eq!(p.cursor(), Some(6));
        p.key_nav(Key::Down, 3, 2);
        assert_eq!(p.cursor(), Some(9));
        // Down from 9 wraps a full row ahead, mod 10 → 2.
        assert_eq!(p.key_nav(Key::Down, 3, 2), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(2));
        // Up from 2 wraps back to 9.
        assert_eq!(p.key_nav(Key::Up, 3, 2), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(9));
    }

    /// Left/Right move within the cursor's row and clamp at its edges — a
    /// 3-column grid: item 0 sits at the start of row 0, item 2 at its end.
    #[test]
    fn grid_nav_left_right_clamp_within_row() {
        let mut p = PickerCore::new(
            (0..10).map(|i| item(&format!("Item {i}"), None, None)).collect(),
            vec![],
        );
        // Enter the grid on the first row.
        p.key_nav(Key::Down, 3, 2);
        assert_eq!(p.cursor(), Some(0));
        // Left at the row start is a no-op (still Moved — it was a nav key).
        assert_eq!(p.key_nav(Key::Left, 3, 2), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
        p.key_nav(Key::Right, 3, 2);
        p.key_nav(Key::Right, 3, 2);
        assert_eq!(p.cursor(), Some(2));
        // Right clamps at the row end (item 2, last of row 0).
        assert_eq!(p.key_nav(Key::Right, 3, 2), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(2));
        // Down to row 1 (items 3-5), Left/Right stay inside it.
        p.key_nav(Key::Down, 3, 2);
        assert_eq!(p.cursor(), Some(5));
        p.key_nav(Key::Left, 3, 2);
        assert_eq!(p.cursor(), Some(4));
        p.key_nav(Key::Left, 3, 2);
        assert_eq!(p.cursor(), Some(3));
        // Left clamps at row 1's start.
        assert_eq!(p.key_nav(Key::Left, 3, 2), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(3));
    }

    #[test]
    fn grid_nav_home_end_and_page() {
        let mut p = PickerCore::new(
            (0..20).map(|i| item(&format!("Item {i}"), None, None)).collect(),
            vec![],
        );
        p.key_nav(Key::Down, 4, 3);
        assert_eq!(p.key_nav(Key::End, 4, 3), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(19));
        assert_eq!(p.key_nav(Key::Home, 4, 3), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
        // PageDown: 3 rows × 4 columns = 12 items; clamps at the last item.
        assert_eq!(p.key_nav(Key::PageDown, 4, 3), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(12));
        assert_eq!(p.key_nav(Key::PageDown, 4, 3), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(19));
        // PageUp back to the top, no wrap.
        assert_eq!(p.key_nav(Key::PageUp, 4, 3), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(7));
        assert_eq!(p.key_nav(Key::PageUp, 4, 3), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
    }

    #[test]
    fn type_and_enter_picks_first_filtered_without_cursor() {
        let mut p = sample();
        p.set_filter("gaussian".to_string());
        assert_eq!(p.cursor(), None);
        // "Gaussian Blur" (label match) and "Blur TOP" (alias match) both
        // pass; Enter with no cursor picks the first in filtered order.
        match p.key_nav(Key::Enter, 3, 2) {
            PickerNav::Picked(idx) => assert_eq!(p.filtered().next().unwrap().0, idx),
            other => panic!("expected Picked, got {other:?}"),
        }
    }

    #[test]
    fn enter_with_no_cursor_and_empty_filter_is_ignored() {
        let mut p = sample();
        assert_eq!(p.key_nav(Key::Enter, 3, 2), PickerNav::Ignored);
    }

    #[test]
    fn escape_dismisses_even_with_empty_filtered_set() {
        let mut p = sample();
        p.set_filter("nonexistent-xyz".to_string());
        assert!(p.filtered().next().is_none());
        assert_eq!(p.key_nav(Key::Escape, 3, 2), PickerNav::Dismissed);
    }
}
