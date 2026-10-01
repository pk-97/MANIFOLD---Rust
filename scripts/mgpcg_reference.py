#!/usr/bin/env python3
"""The GPU FLIP pressure solve in f64: the oracle for the pinned residuals in
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_solve_tests.rs.

Multigrid-preconditioned conjugate gradient (McAdams, Sifakis & Teran 2010),
step for step as the graph runs it (docs/GPU_FLIP_PRESSURE_SOLVE.md): the
masked Poisson equation L p = f on water cells, p = 0 on air, box walls
closed. One V-cycle per iteration: red-black Gauss-Seidel, 2 sweeps before
and 2 after, trilinear transfers, a coarse cell is air if any child is air.
The graph runs `--depth 5 --coarse-sweeps 16`: five levels at every lattice,
the coarsest smoothed by 16 rounds each way. Without them the levels halve
while every side is even and one is over 4 and the coarsest is solved
exactly by its inverse (node.coarse_inverse's sweep and pinning).

Usage: scripts/mgpcg_reference.py crates/manifold-renderer/tests/fixtures/dambreak_pressure_problems.bin.zst
           --depth 5 --coarse-sweeps 16 [--refine 2 | --coarsen 2] [--iterations 4,6,8] [--tol 1e-5]
       scripts/mgpcg_reference.py --frames DUMP.bin [...]
The second form reads solves dumped from a running scene (per record:
u32 frame, step, kind, n; then water, f and another solver's pressure as n³
f32 each, or zeros) and compares against that pressure's residual.
"""
import argparse
import struct
import subprocess

import numpy as np

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


def neighbour_sum(q):
    """Sum of the six face neighbours; past the box walls there are none."""
    pq = np.pad(q, 1)
    return (pq[:-2, 1:-1, 1:-1] + pq[2:, 1:-1, 1:-1] + pq[1:-1, :-2, 1:-1]
            + pq[1:-1, 2:, 1:-1] + pq[1:-1, 1:-1, :-2] + pq[1:-1, 1:-1, 2:])


class Level:
    def __init__(self, water, h):
        self.water = water
        self.w = water.astype(float)
        self.h = h
        self.count = neighbour_sum(np.ones_like(self.w))
        parity = np.indices(water.shape).sum(0) % 2
        self.colors = [(parity == 0) & water, (parity == 1) & water]

    def laplacian(self, p):
        q = p * self.w
        return (neighbour_sum(q) - self.count * q) / self.h**2 * self.w

    def residual(self, rhs, p):
        return (rhs - self.laplacian(p)) * self.w

    def sweep(self, p, rhs, color):
        new = (neighbour_sum(p * self.w) - self.h**2 * rhs) / self.count
        out = p.copy()
        out[self.colors[color]] = new[self.colors[color]]
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


def coarse_inverse(water):
    """A⁻¹ for L = −A / h² by the kernel's symmetric sweep: a pivot under
    1e-4 of its diagonal pins its cell (row and column cleared)."""
    count = neighbour_sum(np.ones(water.shape)).ravel()
    w = water.ravel()
    cells = w.size
    a = np.zeros((cells, cells))
    flat = np.arange(cells).reshape(water.shape)
    for ax in range(3):
        lo = np.take(flat, range(water.shape[ax] - 1), axis=ax).ravel()
        hi = np.take(flat, range(1, water.shape[ax]), axis=ax).ravel()
        both = w[lo] & w[hi]
        a[lo[both], hi[both]] = -1.0
        a[hi[both], lo[both]] = -1.0
    a[np.diag_indices(cells)] = np.where(w, count, 0.0)
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
    """`depth` fixes the level count (every side must halve evenly that many
    times less one); None halves while every side is even and one is over 4.
    `coarse_sweeps` solves the coarsest level by that many red-black rounds
    before and after (a symmetric smoother, so the preconditioner stays
    symmetric); None solves it exactly by its inverse."""

    def __init__(self, water, h, depth=None, coarse_sweeps=None):
        self.levels = [Level(water, h)]
        self.P = []
        def halve(shape):
            if depth is not None:
                return len(self.levels) < depth
            return max(shape) > 4 and all(n % 2 == 0 for n in shape)
        while halve(water.shape):
            nz, ny, nx = water.shape
            assert nz % 2 == ny % 2 == nx % 2 == 0, f"{water.shape} does not halve to {depth} levels"
            water = water.reshape(nz // 2, 2, ny // 2, 2, nx // 2, 2).all(axis=(1, 3, 5))
            h *= 2
            self.P.append([prolong_1d(n) for n in water.shape])
            self.levels.append(Level(water, h))
        self.coarse_sweeps = coarse_sweeps
        self.inverse = coarse_inverse(self.levels[-1].water) if coarse_sweeps is None else None

    def vcycle(self, l, r):
        lv = self.levels[l]
        e = np.zeros_like(r)
        last = l == len(self.levels) - 1
        if last and self.coarse_sweeps is None:
            return (-lv.h**2 * self.inverse @ r.ravel()).reshape(r.shape)
        rounds = self.coarse_sweeps if last else NU
        for _ in range(rounds):
            for color in (0, 1):
                e = lv.sweep(e, r, color)
        if not last:
            rc = apply_axes([M.T / 2.0 for M in self.P[l]], lv.residual(r, e)) * self.levels[l + 1].w
            e = e + apply_axes(self.P[l], self.vcycle(l + 1, rc)) * lv.w
        for _ in range(rounds):
            for color in (1, 0):
                e = lv.sweep(e, r, color)
        return e


def true_residual(p, water, f, h):
    lv = Level(water, h)
    return np.linalg.norm(lv.laplacian(p) - f * water) / np.linalg.norm(f * water)


def solve(water, f, h, iterations, depth=None, coarse_sweeps=None):
    """Residual after each iteration, as the graph's fixed-count loop runs."""
    mg = Multigrid(water, h, depth, coarse_sweeps)
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
        out.append(true_residual(x, water, f, h))
    return out, len(mg.levels)


def coarsen(water, f, k):
    """A k-times coarser problem: a cell is water when at least half its
    children are, and its divergence is their mean."""
    nz, ny, nx = water.shape
    shape = (nz // k, k, ny // k, k, nx // k, k)
    water = water.reshape(shape).mean(axis=(1, 3, 5)) >= 0.5
    f = f.reshape(shape).mean(axis=(1, 3, 5)) * water
    return water, f


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("fixture", nargs="?")
    ap.add_argument("--frames")
    ap.add_argument("--refine", type=int, default=1)
    ap.add_argument("--coarsen", type=int, default=1)
    ap.add_argument("--iterations", default="4,6,8")
    ap.add_argument("--depth", type=int, help="a fixed level count (default: halve down to 4)")
    ap.add_argument("--coarse-sweeps", type=int, help="red-black rounds on the coarsest level (default: its exact inverse)")
    ap.add_argument("--tol", type=float, help="also print the iterations the residual takes to reach this")
    args = ap.parse_args()
    counts = [int(k) for k in args.iterations.split(",")]
    probs = frames(args.frames) if args.frames else load(args.fixture)
    for name, water, f, old in probs:
        if args.refine > 1:
            water, f = refine(water, f, args.refine)
        if args.coarsen > 1:
            water, f = coarsen(water, f, args.coarsen)
        h = L / water.shape[0]
        res, levels = solve(water, f, h, max(counts), args.depth, args.coarse_sweeps)
        line = ", ".join(f"{k}: {res[k - 1]:.3e}" for k in counts)
        old_line = "" if old is None else f"; old solver {true_residual(old, water, f, h):.3e}"
        tol_line = ""
        if args.tol is not None:
            reached = next((k + 1 for k, r in enumerate(res) if r <= args.tol), None)
            tol_line = f"; {args.tol:.0e} after {reached if reached else f'> {len(res)}'}"
        print(f"{water.shape[0]}^3 {name}: {levels} levels; residual after {line}{old_line}{tol_line}", flush=True)


if __name__ == "__main__":
    main()
