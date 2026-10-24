//! The content thread's only mutable path into its project (D13, D24).
//!
//! `ProjectEdit` wraps `&mut Project` with no public way back out: only
//! `EditingService` (execute, undo, redo, `mutate`) and `tick_write` open it,
//! so every content-side write is stamped and audited. The UI thread builds
//! one over its own local copy with `ProjectEdit::new`.

use manifold_core::project::Project;

pub struct ProjectEdit<'a>(&'a mut Project);

impl<'a> ProjectEdit<'a> {
    pub fn new(project: &'a mut Project) -> Self {
        Self(project)
    }

    /// A shorter-lived edit over the same project, for several gateway calls
    /// in a row.
    pub fn reborrow(&mut self) -> ProjectEdit<'_> {
        ProjectEdit(self.0)
    }

    pub fn project(&self) -> &Project {
        self.0
    }

    pub(crate) fn into_mut(self) -> &'a mut Project {
        self.0
    }
}

impl<'a> From<&'a mut Project> for ProjectEdit<'a> {
    fn from(project: &'a mut Project) -> Self {
        Self(project)
    }
}

/// The four fields the tick may write (D24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickField {
    Bpm,
    ClockAuthority,
    SessionTempoPoints,
    RecordingProvenance,
}

/// An audited tick write: moves no stamp, publishes nothing, runs no
/// reaction. The caller compares before calling, so an unchanged value
/// never opens a write. Under `change-audit` it records the opened field
/// for the tick window.
pub fn tick_write(field: TickField, project: ProjectEdit<'_>, f: impl FnOnce(&mut Project)) {
    tick_audit::opened(field);
    f(project.into_mut());
}

/// The change audit around the content tick (D10, D24, INV-22): inside the
/// window only Values and the `TickField`s opened through `tick_write` may
/// change, and the tempo map only through `SessionTempoPoints`.
#[cfg(any(test, feature = "change-audit"))]
pub mod tick_audit {
    use super::TickField;
    use manifold_core::change::audit::{self, TickSlot};
    use manifold_core::change::{ChangeSet, Part};
    use manifold_core::project::Project;
    use std::cell::Cell;

    thread_local! {
        static OPENED: Cell<u8> = const { Cell::new(0) };
    }

    pub(crate) fn opened(field: TickField) {
        OPENED.with(|o| o.set(o.get() | 1 << field as u8));
    }

    pub struct Window(audit::Fingerprint);

    pub fn open(project: &Project) -> Window {
        OPENED.with(|o| o.set(0));
        Window(audit::fingerprint(project))
    }

    /// `gateway` is what `EditingService` stamped inside the window (a
    /// session-end Settings write, a discovery Mappings write): those went
    /// through their own audit.
    pub fn close(window: Window, project: &Project, gateway: ChangeSet) {
        let after = audit::fingerprint(project);
        let opened = OPENED.with(|o| o.replace(0));
        let settings_stamped = gateway.contains(Part::Settings);
        let has = |f: TickField| settings_stamped || opened & (1 << f as u8) != 0;
        let outside = audit::changed_outside_tick_slots(&window.0, &after)
            .minus(ChangeSet::of(&[Part::Values]).union(gateway));
        assert!(
            outside.is_empty(),
            "tick audit: the tick changed parts {:?} outside Values and the audited tick writes",
            outside.parts().collect::<Vec<_>>(),
        );
        for slot in audit::tick_slots_changed(&window.0, &after) {
            let allowed = match slot {
                TickSlot::Bpm => has(TickField::Bpm),
                TickSlot::ClockAuthority => has(TickField::ClockAuthority),
                TickSlot::TempoMap => has(TickField::SessionTempoPoints),
                TickSlot::RecordingProvenance => has(TickField::RecordingProvenance),
            };
            assert!(
                allowed,
                "tick audit: {slot:?} changed without an audited tick write"
            );
        }
    }
}

#[cfg(not(any(test, feature = "change-audit")))]
pub mod tick_audit {
    use super::TickField;
    use manifold_core::project::Project;

    pub struct Window;

    #[inline(always)]
    pub(crate) fn opened(_: TickField) {}

    #[inline(always)]
    pub fn open(_: &Project) -> Window {
        Window
    }

    #[inline(always)]
    pub fn close(_: Window, _: &Project, _: manifold_core::change::ChangeSet) {}
}
