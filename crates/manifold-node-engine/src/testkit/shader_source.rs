// Preserve the dense wrapper while keeping its ABI in lockstep with the
// scheduled node. Optional solid input is bound by the generated wrapper;
// no-solid fixtures pass zero solid dimensions so the constraint is inert.
pub fn dense_source<P: crate::primitive::PrimitiveSpec>(original: &str) -> String {
    use crate::freeze::codegen::{StandaloneKernelSpec, generate_standalone};
    let mut body = original.to_owned();
    let start = body.find("fn body(").unwrap();
    let end = start + body[start..].find(") ->").unwrap();
    let comma = if body[..end].trim_end().ends_with(',') {
        ""
    } else {
        ","
    };
    let derived = P::DERIVED_UNIFORMS.join(", ");
    body.insert_str(end, &format!("{comma} {derived}"));
    let includes: Vec<_> = P::WGSL_INCLUDES
        .iter()
        .copied()
        .filter(|include| {
            !P::DENSE_BUFFER_FUSION.is_some_and(|dense| dense.body_fragments.contains(include))
        })
        .collect();
    // The retained dense oracle emits only the original triangle-list output.
    let outputs: Vec<_> = P::OUTPUTS.iter().filter(|output| output.name != "indices").cloned().collect();
    generate_standalone(&StandaloneKernelSpec {
        fusion_kind: P::FUSION_KIND,
        body: &body,
        inputs: P::INPUTS,
        params: P::PARAMS,
        input_access: P::INPUT_ACCESS,
        derived_uniforms: P::DERIVED_UNIFORMS,
        outputs: &outputs,
        stencil_fetch: P::STENCIL_FETCH,
        includes: &includes,
    })
    .unwrap()
}
