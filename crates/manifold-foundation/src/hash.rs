//! A deterministic, dependency-free byte hash — the shared identity-key
//! utility behind the D6 fire meter
//! (`docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` P3c, BUG-082's
//! fix). Two independent call sites must agree on a fire-mode config's
//! identity hash WITHOUT sharing a domain type: the content thread
//! (`manifold_core::audio_trigger::FireMeterCapture::push`, in
//! `manifold-playback`) and the UI thread (a drawer meter's lookup key, in
//! `manifold-ui`). `manifold-ui` cannot depend on `manifold-core`
//! (`docs/UI_LAYERING_INVERSION.md`), so the hash itself — pure bytes in,
//! `u64` out, no domain semantics — lives here in the shared zero-dependency
//! vocabulary crate; the STATEFUL capture buffer it keys stays in
//! `manifold-core` where the domain logic belongs.

/// FNV-1a over raw byte parts. Not `ahash`/`SipHash`: those hashers seed from
/// a per-process random state in some build configs, which would silently
/// break content-thread/UI-thread agreement in exactly the flaky,
/// hard-to-repro way a hot-path bug likes to hide. Takes raw byte slices (not
/// `&str`) so a caller can hash a `usize` index (`idx.to_le_bytes()`) without
/// an allocating `to_string()`.
pub fn fire_meter_key(parts: &[&[u8]]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for part in parts {
        for &b in *part {
            hash ^= b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        // Separator byte between parts so ("ab", "c") hashes differently
        // from ("a", "bc").
        hash ^= 0xff;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_across_calls() {
        let a = fire_meter_key(&[b"effect-1", b"amount"]);
        let b = fire_meter_key(&[b"effect-1", b"amount"]);
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_parts_never_collide_across_a_boundary() {
        // ("ab", "c") must hash differently from ("a", "bc") — the separator
        // byte between parts is what prevents this.
        let a = fire_meter_key(&[b"ab", b"c"]);
        let b = fire_meter_key(&[b"a", b"bc"]);
        assert_ne!(a, b);
    }

    #[test]
    fn different_inputs_differ() {
        let a = fire_meter_key(&[b"layer-1", &1u64.to_le_bytes()]);
        let b = fire_meter_key(&[b"layer-1", &2u64.to_le_bytes()]);
        assert_ne!(a, b);
    }
}
