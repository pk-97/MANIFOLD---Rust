# World heat: temperature, melting, boiling

<!-- index: Design for heat shared by every material in a Physics World: a temperature on each Matter point, moved through the Matter grid once a tick, lava that cools to crust and glows by temperature, snow that melts into water particles, water that boils into the gas as steam, and fire and Matter trading heat. -->

**Status:** design, 31 Oct. Peter's ask: snow melts to water, water boils to steam, lava cools to crust, fire heats what it touches, all inside one World.

## Today (read from code)

Paths are relative to `crates/`.

- **Matter points** are 160 bytes (`water-gpu-mpm/src/matter.rs` `MatterPoint`). `stress_diag.w` and `stress_off.w` are written 0 by the G2P (`grid_to_matter_body.wgsl:276`) and read by nothing. No temperature anywhere in Matter.
- **Lava** is a viscoplastic Matter with a fixed Firmness and Thickness (`core/src/scene_schema/sand.rs:161`) and a fixed glow (emission at 6, `sand.rs:452`).
- **Water particles** are 32 bytes (position and radius, velocity, id): no room. They are born only in inflow regions (`gpu_flip_step/emit.wgsl`: site flags, scan, write, birth tally) and removed below the kill height (`gpu_flip_step/move.wgsl`).
- **Gas** carries temperature on its fine grid; buoyancy follows it and burning makes it (`docs/SMOKE_AND_FIRE_MAP.md`).
- **The tick-end layer.** On a tick's last substep Points to Grid lays each point's bulk on the Matter grid, two words a node (`bulk`, `matter_to_grid.wgsl`), and the water and the gas read it through `sand_at` (`water-liquid/.../sand_read.wgsl`).
- **Order in a World tick:** Matter, then water, then gas, then the bodies settle (`docs/ONE_WORLD_DESIGN.md`).

## Decisions

**H1. Heat lives where the matter is.** A Matter point holds its heat content per kilogram in `stress_diag.w`, so the record does not grow. Below 0 °C the temperature is heat over capacity; snow between 0 and its latent heat sits at 0 °C, melting, its melted share the heat over the latent heat, so one number carries both. Each material starts at its own temperature (lava 1150 °C, snow −5 °C, everything else air, 20 °C). Water has no temperature: it is always at air temperature and boils only where it touches something above 100 °C. The gas keeps its own.

**H2. Heat moves through the Matter grid once a tick.** On the tick-end substep Points to Grid adds two words a node to the tick-end layer: capacity (Σ w·m·c) and heat (Σ w·m·c·T), so the layer goes from two words to four. The G2P on the same substep reads the grid's temperature at the point and moves the point toward it at the World's Heat Speed. A point also loses heat to the air by how exposed it is (one minus the bulk share at its place) and to the water by its share under water (`f_z.w`, kept by the water push), faster. Real conduction takes minutes to hours; Heat Speed is a stage dial whose default melts a snow point touching lava in about a second.

**H3. Temperature changes behaviour.**
- **Lava** stiffens as it cools: Firmness and Thickness rise from 1150 °C to 700 °C, and below 700 °C it is crust. Its glow follows its temperature (black-body colour and strength) instead of the fixed emission. The surface reads the temperature from the grid's heat words where it draws, as water reads sand there.
- **Snow** softens and darkens as it melts: its melted share drives the wet look and lowers its hardening. Fully melted snow becomes water (H4).

**H4. Snow turns into water.** A fully melted snow point dies on the tick-end substep. With a probability of its mass over a water particle's mass (hashed from its id, so the same run melts the same way) it writes a birth, its position and velocity, to a births list. The water step's first substep of the same tick appends the births through its emit path as one more source of sites, with ids and the birth tally as an inflow's. Mass holds on average.

**H5. Water turns into steam.** Matter above 100 °C under water gives up heat fast (a crust forms where lava meets water) and lays that heat as steam in a fifth word of the tick-end layer. The water removes its particles where the steam word says the Matter boiled them, as many as the heat allows. The gas reads the steam word at its cells as density and heat, so a World with a Smoke or Fire shows the steam; without one the water just boils away.

**H6. Gas and Matter trade heat.** The gas reads the Matter's temperature through `sand_at` at its cells, so lava heats the air above it (shimmer, rising smoke). The Matter reads the gas's temperature at its points as the last tick left it (a state capture, as the water push reads the water), so fire melts snow and dries mud.

**H7. Pay only for active work.** With every material at air temperature and no Fire in the World, the heat words and passes do not run and the layer stays two words a node.

## Order

Each phase commits on its own and ends with a still sheet.

1. **Heat in Matter** (H1, H2, lava in H3): start temperatures, conduction, cooling to air, lava crust and glow. Sheets: `st_lava`, lava onto sand.
2. **Melting** (snow in H3, H4): lava poured on snow; the snow slumps, darkens and runs off as water.
3. **Steam** (H5): lava into water; crust, boiling, steam with a Smoke.
4. **Fire and air** (H6).

## Costs

H2 adds two global atomic words over 27 nodes a point on one substep a tick (sand runs about 14), and a two-word gather in the G2P on that substep. Births and steam cost only where they happen. Measured on `st_lava` and `st_snow` per phase.

## Risks

- Births enter the water's particle buffer: they must keep its ids, birth tally and live cap. At the cap a birth is dropped and counted.
- Melting by probability holds mass only on average: a small pile melts noisily.
- Births land at the Matter's point spacing, not the water's half-cell lattice; the water's crowded-marker removal thins a cell that gets too many.
