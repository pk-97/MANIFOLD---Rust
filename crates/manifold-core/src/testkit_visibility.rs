/// Expose an item to catalog tests while retaining its production visibility.
/// Both configurations are evaluated in the crate invoking this macro.
#[macro_export]
macro_rules! testkit_visible {
    // Preserve explicit variants when fields or test visibility differ.
    (testkit { $test_item:item } production { $production_item:item }) => {
        #[cfg(any(test, feature = "testkit"))]
        $test_item
        #[cfg(not(any(test, feature = "testkit")))]
        $production_item
    };
    ($(#[$attribute:meta])* $visibility:vis $kind:ident $($body:tt)*) => {
        #[cfg(any(test, feature = "testkit"))]
        $(#[$attribute])* pub $kind $($body)*
        #[cfg(not(any(test, feature = "testkit")))]
        $(#[$attribute])* $visibility $kind $($body)*
    };
}
