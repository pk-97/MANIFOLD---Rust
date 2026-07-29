//! `{{slot}}` substitution. Every placeholder must resolve; every input must be
//! used. Both directions loud — an unused input is a brief-authoring bug.

use std::collections::BTreeMap;

pub fn render(template: &str, inputs: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = template.to_string();
    for (key, value) in inputs {
        let slot = format!("{{{{{key}}}}}");
        if !out.contains(&slot) {
            return Err(format!("template never uses input {key:?}"));
        }
        out = out.replace(&slot, value);
    }
    if let Some(start) = out.find("{{") {
        let tail: String = out[start..].chars().take(40).collect();
        return Err(format!("unresolved template slot at {tail:?}"));
    }
    Ok(out)
}
