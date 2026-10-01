#!/usr/bin/env python3
"""The GPU FLIP pressure solve in f64: the oracle for the pinned residuals in
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_solve_tests.rs.

Multigrid-preconditioned conjugate gradient (McAdams, Sifakis & Teran 2010),
step for step as the graph runs it (docs/GPU_FLIP_PRESSURE_SOLVE.md): the
masked Poisson equation L p = f on water cells, p = 0 on air, box walls
closed, each face weighted by its open fraction (1 without solids). One V-cycle per iteration: red-black Gauss-Seidel, 2 sweeps before
and 2 after, trilinear transfers, a coarse cell is air if any child is air,
halving while every side is even and one is over 4, then the coarsest level
solved exactly by its inverse (node.coarse_inverse's sweep and pinning).
A coarse face's weight is the mean of the four fine faces it covers
(node.coarsen_solid_faces); a cell whose faces are all closed drops out.

Usage: scripts/mgpcg_reference.py crates/manifold-renderer/tests/fixtures/dambreak_pressure_problems.bin.zst
           [--refine 2] [--iterations 4,6,8] [--box 0.5,0.25,0.5,0.12,0.12,0.12]
       scripts/mgpcg_reference.py --frames DUMP.bin [...]
The second form reads solves dumped from a running scene (per record:
u32 frame, step, kind, n; then water, f and another solver's pressure as n³
f32 each, or zeros) and compares against that pressure's residual.

--box cx,cy,cz,hx,hy,hz (fractions of the box side) puts a solid box in the
water: face weights from its distance on the cell corners by FLIP Fluids'
fractionInside (node.solid_faces), then per problem the gate of
docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water): the
reference against a direct sparse solve, and the iterations the box takes to
reach the empty tank's residual at each --iterations count.
"""
import argparse
import struct
import subprocess

import numpy as np
import scipy.sparse
import scipy.sparse.linalg

L = 4.0
NU = 2


def load(path):
    raw = subprocess.run(["zstd", "-dc", path], capture_output=True, check=True).stdout
    magic, ver, nx, ny, nz, count = struct.unpack_from("<4sIIIII", raw, 0)
    assert magic == b"SWFX" and ver == 1
    off = 24
    probs = []
    for _ in range(count):
        frame, wc = struct.unpack_from("<II", raw, off)
        off += 8
        nbytes = nx * ny * nz // 8
        bits = np.frombuffer(raw, np.uint8, nbytes, off)
        off += nbytes
        water = np.unpackbits(bits, bitorder="little").astype(bool).reshape(nz, ny, nx)
        vals = np.frombuffer(raw, np.float32, wc, off).astype(np.float64)
        off += 4 * wc
        f = np.zeros(nx * ny * nz)
        f[water.ravel()] = vals
        probs.append((f"frame {frame:3d}", water, f.reshape(nz, ny, nx), None))
    return probs


def frames(path):
    raw = open(path, "rb").read()
    off, probs = 0, []
    while off < len(raw):
        frame, step, kind, n = np.frombuffer(raw, np.uint32, 4, off)
        off += 16
        arrs = []
        for _ in range(3):
            arrs.append(np.frombuffer(raw, np.float32, int(n) ** 3, off).astype(np.float64).reshape(n, n, n))
            off += 4 * int(n) ** 3
        water = arrs[0] > 0.5
        name = f"{n}^3 frame {frame} step {step} {('main', 'density')[kind]}"
        probs.append((name, water, arrs[1] * water, arrs[2]))
    return probs


def refine(water, f, k):
    for ax in range(3):
        water = np.repeat(water, k, axis=ax)
        f = np.repeat(f, k, axis=ax)
    return water, f


def open_faces(shape):
    """Face weights per numpy axis, n + 1 faces along it: 1 inside, 0 on the
    box walls."""
    out = []
    for ax in range(3):
        sh = list(shape)
        sh[ax] += 1
        w = np.ones(sh)
        idx = [slice(None)] * 3
        idx[ax] = [0, -1]
        w[tuple(idx)] = 0.0
        out.append(w)
    return out


def along(x, ax, sl):
    idx = [slice(None)] * 3
    idx[ax] = sl
    return x[tuple(idx)]


def side(w, ax, lo):
    """Each cell's low or high face weight along a numpy axis."""
    return along(w, ax, slice(None, -1) if lo else slice(1, None))


def neighbour_sum(q, faces):
    """Σ over the six faces of the face weight × the neighbour's value."""
    out = np.zeros_like(q)
    for ax, w in enumerate(faces):
        pad = [(0, 0)] * 3
        pad[ax] = (1, 1)
        pq = np.pad(q, pad)
        out += side(w, ax, True) * along(pq, ax, slice(None, -2))
        out += side(w, ax, False) * along(pq, ax, slice(2, None))
    return out


def diagonal(faces):
    return sum(side(w, ax, True) + side(w, ax, False) for ax, w in enumerate(faces))


class Level:
    def __init__(self, water, h, faces):
        self.faces = faces
        self.count = diagonal(faces)
        self.water = water & (self.count > 0)
        self.w = self.water.astype(float)
        self.h = h
        self.safe = np.where(self.count > 0, self.count, 1.0)
        parity = np.indices(water.shape).sum(0) % 2
        self.colors = [(parity == 0) & self.water, (parity == 1) & self.water]

    def laplacian(self, p):
        q = p * self.w
        return (neighbour_sum(q, self.faces) - self.count * q) / self.h**2 * self.w

    def residual(self, rhs, p):
        return (rhs - self.laplacian(p)) * self.w

    def sweep(self, p, rhs, color):
        new = (neighbour_sum(p * self.w, self.faces) - self.h**2 * rhs) / self.safe
        out = p.copy()
        out[self.colors[color]] = new[self.colors[color]]
        return out


def coarsen_faces(faces):
    """Every other face along its axis, the mean of each 2×2 across it."""
    out = []
    for ax, w in enumerate(faces):
        w = along(w, ax, slice(None, None, 2))
        for b in range(3):
            if b != ax:
                w = 0.5 * (along(w, b, slice(0, None, 2)) + along(w, b, slice(1, None, 2)))
        out.append(w)
    return out


def prolong_1d(nc):
    P = np.zeros((2 * nc, nc))
    for f in range(2 * nc):
        c = f // 2
        other = max(c - 1, 0) if f % 2 == 0 else min(c + 1, nc - 1)
        P[f, c] += 0.75
        P[f, other] += 0.25
    return P


def apply_axes(mats, x):
    for ax, M in enumerate(mats):
        x = np.moveaxis(np.tensordot(M, np.moveaxis(x, ax, 0), axes=(1, 0)), 0, ax)
    return x


def matrix(water, faces):
    """A for L = −A / h² on the water: Σw on the diagonal, −w between water
    neighbours, as rows over every cell (zero rows off the water)."""
    count = diagonal(faces).ravel()
    w = water.ravel()
    cells = w.size
    rows, cols, vals = [], [], []
    flat = np.arange(cells).reshape(water.shape)
    for ax in range(3):
        lo = np.take(flat, range(water.shape[ax] - 1), axis=ax).ravel()
        hi = np.take(flat, range(1, water.shape[ax]), axis=ax).ravel()
        idx = [slice(None)] * 3
        idx[ax] = slice(1, -1)
        weight = faces[ax][tuple(idx)].ravel()
        both = w[lo] & w[hi]
        rows += [lo[both], hi[both]]
        cols += [hi[both], lo[both]]
        vals += [-weight[both], -weight[both]]
    rows.append(np.arange(cells))
    cols.append(np.arange(cells))
    vals.append(np.where(w, count, 0.0))
    return np.concatenate(rows), np.concatenate(cols), np.concatenate(vals), count


def coarse_inverse(water, faces):
    """A⁻¹ by the kernel's symmetric sweep: a pivot under 1e-4 of its
    diagonal pins its cell (row and column cleared)."""
    rows, cols, vals, count = matrix(water, faces)
    cells = water.size
    a = np.zeros((cells, cells))
    np.add.at(a, (rows, cols), vals)
    m = a.copy()
    for k in range(cells):
        d = m[k, k]
        if not d > 1e-4 * count[k]:
            m[k, :] = 0.0
            m[:, k] = 0.0
            continue
        col = m[:, k].copy()
        row = m[k, :].copy()
        m -= np.outer(col, row) / d
        m[k, :] = row / d
        m[:, k] = col / d
        m[k, k] = -1.0 / d
    return -m


class Multigrid:
    def __init__(self, water, h, faces):
        self.levels = [Level(water, h, faces)]
        self.P = []
        while max(water.shape) > 4 and all(n % 2 == 0 for n in water.shape):
            nz, ny, nx = water.shape
            water = water.reshape(nz // 2, 2, ny // 2, 2, nx // 2, 2).all(axis=(1, 3, 5))
            faces = coarsen_faces(faces)
            h *= 2
            self.P.append([prolong_1d(n) for n in water.shape])
            self.levels.append(Level(water, h, faces))
        last = self.levels[-1]
        self.inverse = coarse_inverse(last.water, last.faces)

    def vcycle(self, l, r):
        lv = self.levels[l]
        e = np.zeros_like(r)
        if l == len(self.levels) - 1:
            return (-lv.h**2 * self.inverse @ r.ravel()).reshape(r.shape)
        for _ in range(NU):
            for color in (0, 1):
                e = lv.sweep(e, r, color)
        rc = apply_axes([M.T / 2.0 for M in self.P[l]], lv.residual(r, e)) * self.levels[l + 1].w
        e = e + apply_axes(self.P[l], self.vcycle(l + 1, rc)) * lv.w
        for _ in range(NU):
            for color in (1, 0):
                e = lv.sweep(e, r, color)
        return e


def true_residual(p, water, f, h, faces):
    lv = Level(water, h, faces)
    return np.linalg.norm(lv.laplacian(p) - f * lv.w) / np.linalg.norm(f * lv.w)


def solve(water, f, h, iterations, faces, keep=False):
    """Residual after each iteration, as the graph's fixed-count loop runs."""
    mg = Multigrid(water, h, faces)
    lv = mg.levels[0]
    x = np.zeros_like(f)
    r = f * lv.w
    p = np.zeros_like(f)
    rz_old = 0.0
    out = []
    for _ in range(iterations):
        z = mg.vcycle(0, r)
        rz = np.sum(r * z)
        beta = rz / rz_old if abs(rz_old) >= 1e-30 else 0.0
        p = z + beta * p
        s = lv.residual(0.0, p)
        ps = np.sum(p * s)
        alpha = rz / ps if abs(ps) >= 1e-30 else 0.0
        x = x - alpha * p
        r = r - alpha * s
        rz_old = rz
        out.append(true_residual(x, water, f, h, faces))
    return (out, len(mg.levels), x) if keep else (out, len(mg.levels))


def segment(left, right):
    """FLIP Fluids' LevelsetUtils::fractionInside for a segment."""
    if left < 0 and right < 0:
        return 1.0
    if left < 0:
        return left / (left - right)
    if right < 0:
        return right / (right - left)
    return 0.0


def square(bl, br, tl, tr):
    """FLIP Fluids' LevelsetUtils::fractionInside for a square."""
    inside = sum(v < 0 for v in (bl, br, tl, tr))
    v = [bl, br, tr, tl]
    if inside == 4:
        return 1.0
    if inside == 3:
        while v[0] < 0:
            v = v[1:] + v[:1]
        return 1 - 0.5 * (1 - segment(v[0], v[3])) * (1 - segment(v[0], v[1]))
    if inside == 2:
        while v[0] >= 0 or not (v[1] < 0 or v[2] < 0):
            v = v[1:] + v[:1]
        if v[1] < 0:
            return 0.5 * (segment(v[0], v[3]) + segment(v[1], v[2]))
        if 0.25 * sum(v) < 0:
            return 1 - (0.5 * (1 - segment(v[0], v[3])) * (1 - segment(v[2], v[3]))
                        + 0.5 * (1 - segment(v[0], v[1])) * (1 - segment(v[2], v[1])))
        return (0.5 * segment(v[0], v[1]) * segment(v[0], v[3])
                + 0.5 * segment(v[2], v[1]) * segment(v[2], v[3]))
    if inside == 1:
        while v[0] >= 0:
            v = v[1:] + v[:1]
        return 0.5 * segment(v[0], v[3]) * segment(v[0], v[1])
    return 0.0


def box_faces(shape, box):
    """node.solid_faces for a box (fractions of the side): its exact distance
    on the cell corners, each face 1 − fractionInside of its four corners in
    the engine's order, the box walls closed."""
    nz, ny, nx = shape
    c, half = np.array(box[:3]), np.array(box[3:])
    zz, yy, xx = np.meshgrid(*(np.arange(n + 1) / shape[0] for n in shape), indexing="ij")
    d = np.abs(np.stack([xx, yy, zz]) - c[:, None, None, None]) - half[:, None, None, None]
    phi = np.linalg.norm(np.maximum(d, 0), axis=0) + np.minimum(d.max(axis=0), 0)
    sq = np.vectorize(square)
    a = phi
    # numpy axis 2 is x: corners (i,j,k), (i,j+1,k), (i,j,k+1), (i,j+1,k+1)
    fx = 1 - sq(a[:-1, :-1, :], a[:-1, 1:, :], a[1:, :-1, :], a[1:, 1:, :])
    # y: (i,j,k), (i,j,k+1), (i+1,j,k), (i+1,j,k+1)
    fy = 1 - sq(a[:-1, :, :-1], a[1:, :, :-1], a[:-1, :, 1:], a[1:, :, 1:])
    # z: (i,j,k), (i,j+1,k), (i+1,j,k), (i+1,j+1,k)
    fz = 1 - sq(a[:, :-1, :-1], a[:, 1:, :-1], a[:, :-1, 1:], a[:, 1:, 1:])
    return [np.clip(w, 0, 1) * o for w, o in zip((fz, fy, fx), open_faces(shape))]


def direct(water, f, h, faces):
    """L x = f on the water by a sparse direct solve."""
    lv = Level(water, h, faces)
    rows, cols, vals, _ = matrix(lv.water, faces)
    a = scipy.sparse.csr_matrix((vals, (rows, cols)), shape=(water.size, water.size))
    keep = np.flatnonzero(lv.water.ravel())
    x = np.zeros(water.size)
    x[keep] = scipy.sparse.linalg.spsolve(a[keep][:, keep].tocsc(), -h**2 * f.ravel()[keep])
    return x.reshape(water.shape)


def box_gate(name, water, f, h, counts, box):
    empty, levels = solve(water, f, h, max(counts), open_faces(water.shape))
    faces = box_faces(water.shape, box)
    most = 4 * max(counts)
    res, _, x = solve(water, f, h, most, faces, keep=True)
    x_direct = direct(water, f, h, faces)
    gap = np.linalg.norm(x - x_direct) / np.linalg.norm(x_direct)
    parts = []
    for k in counts:
        need = next((i + 1 for i, r in enumerate(res) if r <= empty[k - 1]), None)
        parts.append(f"{k} -> {need if need else f'>{most}'} ({'+' + format(100 * (need - k) / k, '.0f') + '%' if need else 'kill'})")
    closed = int(np.sum(water & (diagonal(faces) == 0)))
    print(f"{water.shape[0]}^3 {name}: {levels} levels, {closed} water cells closed in; "
          f"iterations to the empty tank's residual {', '.join(parts)}; after {most}: residual "
          f"{res[-1]:.3e}, direct residual {true_residual(x_direct, water, f, h, faces):.3e}, "
          f"gap to direct {gap:.3e}", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("fixture", nargs="?")
    ap.add_argument("--frames")
    ap.add_argument("--refine", type=int, default=1)
    ap.add_argument("--iterations", default="4,6,8")
    ap.add_argument("--box")
    args = ap.parse_args()
    counts = [int(k) for k in args.iterations.split(",")]
    probs = frames(args.frames) if args.frames else load(args.fixture)
    for name, water, f, old in probs:
        if args.refine > 1:
            water, f = refine(water, f, args.refine)
        h = L / water.shape[0]
        if args.box:
            box_gate(name, water, f, h, counts, [float(v) for v in args.box.split(",")])
            continue
        res, levels = solve(water, f, h, max(counts), open_faces(water.shape))
        line = ", ".join(f"{k}: {res[k - 1]:.3e}" for k in counts)
        old_line = "" if old is None else f"; old solver {true_residual(old, water, f, h, open_faces(water.shape)):.3e}"
        print(f"{water.shape[0]}^3 {name}: {levels} levels; residual after {line}{old_line}", flush=True)


if __name__ == "__main__":
    main()
