//! `node.cycle_table_row` — emit one row of a curated `Table` of
//! floats, advancing to the next row on each clip trigger.
//!
//! The generic preset-table cycler. Pair it with a generator-side
//! `trigger_count` source and a downstream consumer that takes
//! `Array<f32>` to drive any "snap-to-next-preset" behaviour:
//! NestedCubes pose mode is the first user — six preset 5-tuples of
//! per-instance rotation angles, indexed by clip trigger.
//!
//! The table is set in JSON (see `ParamValue::Table`). The output
//! capacity is `table.col_count()`; downstream consumers receive
//! a flat `Array<f32>` of that length and interpret it however
//! they like (per-instance angles, RGB triplets, rhythm steps).
//! Index advances via `ClipTriggerCycle::step(trigger_count, rows)`
//! so the cycle is idempotent across same-frame retriggers.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: CycleTableRow,
    type_id: "node.cycle_table_row",
    purpose: "Cycle through a curated `Table` of f32 rows on each clip trigger, emitting the selected row as `Array<f32>`. row_idx = ClipTriggerCycle::step(trigger_count, table.row_count()): a repeated trigger_count returns the cached row (idempotent — same-frame callers don't re-roll); a new trigger_count emits trigger_count % row_count, unless that would repeat the previous emission (and row_count > 1), in which case it advances by +1 mod row_count instead — never fires the same row twice in a row. The generic preset-cycler primitive — pair with any consumer that takes a variable-length float buffer (per-instance angles, channel triplets, rhythm steps) plus a trigger source from generator_input. Output capacity = table.col_count(); rows are dimensionally consistent (enforced at JSON load). Unwired trigger_count stays on row 0.",
    inputs: {
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        row: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("table"),
            label: "Table",
            ty: ParamType::Table,
            // Tables can't live in static-const ParamValue (Arc isn't
            // const-constructible). Defaults to a Float(0.0) sentinel
            // that's overridden by the JSON preset's `table` value.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "JSON `table` shape: `{\"type\":\"Table\",\"rows\":[[0.0, 90.0, 180.0, 270.0, 360.0], [...]]}`. The wired `trigger_count` is the row selector (port-shadows-param: pass raw, never pre-wrap — `ClipTriggerCycle::step` handles modulus internally so same-frame retriggers don't double-advance). When unwired the cycler stays on row 0.",
    examples: [],
    picker: { label: "Cycle Table Row", category: Driver },
    summary: "Steps through the rows of a small built-in table on each clip trigger, emitting one row of numbers at a time. A way to sequence preset values.",
    category: Control,
    role: Control,
    aliases: ["cycle table row", "sequence", "step"],
    boundary_reason: NonGpu,
    extra_fields: {
        clip_trigger_cycle: crate::generators::clip_trigger::ClipTriggerCycle = crate::generators::clip_trigger::ClipTriggerCycle::new(),
    },
}

impl Primitive for CycleTableRow {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "row" {
            return None;
        }
        match params.get("table") {
            Some(ParamValue::Table(t)) => Some(t.col_count() as u32),
            // Sentinel default — no table set yet. One float so the
            // allocator doesn't reject a zero-sized buffer; the run
            // path will see col_count=1 and write a single 0.0.
            _ => Some(1),
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(dst) = ctx.outputs.array("row") else {
            log::warn!(
                "node.cycle_table_row: no GpuBuffer bound to output port `row` — \
                 the chain build did not pre-allocate the Array<f32> output.",
            );
            return;
        };
        let f32_size = std::mem::size_of::<f32>() as u64;
        let capacity = (dst.size / f32_size) as usize;
        if capacity == 0 {
            return;
        }

        let table = match ctx.params.get("table") {
            Some(ParamValue::Table(t)) => t.clone(),
            _ => {
                // Sentinel — no table set. Fill output with zeros.
                let zeros = vec![0.0_f32; capacity];
                unsafe { dst.write(0, bytemuck::cast_slice(&zeros)) };
                return;
            }
        };

        // Port-shadows-param for `trigger_count`. The generator runtime
        // exposes `trigger_count` as a ScalarF32 on `system.generator_input`
        // so the cycler's input port hooks straight to it.
        let raw_count = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let count = raw_count.round().max(0.0) as u32;
        let row_idx = self.clip_trigger_cycle.step(count, table.row_count() as u32) as usize;

        let Some(row) = table.row(row_idx) else {
            return;
        };
        // Clamp to the allocated capacity. If the table's col_count
        // exceeds the buffer (shouldn't happen — array_output_capacity
        // sets capacity from col_count — but defend at the write site)
        // we truncate; if it's smaller, the unused tail stays at whatever
        // the buffer was last filled with (which is fine for current
        // consumers — they read exactly col_count entries).
        let write_count = capacity.min(row.len());
        unsafe { dst.write(0, bytemuck::cast_slice(&row[..write_count])) };
    }

    /// BUG-104: release the cycle's idempotence tracking. See
    /// `EffectNode::is_trigger_latch`.
    fn clear_state(&mut self) {
        self.clip_trigger_cycle = crate::generators::clip_trigger::ClipTriggerCycle::new();
    }

    fn is_trigger_latch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::parameters::TableData;
    use crate::node_graph::primitive::PrimitiveSpec;
    use std::sync::Arc;

    #[test]
    fn declares_trigger_count_input_and_array_f32_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        assert_eq!(CycleTableRow::INPUTS.len(), 1);
        assert_eq!(CycleTableRow::INPUTS[0].name, "trigger_count");
        assert!(!CycleTableRow::INPUTS[0].required);
        assert_eq!(
            CycleTableRow::INPUTS[0].ty,
            PortType::Scalar(ScalarType::F32)
        );

        assert_eq!(CycleTableRow::OUTPUTS.len(), 1);
        assert_eq!(CycleTableRow::OUTPUTS[0].name, "row");
        assert_eq!(
            CycleTableRow::OUTPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
    }

    #[test]
    fn declares_single_table_param() {
        assert_eq!(CycleTableRow::PARAMS.len(), 1);
        assert_eq!(CycleTableRow::PARAMS[0].name, "table");
        assert_eq!(CycleTableRow::PARAMS[0].ty, ParamType::Table);
    }

    #[test]
    fn array_output_capacity_reads_table_col_count() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = CycleTableRow::new();
        let table = Arc::new(
            TableData::new(vec![vec![0.0, 1.0, 2.0, 3.0, 4.0], vec![5.0, 6.0, 7.0, 8.0, 9.0]])
                .unwrap(),
        );
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("table"), ParamValue::Table(table));
        let cap = Primitive::array_output_capacity(&prim, "row", &params, &[]);
        assert_eq!(cap, Some(5));
    }

    #[test]
    fn array_output_capacity_unknown_port_returns_none() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = CycleTableRow::new();
        let params = ParamValues::default();
        let cap = Primitive::array_output_capacity(&prim, "out", &params, &[]);
        assert!(cap.is_none());
    }

    #[test]
    fn primitive_registers_as_palette_driver() {
        use crate::node_graph::palette::{PaletteCategory, palette_atoms};
        let atoms = palette_atoms();
        let entry = atoms
            .iter()
            .find(|e| e.type_id == CycleTableRow::TYPE_ID)
            .expect("cycle_table_row should be registered as a palette atom");
        assert_eq!(entry.label, "Cycle Table Row");
        assert!(matches!(entry.category, PaletteCategory::Driver));
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        use crate::node_graph::EffectNode;
        let prim = CycleTableRow::new();
        let node: &dyn EffectNode = &prim;
        assert!(node.is_trigger_latch());
    }

    /// BUG-104 — see `frequency_ratio`'s equivalent test for the full
    /// rationale.
    #[test]
    fn clear_state_releases_the_cycle_idempotence_cache() {
        use crate::node_graph::EffectNode;
        let mut prim = CycleTableRow::new();
        assert_eq!(prim.clip_trigger_cycle.step(0, 4), 0);
        assert_eq!(prim.clip_trigger_cycle.step(4, 4), 1); // would repeat 0 — advances
        assert_eq!(prim.clip_trigger_cycle.step(4, 4), 1); // idempotent on same input

        {
            let node: &mut dyn EffectNode = &mut prim;
            node.clear_state();
        }

        assert_eq!(
            prim.clip_trigger_cycle.step(4, 4),
            0,
            "released cycle should re-arm to a fresh computation"
        );
    }
}
