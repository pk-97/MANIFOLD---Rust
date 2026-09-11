//! Binding-introspection contract tests (BUG-uwgn cluster 4 prerequisite).
//!
//! Every standalone-codegen kernel's declared binding list, reflected through
//! naga, is the one half of the dispatch-tail contract; the primitive's
//! `run()` call site builds the other half inline. The cluster-4 dispatch-tail
//! codemod rewrites call sites to a shared helper, and this census is the
//! independent oracle defining which atoms qualify: the codemod may touch a
//! call site ONLY when the kernel's reflected signature is the canonical
//! texture-path shape — `uniform(0)`, zero or more sampled input textures,
//! at most one sampler, NO storage buffers, exactly one 2D storage-write
//! texture as the last binding. Anything else keeps its hand-written tail
//! and is listed in the census output for review.

use crate::node_graph::freeze::codegen;

/// Reflected binding-resource kinds, in the call site's `GpuBinding` terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    Uniform,
    StorageRead,
    StorageReadWrite,
    TexSampled2d,
    TexSampled3d,
    TexStorageWrite2d,
    TexStorageWrite3d,
    Sampler,
}

fn reflect_bindings(wgsl: &str) -> Vec<(u32, Res)> {
    let module = naga::front::wgsl::parse_str(wgsl).expect("generated kernel must parse");
    let mut out: Vec<(u32, Res)> = module
        .global_variables
        .iter()
        .map(|(_, gv)| {
            let binding = gv.binding.as_ref().expect("kernel globals are bound").binding;
            let ty = &module.types[gv.ty];
            let res = match (&gv.space, &ty.inner) {
                (naga::AddressSpace::Uniform, _) => Res::Uniform,
                (naga::AddressSpace::Storage { access }, _) => {
                    if access.contains(naga::StorageAccess::STORE) {
                        Res::StorageReadWrite
                    } else {
                        Res::StorageRead
                    }
                }
                (
                    _,
                    naga::TypeInner::Image {
                        dim,
                        class: naga::ImageClass::Storage { access, .. },
                        ..
                    },
                ) => {
                    assert!(
                        access.contains(naga::StorageAccess::STORE),
                        "storage image without STORE access"
                    );
                    match dim {
                        naga::ImageDimension::D2 => Res::TexStorageWrite2d,
                        naga::ImageDimension::D3 => Res::TexStorageWrite3d,
                        other => panic!("unexpected storage image dim {other:?}"),
                    }
                }
                (_, naga::TypeInner::Image { dim, .. }) => match dim {
                    naga::ImageDimension::D2 => Res::TexSampled2d,
                    naga::ImageDimension::D3 => Res::TexSampled3d,
                    other => panic!("unexpected sampled image dim {other:?}"),
                },
                (_, naga::TypeInner::Sampler { .. }) => Res::Sampler,
                (space, inner) => panic!("unclassified global: {space:?} {inner:?}"),
            };
            (binding, res)
        })
        .collect();
    out.sort_by_key(|&(b, _)| b);
    out
}

/// The canonical texture-path signature the cluster-4 codemod may rewrite:
/// uniform(0), sampled textures, optional single sampler, one 2D storage
/// texture out last. No storage buffers, no 3D, no multi-output.
fn qualifies_for_dispatch_tail(sig: &[(u32, Res)]) -> bool {
    let Some(&(0, Res::Uniform)) = sig.first() else {
        return false;
    };
    let Some(&(last_b, Res::TexStorageWrite2d)) = sig.last() else {
        return false;
    };
    let middle = &sig[1..sig.len() - 1];
    let samplers = middle.iter().filter(|&&(_, r)| r == Res::Sampler).count();
    samplers <= 1
        && middle
            .iter()
            .all(|&(_, r)| matches!(r, Res::TexSampled2d | Res::TexSampled3d | Res::Sampler))
        && last_b as usize == sig.len() - 1
}

type CensusRow = (String, Vec<(u32, Res)>, bool);

fn census() -> Vec<CensusRow> {
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let mut rows = Vec::new();
    for id in registry.known_type_ids() {
        let Some(node) = registry.construct(id) else {
            continue;
        };
        let Ok(wgsl) = codegen::standalone_for_node(node.as_ref()) else {
            continue; // NoBody / AtomicNonInteger / Boundary — not standalone atoms
        };
        // Specialization tokens (QUALITY_LEVEL, WEIGHTING_MODE, …) resolve at
        // pipeline-compile time from param values; the binding surface doesn't
        // depend on them, so a dummy literal suffices for the parse.
        let mut wgsl = wgsl;
        for (token, _) in node.wgsl_specialization() {
            wgsl = wgsl.replace(token, "1u");
        }
        let sig = reflect_bindings(&wgsl);
        let qualifies = qualifies_for_dispatch_tail(&sig);
        rows.push((id.to_string(), sig, qualifies));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

#[test]
fn standalone_kernels_have_contiguous_bindings_from_zero() {
    for (id, sig, _) in census() {
        for (i, &(b, _)) in sig.iter().enumerate() {
            assert_eq!(b as usize, i, "{id}: binding list has a gap or duplicate");
        }
    }
}

#[test]
fn dispatch_tail_census_is_stable() {
    let rows = census();
    let qualifying = rows.iter().filter(|r| r.2).count();
    let total = rows.len();
    eprintln!("── dispatch-tail census ({total} standalone atoms, {qualifying} canonical) ──");
    for (id, sig, q) in &rows {
        let shape: Vec<String> = sig.iter().map(|(b, r)| format!("{b}:{r:?}")).collect();
        eprintln!("{} [{}] {}", if *q { "QUALIFY" } else { "manual " }, shape.join(" "), id);
    }
    // Ratchet: the codemod's qualifying set (measured 2026-09-11: 172
    // standalone atoms, 89 canonical texture-path). A new atom landing here is
    // fine — it gets the helper by construction — but a drop means an atom's
    // kernel shape drifted out from under already-rewritten call sites.
    // Update the numbers with the reason named in the commit that changes
    // them.
    // BUG-m3af restores six mixed texture/storage draw nodes to dynamic codegen.
    // BUG-e3p6 adds copy_positions, wave_field_3d, and displace_copies.
    // Photoscan slice adds wave_shear_mesh and transform_mesh_patches.
    assert_eq!(total, 172, "standalone atom census drifted");
    assert_eq!(
        qualifying, 89,
        "canonical texture-path population drifted"
    );
}

/// The other half of the contract: for every canonical atom, the helper's
/// slot sequence IS the kernel's reflected binding sequence — so a call site
/// built through `dispatch_standalone_2d` binds in the order the kernel
/// declares, by construction. This is what makes the cluster-4 codemod an
/// order-preserving rewrite rather than a leap of faith per site.
#[test]
fn canonical_kernels_match_dispatch_helper_slot_order() {
    use crate::node_graph::primitives::standalone_pipeline::{
        standalone_2d_slots, StandaloneSlot, STANDALONE_2D_MAX_BINDINGS,
    };
    for (id, sig, qualifies) in census() {
        if !qualifies {
            continue;
        }
        assert!(sig.len() <= STANDALONE_2D_MAX_BINDINGS, "{id}: kernel exceeds stack binding capacity");
        let n_textures = sig
            .iter()
            .filter(|&&(_, r)| matches!(r, Res::TexSampled2d | Res::TexSampled3d))
            .count();
        let has_sampler = sig.iter().any(|&(_, r)| r == Res::Sampler);
        let expected: Vec<StandaloneSlot> = sig
            .iter()
            .map(|&(_, r)| match r {
                Res::Uniform => StandaloneSlot::Uniform,
                Res::TexSampled2d | Res::TexSampled3d => StandaloneSlot::TexIn,
                Res::Sampler => StandaloneSlot::Sampler,
                Res::TexStorageWrite2d => StandaloneSlot::TexOut,
                other => panic!("{id}: canonical atom carries {other:?}"),
            })
            .collect();
        assert_eq!(
            standalone_2d_slots(n_textures, has_sampler),
            expected,
            "{id}: helper slot order disagrees with the kernel"
        );
    }
}
