//! The five native controls for a material map's affine placement.
//!
//! Placement is a view over the six existing matrix slots.  The widget keeps
//! the matrix in its local gesture state and emits one compound action when a
//! gesture ends; it never introduces a second stored transform.

use crate::input::UIEvent;
use crate::node::{NodeId, Rect};
use crate::panels::{
    GraphParamTarget, MaterialEditKind, MaterialParamWrite, PanelAction, ProjectAction,
};
use crate::param_surface::{
    MaterialParamRole, ModifierObjectRef, ParamRow, RowIndex, RowRole, UvComponent,
};
use crate::slider::{BitmapSlider, SliderColors, SliderDragState};
use crate::tree::UITree;
use manifold_foundation::ParamId;
use manifold_foundation::uv_transform::{UvDecomposition, compose_uv_affine, decompose_uv_affine};

const PLACEMENT_ROWS: usize = 6;
const CONTROL_COUNT: usize = 5;
const MATRIX_EPSILON: f32 = 1.0e-5;
const MIN_NATIVE_SCALE: f32 = 1.0e-8;

use crate::param_surface::UvControl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementControl {
    OffsetU,
    OffsetV,
    Rotation,
    ScaleU,
    ScaleV,
}

impl PlacementControl {
    const ALL: [Self; CONTROL_COUNT] = [
        Self::OffsetU,
        Self::OffsetV,
        Self::Rotation,
        Self::ScaleU,
        Self::ScaleV,
    ];

    fn uv_control(self) -> UvControl {
        match self {
            Self::OffsetU => UvControl::OffsetU,
            Self::OffsetV => UvControl::OffsetV,
            Self::Rotation => UvControl::Rotation,
            Self::ScaleU => UvControl::ScaleU,
            Self::ScaleV => UvControl::ScaleV,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OffsetU => "Offset U",
            Self::OffsetV => "Offset V",
            Self::Rotation => "Rotation",
            Self::ScaleU => "Scale U",
            Self::ScaleV => "Scale V",
        }
    }
}

#[derive(Debug, Clone)]
struct PlacementSnapshot {
    target: GraphParamTarget,
    object: ModifierObjectRef,
    material: ModifierObjectRef,
    ids: [ParamId; PLACEMENT_ROWS],
    values: [f32; PLACEMENT_ROWS],
    decomposition: UvDecomposition,
    offset_bounds: [(f32, f32); 2],
    scale_bounds: [(f32, f32); 2],
    matrix_bounds: [(f32, f32); PLACEMENT_ROWS],
}

/// Shared material placement widget.  The widget is intentionally `pub(crate)`:
/// the inspector panel owns its lifetime while the app owns action dispatch.
pub(crate) struct MaterialPlacementWidget {
    snapshot: Option<PlacementSnapshot>,
    working: [f32; PLACEMENT_ROWS],
    controls: [SliderDragState; CONTROL_COUNT],
    slider_ids: [Option<crate::slider::SliderNodeIds>; CONTROL_COUNT],
    reset_actions: [Option<PanelAction>; CONTROL_COUNT],
    row_index: RowIndex,
    active_control: Option<usize>,
    reason: Option<String>,
}

impl Default for MaterialPlacementWidget {
    fn default() -> Self {
        Self {
            snapshot: None,
            working: [0.0; PLACEMENT_ROWS],
            controls: std::array::from_fn(|index| {
                let (min, max) = match index {
                    0 | 1 => (f32::NEG_INFINITY, f32::INFINITY),
                    2 => (-180.0, 180.0),
                    _ => (-1.0, 1.0),
                };
                SliderDragState::with_range(min, max, false)
            }),
            slider_ids: [None; CONTROL_COUNT],
            reset_actions: std::array::from_fn(|_| None),
            row_index: RowIndex::default(),
            active_control: None,
            reason: None,
        }
    }
}

impl MaterialPlacementWidget {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Reconcile the widget with the current six-slot material snapshot.
    /// `rows` must be ordered `[m00, m01, m10, m11, tx, ty]`.
    pub(crate) fn configure(
        &mut self,
        target: GraphParamTarget,
        object: ModifierObjectRef,
        material: ModifierObjectRef,
        rows: [&ParamRow; PLACEMENT_ROWS],
    ) {
        let incoming_values = std::array::from_fn(|index| rows[index].value.base);
        let context_changed = self.snapshot.as_ref().is_some_and(|old| {
            old.target != target || old.object != object || old.material != material
        });
        let matrix_changed_during_drag = self.active_control.is_some()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|old| !same_values(old.values, incoming_values));
        let ids_changed = self.snapshot.as_ref().is_some_and(|old| {
            rows.iter()
                .zip(old.ids.iter())
                .any(|(row, id)| row.id != *id)
        });
        if context_changed || matrix_changed_during_drag || ids_changed {
            self.cancel_drag();
            if context_changed || ids_changed {
                self.clear_controls();
            }
        }

        self.reason = placement_reason(rows);
        if self.reason.is_some() {
            self.cancel_drag();
            self.clear_controls();
            self.snapshot = None;
            return;
        }

        let Some(decomposition) = decompose_uv_affine(incoming_values) else {
            self.cancel_drag();
            self.clear_controls();
            self.reason = Some(
                "Advanced: this UV matrix contains shear, a singular axis, or non-finite values"
                    .into(),
            );
            self.snapshot = None;
            return;
        };
        let offset_bounds = [
            (rows[4].spec.min, rows[4].spec.max),
            (rows[5].spec.min, rows[5].spec.max),
        ];
        let matrix_bounds =
            std::array::from_fn(|index| (rows[index].spec.min, rows[index].spec.max));
        let scale_bounds = [
            coefficient_scale_bounds(&rows, true),
            coefficient_scale_bounds(&rows, false),
        ];
        if scale_bounds.iter().any(|(min, max)| min >= max)
            || !decomposition_is_native(decomposition, offset_bounds, scale_bounds)
            || !values_within_bounds(incoming_values, matrix_bounds)
        {
            self.cancel_drag();
            self.clear_controls();
            self.reason =
                Some("Advanced: this UV placement is outside the native control ranges".into());
            self.snapshot = None;
            return;
        }

        let ids = std::array::from_fn(|index| rows[index].id.clone());
        let keep_live_values = self.active_control.is_some()
            && !context_changed
            && !matrix_changed_during_drag
            && !ids_changed;
        if !keep_live_values {
            self.working = incoming_values;
        }
        self.reason = None;
        self.snapshot = Some(PlacementSnapshot {
            target,
            object,
            material,
            ids,
            values: incoming_values,
            decomposition,
            offset_bounds,
            scale_bounds,
            matrix_bounds,
        });
    }

    /// Build the five native controls and return the first y after them.
    pub(crate) fn build(
        &mut self,
        tree: &mut UITree,
        parent: Option<NodeId>,
        rect: Rect,
        key_base: u64,
    ) -> f32 {
        self.row_index.clear();
        self.slider_ids = [None; CONTROL_COUNT];
        self.reset_actions = std::array::from_fn(|_| None);
        let Some(snapshot) = self.snapshot.as_ref() else {
            return rect.y;
        };

        let values = decomposition_values(
            decompose_uv_affine(self.working).unwrap_or(snapshot.decomposition),
        );
        let ranges = control_ranges(snapshot);
        let colors = SliderColors::default_slider();
        let label_width = crate::slider::label_width_for_row(rect.width);
        let row_height = super::ROW_HEIGHT;
        for (index, control) in PlacementControl::ALL.into_iter().enumerate() {
            let row_rect = Rect::new(
                rect.x,
                rect.y + index as f32 * (row_height + super::ROW_SPACING),
                rect.width,
                row_height,
            );
            let value = control_value(values, control);
            let (min, max) = ranges[index];
            self.controls[index].set_range(min, max, false);
            let reset = self.reset_action(control);
            let slider = super::build_row_slider(
                tree,
                parent,
                row_rect,
                control.label(),
                BitmapSlider::value_to_normalized(value, min, max),
                format_control_value(control, value).as_str(),
                &colors,
                label_width,
                BitmapSlider::value_to_normalized(default_control_value(control), min, max),
                reset,
                Some(placement_key(key_base, &snapshot.ids[0], control)),
            );
            self.controls[index].set_ids(slider.ids);
            self.slider_ids[index] = Some(slider.ids);
            self.reset_actions[index] = Some(slider.reset.clone());
            tree.set_name(
                slider.ids.track,
                placement_name(&snapshot.ids[0], control, "slider").into_boxed_str(),
            );
            tree.set_name(
                slider.ids.value_text,
                placement_name(&snapshot.ids[0], control, "value").into_boxed_str(),
            );
            let role = RowRole::MaterialPlacement(control.uv_control());
            self.row_index
                .insert(tree.widget_of(slider.ids.track), index, role);
            self.row_index
                .insert(tree.widget_of(slider.ids.value_text), index, role);
            if let Some(label) = slider.ids.label {
                self.row_index.insert(tree.widget_of(label), index, role);
            }
        }
        rect.y
            + CONTROL_COUNT as f32 * row_height
            + (CONTROL_COUNT.saturating_sub(1)) as f32 * super::ROW_SPACING
    }

    /// Handle the native slider gesture. A release returns exactly one
    /// `MaterialParamsSet` action when the composed matrix changed.
    pub(crate) fn handle_event(
        &mut self,
        event: &UIEvent,
        tree: &mut UITree,
    ) -> Option<Vec<PanelAction>> {
        self.snapshot.as_ref()?;
        match event {
            UIEvent::RightClick {
                node_id: Some(node_id),
                ..
            } => {
                let index = self.resolve_index(tree, *node_id)?;
                let reset = self.reset_actions[index].as_ref()?.clone();
                let reset_values = self.reset_values(index);
                if same_values(self.current_values(), reset_values) {
                    return Some(Vec::new());
                }
                Some(vec![reset])
            }
            UIEvent::PointerDown { node_id, pos, .. }
            | UIEvent::DragBegin {
                node_id: Some(node_id),
                pos,
                ..
            } => {
                let index = self.resolve_index(tree, *node_id)?;
                if self.slider_ids[index].is_some_and(|ids| ids.track != *node_id) {
                    return None;
                }
                self.start_drag(index, *pos, tree);
                Some(Vec::new())
            }
            UIEvent::Drag { pos, .. } => {
                let index = self.active_control?;
                let value = self.controls[index].apply_drag(pos.x, tree, &|value| {
                    format_control_value(PlacementControl::ALL[index], value)
                })?;
                let previous = decompose_uv_affine(self.working)
                    .map(|decomposition| {
                        control_value(
                            decomposition_values(decomposition),
                            PlacementControl::ALL[index],
                        )
                    })
                    .unwrap_or(value);
                if !self.set_component(index, value, tree) {
                    self.controls[index].sync(tree, previous, &|value| {
                        format_control_value(PlacementControl::ALL[index], value)
                    });
                }
                Some(Vec::new())
            }
            UIEvent::PointerUp { .. } | UIEvent::DragEnd { .. } => self.finish_drag(),
            _ => None,
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.active_control.is_some()
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    fn resolve_index(&self, tree: &UITree, node_id: NodeId) -> Option<usize> {
        self.row_index
            .get(tree.widget_of(node_id))
            .map(|(index, _)| index)
    }

    fn start_drag(&mut self, index: usize, pos: crate::node::Vec2, tree: &mut UITree) {
        if let Some(previous) = self.active_control.take() {
            self.controls[previous].end_drag();
        }
        let Some(ids) = self.slider_ids[index] else {
            return;
        };
        let Some(value) = self.controls[index].try_start_drag(ids.track, pos.x) else {
            return;
        };
        self.active_control = Some(index);
        let previous = decompose_uv_affine(self.working)
            .map(|decomposition| {
                control_value(
                    decomposition_values(decomposition),
                    PlacementControl::ALL[index],
                )
            })
            .unwrap_or(value);
        if !self.set_component(index, value, tree) {
            self.controls[index].sync(tree, previous, &|value| {
                format_control_value(PlacementControl::ALL[index], value)
            });
        } else {
            self.controls[index].apply_drag_custom(
                value,
                BitmapSlider::value_to_normalized(
                    value,
                    self.controls[index].min,
                    self.controls[index].max,
                ),
                tree,
                &format_control_value(PlacementControl::ALL[index], value),
            );
        }
    }

    fn set_component(&mut self, index: usize, value: f32, tree: &mut UITree) -> bool {
        if self.snapshot.is_none() {
            return false;
        }
        let Some(mut decomposition) = decompose_uv_affine(self.working) else {
            return false;
        };
        apply_control_value(&mut decomposition, PlacementControl::ALL[index], value);
        let Some(snapshot) = self.snapshot.as_ref() else {
            return false;
        };
        if !decomposition_is_native(decomposition, snapshot.offset_bounds, snapshot.scale_bounds) {
            return false;
        }
        let composed = compose_uv_affine(decomposition);
        if !values_within_bounds(composed, snapshot.matrix_bounds) {
            return false;
        }
        self.working = composed;
        if let Some(ids) = self.slider_ids[index] {
            let display = control_value(
                decomposition_values(decomposition),
                PlacementControl::ALL[index],
            );
            BitmapSlider::update_value(
                tree,
                &ids,
                BitmapSlider::value_to_normalized(
                    display,
                    self.controls[index].min,
                    self.controls[index].max,
                ),
                &format_control_value(PlacementControl::ALL[index], display),
            );
        }
        true
    }

    fn finish_drag(&mut self) -> Option<Vec<PanelAction>> {
        let index = self.active_control.take()?;
        self.controls[index].end_drag();
        let snapshot = self.snapshot.as_ref()?;
        if same_values(snapshot.values, self.working) {
            return Some(Vec::new());
        }
        Some(vec![self.material_action(self.working)])
    }

    fn cancel_drag(&mut self) {
        for control in &mut self.controls {
            control.end_drag();
        }
        self.active_control = None;
    }

    fn clear_controls(&mut self) {
        for control in &mut self.controls {
            control.clear();
        }
        self.slider_ids = [None; CONTROL_COUNT];
        self.reset_actions = std::array::from_fn(|_| None);
        self.row_index.clear();
    }

    fn current_values(&self) -> [f32; PLACEMENT_ROWS] {
        self.working
    }

    fn reset_values(&self, index: usize) -> [f32; PLACEMENT_ROWS] {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return self.working;
        };
        let mut decomposition = snapshot.decomposition;
        apply_control_value(
            &mut decomposition,
            PlacementControl::ALL[index],
            default_control_value(PlacementControl::ALL[index]),
        );
        compose_uv_affine(decomposition)
    }

    fn reset_action(&self, control: PlacementControl) -> PanelAction {
        self.material_action(self.reset_values_for(control))
    }

    fn reset_values_for(&self, control: PlacementControl) -> [f32; PLACEMENT_ROWS] {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return self.working;
        };
        let mut decomposition = snapshot.decomposition;
        apply_control_value(&mut decomposition, control, default_control_value(control));
        compose_uv_affine(decomposition)
    }

    fn material_action(&self, values: [f32; PLACEMENT_ROWS]) -> PanelAction {
        let snapshot = self
            .snapshot
            .as_ref()
            .expect("placement action requires configuration");
        PanelAction::Project(ProjectAction::MaterialParamsSet {
            target: snapshot.target.clone(),
            object: snapshot.object.clone(),
            material: snapshot.material.clone(),
            kind: MaterialEditKind::Placement,
            writes: snapshot
                .ids
                .iter()
                .cloned()
                .zip(values)
                .map(|(param_id, value)| MaterialParamWrite { param_id, value })
                .collect(),
            description: "Set material placement".into(),
        })
    }
}

fn placement_reason(rows: [&ParamRow; PLACEMENT_ROWS]) -> Option<String> {
    let expected = [
        UvComponent::M00,
        UvComponent::M01,
        UvComponent::M10,
        UvComponent::M11,
        UvComponent::Tx,
        UvComponent::Ty,
    ];
    let mut family = None;
    for (row, component) in rows.into_iter().zip(expected) {
        let Some(MaterialParamRole::Placement(row_family, row_component)) = row.spec.material_role
        else {
            return Some(
                "Advanced: placement requires the six canonical material matrix roles".into(),
            );
        };
        if row_component != component {
            return Some("Advanced: material placement rows are not in matrix order".into());
        }
        if family.is_some_and(|known| known != row_family) {
            return Some("Advanced: placement rows must belong to one map family".into());
        }
        family = Some(row_family);
        if row.spec.disabled.is_some()
            || row.spec.is_toggle
            || row.spec.is_trigger
            || row.spec.value_labels.is_some()
            || !row.value.exposed
            || row.value.driven
            || row.mapping.ableton_display.is_some()
            || row.mapping.ableton_range.is_some()
            || row.modulation.driver_active
            || row.modulation.envelope_active
            || row.modulation.automation_active
            || row.material_attached
            || row.spec.inactive_reason.is_some()
        {
            return Some(format!(
                "Advanced: {} is attached or driven",
                component_label(component)
            ));
        }
        if !row.value.base.is_finite()
            || !row.spec.min.is_finite()
            || !row.spec.max.is_finite()
            || row.spec.min > row.spec.max
        {
            return Some(format!(
                "Advanced: {} has non-finite or invalid bounds",
                component_label(component)
            ));
        }
    }
    None
}

fn component_label(component: UvComponent) -> &'static str {
    match component {
        UvComponent::M00 => "m00",
        UvComponent::M01 => "m01",
        UvComponent::M10 => "m10",
        UvComponent::M11 => "m11",
        UvComponent::Tx => "offset U",
        UvComponent::Ty => "offset V",
    }
}

fn decomposition_values(decomposition: UvDecomposition) -> [f32; 5] {
    [
        decomposition.offset[0],
        decomposition.offset[1],
        decomposition.rotation_radians.to_degrees(),
        decomposition.scale[0],
        decomposition.scale[1],
    ]
}

fn control_value(values: [f32; 5], control: PlacementControl) -> f32 {
    values[match control {
        PlacementControl::OffsetU => 0,
        PlacementControl::OffsetV => 1,
        PlacementControl::Rotation => 2,
        PlacementControl::ScaleU => 3,
        PlacementControl::ScaleV => 4,
    }]
}

fn apply_control_value(decomposition: &mut UvDecomposition, control: PlacementControl, value: f32) {
    match control {
        PlacementControl::OffsetU => decomposition.offset[0] = value,
        PlacementControl::OffsetV => decomposition.offset[1] = value,
        PlacementControl::Rotation => decomposition.rotation_radians = value.to_radians(),
        PlacementControl::ScaleU => decomposition.scale[0] = value,
        PlacementControl::ScaleV => decomposition.scale[1] = value,
    }
}

fn default_control_value(control: PlacementControl) -> f32 {
    match control {
        PlacementControl::OffsetU | PlacementControl::OffsetV | PlacementControl::Rotation => 0.0,
        PlacementControl::ScaleU | PlacementControl::ScaleV => 1.0,
    }
}

fn format_control_value(control: PlacementControl, value: f32) -> String {
    match control {
        PlacementControl::Rotation => format!("{value:.0}°"),
        _ => format!("{value:.2}"),
    }
}

fn decomposition_is_native(
    decomposition: UvDecomposition,
    offset_bounds: [(f32, f32); 2],
    scale_bounds: [(f32, f32); 2],
) -> bool {
    decomposition
        .offset
        .iter()
        .zip(offset_bounds)
        .all(|(value, (min, max))| {
            value.is_finite()
                && min.is_finite()
                && max.is_finite()
                && *value >= min
                && *value <= max
        })
        && decomposition.rotation_radians.is_finite()
        && decomposition.rotation_radians.to_degrees().abs() <= 180.0 + MATRIX_EPSILON
        && decomposition
            .scale
            .iter()
            .zip(scale_bounds)
            .all(|(value, (min, max))| {
                value.is_finite()
                    && *value >= min
                    && *value <= max
                    && value.abs() >= MIN_NATIVE_SCALE
            })
}

fn control_ranges(snapshot: &PlacementSnapshot) -> [(f32, f32); CONTROL_COUNT] {
    [
        snapshot.offset_bounds[0],
        snapshot.offset_bounds[1],
        (-180.0, 180.0),
        snapshot.scale_bounds[0],
        snapshot.scale_bounds[1],
    ]
}

fn coefficient_scale_bounds(rows: &[&ParamRow; PLACEMENT_ROWS], first_column: bool) -> (f32, f32) {
    let indices = if first_column { [0, 2] } else { [1, 3] };
    let limit = indices
        .into_iter()
        .map(|index| rows[index].spec.min.abs().max(rows[index].spec.max.abs()))
        .fold(f32::INFINITY, f32::min);
    (-limit, limit)
}

fn placement_key(key_base: u64, matrix_id: &ParamId, control: PlacementControl) -> u64 {
    // BitmapSlider reserves low bits for its label, track and value nodes.
    (crate::param_surface::stable_key(&placement_name(matrix_id, control, "slider")) ^ key_base)
        << 8
}

fn placement_name(matrix_id: &ParamId, control: PlacementControl, element: &str) -> String {
    let suffix = match control {
        PlacementControl::OffsetU => "offset_u",
        PlacementControl::OffsetV => "offset_v",
        PlacementControl::Rotation => "rotation",
        PlacementControl::ScaleU => "scale_u",
        PlacementControl::ScaleV => "scale_v",
    };
    format!("material.placement.{matrix_id}.{suffix}.{element}")
}

fn same_values(left: [f32; PLACEMENT_ROWS], right: [f32; PLACEMENT_ROWS]) -> bool {
    left.into_iter()
        .zip(right)
        .all(|(a, b)| (a - b).abs() <= MATRIX_EPSILON)
}

fn values_within_bounds(
    values: [f32; PLACEMENT_ROWS],
    bounds: [(f32, f32); PLACEMENT_ROWS],
) -> bool {
    values.into_iter().zip(bounds).all(|(value, (min, max))| {
        value.is_finite() && min.is_finite() && max.is_finite() && value >= min && value <= max
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;
    use crate::panels::param_card::RowMod;
    use crate::panels::param_slider_shared::AudioRowState;
    use crate::param_surface::{MaterialMapFamily, RowMapping, RowSpec, RowValue};
    use std::borrow::Cow;

    fn fixture_rows(values: [f32; 6]) -> [ParamRow; 6] {
        let components = [
            UvComponent::M00,
            UvComponent::M01,
            UvComponent::M10,
            UvComponent::M11,
            UvComponent::Tx,
            UvComponent::Ty,
        ];
        std::array::from_fn(|index| ParamRow {
            id: Cow::Owned(format!("placement-{index}")),
            spec: RowSpec {
                name: format!("placement-{index}"),
                min: if index < 4 { -8.0 } else { -4.0 },
                max: if index < 4 { 8.0 } else { 4.0 },
                default: 0.0,
                whole_numbers: false,
                is_angle: false,
                is_toggle: false,
                is_trigger: false,
                is_trigger_gate: false,
                value_labels: None,
                section: None,
                disabled: None,
                material_role: Some(MaterialParamRole::Placement(
                    MaterialMapFamily::Base,
                    components[index],
                )),
                inactive_reason: None,
            },
            value: RowValue {
                base: values[index],
                effective: values[index],
                exposed: true,
                driven: false,
            },
            audio: AudioRowState::default(),
            modulation: RowMod::default(),
            mapping: RowMapping {
                osc_address: Some(format!("/material/{index}")),
                ableton_display: None,
                ableton_range: None,
                mappable: true,
            },
            scene_addr: None,
            rgb_members: None,
            material_attached: false,
        })
    }

    fn fixture_context() -> (GraphParamTarget, ModifierObjectRef, ModifierObjectRef) {
        (
            GraphParamTarget::Generator,
            ModifierObjectRef {
                scope: vec![manifold_foundation::NodeId::new("scope")],
                node: manifold_foundation::NodeId::new("object"),
            },
            ModifierObjectRef {
                scope: vec![manifold_foundation::NodeId::new("scope")],
                node: manifold_foundation::NodeId::new("material"),
            },
        )
    }

    fn configure_fixture(widget: &mut MaterialPlacementWidget, rows: &[ParamRow; 6]) {
        let (target, object, material) = fixture_context();
        widget.configure(
            target,
            object,
            material,
            std::array::from_fn(|index| &rows[index]),
        );
    }

    #[test]
    fn material_inspector_placement_control_order_is_stable() {
        assert_eq!(PlacementControl::ALL.len(), 5);
        assert_eq!(PlacementControl::OffsetU.label(), "Offset U");
        assert_eq!(PlacementControl::Rotation.label(), "Rotation");
        assert_eq!(default_control_value(PlacementControl::ScaleV), 1.0);
    }

    #[test]
    fn material_inspector_placement_defaults_compose_identity() {
        let decomposition = UvDecomposition {
            offset: [0.0, 0.0],
            rotation_radians: 0.0,
            scale: [1.0, 1.0],
        };
        assert_eq!(
            compose_uv_affine(decomposition),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn material_inspector_placement_rejects_zero_scale_during_conversion() {
        let mut decomposition = UvDecomposition {
            offset: [0.0, 0.0],
            rotation_radians: 0.0,
            scale: [1.0, 1.0],
        };
        apply_control_value(&mut decomposition, PlacementControl::ScaleU, 0.0);
        assert_eq!(decomposition.scale[0], 0.0);
        assert!(!decomposition_is_native(
            decomposition,
            [(-4.0, 4.0); 2],
            [(-8.0, 8.0); 2]
        ));
    }

    #[test]
    fn material_inspector_placement_normal_rows_with_mapping_are_eligible() {
        let rows = fixture_rows([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let mut widget = MaterialPlacementWidget::new();
        configure_fixture(&mut widget, &rows);
        assert_eq!(widget.reason(), None);
    }

    #[test]
    fn material_inspector_placement_gesture_commits_all_six_existing_ids() {
        let rows = fixture_rows([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let mut widget = MaterialPlacementWidget::new();
        configure_fixture(&mut widget, &rows);
        let mut tree = UITree::new();
        widget.build(&mut tree, None, Rect::new(0.0, 0.0, 420.0, 24.0), 7);
        let track = widget.slider_ids[0].expect("offset U slider").track;
        let bounds = tree.get_bounds(track);
        let modifiers = Modifiers::default();
        assert!(
            widget
                .handle_event(
                    &UIEvent::PointerDown {
                        node_id: track,
                        pos: crate::node::Vec2::new(bounds.x, bounds.y),
                        modifiers
                    },
                    &mut tree
                )
                .is_some_and(|actions| actions.is_empty())
        );
        assert!(widget.is_dragging());
        assert!(
            widget
                .handle_event(
                    &UIEvent::Drag {
                        node_id: None,
                        pos: crate::node::Vec2::new(bounds.x + bounds.width * 0.75, bounds.y),
                        delta: crate::node::Vec2::new(0.0, 0.0),
                        modifiers
                    },
                    &mut tree
                )
                .is_some_and(|actions| actions.is_empty())
        );
        let actions = widget
            .handle_event(
                &UIEvent::PointerUp {
                    node_id: Some(track),
                    pos: crate::node::Vec2::new(bounds.x + bounds.width * 0.75, bounds.y),
                },
                &mut tree,
            )
            .expect("release action");
        assert_eq!(actions.len(), 1);
        let PanelAction::Project(ProjectAction::MaterialParamsSet { writes, .. }) = &actions[0]
        else {
            panic!("expected material batch")
        };
        assert_eq!(writes.len(), 6);
        assert!(
            writes
                .iter()
                .zip(rows.iter())
                .all(|(write, row)| write.param_id == row.id)
        );
    }

    #[test]
    fn material_inspector_placement_opening_and_noop_emit_zero_actions() {
        let rows = fixture_rows([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let mut widget = MaterialPlacementWidget::new();
        configure_fixture(&mut widget, &rows);
        let mut tree = UITree::new();
        widget.build(&mut tree, None, Rect::new(0.0, 0.0, 420.0, 24.0), 7);
        assert!(
            widget
                .handle_event(
                    &UIEvent::PointerUp {
                        node_id: None,
                        pos: crate::node::Vec2::new(0.0, 0.0)
                    },
                    &mut tree
                )
                .is_none()
        );
        let track = widget.slider_ids[0].expect("offset U slider").track;
        let bounds = tree.get_bounds(track);
        let pos = crate::node::Vec2::new(bounds.x + bounds.width * 0.5, bounds.y);
        widget.handle_event(
            &UIEvent::PointerDown {
                node_id: track,
                pos,
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(
            widget
                .handle_event(
                    &UIEvent::PointerUp {
                        node_id: Some(track),
                        pos
                    },
                    &mut tree
                )
                .is_some_and(|actions| actions.is_empty())
        );
    }

    #[test]
    fn material_inspector_placement_context_change_cancels_drag() {
        let rows = fixture_rows([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let mut widget = MaterialPlacementWidget::new();
        configure_fixture(&mut widget, &rows);
        let mut tree = UITree::new();
        widget.build(&mut tree, None, Rect::new(0.0, 0.0, 420.0, 24.0), 7);
        let track = widget.slider_ids[0].expect("offset U slider").track;
        let bounds = tree.get_bounds(track);
        widget.handle_event(
            &UIEvent::PointerDown {
                node_id: track,
                pos: crate::node::Vec2::new(bounds.x, bounds.y),
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(widget.is_dragging());
        let (target, mut object, material) = fixture_context();
        object.node = manifold_foundation::NodeId::new("other-object");
        widget.configure(
            target,
            object,
            material,
            std::array::from_fn(|index| &rows[index]),
        );
        assert!(!widget.is_dragging());
    }

    #[test]
    fn material_inspector_placement_shear_is_advanced_only() {
        let rows = fixture_rows([1.0, 0.25, 0.0, 1.0, 0.0, 0.0]);
        let mut widget = MaterialPlacementWidget::new();
        configure_fixture(&mut widget, &rows);
        assert!(
            widget
                .reason()
                .is_some_and(|reason| reason.starts_with("Advanced:"))
        );
    }
}
