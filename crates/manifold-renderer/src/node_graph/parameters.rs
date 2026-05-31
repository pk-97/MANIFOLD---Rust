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

/// Semantic *meaning* of a numeric parameter, orthogonal to its storage
/// [`ParamType`].
///
/// Where [`ParamType`] answers "how is this stored / what widget renders
/// it" (a presentation/storage concern), `ParamSemantic` answers "what
/// kind of quantity is this" (an authoring concern). A `Float` param can
/// be a `Gain`, a `Mix`, a `Radius` — same storage, different sensible
/// default range, response curve, and wrap behaviour.
///
/// This enum is the *infrastructure* for per-kind defaults. As of this
/// pass it is computed (derived from [`ParamType`] for `Angle` /
/// `Frequency`, `Plain` otherwise) but not yet applied anywhere — the
/// per-param kind assignments are a later fan-out. The one behavioural
/// consequence wired now is [`wrap_value`]: an `Angle` value begins
/// wrapping via `rem_euclid`, which is a no-op on the rendered result
/// because every angle consumer feeds it through `cos`/`sin` (2π-periodic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ParamSemantic {
    /// No special semantics — a plain 0..1 knob. The default for every
    /// param that doesn't declare otherwise.
    #[default]
    Plain,
    /// An angle in radians. Wraps over `[0, TAU)`.
    Angle,
    /// A hue in degrees. Wraps over `[-180, 180)` (stored span 360).
    Hue,
    /// An oscillator / cutoff frequency. Logarithmic response.
    Frequency,
    /// A normalized phase in `[0, 1)`. Wraps over `[0, 1)`.
    Phase,
    /// A scale factor. Logarithmic response around 1.0.
    Scale,
    /// A gain / amplitude multiplier. Exponential response.
    Gain,
    /// A 0..1 dry/wet or blend amount.
    Mix,
    /// A colour saturation multiplier.
    Saturation,
    /// An integer-ish count of things.
    Count,
    /// A radius / distance in pixels or world units. Logarithmic.
    Radius,
    /// A power / exponent term. Logarithmic.
    Power,
    /// A signed offset around zero.
    Offset,
    /// A duration in seconds. Logarithmic.
    Time,
}

/// Per-[`ParamSemantic`] authoring defaults: the sensible `(min, max)`
/// range, the response [`MacroCurve`] a slider should map through, and
/// whether the value wraps (and so should be normalized via
/// [`wrap_value`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticDefaults {
    pub range: (f32, f32),
    pub curve: manifold_core::macro_bank::MacroCurve,
    pub wraps: bool,
}

/// The authoring defaults for one [`ParamSemantic`]. One match, the
/// single source of truth for per-kind range / curve / wrap.
pub const fn semantic_defaults(k: ParamSemantic) -> SemanticDefaults {
    use manifold_core::macro_bank::MacroCurve::*;
    use std::f32::consts::TAU;
    match k {
        ParamSemantic::Plain => SemanticDefaults { range: (0.0, 1.0), curve: Linear, wraps: false },
        // Angles loop 0..2π; the slider edits 0..360° at the UI boundary.
        ParamSemantic::Angle => SemanticDefaults { range: (0.0, TAU), curve: Linear, wraps: true },
        ParamSemantic::Hue => SemanticDefaults { range: (-180.0, 180.0), curve: Linear, wraps: true },
        ParamSemantic::Frequency => {
            SemanticDefaults { range: (0.0, 100.0), curve: Logarithmic, wraps: false }
        }
        ParamSemantic::Phase => SemanticDefaults { range: (0.0, 1.0), curve: Linear, wraps: true },
        ParamSemantic::Scale => {
            SemanticDefaults { range: (0.1, 10.0), curve: Logarithmic, wraps: false }
        }
        ParamSemantic::Gain => SemanticDefaults { range: (0.0, 4.0), curve: Exponential, wraps: false },
        ParamSemantic::Mix => SemanticDefaults { range: (0.0, 1.0), curve: Linear, wraps: false },
        ParamSemantic::Saturation => {
            SemanticDefaults { range: (0.0, 2.0), curve: Linear, wraps: false }
        }
        ParamSemantic::Count => SemanticDefaults { range: (1.0, 64.0), curve: Linear, wraps: false },
        ParamSemantic::Radius => {
            SemanticDefaults { range: (0.0, 64.0), curve: Logarithmic, wraps: false }
        }
        ParamSemantic::Power => {
            SemanticDefaults { range: (1.0, 256.0), curve: Logarithmic, wraps: false }
        }
        ParamSemantic::Offset => SemanticDefaults { range: (-1.0, 1.0), curve: Linear, wraps: false },
        ParamSemantic::Time => {
            SemanticDefaults { range: (0.01, 10.0), curve: Logarithmic, wraps: false }
        }
    }
}

/// Normalize a value for a wrapping semantic.
///
/// For wrapping kinds the value is reduced via `rem_euclid` onto the
/// canonical span: `TAU` for [`Angle`], `360` for [`Hue`], `1.0` for
/// [`Phase`]. Non-wrapping kinds pass the value through unchanged.
///
/// `rem_euclid` (not `%`) so a negative input maps into `[0, span)`
/// rather than `(-span, 0]` — a value of `-0.1` rad becomes `TAU - 0.1`,
/// the geometrically-correct angle, not a negative one.
///
/// [`Angle`]: ParamSemantic::Angle
/// [`Hue`]: ParamSemantic::Hue
/// [`Phase`]: ParamSemantic::Phase
pub fn wrap_value(value: f32, k: ParamSemantic) -> f32 {
    if !semantic_defaults(k).wraps {
        return value;
    }
    let span = match k {
        ParamSemantic::Angle => std::f32::consts::TAU,
        ParamSemantic::Hue => 360.0,
        ParamSemantic::Phase => 1.0,
        // Unreachable: every wrapping kind is listed above. A new
        // wrapping kind must add its span here.
        _ => return value,
    };
    value.rem_euclid(span)
}

/// Derive the [`ParamSemantic`] implied by a [`ParamType`] when the
/// author hasn't declared one explicitly.
///
/// `Angle` / `Frequency` carry their semantic for free (the storage
/// type already encodes the meaning). Every other storage type maps to
/// [`ParamSemantic::Plain`] — the per-param kind assignment for those
/// (Gain, Mix, Radius, …) is the later fan-out, not this pass.
pub const fn kind_for_param_type(ty: ParamType) -> ParamSemantic {
    match ty {
        ParamType::Angle => ParamSemantic::Angle,
        ParamType::Frequency => ParamSemantic::Frequency,
        _ => ParamSemantic::Plain,
    }
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

    /// Semantic *meaning* of this parameter (angle, gain, mix, …),
    /// orthogonal to its storage [`ParamType`]. Auto-populated: the
    /// `primitive!` macro derives it from `ty` (`Angle` / `Frequency`
    /// carry their semantic for free, everything else defaults to
    /// [`ParamSemantic::Plain`]) unless the author declares a
    /// `semantic:` override. See [`ParamSemantic`].
    pub kind: ParamSemantic,
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

    // ── ParamSemantic: defaults table + wrap ──────────────────────────

    #[test]
    fn semantic_defaults_table_matches_authored_values() {
        use manifold_core::macro_bank::MacroCurve::*;
        use std::f32::consts::TAU;

        // (kind, range, curve, wraps)
        let cases: &[(ParamSemantic, (f32, f32), manifold_core::macro_bank::MacroCurve, bool)] = &[
            (ParamSemantic::Plain, (0.0, 1.0), Linear, false),
            (ParamSemantic::Angle, (0.0, TAU), Linear, true),
            (ParamSemantic::Hue, (-180.0, 180.0), Linear, true),
            (ParamSemantic::Frequency, (0.0, 100.0), Logarithmic, false),
            (ParamSemantic::Phase, (0.0, 1.0), Linear, true),
            (ParamSemantic::Scale, (0.1, 10.0), Logarithmic, false),
            (ParamSemantic::Gain, (0.0, 4.0), Exponential, false),
            (ParamSemantic::Mix, (0.0, 1.0), Linear, false),
            (ParamSemantic::Saturation, (0.0, 2.0), Linear, false),
            (ParamSemantic::Count, (1.0, 64.0), Linear, false),
            (ParamSemantic::Radius, (0.0, 64.0), Logarithmic, false),
            (ParamSemantic::Power, (1.0, 256.0), Logarithmic, false),
            (ParamSemantic::Offset, (-1.0, 1.0), Linear, false),
            (ParamSemantic::Time, (0.01, 10.0), Logarithmic, false),
        ];
        for &(k, range, curve, wraps) in cases {
            let d = semantic_defaults(k);
            assert_eq!(d.range, range, "range for {k:?}");
            assert_eq!(d.curve, curve, "curve for {k:?}");
            assert_eq!(d.wraps, wraps, "wraps for {k:?}");
        }
    }

    #[test]
    fn wrap_value_reduces_only_wrapping_kinds() {
        use std::f32::consts::TAU;

        // Angle wraps over [0, TAU).
        assert!((wrap_value(TAU + 0.5, ParamSemantic::Angle) - 0.5).abs() < 1e-5);
        // rem_euclid maps negatives into [0, span): -0.1 → TAU - 0.1.
        assert!((wrap_value(-0.1, ParamSemantic::Angle) - (TAU - 0.1)).abs() < 1e-5);
        // Hue wraps over span 360.
        assert!((wrap_value(540.0, ParamSemantic::Hue) - 180.0).abs() < 1e-3);
        // Phase wraps over span 1.0.
        assert!((wrap_value(1.25, ParamSemantic::Phase) - 0.25).abs() < 1e-6);

        // Non-wrapping kinds pass through untouched, even out of range.
        assert_eq!(wrap_value(7.5, ParamSemantic::Plain), 7.5);
        assert_eq!(wrap_value(-3.0, ParamSemantic::Gain), -3.0);
        assert_eq!(wrap_value(500.0, ParamSemantic::Frequency), 500.0);
    }

    #[test]
    fn kind_for_param_type_derives_angle_and_frequency_only() {
        assert_eq!(kind_for_param_type(ParamType::Angle), ParamSemantic::Angle);
        assert_eq!(
            kind_for_param_type(ParamType::Frequency),
            ParamSemantic::Frequency
        );
        assert_eq!(kind_for_param_type(ParamType::Float), ParamSemantic::Plain);
        assert_eq!(kind_for_param_type(ParamType::Int), ParamSemantic::Plain);
        assert_eq!(kind_for_param_type(ParamType::Enum), ParamSemantic::Plain);
        assert_eq!(kind_for_param_type(ParamType::Bool), ParamSemantic::Plain);
    }

    #[test]
    fn param_semantic_defaults_to_plain() {
        assert_eq!(ParamSemantic::default(), ParamSemantic::Plain);
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
