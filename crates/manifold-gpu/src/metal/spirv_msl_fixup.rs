//! SPIR-V rewrite applied just before SPIRV-Cross's MSL backend.
//!
//! SPIRV-Cross (checked at 0.7.2+38681a3 and upstream main) mishandles one
//! shape in a fully inlined module: a constant array whose innermost element
//! is a struct or matrix. `declare_complex_constant_arrays` declares that
//! constant as a thread-local inside the entry function, but `emit_array_copy`
//! tags every copy from a constant id `FromConstantToStack`, whose source
//! parameter is `constant T (&)[N]`. Metal rejects the call. naga produces
//! the shape for every inlined helper with a local array of structs: locals
//! are zero-initialised, and spirv-opt's inliner turns the initialiser into
//! an `OpStore` of a null constant.
//!
//! The rewrite feeds such stores an `OpCompositeConstruct` of the same
//! elements instead (recursively for nested arrays), so SPIRV-Cross copies a
//! thread temporary into the thread variable. Values are unchanged. The
//! predicate is SPIRV-Cross's own: arrays whose innermost element is a scalar
//! or vector are declared in `constant` space and need no rewrite.
//!
//! Not covered: a constant *struct* whose member is an array of structs, if
//! SPIRV-Cross ever hoists that member. No naga shader produces it today.
//!
//! Obsolete when SPIRV-Cross declares complex constant arrays in the address
//! space its copy helpers assume (check `declare_complex_constant_arrays`
//! and the `FromConstantToStack` tag in `spirv_msl.cpp` on a crate bump).

use std::collections::{HashMap, HashSet};

const OP_TYPE_BOOL: u32 = 20;
const OP_TYPE_INT: u32 = 21;
const OP_TYPE_FLOAT: u32 = 22;
const OP_TYPE_VECTOR: u32 = 23;
const OP_TYPE_ARRAY: u32 = 28;
const OP_CONSTANT: u32 = 43;
const OP_CONSTANT_COMPOSITE: u32 = 44;
const OP_CONSTANT_NULL: u32 = 46;
const OP_STORE: u32 = 62;
const OP_COMPOSITE_CONSTRUCT: u32 = 80;

const HEADER_WORDS: usize = 5;

fn instructions(words: &[u32]) -> impl Iterator<Item = (u32, &[u32])> {
    let mut at = HEADER_WORDS.min(words.len());
    std::iter::from_fn(move || {
        if at >= words.len() {
            return None;
        }
        let count = ((words[at] >> 16) as usize).max(1);
        let end = (at + count).min(words.len());
        let inst = (words[at] & 0xffff, &words[at + 1..end]);
        at = end;
        Some(inst)
    })
}

fn push_inst(out: &mut Vec<u32>, opcode: u32, operands: &[u32]) {
    out.push(((operands.len() as u32 + 1) << 16) | opcode);
    out.extend_from_slice(operands);
}

/// What the rewrite knows about a module: which array types SPIRV-Cross
/// hoists into thread storage, and the constants of those types.
struct Scan {
    /// array type -> (element type, length)
    arrays: HashMap<u32, (u32, u32)>,
    /// Array types whose innermost element is neither scalar nor vector.
    complex_arrays: HashSet<u32>,
    /// constant id -> (array type, explicit elements; `None` for a null)
    complex_constants: HashMap<u32, (u32, Option<Vec<u32>>)>,
    /// type -> an existing `OpConstantNull` of it
    nulls: HashMap<u32, u32>,
    needs_rewrite: bool,
}

fn scan(words: &[u32]) -> Scan {
    let mut simple_types = HashSet::new();
    let mut u32_constants: HashMap<u32, u32> = HashMap::new();
    let mut scan = Scan {
        arrays: HashMap::new(),
        complex_arrays: HashSet::new(),
        complex_constants: HashMap::new(),
        nulls: HashMap::new(),
        needs_rewrite: false,
    };
    for (opcode, ops) in instructions(words) {
        match opcode {
            OP_TYPE_BOOL | OP_TYPE_INT | OP_TYPE_FLOAT | OP_TYPE_VECTOR => {
                simple_types.insert(ops[0]);
            }
            OP_TYPE_ARRAY => {
                if let Some(&len) = u32_constants.get(&ops[2]) {
                    scan.arrays.insert(ops[0], (ops[1], len));
                    let mut innermost = ops[1];
                    while let Some(&(elem, _)) = scan.arrays.get(&innermost) {
                        innermost = elem;
                    }
                    if !simple_types.contains(&innermost) {
                        scan.complex_arrays.insert(ops[0]);
                    }
                }
            }
            OP_CONSTANT if ops.len() == 3 => {
                u32_constants.insert(ops[1], ops[2]);
            }
            OP_CONSTANT_NULL => {
                scan.nulls.entry(ops[0]).or_insert(ops[1]);
                if scan.complex_arrays.contains(&ops[0]) {
                    scan.complex_constants.insert(ops[1], (ops[0], None));
                }
            }
            OP_CONSTANT_COMPOSITE => {
                if scan.complex_arrays.contains(&ops[0]) {
                    scan.complex_constants
                        .insert(ops[1], (ops[0], Some(ops[2..].to_vec())));
                }
            }
            OP_STORE => {
                if scan.complex_constants.contains_key(&ops[1]) {
                    scan.needs_rewrite = true;
                }
            }
            _ => {}
        }
    }
    scan
}

/// Rewrites every `OpStore` of a complex constant array to store an
/// `OpCompositeConstruct` of its elements instead. Returns the input
/// unchanged when the shape is absent.
pub(super) fn materialize_complex_constant_array_stores(words: &[u32]) -> Vec<u32> {
    let mut scan = scan(words);
    if !scan.needs_rewrite {
        return words.to_vec();
    }

    let mut bound = words[3];
    let mut fresh = || {
        let id = bound;
        bound += 1;
        id
    };

    // Null constants the construction needs but the module lacks, declared
    // right after the first complex null constant (every type involved is
    // declared before that array type). Nested complex nulls are built from
    // their own elements, so only non-array elements need a null here.
    let mut new_nulls: Vec<(u32, u32)> = Vec::new();
    let mut pending: Vec<u32> = scan
        .complex_constants
        .values()
        .filter(|(_, elements)| elements.is_none())
        .map(|&(ty, _)| ty)
        .collect();
    while let Some(array_ty) = pending.pop() {
        let elem = scan.arrays[&array_ty].0;
        if scan.complex_arrays.contains(&elem) {
            pending.push(elem);
        } else if let std::collections::hash_map::Entry::Vacant(slot) = scan.nulls.entry(elem) {
            let id = fresh();
            slot.insert(id);
            new_nulls.push((elem, id));
        }
    }

    // Emits the construct chain for one complex constant; returns the value id.
    fn construct(
        out: &mut Vec<u32>,
        scan: &Scan,
        fresh: &mut impl FnMut() -> u32,
        constant: u32,
    ) -> u32 {
        let (array_ty, ref elements) = scan.complex_constants[&constant];
        let (elem_ty, len) = scan.arrays[&array_ty];
        let constituents: Vec<u32> = match elements {
            Some(list) => list
                .iter()
                .map(|&c| {
                    if scan.complex_constants.contains_key(&c) {
                        construct(out, scan, fresh, c)
                    } else {
                        c
                    }
                })
                .collect(),
            None => (0..len)
                .map(|_| {
                    if scan.complex_arrays.contains(&elem_ty) {
                        construct_null(out, scan, fresh, elem_ty)
                    } else {
                        scan.nulls[&elem_ty]
                    }
                })
                .collect(),
        };
        let value = fresh();
        let mut operands = vec![array_ty, value];
        operands.extend(constituents);
        push_inst(out, OP_COMPOSITE_CONSTRUCT, &operands);
        value
    }

    // A nested complex array has no constant of its own to name, so build
    // its zero value from the element type directly.
    fn construct_null(
        out: &mut Vec<u32>,
        scan: &Scan,
        fresh: &mut impl FnMut() -> u32,
        array_ty: u32,
    ) -> u32 {
        let (elem_ty, len) = scan.arrays[&array_ty];
        let constituents: Vec<u32> = (0..len)
            .map(|_| {
                if scan.complex_arrays.contains(&elem_ty) {
                    construct_null(out, scan, fresh, elem_ty)
                } else {
                    scan.nulls[&elem_ty]
                }
            })
            .collect();
        let value = fresh();
        let mut operands = vec![array_ty, value];
        operands.extend(constituents);
        push_inst(out, OP_COMPOSITE_CONSTRUCT, &operands);
        value
    }

    let mut out = Vec::with_capacity(words.len() + 64);
    out.extend_from_slice(&words[..HEADER_WORDS]);
    let mut nulls_emitted = false;
    for (opcode, ops) in instructions(words) {
        match opcode {
            OP_CONSTANT_NULL if !nulls_emitted && scan.complex_constants.contains_key(&ops[1]) => {
                push_inst(&mut out, opcode, ops);
                for &(ty, id) in &new_nulls {
                    push_inst(&mut out, OP_CONSTANT_NULL, &[ty, id]);
                }
                nulls_emitted = true;
            }
            OP_STORE if scan.complex_constants.contains_key(&ops[1]) => {
                let value = construct(&mut out, &scan, &mut fresh, ops[1]);
                let mut store = ops.to_vec();
                store[1] = value;
                push_inst(&mut out, OP_STORE, &store);
            }
            _ => push_inst(&mut out, opcode, ops),
        }
    }
    out[3] = bound;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use spirv_tools::val::Validator;

    const OP_CAPABILITY: u32 = 17;
    const OP_MEMORY_MODEL: u32 = 14;
    const OP_ENTRY_POINT: u32 = 15;
    const OP_EXECUTION_MODE: u32 = 16;
    const OP_TYPE_VOID: u32 = 19;
    const OP_TYPE_MATRIX: u32 = 24;
    const OP_TYPE_STRUCT: u32 = 30;
    const OP_TYPE_POINTER: u32 = 32;
    const OP_TYPE_FUNCTION: u32 = 33;
    const OP_FUNCTION: u32 = 54;
    const OP_FUNCTION_END: u32 = 56;
    const OP_VARIABLE: u32 = 59;
    const OP_LABEL: u32 = 248;
    const OP_RETURN: u32 = 253;

    const STORAGE_FUNCTION: u32 = 7;

    /// A compute module with one local variable of `var_ty`, pointed at by
    /// `ptr_ty`, zero-initialised by storing `stored`. The caller supplies
    /// the type and constant declarations; ids below 100 are theirs.
    struct ModuleBuilder {
        words: Vec<u32>,
    }

    impl ModuleBuilder {
        fn new() -> Self {
            let mut words = vec![0x0723_0203, 0x0001_0300, 0, 200, 0];
            push_inst(&mut words, OP_CAPABILITY, &[1]); // Shader
            push_inst(&mut words, OP_MEMORY_MODEL, &[0, 1]); // Logical GLSL450
            // OpEntryPoint GLCompute %100 "main"
            push_inst(&mut words, OP_ENTRY_POINT, &[5, 100, 0x6e69_616d, 0]);
            push_inst(&mut words, OP_EXECUTION_MODE, &[100, 17, 1, 1, 1]); // LocalSize
            Self { words }
        }

        fn inst(&mut self, opcode: u32, operands: &[u32]) -> &mut Self {
            push_inst(&mut self.words, opcode, operands);
            self
        }

        /// %101 void, %102 fn, %103 ptr<Function, var_ty>, %104 variable.
        fn finish(mut self, var_ty: u32, stored: u32) -> Vec<u32> {
            self.inst(OP_TYPE_VOID, &[101])
                .inst(OP_TYPE_FUNCTION, &[102, 101])
                .inst(OP_TYPE_POINTER, &[103, STORAGE_FUNCTION, var_ty])
                .inst(OP_FUNCTION, &[101, 100, 0, 102])
                .inst(OP_LABEL, &[105])
                .inst(OP_VARIABLE, &[103, 104, STORAGE_FUNCTION])
                .inst(OP_STORE, &[104, stored])
                .inst(OP_RETURN, &[])
                .inst(OP_FUNCTION_END, &[]);
            self.words
        }
    }

    /// %1 f32, %2 u32, %3 vec3<f32>, %4 const 2u, %5 struct { vec3, f32 }.
    fn base_types(b: &mut ModuleBuilder) {
        b.inst(OP_TYPE_FLOAT, &[1, 32])
            .inst(OP_TYPE_INT, &[2, 32, 0])
            .inst(OP_TYPE_VECTOR, &[3, 1, 3])
            .inst(OP_CONSTANT, &[2, 4, 2])
            .inst(OP_TYPE_STRUCT, &[5, 3, 1]);
    }

    fn validate(words: &[u32], label: &str) {
        spirv_tools::val::create(None)
            .validate(words, None)
            .unwrap_or_else(|e| panic!("{label}: invalid SPIR-V: {e}"));
    }

    fn ops_of(words: &[u32], opcode: u32) -> Vec<&[u32]> {
        instructions(words)
            .filter(|(op, _)| *op == opcode)
            .map(|(_, ops)| ops)
            .collect()
    }

    #[test]
    fn null_struct_array_store_becomes_a_construct_of_struct_nulls() {
        let mut b = ModuleBuilder::new();
        base_types(&mut b);
        b.inst(OP_TYPE_ARRAY, &[6, 5, 4]) // array<struct, 2>
            .inst(OP_CONSTANT_NULL, &[6, 7]);
        let input = b.finish(6, 7);
        validate(&input, "input");

        let output = materialize_complex_constant_array_stores(&input);
        validate(&output, "output");

        let stores = ops_of(&output, OP_STORE);
        let constructs = ops_of(&output, OP_COMPOSITE_CONSTRUCT);
        assert_eq!(constructs.len(), 1);
        assert_eq!(constructs[0][0], 6);
        assert_eq!(stores[0][1], constructs[0][1]);
        let elements = &constructs[0][2..];
        assert_eq!(elements.len(), 2);
        let struct_nulls: Vec<&[u32]> = ops_of(&output, OP_CONSTANT_NULL)
            .into_iter()
            .filter(|ops| ops[0] == 5)
            .collect();
        assert_eq!(struct_nulls.len(), 1, "one struct null was declared");
        assert!(elements.iter().all(|&e| e == struct_nulls[0][1]));
        assert_eq!(output[3], input[3] + 2, "bound grew by the null and the construct");
    }

    #[test]
    fn explicit_matrix_array_constant_keeps_its_elements() {
        let mut b = ModuleBuilder::new();
        base_types(&mut b);
        b.inst(OP_TYPE_MATRIX, &[6, 3, 3]) // mat3x3<f32>
            .inst(OP_TYPE_ARRAY, &[7, 6, 4]) // array<mat3, 2>
            .inst(OP_CONSTANT_NULL, &[6, 8])
            .inst(OP_CONSTANT, &[1, 9, 1.0f32.to_bits()])
            .inst(OP_CONSTANT_COMPOSITE, &[3, 10, 9, 9, 9])
            .inst(OP_CONSTANT_COMPOSITE, &[6, 11, 10, 10, 10])
            .inst(OP_CONSTANT_COMPOSITE, &[7, 12, 8, 11]);
        let input = b.finish(7, 12);
        validate(&input, "input");

        let output = materialize_complex_constant_array_stores(&input);
        validate(&output, "output");

        let constructs = ops_of(&output, OP_COMPOSITE_CONSTRUCT);
        assert_eq!(constructs.len(), 1);
        assert_eq!(&constructs[0][2..], &[8, 11]);
        assert_eq!(ops_of(&output, OP_STORE)[0][1], constructs[0][1]);
        assert_eq!(
            ops_of(&output, OP_CONSTANT_NULL).len(),
            1,
            "explicit elements need no new null"
        );
    }

    #[test]
    fn nested_struct_array_null_is_constructed_all_the_way_down() {
        let mut b = ModuleBuilder::new();
        base_types(&mut b);
        b.inst(OP_TYPE_ARRAY, &[6, 5, 4]) // array<struct, 2>
            .inst(OP_TYPE_ARRAY, &[7, 6, 4]) // array<array<struct, 2>, 2>
            .inst(OP_CONSTANT_NULL, &[7, 8]);
        let input = b.finish(7, 8);
        validate(&input, "input");

        let output = materialize_complex_constant_array_stores(&input);
        validate(&output, "output");

        let constructs = ops_of(&output, OP_COMPOSITE_CONSTRUCT);
        let outer: Vec<&&[u32]> = constructs.iter().filter(|c| c[0] == 7).collect();
        let inner: Vec<&&[u32]> = constructs.iter().filter(|c| c[0] == 6).collect();
        assert_eq!(outer.len(), 1);
        assert_eq!(inner.len(), 2);
        let inner_ids: Vec<u32> = inner.iter().map(|c| c[1]).collect();
        assert_eq!(&outer[0][2..], inner_ids.as_slice());
        assert_eq!(ops_of(&output, OP_STORE)[0][1], outer[0][1]);
        // No construct names a hoisted complex constant.
        for c in &constructs {
            assert!(c[2..].iter().all(|&e| e != 8));
        }
    }

    #[test]
    fn scalar_and_vector_arrays_are_left_alone() {
        for (label, elem_ty) in [("scalar", 1), ("vector", 3)] {
            let mut b = ModuleBuilder::new();
            base_types(&mut b);
            b.inst(OP_TYPE_ARRAY, &[6, elem_ty, 4])
                .inst(OP_TYPE_ARRAY, &[7, 6, 4])
                .inst(OP_CONSTANT_NULL, &[7, 8]);
            let input = b.finish(7, 8);
            validate(&input, label);
            assert_eq!(materialize_complex_constant_array_stores(&input), input, "{label}");
        }
    }

    #[test]
    fn unstored_complex_constant_leaves_the_module_unchanged() {
        let mut b = ModuleBuilder::new();
        base_types(&mut b);
        b.inst(OP_TYPE_ARRAY, &[6, 5, 4])
            .inst(OP_CONSTANT_NULL, &[6, 7])
            .inst(OP_CONSTANT_NULL, &[5, 8]);
        let input = b.finish(5, 8);
        validate(&input, "input");
        assert_eq!(materialize_complex_constant_array_stores(&input), input);
    }
}
