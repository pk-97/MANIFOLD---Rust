// node.compact_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// kept and scan. The pool's kept slots moved to its front in pool order,
// FLIP's removeParticles: with S = scan[count − 2] the kept total, slot
// idx < S takes the kept slot i, the first with scan[i] ≥ idx + 1 (binary
// search); slots from S on are empty (all zero, kind 3). The header, the
// last slot, passes whole.
//
// ABI: `pool` and `kept` share the WhitewaterParticle struct (Element),
// `scan` u32.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

const CW_EMPTY: u32 = 3u;

fn body(idx: u32, count: u32, e_pool: Element) -> Element {
    if idx + 1u >= count {
        return e_pool;
    }
    var empty: Element;
    empty.kind = CW_EMPTY;
    let slots = min(count - 1u, min(arrayLength(&buf_scan), arrayLength(&buf_kept)));
    if slots == 0u || idx >= buf_scan[slots - 1u] {
        return empty;
    }
    var lo = 0u;
    var hi = slots - 1u;
    while lo < hi {
        let mid = (lo + hi) / 2u;
        if buf_scan[mid] >= idx + 1u {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    return buf_kept[lo];
}
