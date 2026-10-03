#!/usr/bin/env python3
"""Small f64 reference for component-aware Lentine block projection.

The construction follows Lentine, Zheng, and Fedkiw (2010), sections 3.2--3.5:
https://physbam.stanford.edu/papers/stanford2010-02.pdf

This is deliberately a stdlib-only proof, rather than a production solver.  A
full-water 2 x 2 x 2 block has twelve interior faces.  Its pressure equation is
the weighted Neumann graph Laplacian with one gauge row removed.  The face
flux convention is the one used by ``gpu_flip_step.wgsl``:

    F(cell, face) = w * u + (c_cell - w) * v_s

``c_cell`` is read independently for each cell.  Therefore the solid terms on
an interior face do not generally cancel: the two outward contributions leave
``(c_low - c_high) * v_s`` in the block sum.  A coarse gather must retain those
terms (or sum the complete fine divergence); boundary-only aggregation is not
conservative for unequal open volumes.

The executable self-tests below prove the numerical seam needed by the CPU
reference: direct Cholesky, an independent incidence evaluation, fixed
boundary faces, coarse six-face conservation at alpha=0 scatter, and explicit
rejection of incompatible right-hand sides. The original single-component
solver rejects disconnected graphs; component_projection instead preserves
one coarse unknown per connected component in each block and one gauge per
sealed coarse pocket. Its counterexample uses prescribed Dirichlet boundary
conductances; the connected fine-grid surface solve and dynamic rigid-body
reaction remain outside this reference.
"""

from dataclasses import dataclass, replace
from math import sqrt


# Keep the reference readable on Python versions without a vector package.
def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0])


class ReferenceError(ValueError):
    """Base class for inputs that a Neumann solve must reject."""


class IncompatibleSource(ReferenceError):
    """The Neumann right-hand side has non-zero total mass."""


class DisconnectedGraph(ReferenceError):
    """Positive-weight cells are not one connected Neumann component."""


@dataclass(frozen=True)
class InternalFace:
    low: int
    high: int
    axis: int
    weight: float
    velocity: float
    solid_velocity: float
    center: tuple[float, float, float]


@dataclass(frozen=True)
class BoundaryFace:
    cell: int
    axis: int
    side: int  # -1 for the low block boundary, +1 for the high boundary.
    weight: float
    velocity: float
    solid_velocity: float
    center: tuple[float, float, float]


def cell_index(x, y, z):
    return x + 2 * y + 4 * z


def cell_xyz(cell):
    return cell & 1, (cell >> 1) & 1, (cell >> 2) & 1


def rigid_normal_velocity(center, axis):
    """Normal component of a translating and rotating rigid body."""
    linear = (0.17, -0.09, 0.05)
    angular = (0.11, -0.07, 0.23)
    body_center = (0.5, 0.5, 0.5)
    r = tuple(center[i] - body_center[i] for i in range(3))
    return linear[axis] + cross(angular, r)[axis]


def fixture():
    """Unequal cell volumes, 12 positive internal cuts, and 24 block boundaries."""
    volumes = (0.82, 0.67, 0.91, 0.73, 0.76, 0.88, 0.64, 0.95)
    weights = (0.31, 0.47, 0.63, 0.79, 0.36, 0.54,
               0.68, 0.83, 0.42, 0.58, 0.72, 0.89)
    internal = []
    wi = 0
    for z in range(2):
        for y in range(2):
            for x in range(2):
                low = cell_index(x, y, z)
                for axis in range(3):
                    if (x, y, z)[axis] == 1:
                        continue
                    high_xyz = [x, y, z]
                    high_xyz[axis] += 1
                    high = cell_index(*high_xyz)
                    center = [x + 0.5, y + 0.5, z + 0.5]
                    center[axis] = (x, y, z)[axis] + 1.0
                    internal.append(InternalFace(
                        low, high, axis, weights[wi],
                        0.08 * (wi + 1), rigid_normal_velocity(center, axis),
                        tuple(center)))
                    wi += 1

    boundary = []
    for axis in range(3):
        for side in (-1, 1):
            fixed_axis = 0 if side < 0 else 1
            for a in range(2):
                for b in range(2):
                    xyz = [0, 0, 0]
                    xyz[axis] = fixed_axis
                    other = [q for q in range(3) if q != axis]
                    xyz[other[0]] = a
                    xyz[other[1]] = b
                    cell = cell_index(*xyz)
                    center = [v + 0.5 for v in xyz]
                    center[axis] = 0.0 if side < 0 else 2.0
                    boundary.append(BoundaryFace(
                        cell, axis, side, 0.23 + 0.031 * len(boundary),
                        -0.04 + 0.013 * len(boundary),
                        rigid_normal_velocity(center, axis), tuple(center)))
    assert len(internal) == 12 and len(boundary) == 24
    assert all(face.weight > 0.0 for face in internal)
    assert len({face.weight for face in internal}) == 12
    return volumes, internal, boundary


def _face_flux(weight, velocity, solid_velocity, volume):
    return weight * velocity + (volume - weight) * solid_velocity


def direct_divergence(volumes, internal, boundary, h=1.0):
    """Cell-centric six-face stencil, independent of matrix/incidence assembly."""
    out = []
    for cell in range(8):
        net = 0.0
        for axis in range(3):
            for sign in (-1, 1):
                coordinate = cell_xyz(cell)[axis]
                if coordinate + sign not in (0, 1):
                    face = next(f for f in boundary
                                if (f.cell, f.axis, f.side) == (cell, axis, sign))
                else:
                    neighbour = cell + sign * (1 << axis)
                    face = next(f for f in internal
                                if (f.low, f.high) == (min(cell, neighbour),
                                                       max(cell, neighbour)))
                net += sign * _face_flux(face.weight, face.velocity,
                                         face.solid_velocity, volumes[cell])
        out.append(net / h)
    return tuple(out)


def incidence_divergence(volumes, internal, boundary, h=1.0):
    """Face-centric incidence evaluation; solid terms remain cell-local."""
    out = [0.0] * 8
    for face in internal:
        fluid = face.weight * face.velocity
        for cell, sign in ((face.low, 1.0), (face.high, -1.0)):
            out[cell] += sign * (fluid +
                                (volumes[cell] - face.weight) * face.solid_velocity)
    for face in boundary:
        out[face.cell] += face.side * _face_flux(
            face.weight, face.velocity, face.solid_velocity, volumes[face.cell])
    return tuple(value / h for value in out)


def project_local(volumes, internal, boundary, target, h=1.0, gauge=0):
    """Subtract grad(p) on interior faces only; p includes the time step.

    A is the positive weighted graph Laplacian. Since div(-grad(p)) is
    A p / h^2, solve A p = h^2 (target - div(u)).
    """
    raw = direct_divergence(volumes, internal, boundary, h)
    rhs = tuple(h * h * (wanted - value) for wanted, value in zip(target, raw))
    pressure, residual = solve_neumann(internal, rhs, gauge)
    corrected = tuple(replace(f, velocity=f.velocity -
                              (pressure[f.high] - pressure[f.low]) / h)
                      for f in internal)
    return corrected, boundary, pressure, residual


def laplacian(internal):
    matrix = [[0.0] * 8 for _ in range(8)]
    for face in internal:
        w = face.weight
        matrix[face.low][face.low] += w
        matrix[face.high][face.high] += w
        matrix[face.low][face.high] -= w
        matrix[face.high][face.low] -= w
    return matrix


def _connected(internal):
    links = [[] for _ in range(8)]
    for face in internal:
        if face.weight < 0.0:
            raise ReferenceError("negative face weight")
        if face.weight > 0.0:
            links[face.low].append(face.high)
            links[face.high].append(face.low)
    seen = {0}
    todo = [0]
    while todo:
        cell = todo.pop()
        for neighbour in links[cell]:
            if neighbour not in seen:
                seen.add(neighbour)
                todo.append(neighbour)
    return len(seen) == 8


def _cholesky_solve(matrix, rhs):
    """Unpivoted direct Cholesky.  A bad pivot is an input error, never floored."""
    n = len(rhs)
    lower = [[0.0] * n for _ in range(n)]
    for i in range(n):
        for j in range(i + 1):
            value = matrix[i][j]
            value -= sum(lower[i][k] * lower[j][k] for k in range(j))
            if i == j:
                if value <= 0.0:
                    raise DisconnectedGraph("non-positive Cholesky pivot")
                lower[i][j] = sqrt(value)
            else:
                lower[i][j] = value / lower[j][j]
    y = [0.0] * n
    for i in range(n):
        y[i] = (rhs[i] - sum(lower[i][k] * y[k] for k in range(i))) / lower[i][i]
    x = [0.0] * n
    for i in range(n - 1, -1, -1):
        x[i] = (y[i] - sum(lower[k][i] * x[k] for k in range(i + 1, n))) / lower[i][i]
    return x


def solve_neumann(internal, rhs, gauge=0):
    """Solve A p = rhs with p[gauge] = 0 and verify Neumann compatibility."""
    if gauge not in range(8):
        raise ReferenceError("gauge must name one of the eight cells")
    if len(rhs) != 8:
        raise ReferenceError("expected eight cells")
    if not _connected(internal):
        raise DisconnectedGraph("positive-weight graph is disconnected")
    scale = max(1.0, *(abs(v) for v in rhs))
    if abs(sum(rhs)) > 1.0e-12 * scale:
        raise IncompatibleSource("Neumann source sum is not zero")
    matrix = laplacian(internal)
    keep = [i for i in range(8) if i != gauge]
    reduced = [[matrix[i][j] for j in keep] for i in keep]
    solution = _cholesky_solve(reduced, [rhs[i] for i in keep])
    pressure = [0.0] * 8
    for cell, value in zip(keep, solution):
        pressure[cell] = value
    residual = max(abs(sum(matrix[i][j] * pressure[j] for j in range(8)) - rhs[i])
                   for i in range(8))
    if residual > 2.0e-12 * scale:
        raise ReferenceError("direct solve residual exceeded tolerance")
    return tuple(pressure), residual


def _coarse_boundary_from_values(boundary, values):
    """Sum four oriented fine boundary fluxes into each coarse side."""
    result = {(axis, side): 0.0 for axis in range(3) for side in (-1, 1)}
    for face, value in zip(boundary, values):
        result[(face.axis, face.side)] += value
    return result


def boundary_fluxes(boundary, volumes):
    return tuple(face.side * _face_flux(
        face.weight, face.velocity, face.solid_velocity, volumes[face.cell])
        for face in boundary)


def coarse_boundary_fluxes(boundary, volumes):
    """Sum four fine boundary fluxes per side; internal solid source is separate."""
    return _coarse_boundary_from_values(boundary, boundary_fluxes(boundary, volumes))


def scatter_boundary_alpha_zero(boundary, delta):
    """Apply each coarse side's axis velocity change to its four fine faces."""
    return tuple(replace(face, velocity=face.velocity + delta[(face.axis, face.side)])
                 for face in boundary)


def balanced_target(raw):
    delta = (0.09, -0.04, 0.03, -0.02, -0.01, 0.05, -0.07, -0.03)
    assert abs(sum(delta)) < 1.0e-15
    return tuple(value + change for value, change in zip(raw, delta))


def test_full_water_block():
    volumes, internal, boundary = fixture()
    raw_direct = direct_divergence(volumes, internal, boundary)
    raw_incidence = incidence_divergence(volumes, internal, boundary)
    assert max(abs(a - b) for a, b in zip(raw_direct, raw_incidence)) < 1.0e-14

    # This is the counterexample to a boundary-only coarse sum.  Unequal c and
    # a moving rigid body leave an internal solid remainder.
    internal_remainder = sum((volumes[f.low] - volumes[f.high]) * f.solid_velocity
                             for f in internal)
    assert abs(internal_remainder) > 1.0e-4
    assert abs(sum(raw_direct) - sum(coarse_boundary_fluxes(boundary, volumes).values())
               - internal_remainder) < 1.0e-13

    target = balanced_target(raw_direct)  # prescribed, nonzero density source
    projected, fixed, pressure, residual = project_local(
        volumes, internal, boundary, target)
    corrected = direct_divergence(volumes, projected, fixed)
    corrected_incidence = incidence_divergence(volumes, projected, fixed)
    divergence_error = max(abs(a - b) for a, b in zip(corrected, target))
    incidence_error = max(abs(a - b) for a, b in zip(corrected, corrected_incidence))
    assert divergence_error < 2.0e-12
    assert incidence_error < 1.0e-14
    assert fixed == boundary
    # Changing the gauge must leave every corrected velocity unchanged.
    other, _, _, _ = project_local(volumes, internal, boundary, target, gauge=7)
    assert max(abs(a.velocity - b.velocity) for a, b in zip(projected, other)) < 1e-13

    gathered = coarse_boundary_fluxes(boundary, volumes)
    delta = {(axis, side): 0.037 * (axis + 1) * side
             for axis in range(3) for side in (-1, 1)}
    scattered = scatter_boundary_alpha_zero(boundary, delta)
    scattered_gathered = coarse_boundary_fluxes(scattered, volumes)
    alpha_error = 0.0
    for key, total in gathered.items():
        axis, side = key
        area = sum(f.weight for f in boundary if (f.axis, f.side) == key)
        expected = side * delta[key] * area
        alpha_error = max(alpha_error, abs(scattered_gathered[key] - total - expected))
    assert alpha_error < 1.0e-14
    worst_residual = max(residual, divergence_error, incidence_error, alpha_error)
    print("PASS full_water_block: 12 interior, 24 fixed boundary, rigid translation+rotation")
    print("  cholesky_residual=%.3e divergence_error=%.3e incidence_error=%.3e alpha_zero_flux_error=%.3e worst_residual=%.3e internal_solid_remainder=%.9f"
          % (residual, divergence_error, incidence_error, alpha_error,
             worst_residual, internal_remainder))


def test_reject_incompatible_source():
    _, internal, _ = fixture()
    bad = [0.0] * 8
    bad[3] = 0.25
    try:
        solve_neumann(internal, bad)
    except IncompatibleSource:
        print("PASS reject_incompatible_source")
    else:
        raise AssertionError("incompatible source was accepted")


def test_lattice_flux_conservation():
    """Independent cell stencil vs coarse boundary flux, including odd edges."""
    from itertools import product

    worst = 0.0
    for n in ((8, 8, 8), (7, 9, 5)):
        h = 0.25
        coarse = tuple((s + 1) // 2 for s in n)

        def volume(p):
            return 0.5 + 0.03125 * (sum(p) % 8)

        def face(p, axis):
            if p[axis] in (0, n[axis]):
                return 0.0, 0.0, 0.0
            weight = 0.125 * (1 + (sum(p) + axis) % 7)
            velocity = (p[(axis + 1) % 3] - p[(axis + 2) % 3]) * 0.0625
            centre = [h * (q + 0.5) for q in p]
            centre[axis] -= h * 0.5
            return weight, velocity, rigid_normal_velocity(centre, axis)

        for block in product(*(range(s) for s in coarse)):
            direct = 0.0
            solid = 0.0
            for offset in product(range(2), repeat=3):
                p = tuple(2 * b + o for b, o in zip(block, offset))
                if any(q >= s for q, s in zip(p, n)):
                    continue
                for axis in range(3):
                    for side in (0, 1):
                        f = list(p)
                        f[axis] += side
                        w, u, vs = face(f, axis)
                        sign = 2 * side - 1
                        direct += sign * _face_flux(w, u, vs, volume(p)) / h
                        solid += sign * (volume(p) - w) * vs / h
            boundary = 0.0
            for axis in range(3):
                others = [a for a in range(3) if a != axis]
                for side in (0, 1):
                    for offset in product(range(2), repeat=2):
                        f = [2 * b for b in block]
                        f[axis] += 2 * side
                        for a, o in zip(others, offset):
                            f[a] += o
                        if f[axis] >= n[axis] or any(f[a] >= n[a] for a in others):
                            continue
                        w, u, _ = face(f, axis)
                        boundary += (2 * side - 1) * w * u / h
            error = abs((boundary + solid - direct) / 8)
            worst = max(worst, error)
            assert error < 2e-14, (n, block, error)
    print("PASS lattice_flux_conservation: 8^3 and 7x9x5; max error=%.3e" % worst)


def test_reject_disconnected_graph():
    _, internal, _ = fixture()
    # Keep every face record: only the four x-links are removed.
    disconnected = tuple(
        replace(face, weight=0.0) if face.axis == 0 else face for face in internal)
    try:
        solve_neumann(disconnected, (0.0,) * 8)
    except DisconnectedGraph:
        print("PASS reject_disconnected_positive_weight_graph")
    else:
        raise AssertionError("disconnected graph was accepted")


def test_coarse_balance_is_insufficient_for_disconnected_cuts():
    """A coarse-zero flux can leave incompatible disconnected fine pockets.

    Two opposite subface fluxes on the same coarse side are invisible to
    every coarse unknown. A solid plane separates their receiving cells.
    No pressure on internal faces can move flux across that plane, regardless
    of how many local gauges are removed. This needs a coarse representation
    that preserves connected fluid components, not a pivot floor or fallback.
    """
    _, original_internal, original_boundary = fixture()
    volumes = (1.0,) * 8
    internal = tuple(replace(f, weight=float(f.axis != 0), velocity=0.0,
                             solid_velocity=0.0) for f in original_internal)
    boundary = []
    for f in original_boundary:
        u = 0.0
        if f.axis == 1 and f.side == -1 and cell_xyz(f.cell)[2] == 0:
            u = -1.0 if cell_xyz(f.cell)[0] == 0 else 1.0
        boundary.append(replace(f, weight=1.0, velocity=u, solid_velocity=0.0))
    assert all(v == 0.0 for v in coarse_boundary_fluxes(boundary, volumes).values())
    divergence = direct_divergence(volumes, internal, boundary)
    assert sum(divergence) == 0.0
    sums = tuple(sum(divergence[i] for i in range(8) if cell_xyz(i)[0] == x)
                 for x in range(2))
    assert sums == (1.0, -1.0)
    matrix = laplacian(internal)
    # Each component indicator is in the nullspace, so rhs must sum to zero
    # on EACH component. The coarse source enforces only their combined sum.
    for x in range(2):
        for row in matrix:
            assert sum(row[i] for i in range(8) if cell_xyz(i)[0] == x) == 0.0
    print("PASS disconnected_cut_counterexample: coarse faces all zero; component sources +1,-1")



@dataclass(frozen=True)
class Edge:
    low: int
    high: int
    weight: float


def components(count, edges, block=None):
    """Positive face connectivity, optionally restricted to each coarse block."""
    links = [[] for _ in range(count)]
    for edge in edges:
        if edge.weight < 0:
            raise ReferenceError("negative face weight")
        if edge.weight > 0 and (block is None or block[edge.low] == block[edge.high]):
            links[edge.low].append(edge.high)
            links[edge.high].append(edge.low)
    labels = [-1] * count
    for root in range(count):
        if labels[root] >= 0:
            continue
        labels[root] = root
        todo = [root]
        while todo:
            for cell in links[todo.pop()]:
                if labels[cell] < 0:
                    labels[cell] = root
                    todo.append(cell)
    return labels


def graph_solve(count, edges, rhs, anchors=None, reverse_gauge=False):
    """Direct SPD solve with one gauge per SEALED graph component.

    Air contributes a Dirichlet diagonal. No mean is silently removed: an
    incompatible sealed pocket is an input error, even if all pockets sum to 0.
    """
    anchors = anchors or [0.0] * count
    matrix = [[0.0] * count for _ in range(count)]
    for i, a in enumerate(anchors):
        matrix[i][i] = a
    for e in edges:
        if e.low == e.high:
            continue
        matrix[e.low][e.low] += e.weight
        matrix[e.high][e.high] += e.weight
        matrix[e.low][e.high] -= e.weight
        matrix[e.high][e.low] -= e.weight
    labels = components(count, edges)
    gauges = set()
    for root in sorted(set(labels)):
        pocket = [i for i in range(count) if labels[i] == root]
        if sum(anchors[i] for i in pocket) == 0:
            scale = max(1.0, sum(abs(rhs[i]) for i in pocket))
            if abs(sum(rhs[i] for i in pocket)) > 1e-12 * scale:
                raise IncompatibleSource("sealed component source sum is not zero")
            gauges.add(pocket[-1] if reverse_gauge else pocket[0])
    keep = [i for i in range(count) if i not in gauges]
    reduced = [[matrix[i][j] for j in keep] for i in keep]
    solution = _cholesky_solve(reduced, [rhs[i] for i in keep])
    pressure = [0.0] * count
    for i, value in zip(keep, solution):
        pressure[i] = value
    residual = max((abs(sum(matrix[i][j] * pressure[j] for j in range(count)) - rhs[i])
                    for i in range(count)), default=0.0)
    assert residual < 5e-12 * max(1.0, *(abs(v) for v in rhs)), residual
    return pressure


def component_coarse(count, edges, blocks, source, anchors=None, reverse_gauge=False):
    """Stage 2 alone: labels, conservative gather, pressure and outer flux.

    This is shared by the complete projection and the GPU value oracle. No
    local correction or boundary velocity mutation is part of this stage.
    """
    anchors = anchors or [0.0] * count
    labels = components(count, edges, blocks)
    roots = sorted(set(labels))
    slots = {root: i for i, root in enumerate(roots)}
    ids = [slots[root] for root in labels]
    coarse_edges = [Edge(ids[e.low], ids[e.high], e.weight / 2)
                    for e in edges if ids[e.low] != ids[e.high] and e.weight > 0]
    coarse_rhs = [sum(source[i] for i in range(count) if ids[i] == k)
                  for k in range(len(roots))]
    coarse_air = [sum(anchors[i] / 2 for i in range(count) if ids[i] == k)
                  for k in range(len(roots))]
    coarse = graph_solve(len(roots), coarse_edges, coarse_rhs, coarse_air, reverse_gauge)
    flux = [e.weight * (coarse[ids[e.low]] - coarse[ids[e.high]]) / 2
            if ids[e.low] != ids[e.high] else 0.0 for e in edges]
    air = [a * coarse[ids[i]] / 2 for i, a in enumerate(anchors)]
    return labels, roots, coarse_rhs, coarse, flux, air


def component_projection(count, edges, blocks, source, anchors=None, reverse_gauge=False):
    """Conservative outer solve and fixed-boundary local Neumann solves.

    Source is the divergence to REMOVE (raw minus prescribed target). Returned
    fluxes are subtracted from the original oriented fluid flux; the graph
    potential is therefore the negative of the runtime pressure convention.
    Units are integrated fine flux (h=1). P is the component indicator;
    coarse rows are P^T A_boundary P / 2 for H=2h. Parallel fine subfaces
    remain distinct edges. Scatter uses that SAME conductance, so each
    component receives exactly its solved flux, including solid/density source.
    """
    labels, roots, coarse_rhs, coarse, flux, air_flux = component_coarse(
        count, edges, blocks, source, anchors, reverse_gauge)
    slots = {root: i for i, root in enumerate(roots)}
    ids = [slots[root] for root in labels]
    remainder = list(source)
    for k, e in enumerate(edges):
        if ids[e.low] != ids[e.high]:
            remainder[e.low] -= flux[k]
            remainder[e.high] += flux[k]
    remainder = [r - a for r, a in zip(remainder, air_flux)]
    worst_compatibility = max(abs(sum(remainder[i] for i in range(count) if ids[i] == k))
                              for k in range(len(roots)))
    assert worst_compatibility < 5e-12
    interior = [e for e in edges if blocks[e.low] == blocks[e.high]]
    local = graph_solve(count, interior, remainder, reverse_gauge=reverse_gauge)
    outer_flux = tuple(flux)
    for k, e in enumerate(edges):
        if blocks[e.low] == blocks[e.high]:
            flux[k] += e.weight * (local[e.low] - local[e.high])
        else:
            assert flux[k] == outer_flux[k]
    actual = list(air_flux)
    for e, f in zip(edges, flux):
        actual[e.low] += f
        actual[e.high] -= f
    error = max(abs(a - b) for a, b in zip(actual, source))
    assert error < 5e-12, error
    assert all(flux[k] == 0.0 for k, e in enumerate(edges) if e.weight == 0)
    return labels, flux, air_flux, error


def test_component_cut_counterexample():
    volumes, original_internal, original_boundary = fixture()
    volumes = (1.0,) * 8
    internal = tuple(replace(f, weight=float(f.axis != 0), velocity=0.0,
                             solid_velocity=0.0) for f in original_internal)
    boundary = tuple(replace(f, weight=1.0, solid_velocity=0.0,
                            velocity=(-1.0 if cell_xyz(f.cell)[0] == 0 else 1.0)
                            if f.axis == 1 and f.side == -1 and cell_xyz(f.cell)[2] == 0 else 0.0)
                     for f in original_boundary)
    source = direct_divergence(volumes, internal, boundary)
    assert sum(source) == 0.0
    assert all(v == 0 for v in coarse_boundary_fluxes(boundary, volumes).values())
    edges = [Edge(f.low, f.high, f.weight) for f in internal]
    # Open outer boundaries supply separate Dirichlet fluxes for each component.
    anchors = [sum(f.weight for f in boundary if f.cell == i) for i in range(8)]
    labels, flux, air, error = component_projection(8, edges, [0] * 8, source, anchors)
    assert labels == [0, 1, 0, 1, 0, 1, 0, 1]
    assert abs(sum(air[::2]) - 1) < 1e-13
    assert abs(sum(air[1::2]) + 1) < 1e-13
    assert all(flux[k] == 0 for k, e in enumerate(edges) if e.weight == 0)
    print("PASS component_cut_counterexample: separate +1,-1 outer flux; error=%.3e" % error)


def test_component_sealed_two_pockets():
    from itertools import product

    worst = 0.0
    for n in ((4, 2, 2), (7, 3, 2)):
        points = list(product(range(n[2]), range(n[1]), range(n[0])))
        points = [(x, y, z) for z, y, x in points]
        lookup = {p: i for i, p in enumerate(points)}
        blocks = [tuple(v // 2 for v in p) for p in points]
        edges = []
        for i, p in enumerate(points):
            for a in range(3):
                q = list(p)
                q[a] += 1
                if tuple(q) not in lookup:
                    continue
                # y=1 is a closed plane INSIDE the coarse blocks, not between them.
                w = 0.0 if a == 1 and q[a] == 1 else 0.23 + 0.07 * ((i + a) % 9)
                edges.append(Edge(i, lookup[tuple(q)], w))
        count = len(points)
        pocket = components(count, edges)
        assert len(set(pocket)) == 2
        source = [0.0] * count
        for k, e in enumerate(edges):
            # Compatible prescribed density/flux source, nonzero on both pockets.
            f = e.weight * (0.17 + 0.013 * k)
            source[e.low] += f
            source[e.high] -= f
        labels, flux, air, error = component_projection(count, edges, blocks, source)
        assert len(set(labels)) > len(set(blocks))
        assert air == [0.0] * count
        _, other, _, _ = component_projection(count, edges, blocks, source, reverse_gauge=True)
        assert max(abs(a - b) for a, b in zip(flux, other)) < 5e-12
        # Changing one pocket cannot alter the other, including coarse boundary faces.
        changed = [v * (2 if pocket[i] == 0 else 1) for i, v in enumerate(source)]
        _, independent, _, _ = component_projection(count, edges, blocks, changed)
        assert max(abs(flux[k] - independent[k]) for k, e in enumerate(edges)
                   if pocket[e.low] != 0 and pocket[e.high] != 0) < 5e-12
        bad = list(source)
        other_root = sorted(set(pocket))[1]
        bad[0] += 0.25
        bad[other_root] -= 0.25
        assert abs(sum(bad)) < 1e-12
        try:
            component_projection(count, edges, blocks, bad)
        except IncompatibleSource:
            pass
        else:
            raise AssertionError("opposite incompatible sealed sources were coupled")
        worst = max(worst, error)
    print("PASS component_sealed_two_pockets: weighted cuts, odd edges, gauges, isolation; error=%.3e" % worst)


def gpu_component_fixtures():
    """Small exact-f32 inputs with independently Cholesky-solved f64 outputs.

    Dyadic weights/sources make sealed compatibility exact at input precision.
    Dry cells are removed before calling the graph reference, not passed as
    isolated fluid unknowns. No shader implementation is mirrored here.
    """
    cases = []
    specs = [("cut", (2, 2, 2)), ("sealed", (4, 2, 2)),
             ("odd", (7, 3, 2)), ("isolation", (7, 3, 2)),
             ("incompatible", (7, 3, 2)), ("dry", (5, 3, 2)),
             ("moving_source", (3, 3, 2)), ("isolated", (1, 1, 1)),
             ("empty", (3, 1, 1)), ("many", (9, 5, 3))]
    for name, n in specs:
        points = [(x, y, z) for z in range(n[2]) for y in range(n[1]) for x in range(n[0])]
        lookup = {p: i for i, p in enumerate(points)}
        count = len(points)
        water = [float(name != "empty" and (name != "dry" or p[0] != 1)) for p in points]
        links = [[0.0] * 4 for _ in points]
        source = [0.0] * count
        wet = [i for i in range(count) if water[i]]
        compact = {i: j for j, i in enumerate(wet)}
        edges, locations = [], []
        for i, p in enumerate(points):
            if not water[i]:
                continue
            for a in range(3):
                q = list(p)
                q[a] += 1
                j = lookup.get(tuple(q))
                if j is None or not water[j]:
                    continue
                closed = (name == "cut" and a == 0) or (name in ("sealed", "odd", "isolation", "incompatible") and a == 1 and q[a] == 1)
                w = 0.0 if closed else (2 + (i + a) % 7) / 16
                links[i][a] = w
                edges.append(Edge(compact[i], compact[j], w))
                locations.append((i, a, j))
                flux = w * (8 + i + a) / 64
                source[i] += flux
                source[j] -= flux
            if name in ("cut", "dry", "moving_source", "many"):
                links[i][3] = 0.5
        if name == "cut":
            source = [0.0] * count
            source[0], source[1] = 1.0, -1.0
        if name == "moving_source":
            # Explicit unequal-volume moving-solid term on EVERY internal face,
            # plus a prescribed source. A boundary-only gather cannot match it.
            volume = [(4 + i % 4) / 8 for i in range(count)]
            for i, a, j in locations:
                vs = (i + 2 * a - 7) / 32
                source[i] += (volume[i] - links[i][a]) * vs
                source[j] -= (volume[j] - links[i][a]) * vs
            source = [s - (i % 3 - 1) / 16 for i, s in enumerate(source)]
        if name == "isolation":
            source = [s * (2 if points[i][1] == 0 else 1) for i, s in enumerate(source)]
        if name == "incompatible":
            source[0] += 0.25
            source[n[0]] -= 0.25
        if name == "dry":
            # Positive stored weights across air must still NOT connect water.
            # The reference graph above contains wet-to-wet edges only.
            for i, p in enumerate(points):
                for a in range(3):
                    q = list(p)
                    q[a] += 1
                    j = lookup.get(tuple(q))
                    if j is not None and (not water[i] or not water[j]):
                        links[i][a] = 0.5
        case = dict(name=name, lattice=n, water=water, links=links, source=source)
        if not wet:
            result = ([], [], [], [], [], [])
        else:
            try:
                result = component_coarse(len(wet), edges,
                    [tuple(v // 2 for v in points[i]) for i in wet],
                    [source[i] for i in wet], [links[i][3] for i in wet])
            except IncompatibleSource:
                assert name == "incompatible"
                case["status"] = 3
                cases.append(case)
                continue
        labels, roots, rhs, pressure, flux, air = result
        expected_labels = [0xffffffff] * count
        expected_rhs, expected_pressure = [0.0] * count, [0.0] * count
        expected_transfer = [[0.0] * 4 for _ in points]
        for k, i in enumerate(wet):
            expected_labels[i] = wet[labels[k]]
            expected_transfer[i][3] = air[k]
        for k, root in enumerate(roots):
            expected_rhs[wet[root]] = rhs[k]
            expected_pressure[wet[root]] = pressure[k]
        for (i, a, _), f in zip(locations, flux):
            expected_transfer[i][a] = f
        case.update(status=1, labels=expected_labels, rhs=expected_rhs,
                    pressure=expected_pressure, transfer=expected_transfer)
        cases.append(case)
    return cases


def test_component_gpu_oracles():
    cases = gpu_component_fixtures()
    assert len(cases) == 10
    assert sum(c["status"] == 3 for c in cases) == 1
    print("PASS component_gpu_oracles: 10 stage-2 f64 value fixtures")


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--filter", default="", help="run self-tests whose names contain this text")
    parser.add_argument("--gpu-fixtures", action="store_true", help="emit stage-2 f64 GPU oracle JSON")
    args = parser.parse_args()
    if args.gpu_fixtures:
        import json
        print(json.dumps(gpu_component_fixtures(), allow_nan=False))
        raise SystemExit(0)
    tests = [value for name, value in sorted(globals().copy().items())
             if name.startswith("test_") and args.filter in name]
    if not tests:
        parser.error("filter selected no self-tests")
    for test in tests:
        test()
    print("lentine_reference.py: %d self-tests passed" % len(tests))
