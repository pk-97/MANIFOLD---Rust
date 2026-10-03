#!/usr/bin/env python3
"""Analytic proofs for :mod:`narrow_band_reference`.

The cases pin the strict boundaries and lifecycle invariants from Ferstl et
al. 2016 (doi:10.1111/cgf.12825), plus the Option B deviations documented by
the oracle: ``abs(phi) < 3h`` and entry-only reseeding with no surface-layer
seeding.  Expected counts and order are written independently of the oracle's
implementation so a mirrored implementation cannot make these tests pass.
"""

from __future__ import annotations

import math
from pathlib import Path
import sys
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parent))

from narrow_band_reference import (  # noqa: E402
    MAX_PARTICLE_SLOTS,
    Particle,
    band_mask,
    delete_and_compact,
    fill_sites,
    in_band,
    narrow_band_lifecycle,
    reseed_entering_cells,
    restore_interior,
    _check_capacity,
)
from narrow_band_grid_reference import Field  # noqa: E402


CELL = (0, 0, 0)
H_VALUES = (0.5, 1.0, 2.0)
CELLS_8 = tuple((x, y, z) for z in range(8) for y in range(8) for x in range(8))


def particle(index: int, position: tuple[float, float, float], velocity=(0.0, 0.0, 0.0)):
    return Particle(index, position, velocity)


def all_eligible(_point):
    return -1.0


def no_solid(_point):
    return 1.0


def still_in_cell(slots):
    return [p for p in slots if p is not None]


class NarrowBandReferenceTests(unittest.TestCase):
    def test_absolute_band_mask_is_strict_at_varied_h(self):
        for h in H_VALUES:
            just_inside = math.nextafter(3.0 * h, 0.0)
            just_outside = math.nextafter(3.0 * h, math.inf)
            values = (-just_outside, -3.0 * h, -just_inside, 0.0,
                      just_inside, 3.0 * h, just_outside)
            self.assertEqual(
                band_mask(values, h),
                (False, False, True, True, True, False, False),
            )
            self.assertTrue(in_band(-just_inside, h))
            self.assertFalse(in_band(-3.0 * h, h))

    def test_entry_boundaries_are_old_closed_new_floor_closed_surface_open(self):
        for h in H_VALUES:
            base = {CELL: -2.0 * h}
            kwargs = dict(
                particles=(), old_phi=base, new_phi=base,
                liquid_phi_at=lambda _p, h=h: -h, solid_phi_at=no_solid,
                velocity_at=lambda _p: (1.0, 2.0, 3.0), cells=(CELL,),
                h=h, capacity=16, next_id=10,
            )
            for old, new, expected in (
                (-3.0 * h, -h, 8),
                (math.nextafter(-3.0 * h, math.inf), -h, 0),
                (-3.0 * h, -3.0 * h, 0),
                (-3.0 * h, math.nextafter(-3.0 * h, -math.inf), 0),
                (-3.0 * h, math.nextafter(-h, math.inf), 0),
            ):
                result, count = reseed_entering_cells(
                    old_phi={CELL: old}, new_phi={CELL: new}, **{
                        key: value for key, value in kwargs.items()
                        if key not in ("old_phi", "new_phi")
                    }
                )
                self.assertEqual(count, expected)
                self.assertEqual(len(still_in_cell(result)), expected)

    def test_each_deficit_fills_only_to_eight_and_crowded_cells_stay_intact(self):
        for existing in range(10):
            # These particles share a half cell. Occupancy excludes one fill
            # site, while the particle count sets the remaining deficit.
            old_particles = tuple(
                particle(900 + i, (0.05 + 0.01 * i, 0.05, 0.05))
                for i in range(existing)
            )
            slots, count = reseed_entering_cells(
                old_particles,
                old_phi={CELL: -3.0}, new_phi={CELL: -1.0},
                liquid_phi_at=all_eligible, solid_phi_at=no_solid,
                velocity_at=lambda _p: (0.0, 0.0, 0.0), cells=(CELL,),
                h=1.0, capacity=32, next_id=1,
            )
            expected = max(0, 8 - existing)
            self.assertEqual(count, expected)
            self.assertEqual(len(still_in_cell(slots)), existing + expected)
            self.assertEqual(len({p.position for p in still_in_cell(slots)}), existing + expected)
            if existing >= 8:
                self.assertEqual([p.id for p in still_in_cell(slots)[:existing]],
                                 [900 + i for i in range(existing)])

    def test_candidate_sites_require_liquid_at_or_below_negative_h(self):
        sites = fill_sites(CELL, 1.0)

        def liquid(point):
            if point == sites[0]:
                return -1.0
            if point == sites[1]:
                return math.nextafter(-1.0, math.inf)
            return -2.0

        slots, count = reseed_entering_cells(
            (), old_phi={CELL: -3.0}, new_phi={CELL: -1.0},
            liquid_phi_at=liquid, solid_phi_at=no_solid,
            velocity_at=lambda _p: (0.0, 0.0, 0.0), cells=(CELL,),
            h=1.0, capacity=8, next_id=1,
        )
        self.assertEqual(count, 7)
        positions = {p.position for p in still_in_cell(slots)}
        self.assertIn(sites[0], positions)
        self.assertNotIn(sites[1], positions)

    def test_occupied_and_solid_sites_are_skipped_with_strict_signs(self):
        sites = fill_sites(CELL, 1.0)
        occupied, liquid_boundary, solid_boundary = sites[:3]

        def liquid(point):
            return 0.0 if point == liquid_boundary else -1.0

        def solid(point):
            return 0.0 if point == solid_boundary else 1.0

        slots, count = reseed_entering_cells(
            # The existing particle is jittered within the half-cell owned by
            # the first fill site; exact-coordinate occupancy would miss it.
            (particle(7, (0.24, occupied[1], occupied[2])),),
            old_phi={CELL: -3.0}, new_phi={CELL: -1.0},
            liquid_phi_at=liquid, solid_phi_at=solid,
            velocity_at=lambda _p: (0.0, 0.0, 0.0), cells=(CELL,),
            h=1.0, capacity=16, next_id=20,
        )
        # One occupied and two strict sign failures leave five eligible sites;
        # the deficit is seven but the eligible set is the tighter bound.
        self.assertEqual(count, 5)
        positions = {p.position for p in still_in_cell(slots)}
        self.assertNotIn(liquid_boundary, positions)
        self.assertNotIn(solid_boundary, positions)
        self.assertNotIn(occupied, positions)

    def test_multiple_entering_cells_each_get_their_own_deficit(self):
        cells = ((0, 0, 0), (1, 0, 0))
        slots, count = reseed_entering_cells(
            (), old_phi={cell: -3.0 for cell in cells},
            new_phi={cell: -1.0 for cell in cells},
            liquid_phi_at=all_eligible, solid_phi_at=no_solid,
            velocity_at=lambda _p: (0.0, 0.0, 0.0), cells=cells,
            h=1.0, capacity=16, next_id=1,
        )
        self.assertEqual(count, 16)
        self.assertEqual(len({p.position for p in still_in_cell(slots)}), 16)
        self.assertEqual(
            {math.floor(p.position[0]) for p in still_in_cell(slots)}, {0, 1}
        )

    def test_new_velocity_is_sampled_at_each_selected_site(self):
        def velocity(point):
            return (point[0] + 10.0, point[1] + 20.0, point[2] + 30.0)

        slots, count = reseed_entering_cells(
            (), old_phi={CELL: -3.0}, new_phi={CELL: -1.0},
            liquid_phi_at=all_eligible, solid_phi_at=no_solid,
            velocity_at=velocity, cells=(CELL,), h=1.0, capacity=8, next_id=1,
        )
        live = still_in_cell(slots)
        self.assertEqual(count, 8)
        self.assertEqual([p.velocity for p in live], [velocity(p.position) for p in live])

    def test_reentry_is_idempotent(self):
        kwargs = dict(
            old_phi={CELL: -3.0}, new_phi={CELL: -1.0},
            liquid_phi_at=all_eligible, solid_phi_at=no_solid,
            velocity_at=lambda p: p, cells=(CELL,), h=1.0,
            capacity=8, next_id=50,
        )
        once, first_count = reseed_entering_cells((), **kwargs)
        twice, second_count = reseed_entering_cells(once, **kwargs)
        self.assertEqual(first_count, 8)
        self.assertEqual(second_count, 0)
        self.assertEqual(once, twice)

    def test_compaction_uses_cell_then_input_order_not_particle_id(self):
        particles = (
            particle(5, (1.1, 0.1, 0.1)),
            particle(100, (0.1, 0.1, 0.1)),
            particle(1, (0.2, 0.2, 0.2)),
        )
        slots, deleted = delete_and_compact(
            particles, liquid_phi_at=lambda _p: -2.0,
            solid_phi_at=no_solid, h=1.0, capacity=8,
        )
        self.assertEqual(deleted, 0)
        self.assertEqual([p.id for p in still_in_cell(slots)], [100, 1, 5])
        self.assertEqual(slots[3:], (None,) * 5)

    def test_compaction_and_sites_are_x_fastest_on_all_three_axes(self):
        particles = (
            particle(1, (0.1, 0.1, 1.1)),
            particle(2, (0.1, 1.1, 0.1)),
            particle(3, (1.1, 0.1, 0.1)),
            particle(4, (0.1, 0.1, 0.1)),
        )
        slots, _ = delete_and_compact(
            particles, liquid_phi_at=lambda _p: -1.0,
            solid_phi_at=no_solid, h=1.0, capacity=4,
        )
        self.assertEqual([p.id for p in slots], [4, 3, 2, 1])
        self.assertEqual(fill_sites(CELL, 1.0), (
            (0.25, 0.25, 0.25), (0.75, 0.25, 0.25),
            (0.25, 0.75, 0.25), (0.75, 0.75, 0.25),
            (0.25, 0.25, 0.75), (0.75, 0.25, 0.75),
            (0.25, 0.75, 0.75), (0.75, 0.75, 0.75),
        ))

    def test_capacity_boundary_includes_the_exact_count_wire_limit(self):
        self.assertEqual(_check_capacity((1 << 24) - 1), (1 << 24) - 1)
        self.assertEqual(_check_capacity(1 << 24), 1 << 24)
        with self.assertRaises(ValueError):
            _check_capacity((1 << 24) + 1)

    def test_all_and_none_deletion_keep_solid_near_particles(self):
        particles = tuple(particle(i, (float(i), 0.1, 0.1)) for i in range(4))

        def liquid(point):
            return -4.0 if point[0] in (0.0, 1.0) else -3.0

        def solid(point):
            return 4.0 if point[0] in (0.0, 2.0) else 2.0

        slots, deleted = delete_and_compact(
            particles, liquid_phi_at=liquid, solid_phi_at=solid,
            h=1.0, capacity=8,
        )
        # id 0 is deep and far from solid; id 1 is deep but near solid; ids
        # 2/3 are on/above the deep boundary, so only one is deleted.
        self.assertEqual(deleted, 1)
        self.assertEqual([p.id for p in still_in_cell(slots)], [1, 2, 3])

        none_slots, none_deleted = delete_and_compact(
            particles, liquid_phi_at=lambda _p: -3.0,
            solid_phi_at=lambda _p: 100.0, h=1.0, capacity=8,
        )
        self.assertEqual(none_deleted, 0)
        self.assertEqual([p.id for p in still_in_cell(none_slots)], [0, 1, 2, 3])

        all_slots, all_deleted = delete_and_compact(
            particles, liquid_phi_at=lambda _p: -4.0,
            solid_phi_at=lambda _p: 4.0, h=1.0, capacity=8,
        )
        self.assertEqual(all_deleted, 4)
        self.assertEqual(all_slots, (None,) * 8)

    def test_lifecycle_returns_mask_and_clears_tail(self):
        result = narrow_band_lifecycle(
            (particle(9, (0.1, 0.1, 0.1)),),
            old_phi={CELL: -3.0}, new_phi={CELL: -1.0},
            liquid_phi_at=all_eligible, solid_phi_at=no_solid,
            velocity_at=lambda _p: (1.0, 1.0, 1.0), cells=(CELL,),
            h=1.0, capacity=16,
        )
        self.assertEqual(result.band, {CELL: True})
        self.assertEqual(result.deleted, 0)
        self.assertEqual(result.reseeded, 7)
        self.assertEqual(len(result.slots), 16)
        self.assertEqual(result.slots[8:], (None,) * 8)

    def test_capacity_and_finite_input_refuse_without_truncation(self):
        with self.assertRaises(ValueError):
            delete_and_compact((), liquid_phi_at=all_eligible,
                               solid_phi_at=no_solid, h=1.0,
                               capacity=MAX_PARTICLE_SLOTS + 1)
        with self.assertRaises(ValueError):
            delete_and_compact((particle(1, (0.0, 0.0, 0.0)),),
                               liquid_phi_at=all_eligible, solid_phi_at=no_solid,
                               h=1.0, capacity=0)
        with self.assertRaises(ValueError):
            in_band(0.0, 0.0)
        with self.assertRaises(ValueError):
            in_band(math.nan, 1.0)
        with self.assertRaises(ValueError):
            delete_and_compact((particle(1, (0.0, 0.0, 0.0)),),
                               liquid_phi_at=lambda _p: math.inf,
                               solid_phi_at=no_solid, h=1.0, capacity=2)

    def test_capacity_full_refuses_reseed_without_truncating(self):
        with self.assertRaises(ValueError):
            reseed_entering_cells(
                (particle(1, (0.1, 0.1, 0.1)), particle(2, (0.2, 0.2, 0.2))),
                old_phi={CELL: -3.0}, new_phi={CELL: -1.0},
                liquid_phi_at=all_eligible, solid_phi_at=no_solid,
                velocity_at=lambda _p: (0.0, 0.0, 0.0), cells=(CELL,),
                h=1.0, capacity=2,
            )

    def test_restore_interior_fills_an_8_cubed_grid_and_keeps_existing_particles(self):
        existing = particle(77, (0.1, 0.1, 0.1), velocity=(9.0, 8.0, 7.0))
        slots, count = restore_interior(
            (existing,), new_phi={cell: -1.0 for cell in CELLS_8},
            liquid_phi_at=all_eligible, solid_phi_at=no_solid,
            velocity_at=lambda point: point, cells=CELLS_8,
            h=1.0, capacity=8 * len(CELLS_8), next_id=100,
        )
        live = still_in_cell(slots)
        self.assertEqual(count, 8 * len(CELLS_8) - 1)
        self.assertEqual(len(live), 8 * len(CELLS_8))
        self.assertIn(existing, live)
        self.assertEqual(
            [p.velocity for p in live if p.id != existing.id],
            [p.position for p in live if p.id != existing.id],
        )
        counts = {cell: 0 for cell in CELLS_8}
        for p in live:
            counts[tuple(math.floor(axis) for axis in p.position)] += 1
        self.assertTrue(all(value == 8 for value in counts.values()))

    def test_restore_interior_leaves_surface_cells_untouched(self):
        interior = (0, 0, 0)
        surface = (1, 0, 0)
        phi = {cell: 0.0 for cell in CELLS_8}
        phi[interior] = -1.0
        phi[surface] = -0.5
        slots, count = restore_interior(
            (), new_phi=phi, liquid_phi_at=all_eligible,
            solid_phi_at=no_solid, velocity_at=lambda _point: (0.0, 0.0, 0.0),
            cells=CELLS_8, h=1.0, capacity=8 * len(CELLS_8), next_id=1,
        )
        live = still_in_cell(slots)
        self.assertEqual(count, 8)
        self.assertTrue(all(tuple(math.floor(axis) for axis in p.position) == interior for p in live))
        self.assertFalse(any(tuple(math.floor(axis) for axis in p.position) == surface for p in live))

    def test_restore_interior_preserves_occupied_half_cells(self):
        cell = (3, 3, 3)
        occupied_site = fill_sites(cell, 1.0)[0]
        occupied = particle(91, (3.24, occupied_site[1], occupied_site[2]))
        slots, count = restore_interior(
            (occupied,), new_phi={cell: -1.0 for cell in CELLS_8},
            liquid_phi_at=all_eligible, solid_phi_at=no_solid,
            velocity_at=lambda _point: (0.0, 0.0, 0.0), cells=CELLS_8,
            h=1.0, capacity=8 * len(CELLS_8), next_id=100,
        )
        live = still_in_cell(slots)
        in_cell = [p for p in live if tuple(math.floor(axis) for axis in p.position) == cell]
        self.assertEqual(count, 8 * len(CELLS_8) - 1)
        self.assertEqual(len(in_cell), 8)
        self.assertIn(occupied, in_cell)
        self.assertNotIn(occupied_site, [p.position for p in in_cell])

    def test_restore_interior_refuses_the_whole_append_on_capacity_shortage(self):
        particles = ()
        with self.assertRaises(ValueError):
            restore_interior(
                particles, new_phi={cell: -1.0 for cell in CELLS_8},
                liquid_phi_at=all_eligible, solid_phi_at=no_solid,
                velocity_at=lambda _point: (0.0, 0.0, 0.0), cells=CELLS_8,
                h=1.0, capacity=8 * len(CELLS_8) - 1, next_id=1,
            )
        self.assertEqual(particles, ())

    def test_restore_overflow_gpu_fixture_has_exactly_fourteen_missing_sites(self):
        eligible = ((2, 3, 3), (3, 3, 3))
        existing = (
            particle(100, (2.1, 3.1, 3.1), (9.0, 9.0, 9.0)),
            particle(101, (3.1, 3.1, 3.1), (9.0, 9.0, 9.0)),
        )
        # Match the GPU fixture and sample the actual cell-centred field at
        # quarter sites; a constant all-eligible sampler would miss this bug.
        phi = {cell: -4.0 if cell in eligible else 1.0 for cell in CELLS_8}
        field = Field((8, 8, 8), (0.5, 0.5, 0.5), 1.0, tuple(phi.values()))
        kwargs = dict(
            new_phi=phi, liquid_phi_at=field.sample, solid_phi_at=no_solid,
            velocity_at=lambda _point: (1.0, 2.0, 3.0), cells=CELLS_8,
            h=1.0, next_id=102,
        )
        slots, count = restore_interior(existing, capacity=16, **kwargs)
        self.assertEqual(count, 14)
        added = [p for p in still_in_cell(slots) if p.id >= 102]
        self.assertEqual(
            {p.position for p in added},
            {site for cell in eligible for site in fill_sites(cell, 1.0)[1:]},
        )
        self.assertTrue(all(p.velocity == (1.0, 2.0, 3.0) for p in added))
        self.assertTrue(all(p in slots for p in existing))
        with self.assertRaisesRegex(ValueError, "explicit capacity"):
            restore_interior(existing, capacity=8, **kwargs)

        # The original constant field made all 512 cells eligible, explaining
        # the GPU's 4094 additions instead of the intended two-cell deficit.
        full_phi = {cell: -2.0 for cell in CELLS_8}
        _, full_count = restore_interior(
            existing, capacity=8 * len(CELLS_8),
            **{**kwargs, "new_phi": full_phi, "liquid_phi_at": lambda _p: -2.0},
        )
        self.assertEqual(full_count, 4094)


if __name__ == "__main__":
    unittest.main()
