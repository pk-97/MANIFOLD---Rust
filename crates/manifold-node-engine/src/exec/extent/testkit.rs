//! Generic extent checker test helpers.

use super::*;
#[cfg(test)]
use crate::exec::execution_plan::compile;

#[test]
fn duplicate_extent_rule_is_an_error() {
    let mut graph = Graph::new();
    let plan = compile(&graph).expect("empty graph plan");
    let rule = ExtentRule { type_id: "test.duplicate_extent", check: size_bounded };
    let error = check_graph(&mut graph, &plan, &[rule, rule]).unwrap_err();
    assert!(matches!(error, ExtentError::DuplicateRule { type_id } if type_id == rule.type_id));
}

/// Mutate a liquid frame's provided storage after its rule has run, for
/// tests that exercise the checker’s coverage path.
pub fn malformed_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let frame = EXTENT_RULES.iter().find(|rule| rule.type_id == "node.liquid_frame").unwrap();
    (frame.check)(x)?;
    x.provided.iter_mut().find(|(port, _)| *port == "solid_b").unwrap().1 += 4;
    Ok(())
}

/// Full bound-buffer bytes and private storage for the inverse FFT rule.
pub fn inverse_fft_rebind_bytes(graph: &Graph, plan: &ExecutionPlan, padding: u64) -> (u64, u64) {
    let step = plan.steps().iter().find(|step| {
        graph.get_node(step.node).unwrap().node.type_id().as_str() == "node.inverse_fft_2d"
    }).expect("FFT step");
    let node = graph.get_node(step.node).unwrap();
    let wires = AHashMap::default();
    let bytes = AHashMap::default();
    let mut atom = AtomExtent {
        node, step, plan, wires: &wires, bytes: &bytes,
        unresolved: RefCell::new(None), provided: Vec::new(), published: Vec::new(), held: 0,
    };
    let n = atom.param("size", 256.0).round() as u64;
    let batch = atom.param("batch", 6.0).round() as u64;
    let spectrum = batch * n * (n / 2 + 1) * 8 + padding;
    let field = batch * n * n * 4 + padding;
    atom.provided = vec![("spectrum", spectrum), ("field", field)];
    let rule = EXTENT_RULES.iter().find(|rule| rule.type_id == "node.inverse_fft_2d").unwrap();
    (rule.check)(&mut atom).unwrap();
    (spectrum + field, atom.held)
}
