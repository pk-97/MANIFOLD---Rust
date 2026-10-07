//! Extent fault injection and prepared-plan observation.
use super::*;
pub(crate) fn malformed_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let frame = LIQUID_EXTENT_RULES.iter().find(|rule| rule.type_id == "node.liquid_frame").unwrap();
    (frame.check)(x)?;
    x.provided.iter_mut().find(|(port, _)| *port == "solid_b").unwrap().1 += 4;
    Ok(())
}
pub(crate) fn check_with_rules(preset: &mut LiquidPreset, rules: &[ExtentRule]) -> Result<ExtentReport, ExtentError> {
    check_graph(&mut preset.graph, &preset.plan, rules)
}

/// Full bound-buffer bytes and retained private bytes from the FFT rule.
pub(crate) fn inverse_fft_rebind_bytes(preset: &LiquidPreset, padding: u64) -> (u64, u64) {
    let step = preset.plan.steps().iter().find(|step| {
        preset.graph.get_node(step.node).unwrap().node.type_id().as_str() == "node.inverse_fft_2d"
    }).expect("FFT step");
    let node = preset.graph.get_node(step.node).unwrap();
    let wires = AHashMap::default();
    let bytes = AHashMap::default();
    let mut atom = AtomExtent {
        node, step, plan: &preset.plan, wires: &wires, bytes: &bytes,
        unresolved: RefCell::new(None), provided: Vec::new(), published: Vec::new(), held: 0,
    };
    let n = atom.param("size", 256.0).round() as u64;
    let batch = atom.param("batch", 6.0).round() as u64;
    let spectrum = batch * n * (n / 2 + 1) * 8 + padding;
    let field = batch * n * n * 4 + padding;
    atom.provided = vec![("spectrum", spectrum), ("field", field)];
    inverse_fft_2d(&mut atom).unwrap();
    (spectrum + field, atom.held)
}
