// node.append_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// compacted, spawns and live_scan. The bridge's whitewater load: the frame's
// live spawns, in spawn order, into the empty slots after a compacted pool's
// particles, each taking the next id. With S the first empty slot (binary
// search on kind 3; the pool is compacted, its header last), L the live
// total (live_scan's last value) and placed = min(L, capacity − S), slot
// S + k for k < placed takes the spawn with the (k + 1)th live flag and id
// (header id + k) mod 256. The header advances its id by placed and records
// max(0, S + L − capacity), the spawns that found no room, in pad0. Every
// other slot passes whole. Capacity is the slot count less the header.
//
// ABI: `pool` and `compacted` share the WhitewaterParticle struct (Element),
// `spawns` is WhitewaterSpawn (Element2), `live_scan` u32.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

const AP_EMPTY: u32 = 3u;
// FLIP's _diffuseParticleIDLimit.
const AP_ID_LIMIT: u32 = 256u;

fn body(idx: u32, count: u32, e_pool: Element) -> Element {
    if count == 0u || count > arrayLength(&buf_compacted) {
        return e_pool;
    }
    let capacity = count - 1u;
    var lo = 0u;
    var hi = capacity;
    while lo < hi {
        let mid = (lo + hi) / 2u;
        if buf_compacted[mid].kind == AP_EMPTY {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    let start = lo;
    let spawns = min(arrayLength(&buf_spawns), arrayLength(&buf_live_scan));
    var live = 0u;
    if spawns > 0u {
        live = buf_live_scan[spawns - 1u];
    }
    let placed = min(live, capacity - start);
    let header = buf_compacted[capacity];
    var out = e_pool;
    if idx == capacity {
        out.id = (header.id + placed) % AP_ID_LIMIT;
        out.pad0 = start + live - min(start + live, capacity);
        return out;
    }
    if idx < start || idx >= start + placed {
        return out;
    }
    let k = idx - start;
    var a = 0u;
    var b = spawns - 1u;
    while a < b {
        let mid = (a + b) / 2u;
        if buf_live_scan[mid] >= k + 1u {
            b = mid;
        } else {
            a = mid + 1u;
        }
    }
    let spawn = buf_spawns[a];
    var placed_particle: Element;
    placed_particle.position_lifetime = spawn.position_lifetime;
    placed_particle.velocity = spawn.velocity;
    placed_particle.kind = spawn.kind;
    placed_particle.id = (header.id + k) % AP_ID_LIMIT;
    return placed_particle;
}
