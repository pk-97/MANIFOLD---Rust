//! A binding id names one value (typed identity design D5).
//!
//! Every binding under one id takes the same card value, so they all share
//! one default, and a string id's bindings share its spec's default. Each id
//! has at most one spec across `params` and `string_params`. Fan-out is
//! allowed on those terms only: two objects from one file share that file's
//! id, two files never do.
//!
//! [`check`] finds every break. Edits that would add one are refused
//! (`SceneGraphTransaction`, the merge import); [`repair`] fixes a saved
//! graph at load and says what it did.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::effect_graph_def::{PresetMetadata, StringParamSpecDef};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum BindingIdentityError {
    /// Two specs share one id.
    DuplicateSpec { id: String },
    /// String bindings under one id carry different values, or a value
    /// that is not their spec's.
    StringValuesDiffer { id: String },
    /// Numeric bindings under one id carry different defaults.
    NumericValuesDiffer { id: String },
}

impl fmt::Display for BindingIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSpec { id } => write!(f, "card control '{id}' is declared twice"),
            Self::StringValuesDiffer { id } => {
                write!(f, "'{id}' binds different files or texts under one name")
            }
            Self::NumericValuesDiffer { id } => {
                write!(f, "'{id}' binds different values under one name")
            }
        }
    }
}

/// Every break of the one-value rule in `meta`, sorted.
pub fn check(meta: &PresetMetadata) -> Vec<BindingIdentityError> {
    let mut errors = BTreeSet::new();
    let mut specs = BTreeSet::new();
    for id in meta
        .params
        .iter()
        .map(|spec| &spec.id)
        .chain(meta.string_params.iter().map(|spec| &spec.id))
    {
        if !specs.insert(id.as_str()) {
            errors.insert(BindingIdentityError::DuplicateSpec { id: id.clone() });
        }
    }
    let mut numeric: BTreeMap<&str, f32> = BTreeMap::new();
    for binding in &meta.bindings {
        let first = *numeric.entry(binding.id.as_str()).or_insert(binding.default_value);
        if first.to_bits() != binding.default_value.to_bits() {
            errors.insert(BindingIdentityError::NumericValuesDiffer { id: binding.id.clone() });
        }
    }
    for (id, values) in string_values(meta) {
        let spec = meta.string_params.iter().find(|spec| spec.id == id);
        let off_spec = spec.is_some_and(|spec| values.iter().any(|v| *v != spec.default_value));
        if values.len() > 1 || off_spec {
            errors.insert(BindingIdentityError::StringValuesDiffer { id: id.to_string() });
        }
    }
    errors.into_iter().collect()
}

/// Breaks in `after` that `before` did not have: what an edit added.
pub fn added_errors(before: Option<&PresetMetadata>, after: Option<&PresetMetadata>) -> Vec<BindingIdentityError> {
    let Some(after) = after else { return Vec::new() };
    let before: BTreeSet<_> = before.map(check).unwrap_or_default().into_iter().collect();
    check(after).into_iter().filter(|error| !before.contains(error)).collect()
}

/// The string id a binding to `value` should use under `base`: the id that
/// already names `value`, else `base` when it is free, else the first free
/// `base_N` from 2. `spec` is `Some` when the id is new and needs its spec.
pub fn string_id_for(
    meta: Option<&PresetMetadata>,
    base: &str,
    value: &str,
    template: &StringParamSpecDef,
) -> (String, Option<StringParamSpecDef>) {
    let Some(meta) = meta else {
        return (base.to_string(), Some(spec_for(template, base, value, None)));
    };
    let values = string_values(meta);
    let names_value = |id: &str| {
        meta.string_params.iter().find(|spec| spec.id == id).is_some_and(|spec| spec.default_value == value)
            && values.get(id).is_none_or(|set| set.iter().all(|v| *v == value))
    };
    for id in std::iter::once(base.to_string()).chain((2..).map(|n| format!("{base}_{n}")).take(4096)) {
        if names_value(&id) {
            return (id, None);
        }
        if !taken(meta).contains(id.as_str()) {
            let suffix = (id != base).then(|| file_label(value));
            let spec = spec_for(template, &id, value, suffix.as_deref());
            return (id, Some(spec));
        }
    }
    unreachable!("4096 suffixes always hold a free id")
}

/// Split every string id whose bindings carry different values: the spec's
/// value keeps the id, each other value gets its own `<id>_N` with a spec
/// copied from the original. Drops a repeated spec, keeping the first.
/// Returns one load-report line per change. Idempotent.
pub fn repair(meta: &mut PresetMetadata) -> Vec<String> {
    let mut report = Vec::new();
    let mut seen = BTreeSet::new();
    let before = meta.params.len();
    meta.params.retain(|spec| seen.insert(spec.id.clone()));
    let before_strings = meta.string_params.len();
    meta.string_params.retain(|spec| seen.insert(spec.id.clone()));
    let dropped = before - meta.params.len() + before_strings - meta.string_params.len();
    if dropped > 0 {
        report.push(format!("{dropped} repeated card control(s) removed; the first of each kept"));
    }

    let split: Vec<String> = string_values(meta)
        .into_iter()
        .filter(|(id, values)| {
            let spec = meta.string_params.iter().find(|spec| spec.id == *id);
            values.len() > 1 || spec.is_some_and(|spec| values.iter().any(|v| *v != spec.default_value))
        })
        .map(|(id, _)| id.to_string())
        .collect();
    for id in split {
        let Some(spec) = meta.string_params.iter().find(|spec| spec.id == id).cloned() else {
            continue;
        };
        let mut minted: BTreeMap<String, String> = BTreeMap::new();
        let mut order = Vec::new();
        for binding in meta.string_bindings.iter().filter(|binding| binding.id == id) {
            if binding.default_value != spec.default_value && !minted.contains_key(&binding.default_value) {
                minted.insert(binding.default_value.clone(), String::new());
                order.push(binding.default_value.clone());
            }
        }
        // No binding carries the spec's value: the first value keeps the id.
        let keeps_id = !meta
            .string_bindings
            .iter()
            .any(|binding| binding.id == id && binding.default_value == spec.default_value);
        if keeps_id && let Some(first) = order.first().cloned() {
            minted.remove(&first);
            order.remove(0);
            if let Some(own) = meta.string_params.iter_mut().find(|own| own.id == id) {
                own.default_value = first;
            }
        }
        for value in &order {
            let (new_id, new_spec) = string_id_for(Some(meta), &id, value, &spec);
            if let Some(new_spec) = new_spec {
                meta.string_params.push(new_spec);
            }
            minted.insert(value.clone(), new_id);
        }
        for binding in meta.string_bindings.iter_mut().filter(|binding| binding.id == id) {
            if let Some(new_id) = minted.get(&binding.default_value).filter(|new_id| !new_id.is_empty()) {
                binding.id = new_id.clone();
            }
        }
        for value in order {
            report.push(format!("'{id}' split: {} now has its own '{}'", file_label(&value), minted[&value]));
        }
    }
    report
}

fn string_values(meta: &PresetMetadata) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut values: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for binding in &meta.string_bindings {
        values.entry(binding.id.as_str()).or_default().insert(binding.default_value.as_str());
    }
    values
}

fn taken(meta: &PresetMetadata) -> BTreeSet<&str> {
    meta.params
        .iter()
        .map(|spec| spec.id.as_str())
        .chain(meta.string_params.iter().map(|spec| spec.id.as_str()))
        .chain(meta.bindings.iter().map(|binding| binding.id.as_str()))
        .chain(meta.string_bindings.iter().map(|binding| binding.id.as_str()))
        .collect()
}

fn spec_for(template: &StringParamSpecDef, id: &str, value: &str, suffix: Option<&str>) -> StringParamSpecDef {
    let mut spec = template.clone();
    spec.id = id.to_string();
    spec.default_value = value.to_string();
    if let Some(suffix) = suffix {
        spec.name = format!("{} — {suffix}", template.name);
    }
    spec
}

/// A value's readable name: a path's file stem, else the value itself.
fn file_label(value: &str) -> String {
    std::path::Path::new(value)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(bindings: &[(&str, &str, &str)], spec_default: &str) -> PresetMetadata {
        serde_json::from_value(json!({
            "id": "scene", "displayName": "Scene", "category": "Geometry", "oscPrefix": "scene",
            "params": [],
            "bindings": [],
            "stringParams": [{"id": "model_file", "name": "Model File", "defaultValue": spec_default, "isFilePicker": true}],
            "stringBindings": bindings.iter().map(|(id, value, node)| json!({
                "id": id, "label": "Model File", "defaultValue": value,
                "target": {"kind": "node", "nodeId": node, "param": "path"},
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    /// cliftest's shape: three files under one id, the spec names the cliff.
    fn cliftest() -> PresetMetadata {
        meta(
            &[
                ("model_file", "/m/cliff.gltf", "mesh_0"),
                ("model_file", "/m/cliff.gltf", "tex_0"),
                ("model_file", "/m/moss.glb", "mesh_1"),
                ("model_file", "/m/moss.glb", "tex_1"),
                ("model_file", "/m/rock.glb", "mesh_2"),
            ],
            "/m/cliff.gltf",
        )
    }

    #[test]
    fn check_flags_cliftest_and_passes_after_repair() {
        let mut meta = cliftest();
        assert_eq!(check(&meta), vec![BindingIdentityError::StringValuesDiffer { id: "model_file".into() }]);
        let report = repair(&mut meta);
        assert_eq!(report.len(), 2, "{report:?}");
        assert!(check(&meta).is_empty(), "{:?}", check(&meta));
        let id_of = |node: &str| {
            meta.string_bindings.iter().find(|b| format!("{:?}", b.target).contains(node)).unwrap().id.clone()
        };
        assert_eq!(id_of("mesh_0"), "model_file");
        assert_eq!(id_of("tex_1"), "model_file_2");
        assert_eq!(id_of("mesh_1"), "model_file_2");
        assert_eq!(id_of("mesh_2"), "model_file_3");
        let spec = |id: &str| meta.string_params.iter().find(|s| s.id == id).unwrap().clone();
        assert_eq!(spec("model_file").default_value, "/m/cliff.gltf");
        assert_eq!(spec("model_file_2").default_value, "/m/moss.glb");
        assert_eq!(spec("model_file_2").name, "Model File — moss");
        assert!(spec("model_file_3").is_file_picker);
        let again = repair(&mut meta);
        assert!(again.is_empty(), "repair is idempotent: {again:?}");
    }

    #[test]
    fn one_file_fan_out_is_valid() {
        let meta = meta(&[("model_file", "/m/a.glb", "mesh_0"), ("model_file", "/m/a.glb", "tex_0")], "/m/a.glb");
        assert!(check(&meta).is_empty());
    }

    #[test]
    fn string_id_for_reuses_the_id_that_names_the_file_and_mints_otherwise() {
        let meta = cliftest();
        let template = meta.string_params[0].clone();
        let mut fixed = meta.clone();
        repair(&mut fixed);
        assert_eq!(string_id_for(Some(&fixed), "model_file", "/m/moss.glb", &template).0, "model_file_2");
        let (id, spec) = string_id_for(Some(&fixed), "model_file", "/m/new.glb", &template);
        assert_eq!(id, "model_file_4");
        assert_eq!(spec.unwrap().default_value, "/m/new.glb");
        let (id, spec) = string_id_for(None, "model_file", "/m/new.glb", &template);
        assert_eq!((id.as_str(), spec.unwrap().name.as_str()), ("model_file", "Model File"));
    }

    #[test]
    fn repeated_specs_and_numeric_values_are_flagged() {
        let mut meta = cliftest();
        meta.string_bindings.clear();
        let spec = meta.string_params[0].clone();
        meta.string_params.push(spec);
        assert_eq!(check(&meta), vec![BindingIdentityError::DuplicateSpec { id: "model_file".into() }]);
        assert_eq!(repair(&mut meta).len(), 1);
        assert!(check(&meta).is_empty());

        let mut numeric: PresetMetadata = serde_json::from_value(json!({
            "id": "s", "displayName": "S", "category": "Geometry", "oscPrefix": "s", "params": [],
            "bindings": [
                {"id": "x", "label": "", "defaultValue": 1.0, "target": {"kind": "node", "nodeId": "a", "param": "p"}},
                {"id": "x", "label": "", "defaultValue": 2.0, "target": {"kind": "node", "nodeId": "b", "param": "p"}},
            ],
        }))
        .unwrap();
        assert_eq!(check(&numeric), vec![BindingIdentityError::NumericValuesDiffer { id: "x".into() }]);
        numeric.bindings[1].default_value = 1.0;
        assert!(check(&numeric).is_empty());
    }
}
