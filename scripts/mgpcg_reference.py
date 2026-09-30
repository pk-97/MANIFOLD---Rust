#!/usr/bin/env python3
"""The GPU FLIP pressure solve in f64: the oracle for the pinned residuals in
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_solve_tests.rs.

Multigrid-preconditioned conjugate gradient (McAdams, Sifakis & Teran 2010),
step for step as the graph runs it (docs/GPU_FLIP_PRESSURE_SOLVE.md): the
masked Poisson equation L p = f on water cells, p = 0 on air, box walls
closed. One V-cycle per iteration: red-black Gauss-Seidel, 2 sweeps before
and 2 after, trilinear transfers, a coarse cell is air if any child is air,
halving while a side is even and over 8, then a fixed-sweep coarse solve.

Usage: scripts/mgpcg_reference.py crates/manifold-renderer/tests/fixtures/dambreak_pressure_problems.bin.zst
           [--refine 2] [--iterations 4,6,8] [--coarse-sweeps 16]
       scripts/mgpcg_reference.py --frames DUMP.bin [...]
The second form reads solves dumped from a running Dam Break (per record:
u32 frame, step, kind, n; then water, f and the old solver's pressure as n³
f32 each) and compares against the dumped pressure's residual.
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


class Multigrid:
    def __init__(self, water, h, coarse_sweeps):
        self.levels = [Level(water, h)]
        self.P = []
        self.coarse_sweeps = coarse_sweeps
        while max(water.shape) > 8 and all(n % 2 == 0 for n in water.shape):
            nz, ny, nx = water.shape
            water = water.reshape(nz // 2, 2, ny // 2, 2, nx // 2, 2).all(axis=(1, 3, 5))
            h *= 2
            self.P.append([prolong_1d(n) for n in water.shape])
            self.levels.append(Level(water, h))

    def vcycle(self, l, r):
        lv = self.levels[l]
        e = np.zeros_like(r)
        if l == len(self.levels) - 1:
            for order in ((0, 1), (1, 0)):
                for _ in range(self.coarse_sweeps):
                    for color in order:
                        e = lv.sweep(e, r, color)
            return e
        for _ in range(NU):
            for color in (0, 1):
                e = lv.sweep(e, r, color)
        rc = apply_axes([M.T / 2.0 for M in self.P[l]], lv.residual(r, e)) * self.levels[l + 1].w
        e = e + apply_axes(self.P[l], self.vcycle(l + 1, rc)) * lv.w
        for _ in range(NU):
            for color in (1, 0):
                e = lv.sweep(e, r, color)
        return e


def true_residual(p, water, f, h):
    lv = Level(water, h)
    return np.linalg.norm(lv.laplacian(p) - f * water) / np.linalg.norm(f * water)


def solve(water, f, h, iterations, coarse_sweeps):
    """Residual after each iteration, as the graph's fixed-count loop runs."""
    mg = Multigrid(water, h, coarse_sweeps)
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("fixture", nargs="?")
    ap.add_argument("--frames")
    ap.add_argument("--refine", type=int, default=1)
    ap.add_argument("--iterations", default="4,6,8")
    ap.add_argument("--coarse-sweeps", type=int, default=16)
    args = ap.parse_args()
    counts = [int(k) for k in args.iterations.split(",")]
    probs = frames(args.frames) if args.frames else load(args.fixture)
    for name, water, f, old in probs:
        if args.refine > 1:
            water, f = refine(water, f, args.refine)
        h = L / water.shape[0]
        res, levels = solve(water, f, h, max(counts), args.coarse_sweeps)
        line = ", ".join(f"{k}: {res[k - 1]:.3e}" for k in counts)
        old_line = "" if old is None else f"; old solver {true_residual(old, water, f, h):.3e}"
        print(f"{water.shape[0]}^3 {name}: {levels} levels; residual after {line}{old_line}", flush=True)


if __name__ == "__main__":
    main()
