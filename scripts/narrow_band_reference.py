#!/usr/bin/env python3
"""A small f64 oracle for the narrow-band particle lifecycle.

The lifecycle follows Ferstl et al., *Narrow Band FLIP for Liquid
Simulations*, Computer Graphics Forum 35(2), 2016, sections 3.1--3.3,
doi:10.1111/cgf.12825: particles are retained near the free surface, cells
that enter the band are reseeded, particles that move into the deep interior
are removed, and disabling the band restores the dense interior.

This is an analytic CPU reference for later GPU tests.  It deliberately
differs from the paper/runtime boundary choices in two ways requested by the
Option B contract: the band mask is the strict absolute mask
``abs(phi) < 3*h``; and reseeding is only for a cell crossing from
``old_phi <= -3*h`` to ``-3*h < new_phi <= -h``.  The latter avoids seeding
the free-surface layer.  The existing fill convention supplies eight sites at
the quarter/three-quarter positions in each cell (two sites per axis).

The module has no third-party dependencies.  All scalar arithmetic is Python
``float`` (IEEE-754 binary64); every input scalar is checked for finiteness.
"""

from __future__ import annotations

from dataclasses import dataclass
import math
from typing import Callable, Iterable, Mapping, Optional, Sequence


Cell = tuple[int, int, int]
Point = tuple[float, float, float]
Velocity = tuple[float, float, float]
ParticleField = Callable[[Point], float]
VelocityField = Callable[[Point], Velocity]
CellField = Mapping[Cell, float] | Callable[[Cell], float]

REST_PARTICLES = 8
MAX_PARTICLE_SLOTS = 1 << 24
_SITE_COORDINATES = (0.25, 0.75)


@dataclass(frozen=True)
class Particle:
    """The minimum particle state needed by the lifecycle oracle."""

    id: int
    position: Point
    velocity: Velocity


@dataclass(frozen=True)
class LifecycleResult:
    """Fixed-capacity output and counts useful to a proof harness."""

    slots: tuple[Optional[Particle], ...]
    band: Mapping[Cell, bool]
    deleted: int
    reseeded: int


def _finite(value: float, label: str) -> float:
    value = float(value)
    if not math.isfinite(value):
        raise ValueError(f"{label} must be finite, got {value!r}")
    return value


def _check_h(h: float) -> float:
    h = _finite(h, "h")
    if h <= 0.0:
        raise ValueError(f"h must be > 0, got {h!r}")
    return h


def _check_capacity(capacity: int) -> int:
    if isinstance(capacity, bool) or not isinstance(capacity, int):
        raise ValueError(f"capacity must be an integer, got {capacity!r}")
    if capacity < 0:
        raise ValueError(f"capacity must be >= 0, got {capacity}")
    # The existing liquid count wire includes 2^24 exactly; larger pools
    # are refused by the domain capacity check as well.
    if capacity > MAX_PARTICLE_SLOTS:
        raise ValueError(
            f"capacity {capacity} exceeds the 2^24 particle-slot limit"
        )
    return capacity


def _point(point: Iterable[float], label: str) -> Point:
    values = tuple(_finite(value, f"{label}[{i}]") for i, value in enumerate(point))
    if len(values) != 3:
        raise ValueError(f"{label} must have exactly three coordinates")
    return values  # type: ignore[return-value]


def _velocity(value: Iterable[float], label: str) -> Velocity:
    values = _point(value, label)
    return values


def _particle(particle: Particle, label: str) -> Particle:
    if isinstance(particle.id, bool) or not isinstance(particle.id, int):
        raise ValueError(f"{label}.id must be an integer")
    return Particle(
        id=particle.id,
        position=_point(particle.position, f"{label}.position"),
        velocity=_velocity(particle.velocity, f"{label}.velocity"),
    )


def _live_particles(
    particles: Sequence[Optional[Particle]], capacity: int
) -> list[Particle]:
    if len(particles) > capacity:
        raise ValueError(
            f"{len(particles)} particle slots exceed explicit capacity {capacity}"
        )
    return [
        _particle(particle, f"particles[{index}]")
        for index, particle in enumerate(particles)
        if particle is not None
    ]


def _cell(position: Point, h: float, origin: Point) -> Cell:
    return tuple(
        math.floor((coordinate - base) / h)
        for coordinate, base in zip(position, origin)
    )  # type: ignore[return-value]


def _site_key(position: Point, h: float, origin: Point) -> tuple[int, int, int]:
    """Half-cell ownership key used by the runtime's emit-site occupancy test."""

    return tuple(
        math.floor(2.0 * (coordinate - base) / h)
        for coordinate, base in zip(position, origin)
    )  # type: ignore[return-value]


def _cell_point(cell: Cell, offset: tuple[float, float, float], h: float, origin: Point) -> Point:
    return tuple(
        base + (index + fraction) * h
        for index, fraction, base in zip(cell, offset, origin)
    )  # type: ignore[return-value]


def _read_cell(field: CellField, cell: Cell, label: str) -> float:
    try:
        value = field(cell) if callable(field) else field[cell]
    except (KeyError, TypeError) as exc:
        raise ValueError(f"{label} has no value for cell {cell}") from exc
    return _finite(value, f"{label}[{cell}]")


def _sample(field: ParticleField, point: Point, label: str) -> float:
    try:
        value = field(point)
    except (KeyError, TypeError) as exc:
        raise ValueError(f"{label} could not sample point {point}") from exc
    return _finite(value, f"{label}({point})")


def _sample_velocity(field: VelocityField, point: Point) -> Velocity:
    try:
        value = field(point)
    except (KeyError, TypeError) as exc:
        raise ValueError(f"velocity sampler could not sample point {point}") from exc
    return _velocity(value, f"velocity({point})")


def in_band(phi: float, h: float) -> bool:
    """Return the strict Option B band predicate ``abs(phi) < 3*h``."""

    h = _check_h(h)
    phi = _finite(phi, "phi")
    return abs(phi) < 3.0 * h


def band_mask(phi: Mapping[Cell, float] | Iterable[float], h: float):
    """Build an explicit strict band mask, preserving mapping keys or order."""

    h = _check_h(h)
    if isinstance(phi, Mapping):
        return {cell: in_band(value, h) for cell, value in phi.items()}
    return tuple(in_band(value, h) for value in phi)


def fill_sites(cell: Cell, h: float, origin: Point = (0.0, 0.0, 0.0)) -> tuple[Point, ...]:
    """Return the eight deterministic quarter/three-quarter cell sites."""

    h = _check_h(h)
    origin = _point(origin, "origin")
    if len(cell) != 3 or any(not isinstance(index, int) for index in cell):
        raise ValueError(f"cell must be an integer triple, got {cell!r}")
    return tuple(
        _cell_point(cell, (x, y, z), h, origin)
        for z in _SITE_COORDINATES
        for y in _SITE_COORDINATES
        for x in _SITE_COORDINATES
    )


def _compact(
    particles: Sequence[Optional[Particle]], h: float, capacity: int, origin: Point
) -> tuple[Optional[Particle], ...]:
    """Stable cell sort followed by input-order tie breaking and a cleared tail."""

    live = _live_particles(particles, capacity)
    # The runtime flattens x + nx * (y + ny * z), so compare z, y, x.
    ordered = sorted(
        enumerate(live), key=lambda item: (_cell(item[1].position, h, origin)[::-1], item[0])
    )
    if len(ordered) > capacity:
        raise ValueError("particle lifecycle exceeded explicit capacity")
    result = [particle for _, particle in ordered]
    result.extend([None] * (capacity - len(result)))
    return tuple(result)


def delete_and_compact(
    particles: Sequence[Optional[Particle]],
    *,
    liquid_phi_at: ParticleField,
    solid_phi_at: ParticleField,
    h: float,
    capacity: int,
    origin: Point = (0.0, 0.0, 0.0),
) -> tuple[tuple[Optional[Particle], ...], int]:
    """Delete only deep particles, retaining all particles within solid range.

    A particle is deleted exactly when ``liquid_phi < -3*h`` and
    ``solid_phi > 3*h``.  The solid exception preserves particles near a
    body, including a particle whose solid signed distance is inside the body;
    candidate reseed sites separately require ``solid_phi > 0``.
    """

    h = _check_h(h)
    capacity = _check_capacity(capacity)
    origin = _point(origin, "origin")
    live = _live_particles(particles, capacity)
    kept: list[Particle] = []
    deleted = 0
    for particle in live:
        liquid = _sample(liquid_phi_at, particle.position, "liquid_phi_at")
        solid = _sample(solid_phi_at, particle.position, "solid_phi_at")
        if liquid < -3.0 * h and solid > 3.0 * h:
            deleted += 1
        else:
            kept.append(particle)
    return _compact(kept, h, capacity, origin), deleted


def _reseed_cells(
    particles: Sequence[Optional[Particle]],
    *,
    new_phi: CellField,
    old_phi: Optional[CellField],
    liquid_phi_at: ParticleField,
    solid_phi_at: ParticleField,
    velocity_at: VelocityField,
    cells: Iterable[Cell],
    h: float,
    capacity: int,
    origin: Point = (0.0, 0.0, 0.0),
    next_id: Optional[int] = None,
    restore: bool,
) -> tuple[tuple[Optional[Particle], ...], int]:
    """Fill eligible cells up to eight particles using deterministic sites.

    In entering mode, a cell crossing ``old_phi <= -3*h`` to
    ``-3*h < new_phi <= -h`` is eligible.  In restore mode, every cell with
    ``new_phi <= -h`` is eligible.  At most the deficit to eight is selected,
    with occupied sites, non-liquid sites, and solid sites skipped.  New
    velocities come directly from ``velocity_at`` at the selected site.
    """

    h = _check_h(h)
    capacity = _check_capacity(capacity)
    origin = _point(origin, "origin")
    live = _live_particles(particles, capacity)
    used_sites = {
        _site_key(particle.position, h, origin)
        for particle in live
    }
    counts: dict[Cell, int] = {}
    for particle in live:
        particle_cell = _cell(particle.position, h, origin)
        counts[particle_cell] = counts.get(particle_cell, 0) + 1

    if next_id is None:
        next_id = max((particle.id for particle in live), default=0) + 1
    if isinstance(next_id, bool) or not isinstance(next_id, int):
        raise ValueError("next_id must be an integer")

    additions: list[Particle] = []
    seen_cells: set[Cell] = set()
    for cell in cells:
        if len(cell) != 3 or any(not isinstance(index, int) for index in cell):
            raise ValueError(f"cell must be an integer triple, got {cell!r}")
        if cell in seen_cells:
            continue
        seen_cells.add(cell)
        new = _read_cell(new_phi, cell, "new_phi")
        if restore:
            eligible = new <= -h
        else:
            if old_phi is None:
                raise ValueError("entering-cell reseed requires old_phi")
            old = _read_cell(old_phi, cell, "old_phi")
            eligible = old <= -3.0 * h and -3.0 * h < new <= -h
        if not eligible:
            continue
        deficit = max(0, REST_PARTICLES - counts.get(cell, 0))
        if deficit == 0:
            continue
        cell_added = 0
        for site in fill_sites(cell, h, origin):
            if cell_added >= deficit:
                break
            if _site_key(site, h, origin) in used_sites:
                continue
            liquid = _sample(liquid_phi_at, site, "liquid_phi_at")
            solid = _sample(solid_phi_at, site, "solid_phi_at")
            if not (liquid <= -h and solid > 0.0):
                continue
            if len(live) + len(additions) >= capacity:
                raise ValueError(
                    "reseed requires more particles than explicit capacity"
                )
            additions.append(
                Particle(
                    id=next_id,
                    position=site,
                    velocity=_sample_velocity(velocity_at, site),
                )
            )
            next_id += 1
            cell_added += 1
            used_sites.add(_site_key(site, h, origin))
        counts[cell] = counts.get(cell, 0) + cell_added

    if len(live) + len(additions) > capacity:
        raise ValueError("particle reseed exceeded explicit capacity")
    return _compact(live + additions, h, capacity, origin), len(additions)


def reseed_entering_cells(
    particles: Sequence[Optional[Particle]],
    *,
    old_phi: CellField,
    new_phi: CellField,
    liquid_phi_at: ParticleField,
    solid_phi_at: ParticleField,
    velocity_at: VelocityField,
    cells: Iterable[Cell],
    h: float,
    capacity: int,
    origin: Point = (0.0, 0.0, 0.0),
    next_id: Optional[int] = None,
) -> tuple[tuple[Optional[Particle], ...], int]:
    """Fill cells entering the band using the explicit crossing predicate."""

    return _reseed_cells(
        particles,
        old_phi=old_phi,
        new_phi=new_phi,
        liquid_phi_at=liquid_phi_at,
        solid_phi_at=solid_phi_at,
        velocity_at=velocity_at,
        cells=cells,
        h=h,
        capacity=capacity,
        origin=origin,
        next_id=next_id,
        restore=False,
    )


def restore_interior(
    particles: Sequence[Optional[Particle]],
    *,
    new_phi: CellField,
    liquid_phi_at: ParticleField,
    solid_phi_at: ParticleField,
    velocity_at: VelocityField,
    cells: Iterable[Cell],
    h: float,
    capacity: int,
    origin: Point = (0.0, 0.0, 0.0),
    next_id: Optional[int] = None,
) -> tuple[tuple[Optional[Particle], ...], int]:
    """Restore dense interior particles when narrow-band mode is disabled.

    Every cell with ``new_phi <= -h`` is filled to eight deterministic sites.
    Existing particles are retained, no particles are deleted, and a capacity
    shortage raises before returning any appended result.
    """

    return _reseed_cells(
        particles,
        old_phi=None,
        new_phi=new_phi,
        liquid_phi_at=liquid_phi_at,
        solid_phi_at=solid_phi_at,
        velocity_at=velocity_at,
        cells=cells,
        h=h,
        capacity=capacity,
        origin=origin,
        next_id=next_id,
        restore=True,
    )


def narrow_band_lifecycle(
    particles: Sequence[Optional[Particle]],
    *,
    old_phi: Mapping[Cell, float],
    new_phi: Mapping[Cell, float],
    liquid_phi_at: ParticleField,
    solid_phi_at: ParticleField,
    velocity_at: VelocityField,
    cells: Iterable[Cell],
    h: float,
    capacity: int,
    origin: Point = (0.0, 0.0, 0.0),
    next_id: Optional[int] = None,
) -> LifecycleResult:
    """Delete, compact, reseed, and compact one deterministic lifecycle tick."""

    h = _check_h(h)
    capacity = _check_capacity(capacity)
    for cell, value in old_phi.items():
        _finite(value, f"old_phi[{cell}]")
    mask = band_mask(new_phi, h)
    after_delete, deleted = delete_and_compact(
        particles,
        liquid_phi_at=liquid_phi_at,
        solid_phi_at=solid_phi_at,
        h=h,
        capacity=capacity,
        origin=origin,
    )
    after_reseed, reseeded = reseed_entering_cells(
        after_delete,
        old_phi=old_phi,
        new_phi=new_phi,
        liquid_phi_at=liquid_phi_at,
        solid_phi_at=solid_phi_at,
        velocity_at=velocity_at,
        cells=cells,
        h=h,
        capacity=capacity,
        origin=origin,
        next_id=next_id,
    )
    return LifecycleResult(after_reseed, mask, deleted, reseeded)
