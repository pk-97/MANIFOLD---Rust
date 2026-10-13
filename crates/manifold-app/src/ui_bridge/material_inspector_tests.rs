//! Material inspector behaviour on a real glTF import, driven through the
//! production UI path headlessly: `UIRoot` build, real pointer events,
//! `ui_bridge::dispatch`, and a real undo stack. CPU only; no GPU device.
use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};
use manifold_editing::command::Command;
use manifold_editing::commands::layer::ImportModelLayerCommand;
use manifold_editing::undo::UndoRedoManager;
use manifold_ui::automation::{self, AutomationTarget, MatchInfo, SelectorQuery};
use manifold_ui::input::{Modifiers, PointerAction};
use manifold_ui::node::Vec2;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::ui_bridge::{push_state, sync_inspector_data, sync_project_data, TransportDisplayCache};
use crate::ui_root::UIRoot;

/// The imported azalea's first submesh object, as the outliner labels it.
const OBJECT_ROW: &str = "■ QS1694-W02-1-1";

struct Harness {
    ui: UIRoot,
    project: Project,
    selection: manifold_ui::UIState,
    active_layer: Option<manifold_core::LayerId>,
    content: ContentState,
    content_tx: crossbeam_channel::Sender<ContentCommand>,
    content_rx: crossbeam_channel::Receiver<ContentCommand>,
    undo: UndoRedoManager,
    prefs: crate::user_prefs::UserPrefs,
    scrub: crate::ui_bridge::ScrubState,
}

fn name(n: &str) -> SelectorQuery {
    SelectorQuery { name: Some(n.into()), ..Default::default() }
}

fn text(t: &str) -> SelectorQuery {
    SelectorQuery { text: Some(t.into()), ..Default::default() }
}

impl Harness {
    /// The same import sequence as the `gltfscene` ui-snap fixture and
    /// `Application::import_model_file`, then Scene tab, expand objects,
    /// select the first submesh.
    fn gltf_object_selected() -> Self {
        let mut h = Self::gltf_scene();
        h.click(&text("Scene"));
        h.click(&name("scene_setup.objects.group_toggle"));
        h.click(&text(OBJECT_ROW));
        h
    }

    fn gltf_scene() -> Self {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
        let (def, _) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&path)
            .expect("azalea fixture imports");
        let display_name = def
            .preset_metadata
            .as_ref()
            .map(|m| m.display_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Azalea".to_string());
        let embedded = EmbeddedPreset {
            kind: manifold_core::preset_def::PresetKind::Generator,
            def,
            origin: EmbeddedOrigin::Saved,
        };
        crate::project_io::install_embedded_presets(std::slice::from_ref(&embedded));
        let mut project = Project::default();
        let mut cmd = ImportModelLayerCommand::new(display_name, embedded, 0, None);
        cmd.execute(&mut project);
        project.reconcile_param_manifests();
        let layer = cmd.inserted_layer_id().expect("layer inserted");
        let mut selection = manifold_ui::UIState::default();
        selection.select_layer(layer.clone());
        let mut ui = UIRoot::new();
        // Tall enough that every material row is built without scrolling.
        ui.resize(1920.0, 12000.0);
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let mut h = Self {
            ui,
            project,
            selection,
            active_layer: Some(layer),
            content: ContentState::default(),
            content_tx,
            content_rx,
            undo: UndoRedoManager::new(),
            prefs: crate::user_prefs::UserPrefs::in_memory(),
            scrub: crate::ui_bridge::ScrubState::default(),
        };
        h.rebuild();
        h
    }

    fn active_index(&self) -> Option<usize> {
        self.active_layer
            .as_ref()
            .and_then(|id| self.project.timeline.find_layer_index_by_id(id))
    }

    fn rebuild(&mut self) {
        let active = self.active_index();
        sync_project_data(&mut self.ui, &self.project, active, &self.selection);
        sync_inspector_data(
            &mut self.ui,
            &self.project,
            active,
            &self.selection,
            &self.content.automation_latched_params,
            self.content.led_preview.as_ref(),
        );
        self.ui.build();
        if self.ui.inspector.skip_to_settled(&mut self.ui.tree)
            | self.ui.scene_setup_panel.skip_cards_to_settled(&mut self.ui.tree)
        {
            self.ui.build();
        }
        let mut cache = TransportDisplayCache::new();
        push_state(
            &mut self.ui,
            &self.project,
            &self.content,
            active,
            &self.selection,
            false,
            None,
            &mut cache,
        );
        self.ui.update();
    }

    fn all(&self, query: &SelectorQuery) -> Vec<MatchInfo> {
        automation::resolve_all(&self.ui.tree, &[], &AutomationTarget::Query(query.clone())).0
    }

    fn count(&self, query: &SelectorQuery) -> usize {
        self.all(query).len()
    }

    fn one(&self, query: &SelectorQuery) -> MatchInfo {
        let mut matches = self.all(query);
        assert_eq!(matches.len(), 1, "exactly one match for {query:?}");
        matches.remove(0)
    }

    fn text_of(&self, query: &SelectorQuery) -> String {
        self.one(query).text.unwrap_or_default()
    }

    fn value(&self, param: &str) -> String {
        self.text_of(&name(&format!("param_row.10_{param}.value")))
    }

    fn above(&self, upper: &SelectorQuery, lower: &SelectorQuery) -> bool {
        self.one(upper).rect.y_max() <= self.one(lower).rect.y
    }

    fn center(&self, query: &SelectorQuery) -> Vec2 {
        let r = self.one(query).rect;
        Vec2::new(r.x + r.width * 0.5, r.y + r.height * 0.5)
    }

    fn click(&mut self, query: &SelectorQuery) {
        let at = self.center(query);
        self.ui.input.set_modifiers(Modifiers::NONE);
        self.ui.pointer_event(at, PointerAction::Down, 0.0);
        self.ui.pointer_event(at, PointerAction::Up, 0.0);
        self.dispatch();
    }

    fn drag_to(&mut self, query: &SelectorQuery, to: Vec2, steps: u32) {
        let from = self.center(query);
        self.ui.input.set_modifiers(Modifiers::NONE);
        self.ui.pointer_event(from, PointerAction::Down, 0.0);
        for point in automation::interpolate_drag(from, to, steps) {
            self.ui.pointer_event(point, PointerAction::Move, 0.0);
        }
        self.ui.pointer_event(to, PointerAction::Up, 0.0);
        self.dispatch();
    }

    fn dispatch(&mut self) {
        let actions = self.ui.process_events();
        for action in &actions {
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project: &mut self.project,
                content_tx: &self.content_tx,
                content_state: &self.content,
                ui: &mut self.ui,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.prefs,
                editor_target: None,
                scrub: &mut self.scrub,
                dialogs: Default::default(),
            };
            crate::ui_bridge::dispatch(action, &mut ctx);
        }
        // The bridge already applied Execute commands to the local project;
        // record them so undo has the real units. Content-owned commands run here.
        while let Ok(cmd) = self.content_rx.try_recv() {
            match cmd {
                ContentCommand::Execute(cmd) => self.undo.record(cmd),
                ContentCommand::ExecuteOnContent(cmd) => {
                    self.undo.execute(cmd, &mut self.project);
                    assert_eq!(self.undo.take_rejection(), None);
                }
                ContentCommand::ExecuteBatch(cmds, desc) => self.undo.record(Box::new(
                    manifold_editing::command::CompositeCommand::new(cmds, desc),
                )),
                _ => {}
            }
        }
        self.rebuild();
    }

    fn undo(&mut self) {
        assert!(self.undo.undo(&mut self.project), "undo had a unit to undo");
        self.rebuild();
    }

    fn redo(&mut self) {
        assert!(self.undo.redo(&mut self.project), "redo had a unit to redo");
        self.rebuild();
    }
}

#[test]
fn material_inspector_groups_show_material_sections_and_roughness_drag_undoes() {
    let mut h = Harness::gltf_object_selected();
    assert_eq!(h.count(&text("Object gain")), 0);
    assert!(h.count(&text("Surface")) > 0);
    assert!(h.count(&text("Opacity & Cutout")) > 0);
    assert_eq!(h.count(&name("param_row.10_uv_m00.slider")), 0);

    h.drag_to(&name("param_row.10_roughness.slider"), Vec2::new(0.0, 400.0), 8);
    assert_eq!(h.value("roughness"), "0.01");
    h.undo();
    assert_eq!(h.value("roughness"), "1.00");
    // The mapping drawer button stays reachable after undo.
    assert_eq!(h.count(&name("param_row.10_roughness.driver_btn")), 1);
}

#[test]
fn material_inspector_coated_look_applies_undoes_redoes_and_default_restores() {
    let mut h = Harness::gltf_object_selected();
    h.click(&text("Coated"));
    assert_eq!(h.value("roughness"), "0.35");
    assert_eq!(h.value("clearcoat"), "1.00");
    h.undo();
    assert_eq!(h.value("roughness"), "1.00");
    h.redo();
    assert_eq!(h.value("roughness"), "0.35");
    h.click(&text("Default"));
    assert_eq!(h.value("roughness"), "1.00");
    h.undo();
    assert_eq!(h.value("roughness"), "0.35");
}

#[test]
fn material_inspector_textures_expose_no_placement_or_sampler_controls() {
    let mut h = Harness::gltf_scene();
    h.click(&text("Scene"));
    for absent in [
        "material.section.10_uv_m00",
        "param_row.10_uv_tx.slider",
        "param_row.10_nrm_uv_m00.slider",
        "param_row.10_mr_uv_m00.slider",
        "param_row.10_occ_uv_m00.slider",
        "param_row.10_em_uv_m00.slider",
        "param_row.10_wrap_u.slider",
        "material.placement.10_uv_m00.offset_u.slider",
    ] {
        assert_eq!(h.count(&name(absent)), 0, "{absent} must not be in the Scene dock");
    }
}

#[test]
fn material_inspector_features_add_toggle_remove_and_undo() {
    let mut h = Harness::gltf_object_selected();
    let coat = name("param_row.10_coat_mode.value");
    let clearcoat = name("param_row.10_clearcoat.value");
    let iridescence = name("param_row.10_iridescence_mode.value");
    let add_glass = text("+ Add Glass");

    h.click(&text("+ Add Coat"));
    assert!(h.above(&coat, &clearcoat));
    assert_eq!(h.value("coat_mode"), "On");
    assert_eq!(h.value("clearcoat"), "1.00");
    h.click(&coat);
    assert_eq!(h.value("coat_mode"), "Off");
    assert_eq!(h.value("clearcoat"), "1.00");
    h.undo();
    assert_eq!(h.value("coat_mode"), "On");
    h.undo();
    assert_eq!(h.value("coat_mode"), "Auto");

    h.click(&text("+ Add Iridescence"));
    assert!(h.above(&coat, &iridescence));
    assert!(h.above(&iridescence, &add_glass));
    h.click(&name("material.feature.remove.10_coat_mode"));
    assert_eq!(h.count(&coat), 0);
    assert_eq!(h.count(&text("+ Add Coat")), 1);
    h.undo();
    assert_eq!(h.value("coat_mode"), "Auto");
    assert!(h.above(&coat, &iridescence));
    h.redo();
    assert_eq!(h.count(&coat), 0);

    h.click(&text("+ Add Coat"));
    assert!(h.above(&iridescence, &coat));
    assert_eq!(h.value("coat_mode"), "On");
    assert_eq!(h.value("clearcoat"), "0.00");
    assert!(h.above(&coat, &add_glass));
    h.click(&coat);
    assert_eq!(h.value("coat_mode"), "Off");
    h.click(&name("material.feature.remove.10_coat_mode"));
    assert_eq!(h.count(&coat), 0);
    h.undo();
    assert_eq!(h.value("coat_mode"), "Off");

    h.click(&text("+ Add Subsurface"));
    assert_eq!(h.value("subsurface_feature_mode"), "On");
    assert_eq!(h.value("subsurface_weight"), "0.50");
    h.click(&name("param_row.10_subsurface_feature_mode.value"));
    assert_eq!(h.value("subsurface_feature_mode"), "Off");
    assert_eq!(h.value("subsurface_weight"), "0.50");
    h.click(&name("material.feature.remove.10_subsurface_feature_mode"));
    assert_eq!(h.count(&name("param_row.10_subsurface_feature_mode.value")), 0);
    assert_eq!(h.count(&text("+ Add Subsurface")), 1);
    h.undo();
    assert_eq!(h.value("subsurface_feature_mode"), "Off");
    h.redo();
    h.click(&text("+ Add Subsurface"));
    assert_eq!(h.value("subsurface_feature_mode"), "On");
    assert_eq!(h.value("subsurface_weight"), "0.50");
}
