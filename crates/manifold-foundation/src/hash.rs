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
//!
//! Production code never calls [`fire_meter_key`] directly — build a key via
//! [`fire_meter_key_for_param`] or [`fire_meter_key_for_clip_trigger`], the
//! two sanctioned constructors below. `fire_meter_key` stays `pub` only
//! because its own unit tests (and a sibling `manifold-core` re-export used
//! by that crate's tests) exercise the raw hash directly with shapes the
//! typed constructors can't express (e.g. a single-part key).

/// FNV-1a over raw byte parts. Not `ahash`/`SipHash`: those hashers seed from
/// a per-process random state in some build configs, which would silently
/// break content-thread/UI-thread agreement in exactly the flaky,
/// hard-to-repro way a hot-path bug likes to hide. Takes raw byte slices (not
/// `&str`) so a caller can hash a `usize` index (`idx.to_le_bytes()`) without
/// an allocating `to_string()`. Not the sanctioned entry point for a fire-meter
/// key — see the module doc.
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

/// Build a fire-meter key for a param-gate audio mod on an effect/generator
/// instance (`owner_id` = the instance's `EffectId` as a string, `param_id` =
/// the modulated param's id). Together with
/// [`fire_meter_key_for_clip_trigger`], this is the ONLY sanctioned way to
/// build a fire-meter key — the content-thread producer
/// (`manifold_playback::modulation::evaluate_instance_audio_mods`) and the
/// UI-thread consumer (`ParamCardPanel::update_fire_meters`) must call this
/// SAME function, never hand-assemble the byte-list themselves. A
/// hand-assembled list drifting from the producer's is exactly how a
/// generator card's meter went dead: the UI resolved a key built from a
/// blanked-out display id that the content thread never produced, so the two
/// sides silently never agreed.
pub fn fire_meter_key_for_param(owner_id: &str, param_id: &str) -> u64 {
    fire_meter_key(&[owner_id.as_bytes(), param_id.as_bytes()])
}

/// Build a fire-meter key for one layer's clip-trigger config, addressed by
/// the owning layer plus its index within `Layer::clip_triggers`. Together
/// with [`fire_meter_key_for_param`], this is the ONLY sanctioned way to
/// build a fire-meter key — the content-thread producer
/// (`manifold_playback::live_trigger::LiveTriggerState`) and the UI-thread
/// consumer (`AudioTriggerSectionPanel::update_fire_meters`) must call this
/// SAME function, never hand-assemble the byte-list themselves.
pub fn fire_meter_key_for_clip_trigger(layer_id: &str, row: u64) -> u64 {
    fire_meter_key(&[layer_id.as_bytes(), &row.to_le_bytes()])
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
