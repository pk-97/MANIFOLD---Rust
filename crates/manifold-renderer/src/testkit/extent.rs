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
