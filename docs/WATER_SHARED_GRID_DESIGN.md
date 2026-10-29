# Water shared grid: one table, one budget, memory that follows the water

<!-- index: Design for cutting GPU FLIP water memory: one world-tile table per World shared by every Water's sim, surface and whitewater, so Waters push on each other and merge; one memory budget from the device; scratch fields sharing one arena by lifetime; no padding or dead lanes; spray off the surface table; pools that shrink; 16-bit storage. Starts from the 28 Oct inventory and the waterBoxTests 22 GB refusal. -->

**Status:** phase 1 partly landed (0aaaee527: spray and foam hold no display tiles, the display pool shrinks). D1 and D2 are per World since 29 Oct (Peter). Phase 2 partly landed: water, sand, whitewater and their display fields draw on one World budget, two thirds of the recommended working set (`admit_world_bytes`, 0dc6b91cd, 22b597bac); a refused water growth keeps its last pool, stops births and says so on the card. The rest of D7's order (spray, off screen, farthest) is not built. One lattice per World (2308d0a60): every solver's cell is the World's Cell Size times its item's Quality scale, so their tile keys line up, but each solver still holds its own table (D2 not built). Each phase needs Peter's go-ahead.
**Companions:** [GPU_FLIP_END_GAME_DESIGN.md](GPU_FLIP_END_GAME_DESIGN.md) (D4 memory follows the water, D5 16-bit storage, D12 one shared grid, D13 spray as impostors), [WATER_FINE_BLOCKS_DESIGN.md](WATER_FINE_BLOCKS_DESIGN.md) (its second display table becomes a fine level here, D2).

## Why

On 28 Oct, waterBoxTests (an Inflow pouring nonstop onto a 10 m ground, two moving bodies, spray on, 6.25 cm cells, Bounds off) went from 2.7 GB at load to 22.4 GB in about five minutes. Then it refused memory: `current 22451552256 + candidate 449314820 exceeds allowed 22613004288`, the allowed figure being 75% of the 30 GB the M4 Max reports (`~/Library/Logs/com.latentspace.manifold/session-1793174207830-58982.log`). Every refused request decodes to a field of the display table at 215k to 299k tiles (`pool_len = 512·P + 1`, 4 B and 12 B elements). The sim's own pool was capped far below that.

On a 5080 the same scene would refuse at about half the size. The card is not the cause; the code is.

What the inventory found (all read from code on 28 Oct, nothing run):

1. **Separate tables for one water.** The sim keeps its world-tile table (`gpu_flip_step/tiles.rs`). The surface builds a second one each frame from the particles (`surface_tiles.rs`). Matter keeps a third (`matter_state.rs`). WATER_FINE_BLOCKS proposes a fourth. Each table has its own pool, growth rule and budget.
2. **Spray and foam claim surface tiles.** Every live liquid, foam or spray record holds its own tile plus a one-tile ring (`surface_tiles.rs` `REACH`), so 27 tiles, or 13,824 display cells. At 61 to 85 B per display cell that is 0.85 to 1.2 MB for one isolated droplet. Spray is capped at 100,000 droplets (250,000 at most, `whitewater.rs:104-105`), and the cap cannot safely rise while a droplet costs a megabyte.
3. **Budgets disagree.** The sim pool has a fixed 1.5 GiB (`tiles.rs:40`), the same on every card, and past it water is deleted silently (only the stats count it). The display pool has no budget except the whole-device check (`buffer_budget.rs:400`, 75% of the recommended working set).
4. **Memory only grows.** Pools grow to twice the held tiles once two thirds are held (`tile_pool.rs:83-107`) and never shrink. Engine array growth only grows (`array_growth.rs:62`). While a pool grows, the old and new buffers both stay alive for a frame or two.
5. **Padding and dead lanes.** Face grids store `FaceSample { vec4 velocity, vec4 weight }` (32 B) where 24 B are used. The open-fraction grid `s` never writes its velocity half (`solids.wgsl:189-195`). Coarse pressure faces use 12 of 32 B. Narrow Band's 48 B/cell is counted in the budget even with Narrow Band off. Whitewater's `empty_source` (16 B/cell) is allocated even when the obstacle source is wired.
6. **No sharing by lifetime.** Every stage owns its fields for the whole frame. The pressure solver's 132 B are dead before sheeting's 128 B are first written, yet both are allocated. Growing display arrays are excluded from temporary reuse (`resource_allocation.rs:375-380`).
7. **Particles are sized by capacity and copied many times.** An Inflow gets 1M slots from the first frame (`gpu_flip_domain.rs:307`; its comment puts a slot at about 1 kB across the particle buffers).

## Cost today

Shipped water: sheeting on, Narrow Band off, 6 substep history rows, whitewater on.

Per sim pool cell:

| Part | B | Where |
|---|---|---|
| Step lattice: 9 cell arrays, 7 face grids | 260 | `gpu_flip_step/lattice.rs:37-95` |
| Pressure: 5 fine vectors, rows, coarse levels | 132 | `gpu_flip_pressure.rs:171-185` |
| Sheeting and sheet distance | 128 | `gpu_flip_sheeting.rs:234-241`, `gpu_flip_sheet_distance.rs:33` |
| Substep history (6 face rows and a stash) | 112 | `substep_history.rs:151` |
| Pocket extras, sort ranges and scan | ~28 | `lattice.rs:29`, `pockets.rs:13` |
| Whitewater fields on the step pool | 72 | `whitewater_step.rs:568-579` |
| **Total** | **~730** | of which ~130 B are padding or dead lanes |

Per display cell: about 61 B (solid 4, sort ranges and scan 12, volume 4, weights 4, velocity 12, three smoothing blends 12, carry 4, mist 8, table ~1). Add about 24 B if the four array switches each hold an unused pool-sized output: `grow_step_arrays` sizes them before the switch aliases its input. That is likely but not yet confirmed. Pool headroom makes the allocation at least 1.5 times the held cells, and more once the water shrinks, since the pool never does.

Per liquid particle slot: the sim loop holds 164 B (fill seed, state, step output and sorted copies at 32 B each, plus scratch). Frame history holds 32 B per kept frame, at least three (A, B, pending; up to 16, `frame_history.rs:16`). The publication sort holds 16 B. The display chain holds 156 B (table points, blend, push-out and sorted copies at 32 B each, plus blobs, ranks and counts). The total is about 430 B, and the code's own estimate is about 1 kB.

Per full water cell, at 8 particles: about 730 + 8 × 430, or **4 KB or more, and particles are four fifths of it.** Spray scenes are worse still: each scattered droplet holds a megabyte of display cells.

## Decisions

**D1. One budget per World, from the device.** All the World's water pools and arrays draw on one budget, which is a share of what the device check admits (`admit_candidate_bytes`: 75% of the recommended working set on Metal; the heap budget on Vulkan). `POOL_BUDGET_BYTES` is deleted. Rejected: per-pool constants, because they cannot follow the card and they disagree.

**D2. One table per World, with levels.** Every Water in a World is one liquid on one world-tile table with one pressure solve, so two Waters push on each other and merge where they meet. Each Water item contributes its particles with a water index; its look, whitewater and wet marks read that index, so a merged pool keeps each Water's own look up to where they mix. The sim's table is the only table. The surface's fields live on its held tiles at Surface Detail 1, so the display stops building its own. Finer Surface Detail is a fine level of the same table holding only tiles at the surface; this replaces WATER_FINE_BLOCKS' second table. Mist and the multigrid's coarse grids are coarse levels. Matter joins later (D12 in the end-game doc). A Speed per region of the table is where mixed sim speeds would come in (docs/IDEAS.md); today every Water follows the World Speed. Open: the World's water has one cell size, so per-Water Cell Size either moves to the World or the finest Water's sets it. Rejected: one table per water with a shared budget, because Waters then never touch and two tables still hold two sets of rings and headroom.

**D3. Fields by lifetime.** Every field is one of three kinds:
- **Persistent:** kept across steps. These are the output faces, pressure as the warm start, phi, Narrow Band's grid, and anything the renderer reads after the graph (level set, velocity, mist).
- **Step scratch:** written and dead within one substep.
- **Frame scratch:** display intermediates (sorted particles, weights, the smoothing blends, the volume, the solid distance).

Step and frame scratch share one arena per pool. A lifetime table in code, not a hand count, sizes the arena for its peak live set and gives each field its range. This changes no result. Rejected: aliasing pairs by hand, because the next stage added breaks it silently.

**D4. No padding, no dead lanes, nothing for features that are off.** Face grids become separate arrays per component (velocity xyz, weight xyz, plus w only where it is used). Fields are allocated only when their feature is on: Narrow Band, the body vectors, whitewater's fallback source. This changes no result.

**D5. Spray, foam and bubbles hold no surface tiles.** Spray and bubbles draw as sphere impostors (end-game D13). Foam already draws as particles. Mist moves to a coarse level, or is dropped if Peter prefers impostors alone; it reads as glow today. This changes the look on purpose.

**D6. Memory comes back.** A pool shrinks once its held tiles stay under a third of it for two seconds. Arrays rebuilt every frame are freed before their larger replacement is allocated, so they never hold two copies. The engine's array growth gains the matching shrink.

**D7. Full means the least visible water goes first.** At the budget, requests are refused in this order: new births (Inflow and sheeting stop), spray and mist tiles, liquid tiles off screen, then liquid farthest from the camera. The card says which. Nothing is deleted silently.

**D8. 16-bit is storage, 32-bit is maths** (end-game D5, unchanged). Particles become 16 B: a position relative to their tile, a half-float velocity, the id. Stored faces and history rows become half floats. Pressure, its sums, the FLIP velocity change and all accumulation stay 32-bit. Values are packed into 32-bit words (`pack2x16float`, already used in 13 shader files), so neither backend needs an f16 feature. This is not bit-identical; the settled-pool long-run gate from end-game D5 decides it.

**D9. A buffer binding knows its own length.** `collect_buffer_sizes` passes `buffer.size as u32` (`manifold-gpu/src/metal/encoder.rs:4210`). It ignores the binding's offset and wraps past 4 GiB with no error. Arena ranges need views that carry their own length, and a view over 4 GiB is refused by name. This is a GPU-layer change, so it is coordinated with the Vulkan port.

## Target per cell, on paper

| | Today | Exact layout (D2-D4) | With 16-bit (D8) |
|---|---|---|---|
| Sim grid | ~730 | ~310 (32 persistent, ~190 arena peak at the pressure solve, 84 history) | ~200 |
| Surface on the sim's tiles | 61-85 × headroom, own table | ~20 persistent, the rest in the arena | ~16 |
| Per particle slot | ~430 | ~300 (no kept seed, state ping-pongs, display reads in place, blend and push-out fused) | ~160 |
| Full water cell (8 particles) | ~4 KB | ~2.7 KB | ~1.5 KB |
| One scattered spray droplet | ~1 MB of display cells | ~0.9 kB of particle buffers | |

So layout alone cuts deep water by about 1.5×, and 16-bit storage by about 2.8×. Spray scenes drop by orders of magnitude. The big lever for deep water is outside the layout: Narrow Band (built, off by default, `gpu_flip.rs:176`) keeps particles only within 12 cells of the surface, which takes deep water down to the grid's 200 to 300 B. It changes the sim, so it gets its own look gate.

## Phases

Each phase is one landing, measured on the same scenes before and after: peak `currentAllocatedSize`, frame clock p50 at 1080p, and bit-identical frames for the exact phases. Scenes: waterBoxTests (Peter's, `~/Downloads/waterBoxTests.manifold`, five minutes of Inflow), Dam Break, Sea Wall and open ground (`scripts/water_scenes.py`).

1. **Stop the blowup.** Spray, foam and bubbles stop claiming display tiles (D5; the mist choice is Peter's). The display pool joins the device budget and shrinks (D1 and D6 for the display only). Rebuilt arrays free before they grow. Confirm the array-switch shadows and stop them. Confirm that looks switched off skip their work. Done when waterBoxTests holds flat memory for five minutes.
2. **One budget.** D1 and D7 across the sim, whitewater and display. `POOL_BUDGET_BYTES` deleted. The node card reports refusals.
3. **Exact layout.** D9 first, then D4 and D3 for the sim pool (step, pressure, sheeting, whitewater fields), and the particle copies. Bit-identical frames on Dam Break and Sea Wall. A test-only poison of each arena range between its uses proves no field is read after its range is reused, following the sparse blocks D-6 oracle.
4. **One table.** D2: the surface on the sim's tiles at Surface Detail 1, a fine level for higher detail, mist and multigrid as coarse levels.
5. **16-bit storage.** D8, behind its long-run gate.
6. **Wider tile keys.** Keys wrap at 2048 × 512 × 2048 tiles (`world_tiles.rs:95`), so a scene can be at most 4,032 cells tall: 16 m at 4 mm cells, 8 m at 2 mm. Small cells in tall scenes need wider keys.

Matter on the shared table, and Narrow Band on by default, are their own decisions after these.

## Risks

- **Reading a reused range.** An arena bug reads another field's values without any error. The poison test and the bit-identical gate catch it.
- **Overlapping queues.** If the sim runs on the async lane while the previous frame renders, anything the renderer reads cannot sit in the arena. D3 already keeps those persistent; any new renderer read must be checked against it.
- **Shrink hitches.** A shrink reallocates. The two-second hold stops it flapping, and it never runs in the middle of a frame.
- **Breadth.** D2 touches the surface, whitewater, the wet map and interior fill. That is why it comes after the budget and layout phases, which leave the tables alone.
- **Vulkan in flight.** D9 and any arena views change `manifold-gpu`, which the Vulkan port is moving. They land after it, or with its owner.
