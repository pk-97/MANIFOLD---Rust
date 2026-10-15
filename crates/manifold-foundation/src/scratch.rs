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

/// Text of at most `N` bytes held on the stack, so formatting a shown value
/// each frame allocates nothing. Written with `write!`; a write that would
/// overflow fails and leaves the text as it was before that piece.
#[derive(Clone, Copy)]
pub struct StackText<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> StackText<N> {
    pub fn as_str(&self) -> &str {
        // Only whole `&str` pieces are ever written, so the bytes are UTF-8.
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> Default for StackText<N> {
    fn default() -> Self {
        Self { bytes: [0; N], len: 0 }
    }
}

impl<const N: usize> PartialEq for StackText<N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<const N: usize> Eq for StackText<N> {}

impl<const N: usize> std::fmt::Debug for StackText<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
}

impl<const N: usize> std::fmt::Write for StackText<N> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        if end > N {
            return Err(std::fmt::Error);
        }
        self.bytes[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_text_formats_and_refuses_overflow() {
        use std::fmt::Write;
        let mut text = StackText::<8>::default();
        write!(text, "{:.2}", 1.5f32).unwrap();
        assert_eq!(text.as_str(), "1.50");
        assert!(text.write_str("too long").is_err());
        assert_eq!(text.as_str(), "1.50");
        text.clear();
        write!(text, "M{}", 3).unwrap();
        assert_eq!(text, {
            let mut other = StackText::<8>::default();
            other.write_str("M3").unwrap();
            other
        });
    }

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
