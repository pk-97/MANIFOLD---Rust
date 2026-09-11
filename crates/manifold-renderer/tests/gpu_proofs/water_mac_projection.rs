//! Native production pressure operators versus the independent f64 fractional oracle.
use crate::harness;
use bytemuck::Pod;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_renderer::node_graph::primitives::{
    mac_pressure_iteration as iteration, mac_projection as atoms, mac_resolve::MacResolvedCell,
};
#[path = "../water_fractional_projection_reference.rs"]
mod reference;
const N: usize = 64;
const CELLS: usize = N * N * N;
const PAD: usize = 65 * 65 * 65;
const H: f64 = 0.0625;
fn padded(c: [usize; 3]) -> usize {
    c[0] + 65 * (c[1] + 65 * c[2])
}
fn buffer<T: Pod>(d: &GpuDevice, data: &[T]) -> GpuBuffer {
    let b = d.create_buffer_shared(std::mem::size_of_val(data) as u64);
    unsafe {
        b.write(0, bytemuck::cast_slice(data));
    }
    b
}
fn read<T: Pod>(b: &GpuBuffer, n: usize) -> Vec<T> {
    unsafe { std::slice::from_raw_parts(b.mapped_ptr().unwrap().cast::<T>(), n).to_vec() }
}
fn dispatch(d: &GpuDevice, p: &GpuComputePipeline, u: &[u8], buffers: &[&GpuBuffer], n: u32) {
    let mut bindings = vec![GpuBinding::Bytes {
        binding: 0,
        data: u,
    }];
    bindings.extend(buffers.iter().enumerate().map(|(i, b)| GpuBinding::Buffer {
        binding: i as u32 + 1,
        buffer: b,
        offset: 0,
    }));
    let mut enc = d.create_encoder("mac-pressure-proof");
    enc.dispatch_compute(p, &bindings, [n.div_ceil(256), 1, 1], "mac-pressure-proof");
    enc.commit_and_wait_completed();
}
#[test]
fn native_fractional_rows_pressure_and_gradient_match_f64_and_reject_unsolved() {
    let d = harness::shared().device.as_ref();
    let mut g = reference::Grid::new([N; 3], H);
    g.phi.fill(H);
    for a in 0..3 {
        g.open[a].fill(0.0);
    }
    // A free surface above a small pool, fractional areas near an obstacle.
    for z in 16..24 {
        for y in 16..24 {
            for x in 16..24 {
                let i = g.cell([x, y, z]);
                g.phi[i] = -0.3 * H;
            }
        }
    }
    let mut v = std::array::from_fn::<_, 3, _>(|a| vec![0.0; g.open[a].len()]);
    for a in 0..3 {
        let dims = g.dims(a);
        for z in 16..=24 {
            for y in 16..=24 {
                for x in 16..=24 {
                    let c = [x, y, z];
                    if (0..3).any(|k| k != a && c[k] >= 24) {
                        continue;
                    }
                    if c[a] == 16 || (c[a] == 24 && a != 1) {
                        continue;
                    }
                    let i = g.face(a, c);
                    g.open[a][i] = if x == 20 && (19..22).contains(&z) {
                        0.37
                    } else {
                        1.0
                    };
                    v[a][i] = if a == 1 {
                        -0.08175
                    } else {
                        0.03 * ((x * 17 + y * 7 + z * 3 + a) % 11) as f64
                    };
                }
            }
        }
        assert_eq!(v[a].len(), dims.iter().product::<usize>());
    }
    let system = g.assemble(&v).unwrap();
    let mut expected = v.clone();
    let solved = g.project(&mut expected, 1e-10, 512).unwrap();
    assert!(solved.residual < 1e-9);
    assert!(solved.iterations > 0);
    let mut geometry = vec![[0.0f32; 4]; PAD];
    let mut grid = vec![
        MacResolvedCell {
            mac_velocity: [0.; 4],
            mac_valid: [0.; 4]
        };
        PAD
    ];
    for a in 0..3 {
        let dims = g.dims(a);
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let c = [x, y, z];
                    let j = padded(c);
                    let i = g.face(a, c);
                    geometry[j][a] = g.open[a][i] as f32;
                    geometry[j][3] = 1.;
                    grid[j].mac_velocity[a] = v[a][i] as f32;
                    grid[j].mac_valid[a] = 1.;
                }
            }
        }
    }
    let phi = buffer(d, &g.phi.iter().map(|x| *x as f32).collect::<Vec<_>>());
    let geom = buffer(d, &geometry);
    let velocities = buffer(d, &grid);
    let rows = d.create_buffer_shared((CELLS * 32) as u64);
    let pressure = d.create_buffer_shared((CELLS * 4) as u64);
    let status = d.create_buffer_shared(4);
    let projected = d.create_buffer_shared((PAD * 32) as u64);
    pressure.zero_fill();
    status.zero_fill();
    let rowpipe = d.create_compute_pipeline(
        &atoms::mac_pressure_rows_source(),
        "cs_main",
        "proof.mac-rows",
    );
    let relax = d.create_compute_pipeline(iteration::WGSL, "cs_relax", "proof.mac-relax");
    let check = d.create_compute_pipeline(iteration::WGSL, "cs_validate", "proof.mac-residual");
    let gradient = d.create_compute_pipeline(
        &atoms::mac_pressure_gradient_source(),
        "cs_main",
        "proof.mac-gradient",
    );
    let count = atoms::MacCountUniforms {
        dispatch_count: CELLS as u32,
        pad: [0; 3],
    };
    dispatch(
        d,
        &rowpipe,
        bytemuck::bytes_of(&count),
        &[&phi, &geom, &velocities, &rows],
        CELLS as u32,
    );
    let actual = read::<atoms::MacPressureRow>(&rows, CELLS);
    for (ri, &ci) in system.cells.iter().enumerate() {
        let r = actual[ci];
        let diag = system.rows[ri].iter().find(|(j, _)| *j == ri).unwrap().1 * H * H;
        assert!(
            (r.mac_lower_diag[3] as f64 - diag).abs() < 2e-6,
            "diag at {ci}"
        );
        assert!(
            (r.mac_upper_rhs[3] as f64 - system.rhs[ri] * H * H).abs() < 2e-7,
            "rhs at {ci}"
        );
    }
    let u = iteration::MacIterationUniforms {
        parity: 0,
        omega: 1.7,
        absolute_tolerance: 0.001,
        relative_tolerance: 0.0001,
    };
    dispatch(
        d,
        &check,
        bytemuck::bytes_of(&u),
        &[&rows, &pressure, &status],
        CELLS as u32,
    );
    assert_eq!(
        read::<u32>(&status, 1)[0],
        32,
        "unsolved negative control must be rejected"
    );
    status.zero_fill();
    // Fixed bounded graph schedule. Residual acceptance, not iteration count,
    // decides whether this projection may be committed.
    let mut enc = d.create_encoder("mac-pressure-128-sweeps");
    for _ in 0..128 {
        for parity in 0..2 {
            let u = iteration::MacIterationUniforms { parity, ..u };
            enc.dispatch_compute(
                &relax,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&u),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &rows,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &pressure,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: &status,
                        offset: 0,
                    },
                ],
                [1024, 1, 1],
                "mac-pressure-sweep",
            );
        }
    }
    enc.commit_and_wait_completed();
    dispatch(
        d,
        &check,
        bytemuck::bytes_of(&u),
        &[&rows, &pressure, &status],
        CELLS as u32,
    );
    assert_eq!(read::<u32>(&status, 1)[0], 0);
    let q = read::<f32>(&pressure, CELLS);
    for &i in &system.cells {
        assert!(
            (q[i] as f64 - solved.q[i]).abs() < 3e-5,
            "pressure {i}: {} vs {}",
            q[i],
            solved.q[i]
        );
    }
    let count = atoms::MacCountUniforms {
        dispatch_count: PAD as u32,
        pad: [0; 3],
    };
    dispatch(
        d,
        &gradient,
        bytemuck::bytes_of(&count),
        &[&velocities, &geom, &phi, &pressure, &projected],
        PAD as u32,
    );
    let actual = read::<MacResolvedCell>(&projected, PAD);
    for (a, expected_axis) in expected.iter().enumerate() {
        let dims = g.dims(a);
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let c = [x, y, z];
                    let i = g.face(a, c);
                    let e = expected_axis[i];
                    assert!(
                        (actual[padded(c)].mac_velocity[a] as f64 - e).abs() < 0.0005,
                        "gradient axis{a} at{c:?}"
                    );
                }
            }
        }
    }
}
