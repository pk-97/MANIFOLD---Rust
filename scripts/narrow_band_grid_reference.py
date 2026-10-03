#!/usr/bin/env python3
"""Independent f64 grid oracle for GPU FLIP narrow-band pass proofs.

Ferstl, Ando, Wojtan, Westermann & Thuerey (2016), Narrow Band FLIP for
Liquid Simulations, doi:10.1111/cgf.12825, equations 3 and 4. This is a CPU
reference, not an alternate runtime solver. Particle lifecycle is in
narrow_band_reference.py. Fields use physical metres and x-fastest storage;
origins are the first sample's position (cell centre or MAC face centre).

Semi-Lagrangian advection uses RK4 departure points and trilinear sampling.
Samples outside the lattice extend the nearest boundary value, explicitly;
the GPU's solid/open-boundary treatment still needs its own integration proof.
Surface union alone is NOT redistancing. Its output must be reinitialized
before using it as a distance for the particle band.
"""

from dataclasses import dataclass
from itertools import product
from math import floor, isfinite, prod
from typing import Callable

Vec3 = tuple[float, float, float]
Velocity = Callable[[Vec3], Vec3]


@dataclass(frozen=True)
class Field:
    shape: tuple[int, int, int]
    origin: Vec3
    h: float
    values: tuple[float, ...]

    def __post_init__(self):
        if len(self.shape) != 3 or any(type(n) is not int or n < 1 for n in self.shape):
            raise ValueError("field shape needs three positive integer dimensions")
        if not isfinite(self.h) or self.h <= 0:
            raise ValueError("cell size must be finite and positive")
        if len(self.origin) != 3 or not all(map(isfinite, self.origin)):
            raise ValueError("field origin must be finite")
        if len(self.values) != prod(self.shape) or not all(map(isfinite, self.values)):
            raise ValueError("field values must be finite and cover the lattice")

    def position(self, i: int) -> Vec3:
        nx, ny, _ = self.shape
        cell = (i % nx, (i // nx) % ny, i // (nx * ny))
        return tuple(o + c * self.h for o, c in zip(self.origin, cell))

    def sample(self, p: Vec3) -> float:
        if len(p) != 3 or not all(map(isfinite, p)):
            raise ValueError("sample position must be finite")
        q = tuple(min(max((x - o) / self.h, 0.0), n - 1)
                  for x, o, n in zip(p, self.origin, self.shape))
        lo = tuple(floor(x) for x in q)
        hi = tuple(min(i + 1, n - 1) for i, n in zip(lo, self.shape))
        t = tuple(x - i for x, i in zip(q, lo))
        nx, ny, _ = self.shape
        total = 0.0
        for bits in product((0, 1), repeat=3):
            c = tuple(hi[a] if bits[a] else lo[a] for a in range(3))
            weight = prod(t[a] if bits[a] else 1.0 - t[a] for a in range(3))
            total += weight * self.values[c[0] + nx * (c[1] + ny * c[2])]
        return total


def mac_velocity(faces: tuple[Field, Field, Field]) -> Velocity:
    """Sample each component on its own staggered grid, never at cell nodes."""
    if len(faces) != 3:
        raise ValueError("MAC velocity needs three face fields")
    u, v, w = faces
    cells = (u.shape[0] - 1, v.shape[1] - 1, w.shape[2] - 1)
    for a, field in enumerate(faces):
        expected = tuple(n + int(b == a) for b, n in enumerate(cells))
        if field.shape != expected or field.h != u.h:
            raise ValueError("MAC face extents or spacing disagree")
    box_min = (u.origin[0], v.origin[1], w.origin[2])
    for a, field in enumerate(faces):
        expected = tuple(x + (0.0 if a == b else 0.5 * u.h)
                         for b, x in enumerate(box_min))
        if field.origin != expected:
            raise ValueError("MAC face origins disagree")
    return lambda p: tuple(field.sample(p) for field in faces)


def departure(p: Vec3, velocity: Velocity, dt: float) -> Vec3:
    """Fourth-order Runge-Kutta backtrace through a frozen velocity field."""
    if not isfinite(dt) or dt < 0:
        raise ValueError("step duration must be finite and nonnegative")
    if len(p) != 3 or not all(map(isfinite, p)):
        raise ValueError("departure position must be finite")

    def offset(v, scale):
        if len(v) != 3 or not all(map(isfinite, v)):
            raise ValueError("grid velocity must be finite")
        return tuple(x - scale * dt * speed for x, speed in zip(p, v))

    k1 = velocity(p)
    k2 = velocity(offset(k1, 0.5))
    k3 = velocity(offset(k2, 0.5))
    k4 = velocity(offset(k3, 1.0))
    offset(k4, 0.0)  # Validate the last sample before forming the weighted sum.
    return tuple(x - dt * (a + 2 * b + 2 * c + d) / 6
                 for x, a, b, c, d in zip(p, k1, k2, k3, k4))


def advect(field: Field, velocity: Velocity, dt: float) -> Field:
    values = tuple(field.sample(departure(field.position(i), velocity, dt))
                   for i in range(len(field.values)))
    return Field(field.shape, field.origin, field.h, values)


def combine_velocity(phi: float, h: float, particle: float, grid: float) -> float:
    """Equation 3: sharp transition at -2h, inside the three-cell band."""
    if not all(map(isfinite, (phi, h, particle, grid))) or h <= 0:
        raise ValueError("combination inputs must be finite, with positive cell size")
    return particle if phi >= -2.0 * h else grid


def surface_union(advected_phi: float, particle_phi: float, h: float) -> float:
    """Equation 4, before redistancing; retain particle surface authority."""
    if not all(map(isfinite, (advected_phi, particle_phi, h))) or h <= 0:
        raise ValueError("surface inputs must be finite, with positive cell size")
    return min(advected_phi + h, particle_phi)


def redistance(field: Field) -> Field:
    """Reinitialize ``field`` from subcell sign crossings with L1 distance.

    Each lattice node first receives the nearest interpolated crossing on one
    of its six edges.  The final distance is the direct minimum of that seed
    distance plus the grid-space Manhattan distance to every seed.  This is a
    small, independent oracle for the shader's separable sweeps, so it keeps
    the brute-force minimum instead of reproducing the sweep implementation.
    """

    nx, ny, nz = field.shape
    total = len(field.values)
    sentinel = sum(field.shape) * field.h
    seeds = [sentinel] * total

    def index(x: int, y: int, z: int) -> int:
        return x + nx * (y + ny * z)

    for z in range(nz):
        for y in range(ny):
            for x in range(nx):
                i = index(x, y, z)
                here = field.values[i]
                if here == 0.0:
                    seeds[i] = 0.0
                for axis, extent in enumerate(field.shape):
                    if (x, y, z)[axis] + 1 >= extent:
                        continue
                    neighbor_coords = [x, y, z]
                    neighbor_coords[axis] += 1
                    j = index(*neighbor_coords)
                    other = field.values[j]
                    if (here < 0.0) == (other < 0.0):
                        continue
                    denominator = abs(here) + abs(other)
                    crossing = field.h * abs(here) / denominator
                    seeds[i] = min(seeds[i], crossing)
                    other_crossing = field.h * abs(other) / denominator
                    seeds[j] = min(seeds[j], other_crossing)

    result = []
    coordinates = [
        (x, y, z)
        for z in range(nz)
        for y in range(ny)
        for x in range(nx)
    ]
    for i, (x, y, z) in enumerate(coordinates):
        distance = min(
            seed + field.h * (abs(x - sx) + abs(y - sy) + abs(z - sz))
            for seed, (sx, sy, sz) in zip(seeds, coordinates)
        )
        value = field.values[i]
        result.append(-distance if value < 0.0 else distance)
    return Field(field.shape, field.origin, field.h, tuple(result))
