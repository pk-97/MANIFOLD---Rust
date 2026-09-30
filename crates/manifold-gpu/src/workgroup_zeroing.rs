//! Zeroing of WGSL workgroup memory, spread across the workgroup.
//!
//! WGSL requires every `var<workgroup>` to start each workgroup at zero.
//! naga's SPIR-V polyfill has invocation 0 store each whole variable while
//! the rest wait at a barrier; SPIRV-Cross turns that into one thread copying
//! a zero array out of constant memory, element by element. A 16 KB array
//! costs about 25 µs per workgroup that way. This pass writes the same zeros
//! from every invocation instead: element `i` of each variable is stored by
//! invocation `i mod threads`, then one workgroup barrier, as naga IR at the
//! top of each compute entry point. `compile_to_optimized_spirv` applies it
//! and turns naga's own zeroing off, so the two never disagree.
//!
//! Invariant the stride relies on: every compute dispatch runs whole
//! workgroups of the entry point's declared `@workgroup_size` (the Metal
//! encoder dispatches threadgroups of exactly `pipeline.workgroup_size`).

use naga::{
    Arena, ArraySize, BinaryOperator, Binding, Block, BuiltIn, Expression, Function,
    FunctionArgument, GlobalVariable, Handle, Literal, LocalVariable, Range, Scalar, ShaderStage,
    Span, Statement, Type, TypeInner, UniqueArena,
};

use crate::shader_common::collect_entry_point_globals;

/// One run of same-typed elements to zero inside a workgroup variable: the
/// access path from the variable to the elements, and what each one is.
struct Region {
    global: Handle<GlobalVariable>,
    steps: Vec<Step>,
    /// Elements in the run: the product of the path's array lengths.
    count: u32,
    /// Type of the zero stored per element: the leaf type, or an atomic
    /// leaf's scalar once `with_workgroup_zeroing` has swapped it in.
    value: Handle<Type>,
}

#[derive(Clone, Copy)]
enum Step {
    Index { len: u32 },
    Member { index: u32 },
}

/// `module` with a zeroing prologue at the top of every compute entry point
/// that uses workgroup memory, or `None` when none does.
///
/// Panics on a workgroup array without a constant length or an entry point
/// whose workgroup size is an override: the prologue needs both at
/// translation time.
pub(crate) fn with_workgroup_zeroing(module: &naga::Module, label: &str) -> Option<naga::Module> {
    let mut planned: Vec<(usize, Vec<Region>)> = Vec::new();
    for (ep_index, ep) in module.entry_points.iter().enumerate() {
        if ep.stage != ShaderStage::Compute {
            continue;
        }
        let mut globals: Vec<_> = collect_entry_point_globals(module, &ep.name)
            .into_iter()
            .filter(|&g| module.global_variables[g].space == naga::AddressSpace::WorkGroup)
            .collect();
        if globals.is_empty() {
            continue;
        }
        globals.sort_by_key(|g| g.index());
        let mut runs = Vec::new();
        for global in globals {
            let mut steps = Vec::new();
            collect_runs(&module.types, module.global_variables[global].ty, global, &mut steps, &mut runs, label);
        }
        planned.push((ep_index, runs));
    }
    if planned.is_empty() {
        return None;
    }

    let mut module = module.clone();
    let u32_ty = module.types.insert(
        Type { name: None, inner: TypeInner::Scalar(Scalar::U32) },
        Span::UNDEFINED,
    );
    for (ep_index, mut regions) in planned {
        for region in &mut regions {
            if let TypeInner::Atomic(scalar) = module.types[region.value].inner {
                region.value = module.types.insert(
                    Type { name: None, inner: TypeInner::Scalar(scalar) },
                    Span::UNDEFINED,
                );
            }
        }
        let naga::Module { types, entry_points, .. } = &mut module;
        let ep = &mut entry_points[ep_index];
        if ep.workgroup_size_overrides.is_some() {
            panic!(
                "{label}: entry '{}' takes its workgroup size from an override; workgroup \
                 zeroing needs the size at translation time",
                ep.name
            );
        }
        let threads = ep.workgroup_size.iter().product::<u32>();
        let prologue = build_prologue(&mut ep.function, types, u32_ty, threads, &regions);
        ep.function.body.splice(0..0, prologue);
    }
    Some(module)
}

/// Walk `ty` down to its non-aggregate leaves, one run per leaf position:
/// arrays multiply the run, struct members start new ones.
fn collect_runs(
    types: &UniqueArena<Type>,
    ty: Handle<Type>,
    global: Handle<GlobalVariable>,
    steps: &mut Vec<Step>,
    runs: &mut Vec<Region>,
    label: &str,
) {
    match types[ty].inner {
        TypeInner::Array { base, size, .. } => {
            let ArraySize::Constant(len) = size else {
                panic!(
                    "{label}: workgroup array of {size:?} length; workgroup zeroing needs a \
                     constant length"
                );
            };
            steps.push(Step::Index { len: len.get() });
            collect_runs(types, base, global, steps, runs, label);
            steps.pop();
        }
        TypeInner::Struct { ref members, .. } => {
            for (index, member) in members.iter().enumerate() {
                steps.push(Step::Member { index: index as u32 });
                collect_runs(types, member.ty, global, steps, runs, label);
                steps.pop();
            }
        }
        _ => {
            let count = steps.iter().try_fold(1u32, |n, step| match *step {
                Step::Index { len } => n.checked_mul(len),
                Step::Member { .. } => Some(n),
            });
            let count = count.unwrap_or_else(|| panic!("{label}: workgroup variable has more than u32::MAX elements"));
            runs.push(Region { global, steps: steps.clone(), count, value: ty });
        }
    }
}

fn build_prologue(
    function: &mut Function,
    types: &UniqueArena<Type>,
    u32_ty: Handle<Type>,
    threads: u32,
    regions: &[Region],
) -> Block {
    let mut block = Block::new();
    let invocation = invocation_index(function, types, u32_ty, &mut block);
    let Function { expressions, local_variables, .. } = function;
    for region in regions {
        let zero = push(expressions, &mut block, Expression::ZeroValue(region.value));
        if region.count <= threads {
            // One element per invocation at most.
            let mut body = Block::new();
            store_element(expressions, &mut body, region, invocation, zero);
            if region.count == threads {
                block.append(&mut body);
            } else {
                let count = push(expressions, &mut block, Expression::Literal(Literal::U32(region.count)));
                let condition = push(
                    expressions,
                    &mut block,
                    Expression::Binary { op: BinaryOperator::Less, left: invocation, right: count },
                );
                block.push(
                    Statement::If { condition, accept: body, reject: Block::new() },
                    Span::UNDEFINED,
                );
            }
        } else {
            // for (i = invocation; i < count; i += threads)
            let counter = local_variables.append(
                LocalVariable { name: None, ty: u32_ty, init: None },
                Span::UNDEFINED,
            );
            let counter = push(expressions, &mut block, Expression::LocalVariable(counter));
            block.push(Statement::Store { pointer: counter, value: invocation }, Span::UNDEFINED);
            let mut body = Block::new();
            let i = push(expressions, &mut body, Expression::Load { pointer: counter });
            let count = push(expressions, &mut body, Expression::Literal(Literal::U32(region.count)));
            let done = push(
                expressions,
                &mut body,
                Expression::Binary { op: BinaryOperator::GreaterEqual, left: i, right: count },
            );
            body.push(
                Statement::If {
                    condition: done,
                    accept: Block::from_vec(vec![Statement::Break]),
                    reject: Block::new(),
                },
                Span::UNDEFINED,
            );
            store_element(expressions, &mut body, region, i, zero);
            let stride = push(expressions, &mut body, Expression::Literal(Literal::U32(threads)));
            let next = push(
                expressions,
                &mut body,
                Expression::Binary { op: BinaryOperator::Add, left: i, right: stride },
            );
            body.push(Statement::Store { pointer: counter, value: next }, Span::UNDEFINED);
            block.push(
                Statement::Loop { body, continuing: Block::new(), break_if: None },
                Span::UNDEFINED,
            );
        }
    }
    block.push(Statement::ControlBarrier(naga::Barrier::WORK_GROUP), Span::UNDEFINED);
    block
}

/// The entry point's `local_invocation_index`, from its own argument or
/// struct member when it declares one, else from a new argument.
fn invocation_index(
    function: &mut Function,
    types: &UniqueArena<Type>,
    u32_ty: Handle<Type>,
    block: &mut Block,
) -> Handle<Expression> {
    let is_index = |binding: &Option<Binding>| {
        matches!(binding, Some(Binding::BuiltIn(BuiltIn::LocalInvocationIndex)))
    };
    for (arg, argument) in function.arguments.iter().enumerate() {
        if is_index(&argument.binding) {
            return push(&mut function.expressions, block, Expression::FunctionArgument(arg as u32));
        }
        if let TypeInner::Struct { ref members, .. } = types[argument.ty].inner
            && let Some(member) = members.iter().position(|m| is_index(&m.binding))
        {
            let base = push(&mut function.expressions, block, Expression::FunctionArgument(arg as u32));
            return push(
                &mut function.expressions,
                block,
                Expression::AccessIndex { base, index: member as u32 },
            );
        }
    }
    function.arguments.push(FunctionArgument {
        name: None,
        ty: u32_ty,
        binding: Some(Binding::BuiltIn(BuiltIn::LocalInvocationIndex)),
    });
    let arg = function.arguments.len() as u32 - 1;
    push(&mut function.expressions, block, Expression::FunctionArgument(arg))
}

/// Store `zero` into element `index` of the region's run.
fn store_element(
    expressions: &mut Arena<Expression>,
    block: &mut Block,
    region: &Region,
    index: Handle<Expression>,
    zero: Handle<Expression>,
) {
    let mut pointer = push(expressions, block, Expression::GlobalVariable(region.global));
    let mut stride = region.count;
    let mut outermost = true;
    for step in &region.steps {
        match *step {
            Step::Member { index: member } => {
                pointer = push(expressions, block, Expression::AccessIndex { base: pointer, index: member });
            }
            Step::Index { len } => {
                stride /= len;
                let mut element = index;
                if stride > 1 {
                    let divisor = push(expressions, block, Expression::Literal(Literal::U32(stride)));
                    element = push(
                        expressions,
                        block,
                        Expression::Binary { op: BinaryOperator::Divide, left: element, right: divisor },
                    );
                }
                // `index < count` already bounds the outermost array.
                if !outermost {
                    let modulus = push(expressions, block, Expression::Literal(Literal::U32(len)));
                    element = push(
                        expressions,
                        block,
                        Expression::Binary { op: BinaryOperator::Modulo, left: element, right: modulus },
                    );
                }
                outermost = false;
                pointer = push(expressions, block, Expression::Access { base: pointer, index: element });
            }
        }
    }
    block.push(Statement::Store { pointer, value: zero }, Span::UNDEFINED);
}

/// Append `expr` and, unless naga treats it as in scope from the function's
/// start, emit it into `block` where it is used.
fn push(expressions: &mut Arena<Expression>, block: &mut Block, expr: Expression) -> Handle<Expression> {
    let emit = !expr.needs_pre_emit();
    let handle = expressions.append(expr, Span::UNDEFINED);
    if emit {
        block.push(Statement::Emit(Range::new_from_bounds(handle, handle)), Span::UNDEFINED);
    }
    handle
}

#[cfg(test)]
mod tests {
    use super::with_workgroup_zeroing;

    fn validate(module: &naga::Module) {
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(module)
            .expect("the zeroed module validates");
    }

    #[test]
    fn only_entries_that_use_workgroup_memory_get_a_prologue() {
        let wgsl = r#"
            var<workgroup> partial: array<f32, 256>;
            @group(0) @binding(0) var<storage, read_write> out: array<f32>;
            fn total(i: u32) -> f32 { return partial[i] + partial[255u - i]; }
            @compute @workgroup_size(64)
            fn uses(@builtin(global_invocation_id) id: vec3<u32>) { out[id.x] = total(id.x % 256u); }
            @compute @workgroup_size(64)
            fn ignores(@builtin(global_invocation_id) id: vec3<u32>) { out[id.x] = 2.0; }
        "#;
        let module = naga::front::wgsl::parse_str(wgsl).expect("parses");
        let zeroed = with_workgroup_zeroing(&module, "test").expect("`uses` reaches workgroup memory through a helper");
        validate(&zeroed);
        for (before, after) in module.entry_points.iter().zip(&zeroed.entry_points) {
            let changed = after.function.body.len() != before.function.body.len()
                || after.function.arguments.len() != before.function.arguments.len();
            assert_eq!(changed, before.name == "uses", "{}", before.name);
        }
    }

    #[test]
    fn modules_without_workgroup_memory_are_left_alone() {
        let wgsl = r#"
            @group(0) @binding(0) var<storage, read_write> out: array<f32>;
            @compute @workgroup_size(64)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) { out[id.x] = 1.0; }
        "#;
        let module = naga::front::wgsl::parse_str(wgsl).expect("parses");
        assert!(with_workgroup_zeroing(&module, "test").is_none());
    }
}
