// node.water_commit — fusable BUFFER body. Selects the next accepted state
// after validation: candidate when the sticky status word is clean, the last
// accepted record otherwise. Accepted stays byte-identical through a fault —
// that retention is the whole point of the candidate/accepted split.
//
// ABI (buffer standalone codegen): `accepted` and `candidate` are coincident
// (pre-read element-wise); `status` is BufferGather — the body reads the
// single global sticky word through `buf_status[0]`. The output aliases the
// accepted wire (aliased_array_io), which is safe because the body is a pure
// per-element select: it reads element idx and writes element idx.
fn body(idx: u32, count: u32, e_accepted: Element, e_candidate: Element) -> Element {
    if (buf_status[0] == 0u) {
        return e_candidate;
    }
    return e_accepted;
}
