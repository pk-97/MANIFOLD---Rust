//! v1.25.0 → v1.26.0: a scene Water is always GPU FLIP, and Matter runs as
//! its own Sand item, so a Water's fields lose the `solver` key every saved
//! Water wrote. Only `fields.water.solver` is touched. Idempotent.

use serde_json::Value;

pub(crate) fn migrate(root: &mut Value) {
    let mut dropped = 0usize;
    drop_solver(root, &mut dropped);
    if dropped > 0 {
        super::note_migration(format!(
            "{dropped} Water(s) dropped the Solver setting; Water is GPU FLIP (v1.26.0)"
        ));
    }
}

fn drop_solver(value: &mut Value, dropped: &mut usize) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(water)) = map
                .get_mut("fields")
                .and_then(|fields| fields.get_mut("water"))
                && water.remove("solver").is_some()
            {
                *dropped += 1;
            }
            map.values_mut().for_each(|v| drop_solver(v, dropped));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| drop_solver(v, dropped)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn water_items_lose_their_solver_and_nothing_else_changes() {
        let mut root = json!({
            "layers": [{"scene": {"items": [
                {"id": "w", "fields": {"water": {"solver": "flip", "draw": "surface", "bounds": false}},
                 "children": [{"id": "inner", "fields": {"water": {"solver": "flip"}}}]},
                {"id": "o", "fields": {"object": {"solver": "keep"}}},
            ]}}]
        });
        migrate(&mut root);
        let items = &root["layers"][0]["scene"]["items"];
        assert_eq!(
            items[0]["fields"]["water"],
            json!({"draw": "surface", "bounds": false})
        );
        assert_eq!(items[0]["children"][0]["fields"]["water"], json!({}));
        assert_eq!(items[1]["fields"]["object"]["solver"], "keep");
        let once = root.clone();
        migrate(&mut root);
        assert_eq!(root, once);
    }
}
