//! Per-frame scratch storage that keeps its allocation across frames.

/// An empty `Vec<U>` that owns `v`'s allocation.
///
/// For per-frame lists whose element type borrows from the frame, such as
/// `Descriptor<'a>`: keep a `Vec<Descriptor<'static>>` field, recycle it into
/// this frame's lifetime, fill it, and recycle it back when the frame ends.
/// Only the allocation crosses frames; every element is dropped first.
pub fn recycle_vec<T, U>(mut v: Vec<T>) -> Vec<U> {
    const {
        assert!(
            size_of::<T>() == size_of::<U>() && align_of::<T>() == align_of::<U>(),
            "recycle_vec needs element types of one layout"
        )
    };
    v.clear();
    let mut v = std::mem::ManuallyDrop::new(v);
    // SAFETY: `v` is empty, so no `T` value is reinterpreted. `T` and `U`
    // have the same size and alignment, so the allocation's layout for
    // `capacity` elements is the same for both and the new Vec frees it
    // correctly.
    unsafe { Vec::from_raw_parts(v.as_mut_ptr().cast::<U>(), 0, v.capacity()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_allocation_and_drops_the_elements() {
        struct Borrowing<'a>(&'a str);
        let text = String::from("frame");
        let mut first: Vec<Borrowing<'_>> = Vec::with_capacity(8);
        first.push(Borrowing(&text));
        assert_eq!(first[0].0, "frame");
        let ptr = first.as_ptr() as usize;
        let kept: Vec<Borrowing<'static>> = recycle_vec(first);
        assert!(kept.is_empty());
        assert_eq!(kept.capacity(), 8);
        assert_eq!(kept.as_ptr() as usize, ptr);
        drop(text);
        let again: Vec<Borrowing<'_>> = recycle_vec(kept);
        assert_eq!(again.capacity(), 8);
    }
}
