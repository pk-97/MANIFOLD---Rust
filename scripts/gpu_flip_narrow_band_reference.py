#!/usr/bin/env python3
"""Dependency-free CPU proofs for the GPU FLIP narrow-band lifecycle.

This file is an executable f64 oracle, not a second runtime implementation.
The particle rules follow Ferstl et al. (2016), equations 3 and 4: the
particle velocity is selected at ``phi_face >= -2h``; particle support is the
strict band ``abs(phi) < 3h``; and a cell entering ``-3h < phi <= -h`` is
resampled at the eight quarter/three-quarter sites.  Sites are at least one
cell below the surface (``liquid_phi <= -h``) and outside a solid
(``solid_phi > 0``).

The small scalar lattice below supplies a semi-Lagrangian translation hand
case.  The lifecycle code uses no third-party modules and has deterministic
stable cell compaction, an explicitly cleared tail, and a strict 2**24 slot
limit.  Run this file directly to execute its self-tests.
"""

from __future__ import annotations

from dataclasses import dataclass
import math
from itertools import product
from typing import Callable, Iterable, Mapping, Optional, Sequence


Point = tuple[float, float, float]
Cell = tuple[int, int, int]
Velocity = tuple[float, float, float]
ScalarSampler = Callable[[Point], float]
VelocitySampler = Callable[[Point], Velocity]

REST_PARTICLES = 8
MAX_SLOTS_EXCLUSIVE = 1 << 24
SITE_FRACTIONS = (0.25, 0.75)


def _finite(value: float, label: str) -> float:
    value = float(value)
    if not math.isfinite(value):
        raise ValueError(f"{label} must be finite")
    return value


def _point(value: Iterable[float], label: str) -> Point:
    result = tuple(_finite(x, f"{label}[{i}]") for i, x in enumerate(value))
    if len(result) != 3:
        raise ValueError(f"{label} needs three coordinates")
    return result  # type: ignore[return-value]


def _velocity(value: Iterable[float], label: str) -> Velocity:
    return _point(value, label)


def _h(value: float) -> float:
    value = _finite(value, "h")
    if value <= 0.0:
        raise ValueError("h must be positive")
    return value


def _capacity(value: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError("capacity must be a non-negative integer")
    if value >= MAX_SLOTS_EXCLUSIVE:
        raise ValueError("capacity reaches the strict 2^24 slot limit")
    return value


@dataclass(frozen=True)
class Particle:
    id: int
    position: Point
    velocity: Velocity


@dataclass(frozen=True)
class ScalarField:
    """Uniform scalar lattice with clamped trilinear samples."""

    shape: tuple[int, int, int]
    origin: Point
    h: float
    values: tuple[float, ...]

    def __post_init__(self) -> None:
        if len(self.shape) != 3 or any(type(n) is not int or n < 1 for n in self.shape):
            raise ValueError("shape needs three positive integer dimensions")
        if len(self.values) != self.shape[0] * self.shape[1] * self.shape[2]:
            raise ValueError("values do not cover shape")
        _point(self.origin, "origin")
        _h(self.h)
        if not all(math.isfinite(float(v)) for v in self.values):
            raise ValueError("field values must be finite")

    def position(self, index: int) -> Point:
        nx, ny, _ = self.shape
        x = index % nx
        y = (index // nx) % ny
        z = index // (nx * ny)
        return tuple(base + coordinate * self.h
                     for base, coordinate in zip(self.origin, (x, y, z)))  # type: ignore[return-value]

    def sample(self, point: Point) -> float:
        point = _point(point, "sample point")
        q = tuple(min(max((x - base) / self.h, 0.0), size - 1)
                  for x, base, size in zip(point, self.origin, self.shape))
        lo = tuple(math.floor(x) for x in q)
        hi = tuple(min(x + 1, size - 1) for x, size in zip(lo, self.shape))
        t = tuple(x - lower for x, lower in zip(q, lo))
        nx, ny, _ = self.shape
        result = 0.0
        for bits in product((0, 1), repeat=3):
            coordinate = tuple(hi[a] if bits[a] else lo[a] for a in range(3))
            weight = math.prod(t[a] if bits[a] else 1.0 - t[a] for a in range(3))
            result += weight * self.values[coordinate[0] + nx * (coordinate[1] + ny * coordinate[2])]
        return result


def semi_lagrangian_translate(field: ScalarField, velocity: Velocity, dt: float) -> ScalarField:
    """Advect a scalar by a frozen velocity with a clamped backtrace."""

    velocity = _velocity(velocity, "velocity")
    dt = _finite(dt, "dt")
    if dt < 0.0:
        raise ValueError("dt must be non-negative")
    values = tuple(field.sample(tuple(x - dt * v for x, v in zip(field.position(i), velocity)))
                   for i in range(len(field.values)))
    return ScalarField(field.shape, field.origin, field.h, values)


def sharp_face_velocity(phi_face: float, h: float, particle: float, grid: float) -> float:
    """Equation 3's sharp two-cell switch, including its closed face edge."""

    h = _h(h)
    phi_face = _finite(phi_face, "phi_face")
    particle = _finite(particle, "particle velocity")
    grid = _finite(grid, "grid velocity")
    return particle if phi_face >= -2.0 * h else grid


def in_particle_band(phi: float, h: float) -> bool:
    h = _h(h)
    phi = _finite(phi, "phi")
    return abs(phi) < 3.0 * h


def fill_sites(cell: Cell, h: float, origin: Point = (0.0, 0.0, 0.0)) -> tuple[Point, ...]:
    h = _h(h)
    origin = _point(origin, "origin")
    if len(cell) != 3 or any(type(index) is not int for index in cell):
        raise ValueError("cell needs three integer indices")
    return tuple(tuple(base + (index + fraction) * h
                        for base, index, fraction in zip(origin, cell, offset))  # type: ignore[return-value]
                 for offset in product(SITE_FRACTIONS, repeat=3))


def _cell(point: Point, h: float, origin: Point) -> Cell:
    return tuple(math.floor((coordinate - base) / h)
                 for coordinate, base in zip(point, origin))  # type: ignore[return-value]


def _check_particle(particle: Particle, label: str) -> Particle:
    if isinstance(particle.id, bool) or not isinstance(particle.id, int):
        raise ValueError(f"{label}.id must be an integer")
    return Particle(particle.id, _point(particle.position, f"{label}.position"),
                    _velocity(particle.velocity, f"{label}.velocity"))


def _live(particles: Sequence[Optional[Particle]], capacity: int) -> list[Particle]:
    if len(particles) > capacity:
        raise ValueError("particle input exceeds capacity")
    return [_check_particle(p, f"particles[{i}]")
            for i, p in enumerate(particles) if p is not None]


def _compact(particles: Sequence[Particle], h: float, capacity: int, origin: Point) -> tuple[Optional[Particle], ...]:
    if len(particles) > capacity:
        raise ValueError("particle operation exceeds capacity")
    ordered = sorted(enumerate(particles), key=lambda item: (_cell(item[1].position, h, origin), item[0]))
    slots: list[Optional[Particle]] = [particle for _, particle in ordered]
    slots.extend([None] * (capacity - len(slots)))
    return tuple(slots)


def delete_and_compact(
    particles: Sequence[Optional[Particle]], *, liquid_phi_at: ScalarSampler,
    solid_phi_at: ScalarSampler, h: float, capacity: int,
    origin: Point = (0.0, 0.0, 0.0),
) -> tuple[tuple[Optional[Particle], ...], int]:
    """Delete deep, free-liquid particles and stably compact the survivors.

    A particle is deleted only below ``-3h`` and farther than ``3h`` from a
    solid.  Thus the near-solid exception retains particles within the solid
    proximity band, including particles whose liquid value is deep.
    """

    h = _h(h)
    capacity = _capacity(capacity)
    origin = _point(origin, "origin")
    live = _live(particles, capacity)
    kept: list[Particle] = []
    deleted = 0
    for particle in live:
        liquid = _finite(liquid_phi_at(particle.position), "liquid phi")
        solid = _finite(solid_phi_at(particle.position), "solid phi")
        if liquid < -3.0 * h and solid > 3.0 * h:
            deleted += 1
        else:
            kept.append(particle)
    return _compact(kept, h, capacity, origin), deleted


def reseed_entering_cells(
    particles: Sequence[Optional[Particle]], *, old_phi: Mapping[Cell, float],
    new_phi: Mapping[Cell, float], liquid_phi_at: ScalarSampler,
    solid_phi_at: ScalarSampler, velocity_at: VelocitySampler,
    cells: Iterable[Cell], h: float, capacity: int,
    origin: Point = (0.0, 0.0, 0.0), next_id: Optional[int] = None,
) -> tuple[tuple[Optional[Particle], ...], int]:
    """Fill each entering cell's particle deficit using grid velocities.

    Entry is closed at the old deep edge and at the new ``-h`` edge:
    ``old_phi <= -3h < new_phi <= -h``.  A candidate site must be at least
    one h below the surface and outside the solid.  The local additions are
    built before returning, so a capacity shortage raises instead of
    truncating the result.
    """

    h = _h(h)
    capacity = _capacity(capacity)
    origin = _point(origin, "origin")
    live = _live(particles, capacity)
    if next_id is None:
        next_id = max((particle.id for particle in live), default=0) + 1
    if isinstance(next_id, bool) or not isinstance(next_id, int):
        raise ValueError("next_id must be an integer")

    used_positions = {particle.position for particle in live}
    counts: dict[Cell, int] = {}
    for particle in live:
        key = _cell(particle.position, h, origin)
        counts[key] = counts.get(key, 0) + 1

    additions: list[Particle] = []
    seen_cells: set[Cell] = set()
    for cell in cells:
        if len(cell) != 3 or any(type(index) is not int for index in cell):
            raise ValueError("cell needs three integer indices")
        if cell in seen_cells:
            continue
        seen_cells.add(cell)
        old = _finite(old_phi[cell], f"old_phi[{cell}]")
        new = _finite(new_phi[cell], f"new_phi[{cell}]")
        if not (old <= -3.0 * h and -3.0 * h < new <= -h):
            continue
        deficit = max(0, REST_PARTICLES - counts.get(cell, 0))
        for site in fill_sites(cell, h, origin):
            if len(additions) >= deficit:
                break
            if site in used_positions:
                continue
            liquid = _finite(liquid_phi_at(site), "liquid phi")
            solid = _finite(solid_phi_at(site), "solid phi")
            if liquid > -h or solid <= 0.0:
                continue
            if len(live) + len(additions) >= capacity:
                raise ValueError("reseed requires more slots than capacity")
            additions.append(Particle(next_id, site, _velocity(velocity_at(site), "grid velocity")))
            next_id += 1
            used_positions.add(site)
        counts[cell] = counts.get(cell, 0) + sum(
            1 for particle in additions if _cell(particle.position, h, origin) == cell
        )
    return _compact(live + additions, h, capacity, origin), len(additions)


def narrow_band_tick(
    particles: Sequence[Optional[Particle]], *, old_phi: Mapping[Cell, float],
    new_phi: Mapping[Cell, float], liquid_phi_at: ScalarSampler,
    solid_phi_at: ScalarSampler, velocity_at: VelocitySampler,
    cells: Iterable[Cell], h: float, capacity: int,
    origin: Point = (0.0, 0.0, 0.0), next_id: Optional[int] = None,
) -> tuple[tuple[Optional[Particle], ...], Mapping[Cell, bool], int, int]:
    """Delete, compact, reseed, and compact one CPU lifecycle tick."""

    h = _h(h)
    mask = {cell: in_particle_band(phi, h) for cell, phi in new_phi.items()}
    after_delete, deleted = delete_and_compact(
        particles, liquid_phi_at=liquid_phi_at, solid_phi_at=solid_phi_at,
        h=h, capacity=capacity, origin=origin,
    )
    after_reseed, reseeded = reseed_entering_cells(
        after_delete, old_phi=old_phi, new_phi=new_phi,
        liquid_phi_at=liquid_phi_at, solid_phi_at=solid_phi_at,
        velocity_at=velocity_at, cells=cells, h=h, capacity=capacity,
        origin=origin, next_id=next_id,
    )
    return after_reseed, mask, deleted, reseeded


if __name__ == "__main__":
    import unittest

    class CpuReferenceTests(unittest.TestCase):
        cell0 = (0, 0, 0)

        def test_strict_band_and_sharp_two_cell_face(self):
            h = 1.0
            self.assertTrue(in_particle_band(math.nextafter(3.0, 0.0), h))
            self.assertFalse(in_particle_band(-3.0, h))
            self.assertEqual(sharp_face_velocity(-2.0, h, 11.0, -7.0), 11.0)
            self.assertEqual(sharp_face_velocity(math.nextafter(-2.0, -math.inf), h, 11.0, -7.0), -7.0)

        def test_semi_lagrangian_scalar_translation(self):
            shape = (8, 8, 8)
            h = 1.0
            field = ScalarField(shape, (0.0, 0.0, 0.0), h, tuple(
                float(x + 2 * y - z)
                for z in range(shape[2]) for y in range(shape[1]) for x in range(shape[0])
            ))
            moved = semi_lagrangian_translate(field, (1.0, 0.0, 0.0), 0.5)
            # Affine interpolation is exact one half-cell inside the boundary.
            self.assertEqual(moved.sample((3.0, 3.0, 3.0)), 3.0 + 6.0 - 3.0 - 0.5)

        def test_multiple_entering_cells_deficits_grid_velocity_and_unique_sites(self):
            cells = ((0, 0, 0), (2, 0, 0))
            existing = (Particle(90, (0.05, 0.05, 0.05), (0.0, 0.0, 0.0)),
                        Particle(91, (2.05, 0.05, 0.05), (0.0, 0.0, 0.0)))
            def velocity(point):
                return (point[0], point[1] + 10.0, point[2] + 20.0)
            slots, count = reseed_entering_cells(
                existing, old_phi={cell: -3.0 for cell in cells},
                new_phi={cell: -1.0 for cell in cells},
                liquid_phi_at=lambda point: -2.0 if point[0] < 1.0 else -0.5,
                solid_phi_at=lambda _point: 1.0, velocity_at=velocity,
                cells=cells, h=1.0, capacity=32, next_id=100,
            )
            live = [particle for particle in slots if particle is not None]
            self.assertEqual(count, 7)
            self.assertEqual(len(live), 9)
            self.assertEqual(len({particle.position for particle in live}), len(live))
            added = [particle for particle in live if particle.id >= 100]
            self.assertTrue(added)
            self.assertTrue(all(particle.velocity == velocity(particle.position) for particle in added))
            self.assertEqual(sum(_cell(p.position, 1.0, (0.0, 0.0, 0.0)) == cells[0] for p in live), 8)
            self.assertEqual(sum(_cell(p.position, 1.0, (0.0, 0.0, 0.0)) == cells[1] for p in live), 1)

        def test_entry_boundaries_and_one_h_resampling(self):
            kwargs = dict(
                particles=(), liquid_phi_at=lambda _point: -1.0,
                solid_phi_at=lambda _point: 1.0, velocity_at=lambda _point: (1.0, 2.0, 3.0),
                cells=(self.cell0,), h=1.0, capacity=16,
            )
            slots, count = reseed_entering_cells(
                old_phi={self.cell0: -3.0}, new_phi={self.cell0: -1.0}, **kwargs)
            self.assertEqual(count, 8)
            self.assertEqual(len({p.position for p in slots if p is not None}), 8)
            slots, count = reseed_entering_cells(
                old_phi={self.cell0: math.nextafter(-3.0, math.inf)},
                new_phi={self.cell0: -1.0}, **kwargs)
            self.assertEqual(count, 0)
            slots, count = reseed_entering_cells(
                old_phi={self.cell0: -3.0}, new_phi={self.cell0: -0.5}, **kwargs)
            self.assertEqual(count, 0)
            # The free-surface half-cell is rejected; solid proximity remains eligible.
            slots, count = reseed_entering_cells(
                old_phi={self.cell0: -3.0}, new_phi={self.cell0: -1.0},
                particles=(), liquid_phi_at=lambda _point: -1.0,
                solid_phi_at=lambda point: 0.0 if point == (0.25, 0.25, 0.25) else 1.0,
                velocity_at=lambda _point: (0.0, 0.0, 0.0), cells=(self.cell0,), h=1.0,
                capacity=16,
            )
            self.assertEqual(count, 7)

        def test_deep_deletion_near_solid_stable_sort_and_cleared_tail(self):
            particles = (
                Particle(4, (1.1, 0.1, 0.1), (0.0, 0.0, 0.0)),
                Particle(5, (0.1, 0.1, 0.1), (0.0, 0.0, 0.0)),
                Particle(6, (2.1, 0.1, 0.1), (0.0, 0.0, 0.0)),
            )
            def liquid(point):
                return -4.0 if point[0] != 2.1 else -2.0
            def solid(point):
                return 4.0 if point[0] == 1.1 else 2.0
            slots, deleted = delete_and_compact(
                particles, liquid_phi_at=liquid, solid_phi_at=solid,
                h=1.0, capacity=8,
            )
            self.assertEqual(deleted, 1)
            self.assertEqual([p.id for p in slots if p is not None], [5, 6])
            self.assertEqual(slots[2:], (None,) * 6)

        def test_capacity_refuses_shortage_and_2pow24(self):
            with self.assertRaises(ValueError):
                _capacity(MAX_SLOTS_EXCLUSIVE)
            with self.assertRaises(ValueError):
                reseed_entering_cells(
                    (), old_phi={self.cell0: -3.0}, new_phi={self.cell0: -1.0},
                    liquid_phi_at=lambda _point: -1.0, solid_phi_at=lambda _point: 1.0,
                    velocity_at=lambda _point: (0.0, 0.0, 0.0), cells=(self.cell0,),
                    h=1.0, capacity=4,
                )

        def test_eight_and_sixteen_lattices_are_bounded(self):
            for size in (8, 16):
                field = ScalarField((size, size, size), (0.0, 0.0, 0.0), 1.0,
                                    tuple(float(i % size) for i in range(size ** 3)))
                moved = semi_lagrangian_translate(field, (0.25, 0.0, 0.0), 0.5)
                self.assertEqual(len(moved.values), size ** 3)

    unittest.main()
