//! `PickerCore` — the reusable pick-from-a-list model shared by every
//! search + category-chip + filtered-grid + keyboard-nav surface.
//!
//! `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §4, D3. Owns items, categories,
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
    /// library file is gone (PRESET_LIBRARY_DESIGN §3/D6: "listed only when
    /// their source file is gone, badged 'missing from library'"). Distinct
    /// from `source`/`badge` because it also gates the browser's right-click
    /// management menu off (an auto-captured cache isn't user-manageable the
    /// way a `Saved` project preset is).
    pub missing_from_library: bool,
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

    /// Handle Up/Down/Enter/Escape. Up/Down move the cursor within the
    /// filtered list with wraparound. Enter picks the cursor's item; with no
    /// cursor and a non-empty filter it picks `filtered[0]` — the
    /// type-and-enter fast path (click Add, type three letters, Enter — an
    /// item lands without the mouse ever finding a cell). Any other key is
    /// `Ignored`.
    pub fn key_nav(&mut self, key: Key) -> PickerNav {
        if key == Key::Escape {
            return PickerNav::Dismissed;
        }
        if self.filtered.is_empty() {
            return PickerNav::Ignored;
        }
        match key {
            Key::Up => {
                self.cursor = Some(match self.cursor {
                    None => self.filtered.len() - 1,
                    Some(0) => self.filtered.len() - 1,
                    Some(c) => c - 1,
                });
                PickerNav::Moved
            }
            Key::Down => {
                self.cursor = Some(match self.cursor {
                    Some(c) if c + 1 < self.filtered.len() => c + 1,
                    _ => 0,
                });
                PickerNav::Moved
            }
            Key::Enter => match self.cursor {
                Some(pos) if pos < self.filtered.len() => PickerNav::Picked(self.filtered[pos]),
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
        // 4 items, no filter/category → all four in filtered order.
        assert_eq!(p.key_nav(Key::Down), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
        assert_eq!(p.key_nav(Key::Up), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(3));
    }

    #[test]
    fn nav_down_from_last_wraps_to_first() {
        let mut p = sample();
        for _ in 0..4 {
            p.key_nav(Key::Down);
        }
        assert_eq!(p.cursor(), Some(3));
        assert_eq!(p.key_nav(Key::Down), PickerNav::Moved);
        assert_eq!(p.cursor(), Some(0));
    }

    #[test]
    fn type_and_enter_picks_first_filtered_without_cursor() {
        let mut p = sample();
        p.set_filter("gaussian".to_string());
        assert_eq!(p.cursor(), None);
        // "Gaussian Blur" (label match) and "Blur TOP" (alias match) both
        // pass; Enter with no cursor picks the first in filtered order.
        match p.key_nav(Key::Enter) {
            PickerNav::Picked(idx) => assert_eq!(p.filtered().next().unwrap().0, idx),
            other => panic!("expected Picked, got {other:?}"),
        }
    }

    #[test]
    fn enter_with_no_cursor_and_empty_filter_is_ignored() {
        let mut p = sample();
        assert_eq!(p.key_nav(Key::Enter), PickerNav::Ignored);
    }

    #[test]
    fn escape_dismisses_even_with_empty_filtered_set() {
        let mut p = sample();
        p.set_filter("nonexistent-xyz".to_string());
        assert!(p.filtered().next().is_none());
        assert_eq!(p.key_nav(Key::Escape), PickerNav::Dismissed);
    }
}
