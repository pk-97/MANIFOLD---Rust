//! Change audit (D10): fingerprint every part before an edit, and after it
//! fail on any part that changed without being declared. Compiled in under
//! test and the `change-audit` feature; a no-op otherwise.

#[cfg(any(test, feature = "change-audit"))]
mod on {
    use manifold_core::change::{ChangeSet, audit};
    use manifold_core::project::Project;

    pub(crate) struct Window(audit::Fingerprint);

    pub(crate) fn open(project: &Project) -> Window {
        Window(audit::fingerprint(project))
    }

    pub(crate) fn close(window: Window, project: &Project, declared: ChangeSet, what: &str) {
        let undeclared = audit::changed(&window.0, &audit::fingerprint(project)).minus(declared);
        assert!(
            undeclared.is_empty(),
            "change audit: {what} changed undeclared parts {:?} (declared {:?})",
            undeclared.parts().collect::<Vec<_>>(),
            declared.parts().collect::<Vec<_>>(),
        );
    }
}

#[cfg(not(any(test, feature = "change-audit")))]
mod on {
    use manifold_core::change::ChangeSet;
    use manifold_core::project::Project;

    pub(crate) struct Window;

    #[inline(always)]
    pub(crate) fn open(_: &Project) -> Window {
        Window
    }

    #[inline(always)]
    pub(crate) fn close(_: Window, _: &Project, _: ChangeSet, _: &str) {}
}

pub(crate) use on::{close, open};
