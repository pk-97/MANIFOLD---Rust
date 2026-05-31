//! Parameter types exposed by an [`EffectNode`](crate::node_graph::EffectNode).
//!
//! Parameters are the user-facing knobs on a node: numeric values, booleans,
//! vectors, colours, enums. Each `EffectNode` declares its parameters
//! statically via `EffectNode::parameters()`. Per-instance values live
//! separately in `ParamValues` and are supplied to the node each frame.
//!
//! Parameters are also the public surface of a graph: the per-parameter
//! "expose" flag (V2 graph editor) decides which internal node parameters
//! become slots on the outer effect card. The modulation system targets
//! exposed parameters by name, identical to today's effect parameters.
//!
//! ## Numeric storage is `Float` only
//!
//! There used to be separate `Float` and `Int` variants on [`ParamValue`].
//! That asymmetry was the source of a class of silent bugs: any reader
//! that pattern-matched on `Float` only would fall through to a default
//! whenever a value was constructed as `Int` (e.g. from a JSON preset that
//! declared `{"type":"Int","value":N}`). The slider would move and nothing
//! visible would change.
//!
//! The fix: numbers live in one cell — `Float(f32)`. `f32`'s 24-bit mantissa
//! exactly represents every integer in our param ranges, so there's no
//! precision loss. [`ParamType::Int`] survives on [`ParamDef`] as a
//! *presentation* hint — it tells the editor to render a whole-number
//! stepper, format without decimals, and round-on-drag — but storage
//! doesn't need to redundantly enforce the constraint.
//!
//! ## Tables — preset N×M data, JSON-authored only
//!
//! [`ParamValue::Table`] carries fixed-shape preset data (rows × cols of
//! `f32`) — e.g. a pose table indexed by clip trigger. Tables are read-only
//! after deserialize and are wrapped in an `Arc` so cloning is one atomic
//! increment regardless of size. The editor surfaces a read-only summary
//! ("Table N×M"); editing happens in JSON until a second consumer
//! materializes and earns a proper grid widget.
//!
//! ## Strings — single editable text values
//!
//! [`ParamValue::String`] carries a single text value (folder paths, font
//! names, future LUT references). Arc-wrapped so clones are cheap. Unlike
//! Table, strings are user-editable per-instance — the inner-node editor
//! renders a text field. The intended bridge is generator-level
//! `string_params` (existing outer-card UI with Browse buttons) feeding
//! into inner-graph String params via the binding system, so a single
//! preset can be re-pointed per-clip without graph edits.
//!
//! String is not modulated — no `ParamConvert` variant exists for it. The
//! interpretation ("is this a filesystem path?", "is this a font name?")
//! is the consuming primitive's contract, not core's job.

use std::sync::Arc;

/// Type of a parameter knob on an [`EffectNode`](crate::node_graph::EffectNode).
///
/// `Int` is a *presentation* hint, not a storage discriminator: the editor
/// uses it to choose a whole-number stepper widget and integer-formatted
/// display. The actual value lives in `ParamValue::Float` like every other
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamType {
    Float,
    /// An angle. *Presentation* hint only, like `Int`: storage stays
    /// `ParamValue::Float` in RADIANS, which is the internal and wire unit
    /// everywhere (correct for the math, and 3D/4D rotation is radian-native).
    /// The editor and outer-card sliders display and edit this in DEGREES,
    /// converting radians<->degrees at the UI boundary only. The stored value,
    /// every wire, and every node's `run()` are untouched, so presets that wire
    /// a `math` node's radians straight into a rotation are unaffected. Never
    /// expose radians to the user. `range` stays in radians like the stored
    /// value (so modulation, which maps a driver onto the range, stays
    /// correct); the UI converts the bounds to degrees for display.
    Angle,
    /// A frequency. *Presentation* hint only, exactly like `Angle`: storage
    /// stays `ParamValue::Float` in RADIANS PER SECOND (the internal unit the
    /// oscillator math uses: phase advances `seconds * rate`). The editor
    /// displays and edits this in HERTZ, converting rad/s<->Hz (× / ÷ 2π) at
    /// the UI boundary only. The stored value, every wire, and every `run()`
    /// are untouched, so presets that set or bind the rate are unaffected.
    /// Never expose rad/s to the user — it is nice to think in Hz. `range`
    /// stays in rad/s like the stored value; the UI converts the bounds to Hz.
    Frequency,
    Int,
    Bool,
    Vec2,
    Vec3,
    Vec4,
    Color,
    /// Index into the parameter definition's `enum_values` list.
    Enum,
    /// Read-only N×M `f32` table — set in JSON, surfaced as a readonly
    /// summary in the editor. See module docs.
    Table,
    /// Single editable text value (paths, font names, identifiers). See
    /// module docs.
    String,
    /// Momentary "fire once" button. Storage is a monotonic `u32`
    /// counter held in `ParamValue::Float`. The outer-card click
    /// handler increments by one per press (no toggle state — the
    /// button always reads "fire"). Consuming primitives detect rising
    /// edges via the standard `last_count: Option<u32>` cold-start
    /// pattern — same as `node.trigger_gate`.
    Trigger,
}

/// Read-only N×M `f32` table.
///
/// All rows have the same length, enforced by [`TableData::new`]. Cloning
/// the wrapping `Arc<TableData>` is O(1).
#[derive(Debug, Clone, PartialEq)]
pub struct TableData {
    rows: Vec<Vec<f32>>,
    cols: usize,
}

impl TableData {
    /// Build a table. Returns `None` if rows are dimensionally inconsistent
    /// or empty.
    pub fn new(rows: Vec<Vec<f32>>) -> Option<Self> {
        if rows.is_empty() {
            return None;
        }
        let cols = rows[0].len();
        if cols == 0 || !rows.iter().all(|r| r.len() == cols) {
            return None;
        }
        Some(Self { rows, cols })
    }

    #[inline]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    #[inline]
    pub fn col_count(&self) -> usize {
        self.cols
    }

    /// Borrow row `i`. Returns `None` if out of bounds.
    #[inline]
    pub fn row(&self, i: usize) -> Option<&[f32]> {
        self.rows.get(i).map(|r| r.as_slice())
    }

    /// Borrow the full row slice for iteration.
    #[inline]
    pub fn rows(&self) -> &[Vec<f32>] {
        &self.rows
    }
}

/// Runtime value of one parameter.
///
/// Numeric storage collapses to a single `Float(f32)` cell — see the
/// module-level docs for the rationale. Use [`ParamValue::as_scalar`] /
/// [`ParamValue::as_u32_clamped`] when reading; they're the single
/// point of truth for coercion and shield primitives from the "did
/// this come in as Int or Float" question that no longer exists.
///
/// `Table` carries `Arc<TableData>` so cloning is O(1) regardless of
/// table size. The variant is non-`Copy`, which propagates to
/// `ParamValue` itself.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Float(f32),
    Bool(bool),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    Color([f32; 4]),
    Enum(u32),
    Table(Arc<TableData>),
    String(Arc<String>),
}

impl ParamValue {
    /// Coerce to `f32` for scalar reads.
    ///
    /// Returns `Some` only for `Float` — `Bool`/`Enum` are intentionally
    /// not auto-coerced because they're not "numbers" in the user's mental
    /// model (toggles and dropdowns). If a primitive genuinely wants a
    /// 0.0/1.0 from a Bool param, it should match `Bool` explicitly so the
    /// intent is visible at the call site.
    #[inline]
    pub fn as_scalar(&self) -> Option<f32> {
        match self {
            ParamValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Read as a `u32`, rounding then clamping to `min..`.
    ///
    /// Replaces the recurring pattern (and the bug class it spawned — a
    /// pre-collapse reader that handled only one of the old `Int` /
    /// `Float` storage variants silently fell through for the other):
    /// ```ignore
    /// match params.get("foo") {
    ///     Some(ParamValue::Float(f)) => f.round().max(min as f32) as u32,
    ///     _ => default,
    /// }
    /// ```
    /// One helper, one truth, no `Int` branch to forget.
    #[inline]
    pub fn as_u32_clamped(&self, min: u32) -> Option<u32> {
        self.as_scalar()
            .map(|f| f.round().max(min as f32) as u32)
    }

    /// Borrow as a table. Returns `None` for non-Table variants.
    #[inline]
    pub fn as_table(&self) -> Option<&TableData> {
        match self {
            ParamValue::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Borrow as a string. Returns `None` for non-String variants.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ParamValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Static description of one parameter on an
/// [`EffectNode`](crate::node_graph::EffectNode).
#[derive(Debug, Clone)]
pub struct ParamDef {
    /// Stable parameter name. Wire-key for modulation bindings and the
    /// save format. Treated as public API once shipped — never rename in place.
    pub name: &'static str,

    /// Human-readable label for the effect card UI. Free to change.
    pub label: &'static str,

    pub ty: ParamType,
    pub default: ParamValue,

    /// Numeric range `(min, max)`. Applied to `Float` and `Int` types only.
    pub range: Option<(f32, f32)>,

    /// Discrete options for `ParamType::Enum`. Empty for non-Enum types.
    pub enum_values: &'static [&'static str],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_data_rejects_empty_or_ragged_rows() {
        assert!(TableData::new(vec![]).is_none(), "empty rejected");
        assert!(TableData::new(vec![vec![]]).is_none(), "zero-cols rejected");
        assert!(
            TableData::new(vec![vec![1.0, 2.0], vec![3.0]]).is_none(),
            "ragged rejected"
        );
    }

    #[test]
    fn table_data_round_trips_rows_and_cols() {
        let t = TableData::new(vec![vec![0.0, 90.0, 180.0], vec![45.0, 135.0, 225.0]]).unwrap();
        assert_eq!(t.row_count(), 2);
        assert_eq!(t.col_count(), 3);
        assert_eq!(t.row(0), Some(&[0.0, 90.0, 180.0][..]));
        assert_eq!(t.row(1), Some(&[45.0, 135.0, 225.0][..]));
        assert_eq!(t.row(2), None);
    }

    #[test]
    fn param_value_table_accessor_returns_only_for_table_variant() {
        let t = Arc::new(TableData::new(vec![vec![1.0]]).unwrap());
        assert!(ParamValue::Table(t.clone()).as_table().is_some());
        assert!(ParamValue::Float(0.5).as_table().is_none());
        assert!(ParamValue::Bool(true).as_table().is_none());
        assert!(ParamValue::Enum(0).as_table().is_none());
    }

    #[test]
    fn param_value_table_clone_shares_arc() {
        let t = Arc::new(TableData::new(vec![vec![1.0, 2.0]]).unwrap());
        let a = ParamValue::Table(Arc::clone(&t));
        let b = a.clone();
        // Both Param values point at the same underlying data — cheap clone.
        if let (ParamValue::Table(ta), ParamValue::Table(tb)) = (&a, &b) {
            assert!(Arc::ptr_eq(ta, tb), "clone must share Arc");
        } else {
            panic!("expected Table variants");
        }
    }
}
