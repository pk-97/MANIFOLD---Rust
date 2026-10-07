use manifold_node_engine::testkit::atom::*;
use manifold_node_engine::testkit::liquid_surface::params;
use crate::node_graph::primitives::divide_by_value::DivideByValue;
use serde_json::json;
#[test]
fn gpu_flip_divide_by_value_matches_cpu_and_guards_zero() {
    let values = random_values(700, 0xd1f0);
    let got = run_atom(&mut DivideByValue::new(), &[("values", &values), ("divisor", &[0.25])], 700, &params(&[]));
    let want: Vec<f64> = values.iter().map(|&v| f64::from(v) / 0.25).collect();
    assert_close(&got, &want, "divide");
    let zero = run_atom(&mut DivideByValue::new(), &[("values", &values), ("divisor", &[0.0])], 700, &params(&[]));
    assert!(zero.iter().all(|&v| v == 0.0), "a zero divisor must give zeros");
}
/// Two divisions chained, the second reading the first coincident.
#[test]
fn gpu_flip_divides_fuse() {
    let values = random_values(700, 0xd1f1);
    let mut chain = Chain::new();
    let x = chain.source("values", values.clone());
    let quarter = chain.source("quarter", vec![0.25]);
    let half = chain.source("half", vec![0.5]);
    let first = chain.node("first", "node.divide_by_value", json!({}));
    chain.wire(x, "out", first, "values");
    chain.wire(quarter, "out", first, "divisor");
    let second = chain.node("second", "node.divide_by_value", json!({}));
    chain.wire(first, "out", second, "values");
    chain.wire(half, "out", second, "divisor");
    let got = chain.fused_matches_unfused(second, values.len());
    let want: Vec<f64> = values.iter().map(|&v| f64::from(v) / 0.125).collect();
    assert_close(&got, &want, "fused divides");
}