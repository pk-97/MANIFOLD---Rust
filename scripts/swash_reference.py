#!/usr/bin/env python3
"""SWASH collar pressure solve in f64: the oracle for the pinned residuals in
crates/manifold-renderer/src/node_graph/primitives/swash_solve_tests.rs.

The same algorithm as the engine (docs/FFT_WATER_SOLVER_DESIGN.md D3, D4, D10,
D11): air removed, collar sources plus one constant, whole-box cosine solve,
six-view surface helper, fixed-pass right-preconditioned GMRES (classical
Gram-Schmidt twice, Givens). The defaults are the engine's choices; the other
flag values are the variants measured against the MLX record in BUG-wsim
(FFT pressure split research), kept so the comparison can be rerun.

Usage: scripts/swash_reference.py crates/manifold-renderer/tests/fixtures/swash_dambreak_problems.bin.zst
       [--refine 2] [--passes 12,16,24,32] [--helper charts|column]
       [--smooth 3] [--sheets runs-1|runs] [--weights abs|square] [--counts all|weighted]
"""
import argparse
import struct
import subprocess
import time

import numpy as np
import scipy.fft as sf

L = 4.0


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
        probs.append((frame, water, f.reshape(nz, ny, nx)))
    return probs


def refine(water, f, k):
    for ax in range(3):
        water = np.repeat(water, k, axis=ax)
        f = np.repeat(f, k, axis=ax)
    return water, f


def shift(a, axis, s, fill=0):
    out = np.full_like(a, fill)
    src = [slice(None)] * 3
    dst = [slice(None)] * 3
    if s > 0:
        src[axis] = slice(0, -s)
        dst[axis] = slice(s, None)
    else:
        src[axis] = slice(-s, None)
        dst[axis] = slice(0, s)
    out[tuple(dst)] = a[tuple(src)]
    return out


class Box:
    def __init__(self, shape, h):
        lam = [-(2 - 2 * np.cos(np.pi * np.arange(n) / n)) / h**2 for n in shape]
        LAM = lam[0][:, None, None] + lam[1][None, :, None] + lam[2][None, None, :]
        self.inv = np.where(LAM == 0, 0.0, 1.0 / np.where(LAM == 0, 1.0, LAM))

    def solve(self, f):
        return sf.idctn(sf.dctn(f, type=2) * self.inv, type=2)


def masked_apply(p, water, h):
    q = p * water
    out = np.zeros_like(q)
    ones = np.ones_like(q)
    for ax in range(3):
        for s in (1, -1):
            out += shift(q, ax, s) - q * shift(ones, ax, s)
    return out / h**2 * water


def smooth_axis(a, axis, p):
    """node.smooth_lattice: (2p+1)-tap binomial along one axis, clamped indices."""
    if p == 0:
        return a
    from math import comb
    w = np.array([comb(2 * p, k) for k in range(2 * p + 1)], float) / 4**p
    n = a.shape[axis]
    out = np.zeros_like(a)
    for d in range(-p, p + 1):
        idx = np.clip(np.arange(n) + d, 0, n - 1)
        out += w[d + p] * np.take(a, idx, axis=axis)
    return out


class Charts:
    """Six signed views x NL sheets of M x M planes (numpy axis 2 = engine x)."""

    def __init__(self, water, collar, h, args):
        nz, ny, nx = water.shape
        self.h = h
        self.NL = args.sheets_max
        M = max(water.shape)
        self.M = M
        cz, cy, cx = np.nonzero(collar)  # engine linear order = C order of [z, y, x]
        self.cells = (cz, cy, cx)
        n = len(cz)
        phi = water.astype(float)
        for p in args.smooth:
            for ax in range(3):
                phi = smooth_axis(phi, ax, p)
        grad = []
        for ax_np in (2, 1, 0):  # engine x, y, z
            size = water.shape[ax_np]
            ip = np.clip(np.arange(size) + 1, 0, size - 1)
            im = np.clip(np.arange(size) - 1, 0, size - 1)
            g = (np.take(phi, ip, axis=ax_np) - np.take(phi, im, axis=ax_np)) * 0.5
            grad.append(g[cz, cy, cx])
        grad = np.stack(grad, 1)
        norm = np.linalg.norm(grad, axis=1)
        normal = np.where(norm[:, None] > 1e-12, -grad / np.maximum(norm, 1e-12)[:, None], 0.0)
        engine = [cx, cy, cz]
        self.slot = np.zeros((n, 6), np.int64)
        self.w = np.zeros((n, 6))
        for a in range(3):
            ax_np = 2 - a
            starts = water & ~shift(water, ax_np, 1, fill=False)
            cs = np.cumsum(starts, axis=ax_np)
            total = np.take(cs, [water.shape[ax_np] - 1], axis=ax_np)
            before = cs[cz, cy, cx]
            after = np.broadcast_to(total, water.shape)[cz, cy, cx] - before
            others = [b for b in range(3) if b != a]
            first, second = engine[others[0]], engine[others[1]]
            for sgn, runs in ((0, before), (1, after)):
                v = 2 * a + sgn
                if args.sheets == "runs-1":
                    sheet = np.clip(runs - 1, 0, self.NL - 1)
                else:
                    sheet = np.minimum(runs, self.NL - 1)
                comp = normal[:, a] if sgn == 0 else -normal[:, a]
                comp = np.maximum(comp, 0.0)
                self.w[:, v] = comp**2 if args.weights == "square" else comp
                self.slot[:, v] = ((v * self.NL + sheet) * M + second) * M + first
        planes = 6 * self.NL * M * M
        self.D = np.zeros(planes)
        for v in range(6):
            np.add.at(self.D, self.slot[:, v], 1.0 if args.counts == "all" else self.w[:, v])
        self.rs = 1.0 / np.sqrt(np.maximum(self.D, 1e-30 if args.counts != "all" else 1.0))
        self.rs[self.D == 0] = 0.0
        k = np.arange(M)
        s = np.sin(np.pi * k / (2 * M)) ** 2
        Q = np.sqrt(4 * (s[:, None] + s[None, :]) / h**2 + (2 * np.pi / L) ** 2)
        self.symbol = Q - 2.0 / h
        self.n = n

    def apply(self, x):
        lam, c = x[:-1], x[-1]
        S = np.zeros(6 * self.NL * self.M * self.M)
        for v in range(6):
            np.add.at(S, self.slot[:, v], self.w[:, v] * lam)
        S *= self.rs
        P = S.reshape(6 * self.NL, self.M, self.M)
        T = sf.idctn(sf.dctn(P, type=2, axes=(1, 2)) * self.symbol, type=2, axes=(1, 2)).ravel()
        T *= self.rs
        out = (2.0 / self.h) * lam
        for v in range(6):
            out += self.w[:, v] * T[self.slot[:, v]]
        return np.concatenate([out, [c]])


class Column:
    """The one-view column helper of dambreak_mlx.py (vertical = engine y)."""

    def __init__(self, water, collar, h):
        self.C = collar.astype(float)
        self.h = h
        self.cells = np.nonzero(collar)
        self.D = np.maximum(self.C.sum(axis=1), 1.0)  # [z, x]
        nz, ny, nx = water.shape
        lz = -(2 - 2 * np.cos(np.pi * np.arange(nz) / nz)) / h**2
        lx = -(2 - 2 * np.cos(np.pi * np.arange(nx) / nx)) / h**2
        self.QS = np.sqrt(-(lz[:, None] + lx[None, :]) + (2 * np.pi / L) ** 2)

    def apply(self, x):
        lam, c = x[:-1], x[-1]
        g = np.zeros(self.C.shape)
        g[self.cells] = lam
        cs = g.sum(axis=1)
        sc = sf.idctn(sf.dctn(cs, type=2) * self.QS, type=2)
        out = self.C * (sc / self.D)[:, None, :] + (2.0 / self.h) * self.C * (g - (cs / self.D)[:, None, :])
        return np.concatenate([out[self.cells], [c]])


def solve(water, f, h, passes, args):
    n3 = water.size
    box = Box(water.shape, h)
    collar = np.zeros_like(water)
    for ax in range(3):
        collar |= shift(water, ax, 1, fill=False) | shift(water, ax, -1, fill=False)
    collar &= ~water
    helper = Charts(water, collar, h, args) if args.helper == "charts" else Column(water, collar, h)
    cells = np.nonzero(collar)

    def A(x):
        g = np.zeros(water.shape)
        g[cells] = x[:-1]
        p = box.solve(g)
        return np.concatenate([p[cells] - x[-1], [x[:-1].sum() / n3]])

    Gf = box.solve(f)
    b = np.concatenate([Gf[cells], [f.sum() / n3]])
    beta = np.linalg.norm(b)
    V = [b / beta]
    m = passes
    H = np.zeros((m + 1, m))
    cs = np.zeros(m)
    sn = np.zeros(m)
    gv = np.zeros(m + 1)
    gv[0] = beta
    for j in range(m):
        w = A(helper.apply(V[j]))
        col = np.zeros(j + 2)
        for _ in range(2):
            proj = np.array([w @ v for v in V])
            col[: j + 1] += proj
            for i, v in enumerate(V):
                w = w - proj[i] * v
        nw = np.linalg.norm(w)
        col[j + 1] = nw
        V.append(w / nw if nw > 1e-30 else np.zeros_like(w))
        for i in range(j):
            t = cs[i] * col[i] + sn[i] * col[i + 1]
            col[i + 1] = -sn[i] * col[i] + cs[i] * col[i + 1]
            col[i] = t
        r = np.hypot(col[j], col[j + 1])
        cs[j], sn[j] = (col[j] / r, col[j + 1] / r) if r > 1e-30 else (1.0, 0.0)
        col[j] = r
        col[j + 1] = 0.0
        gv[j + 1] = -sn[j] * gv[j]
        gv[j] *= cs[j]
        H[: j + 1, j] = col[: j + 1]
    y = np.zeros(m)
    for k in range(m - 1, -1, -1):
        acc = gv[k] - H[k, k + 1 :] @ y[k + 1 :]
        y[k] = acc / H[k, k] if abs(H[k, k]) > 1e-30 else 0.0
    u = sum(y[j] * V[j] for j in range(m))
    x = helper.apply(u)
    g = np.zeros(water.shape)
    g[cells] = x[:-1]
    p = (Gf - box.solve(g) + x[-1]) * water
    resid = np.linalg.norm(masked_apply(p, water, h) - f) / np.linalg.norm(f)
    return resid, abs(gv[m]) / beta, len(cells[0])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("fixture")
    ap.add_argument("--refine", type=int, default=1)
    ap.add_argument("--passes", type=str, default="24")
    ap.add_argument("--helper", default="charts")
    ap.add_argument("--smooth", type=lambda s: [int(v) for v in s.split(",") if v], default=[3])
    ap.add_argument("--sheets", default="runs-1")
    ap.add_argument("--sheets-max", type=int, default=4)
    ap.add_argument("--weights", default="abs")
    ap.add_argument("--counts", default="all")
    args = ap.parse_args()
    probs = load(args.fixture)
    for passes in [int(p) for p in args.passes.split(",")]:
        rs = []
        for frame, water, f in probs:
            if args.refine > 1:
                water, f = refine(water, f, args.refine)
            h = L / water.shape[0]
            t0 = time.time()
            r, est, ncol = solve(water, f, h, passes, args)
            rs.append(r)
            print(f"  frame {frame:3d}: collar {ncol:6d}, true residual {r:.4e}, GMRES estimate {est:.3e} ({time.time()-t0:.1f} s)", flush=True)
        print(f"{args.helper} passes {passes} refine {args.refine}: median {np.median(rs):.3e} max {max(rs):.3e}", flush=True)


if __name__ == "__main__":
    main()
