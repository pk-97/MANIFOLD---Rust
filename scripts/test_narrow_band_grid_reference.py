"""Analytic checks of the Ferstl et al. 2016 narrow-band grid reference.

These establish CPU mathematics, not GPU execution or water conservation.
"""

import math
import unittest

from narrow_band_grid_reference import (
    Field, advect, combine_velocity, departure, mac_velocity, redistance,
    surface_union,
)


def field(shape, origin, h, formula):
    nx, ny, nz = shape
    return Field(shape, origin, h, tuple(
        formula(tuple(o + c * h for o, c in zip(origin, (x, y, z))))
        for z in range(nz) for y in range(ny) for x in range(nx)
    ))


class NarrowBandGridProofs(unittest.TestCase):
    def test_trilinear_reproduces_affine_fields_on_odd_rectangular_lattice(self):
        formula = lambda p: 2 * p[0] - 3 * p[1] + 0.5 * p[2] + 7
        f = field((5, 7, 3), (-0.25, 1.0, 2.0), 0.25, formula)
        for p in ((0.13, 1.32, 2.11), (0.75, 2.5, 2.5), (-0.25, 1.0, 2.0)):
            self.assertAlmostEqual(f.sample(p), formula(p), places=13)

    def test_boundary_extension_and_singleton_dimensions(self):
        f = Field((1, 2, 1), (0.0, 0.0, 0.0), 1.0, (2.0, 4.0))
        self.assertEqual(f.sample((-100.0, 0.25, 100.0)), 2.5)
        self.assertEqual(f.sample((0.0, -1.0, 0.0)), 2.0)
        self.assertEqual(f.sample((0.0, 3.0, 0.0)), 4.0)

    def test_planar_level_set_translates_by_known_displacement(self):
        f = field((5, 7, 3), (0.125, 0.125, 0.125), 0.25, lambda p: p[1] - 1.0)
        moved = advect(f, lambda _: (0.0, 0.5, 0.0), 0.25)
        for i, actual in enumerate(moved.values):
            y = f.position(i)[1]
            expected = max(y - 0.125, 0.125) - 1.0
            self.assertEqual(actual, expected)

    def test_zero_velocity_preserves_scalar_lattice(self):
        f = field((5, 3, 7), (0.25, 0.25, 0.25), 0.5,
                  lambda p: p[0] ** 2 - p[1] * p[2])
        self.assertEqual(advect(f, lambda _: (0.0, 0.0, 0.0), 1.0), f)

    def test_rk4_integrates_shear_backtrace_exactly(self):
        # dx/dt=y, dy/dt=2, dz/dt=0. Backward solution is
        # (x - y*dt + dt^2, y - 2*dt, z).
        self.assertEqual(departure((4.0, 3.0, 2.0), lambda p: (p[1], 2.0, 0.0), 0.5),
                         (2.75, 2.0, 2.0))

    def test_mac_sampling_preserves_staggered_rigid_rotation(self):
        faces = (
            field((6, 3, 7), (0.0, 0.25, 0.25), 0.5, lambda p: -p[1]),
            field((5, 4, 7), (0.25, 0.0, 0.25), 0.5, lambda p: p[0]),
            field((5, 3, 8), (0.25, 0.25, 0.0), 0.5, lambda _: 2.0),
        )
        velocity = mac_velocity(faces)
        self.assertEqual(velocity((1.0, 0.75, 1.5)), (-0.75, 1.0, 2.0))
        bad = (Field(faces[0].shape, (1.0, 0.25, 0.25), 0.5, faces[0].values),
               faces[1], faces[2])
        with self.assertRaisesRegex(ValueError, "origins"):
            mac_velocity(bad)

    def test_mac_component_advection_translates_face_values(self):
        # A face component is a scalar lattice at its own MAC origin.
        f = field((6, 3, 7), (0.0, 0.25, 0.25), 0.5,
                  lambda p: 3 * p[0] - p[2])
        moved = advect(f, lambda _: (0.5, 0.0, 0.0), 0.5)
        for i, actual in enumerate(moved.values):
            x, _, z = f.position(i)
            self.assertEqual(actual, 3 * max(x - 0.25, 0.0) - z)

    def test_combination_uses_two_cell_boundary_not_particle_edge(self):
        for h in (0.0625, 0.125, 1.0):
            for phi in (-3 * h, math.nextafter(-2 * h, -math.inf)):
                self.assertEqual(combine_velocity(phi, h, 11.0, -7.0), -7.0)
            for phi in (-2 * h, -h, 0.0, h):
                self.assertEqual(combine_velocity(phi, h, 11.0, -7.0), 11.0)

    def test_union_preserves_particle_surface_and_particle_free_interior(self):
        h = 0.125
        # No particle nearby: positive particle distance must not erase deep water.
        self.assertEqual(surface_union(-10 * h, 3 * h, h), -9 * h)
        # Grid's surface drifted outwards: particles still pin the zero crossing.
        self.assertEqual(surface_union(-0.5 * h, 0.0, h), 0.0)
        # A detached drop survives outside the grid's liquid.
        self.assertEqual(surface_union(3 * h, -0.5 * h, h), -0.5 * h)

    def test_redistance_handles_non_cubic_planes_on_each_axis(self):
        shape = (4, 5, 3)
        h = 0.25
        origin = (-0.5, 1.0, 2.0)
        for axis in range(3):
            f = field(
                shape, origin, h,
                lambda p, axis=axis: p[axis] - (origin[axis] + 1.5 * h),
            )
            actual = redistance(f)
            for i, value in enumerate(actual.values):
                coords = (i % shape[0], (i // shape[0]) % shape[1], i // (shape[0] * shape[1]))
                self.assertEqual(value, (coords[axis] - 1.5) * h)

    def test_redistance_diagonal_sign_field_uses_subcell_and_manhattan_distance(self):
        shape = (4, 4, 4)
        f = field(shape, (0.0, 0.0, 0.0), 1.0,
                  lambda p: sum(p) - 2.25)
        actual = redistance(f)
        self.assertEqual(actual.values[0], -2.25)
        self.assertEqual(actual.values[3], 0.75)
        self.assertEqual(actual.values[-1], 6.75)

    def test_redistance_uses_signed_sentinel_without_crossings(self):
        shape = (2, 3, 1)
        h = 0.5
        sentinel = sum(shape) * h
        positive = Field(shape, (0.0, 0.0, 0.0), h, (2.0,) * 6)
        negative = Field(shape, (0.0, 0.0, 0.0), h, (-2.0,) * 6)
        self.assertEqual(redistance(positive).values, (sentinel,) * 6)
        self.assertEqual(redistance(negative).values, (-sentinel,) * 6)

    def test_redistance_exact_zero_is_a_seed(self):
        shape = (3, 2, 2)
        h = 0.5
        values = [1.0] * 12
        values[1] = 0.0
        f = Field(shape, (0.0, 0.0, 0.0), h, tuple(values))
        self.assertEqual(redistance(f).values, (
            h, 0.0, h,
            2.0 * h, h, 2.0 * h,
            2.0 * h, h, 2.0 * h,
            3.0 * h, 2.0 * h, 3.0 * h,
        ))

    def test_refuses_nonfinite_fields_and_bad_steps(self):
        for h in (0.0, -1.0, math.nan, math.inf):
            with self.assertRaises(ValueError):
                Field((1, 1, 1), (0.0, 0.0, 0.0), h, (0.0,))
        for dt in (-1.0, math.nan, math.inf):
            with self.assertRaises(ValueError):
                departure((0.0, 0.0, 0.0), lambda _: (0.0, 0.0, 0.0), dt)
        with self.assertRaises(ValueError):
            departure((0.0, 0.0, 0.0), lambda _: (math.nan, 0.0, 0.0), 1.0)


if __name__ == "__main__":
    unittest.main()
