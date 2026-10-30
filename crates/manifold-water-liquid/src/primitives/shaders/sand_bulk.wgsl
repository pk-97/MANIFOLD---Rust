// A Sand's bulk on its own grid (node.matter_to_grid's `bulk`): two words a
// pooled node, laid out as the grid's nodes are, added on the tick's last
// substep with each point's quadratic weights: its rest volume, then the
// part of it a porous material's grains fill (0 for a continuous one, whose
// solid share is 1), both in Matter cells at SAND_FIXED.
const SAND_BULK_WORDS: u32 = 2u;
const SAND_FIXED: f32 = 65536.0;
