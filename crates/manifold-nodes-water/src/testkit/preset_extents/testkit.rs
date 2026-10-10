//! Extent fault injection and prepared-plan observation.

use super::*;

pub fn check_with_rules(preset: &mut LiquidPreset, rules: &[ExtentRule]) -> Result<ExtentReport, ExtentError> {
    check_graph(&mut preset.graph, &preset.plan, rules)
}

/// Full bound-buffer bytes and retained private bytes from the FFT rule.
pub fn inverse_fft_rebind_bytes(preset: &LiquidPreset, padding: u64) -> (u64, u64) {
    manifold_node_engine::exec::extent::testkit::inverse_fft_rebind_bytes(&preset.graph, &preset.plan, padding)
}
