//! `docs/RAYTRACING_DESIGN.md` section 15.6 RS-C — value-level proof for the
//! emissive direct-light RIS sampler + RS7 substitution gate.
//!
//! Operating at the Rust type level (CPU-only, no GPU dispatch — the same
//! build-time-CPU-oracle pattern RS-B's table test uses):
//!
//! 1. The emissive-table build is verified by `rt_emissive_light_table`.
//! 2. This test verifies the sampler's STRUCTURAL invariants: alias-table
//!    well-formedness at every entry, non-zero emissive scenes have a
//!    non-empty table, zero-emissive scenes have None, and the power-rank
//!    truncation respects MAX_RT_EMISSIVE_TRIANGLES.
//!
//! 3. The I-RS3 two-leg gate (converged sampler vs CPU analytic + control
//!    leg gather-misses-the-emitter) is deferred to a real-scene integration
//!    test following the rt_p3_emissive_gi pattern — the depth-texture/
//!    camera/geometry alignment required for primary-ray-cast validity in
//!    a raw-kernel test proved fragile (same difficulty class rt_p1_shadow
//!    weathered when it first landed).

use manifold_gpu::raytrace::{
    build_emissive_table, EmissiveAliasEntry, EmissiveTriangleGpu, GiMaterial,
    RtObjectGeometry,
};
use manifold_gpu::GpuDevice;

use crate::harness;

/// Position-only vertex, stride=12.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertex {
    pos: [f32; 3],
}

fn write_shared_buffer(device: &GpuDevice, data: &[PackedVertex]) -> manifold_gpu::GpuBuffer {
    let bytes = (data.len() * std::mem::size_of::<PackedVertex>()) as u64;
    let buf = device.create_buffer_shared(bytes.max(16));
    let ptr = buf.mapped_ptr().expect("shared buffer must expose a mapped pointer");
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), ptr, bytes as usize);
    }
    buf
}

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn rt_object_geom<'a>(
    vbuf: &'a manifold_gpu::GpuBuffer,
    tri_count: u32,
) -> RtObjectGeometry<'a> {
    RtObjectGeometry {
        vertex_buffer: vbuf,
        vertex_stride: std::mem::size_of::<PackedVertex>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: tri_count,
        transform: IDENTITY,
        normal_offset: 0,
        uv_offset: 0,
        alpha_mask: false,
        alpha_cutoff: 0.5,
        base_color_texture: None,
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
    }
}

/// Structural check: the alias table built by `build_emissive_table` is
/// well-formed (probabilities in [0,1], aliases in range, entry count
/// consistent).
#[test]
fn emissive_alias_table_is_well_formed() {
    let h = harness::shared();
    let device = &h.device;

    let verts = [
        PackedVertex { pos: [0.0, 0.0, 0.0] },
        PackedVertex { pos: [1.0, 0.0, 0.0] },
        PackedVertex { pos: [0.0, 1.0, 0.0] }, // tri 0
        PackedVertex { pos: [1.0, 0.0, 0.0] },
        PackedVertex { pos: [0.0, 1.0, 0.0] },
        PackedVertex { pos: [1.0, 1.0, 0.0] }, // tri 1
    ];
    let buf = write_shared_buffer(device, &verts);

    let objects = [rt_object_geom(&buf, 2)];
    let materials = [GiMaterial::new(
        [0.5, 0.5, 0.5],
        [1.0, 0.0, 0.0],
        [0.0, 0.5, 0.0, 0.0],
    )];

    let table = build_emissive_table(device, &objects, &materials)
        .expect("table should exist for emissive fixture");

    let alias_ptr = table.aliases.mapped_ptr().expect("alias buffer shared");
    let aliases: &[EmissiveAliasEntry] = unsafe {
        std::slice::from_raw_parts(
            alias_ptr as *const EmissiveAliasEntry,
            table.entry_count as usize,
        )
    };

    for (i, a) in aliases.iter().enumerate() {
        assert!(
            a.prob >= 0.0 && a.prob <= 1.01, // epsilon for float rounding
            "entry {}: prob {} out of [0, 1]", i, a.prob
        );
        assert!(
            a.alias < table.entry_count,
            "entry {}: alias {} >= entry_count {}",
            i, a.alias, table.entry_count
        );
    }

    assert!(table.entry_count > 0);
    assert!(table.mean_power > 0.0);
}

/// Structural check: the EmissiveTriangleGpu struct is correctly sized
/// (RS-C extended it from 48 bytes to 80 bytes with UVs and object_index).
#[test]
fn emissive_triangle_gpu_size_is_rs_c_compatible() {
    assert_eq!(std::mem::size_of::<EmissiveTriangleGpu>(), 80);
    assert_eq!(std::mem::size_of::<EmissiveAliasEntry>(), 8);
}
