use super::*;

impl ScenePanel {
    pub(super) fn build_fluid_role_action(
        &mut self,
        tree: &mut UITree,
        x: f32,
        width: f32,
        y: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        if !row.fluid_role_available {
            return y;
        }
        let button = tree.add_button_keyed(
            Some(self.content_parent), x, y, width, ROW_H, btn_style(),
            "+ Fluid Role", obj_key(row.object_node_id as usize, OBJ_OFF_FLUID_ROLE),
        );
        tree.set_name(button, "scene_setup.properties.add_fluid_role");
        self.object_fluid_role_ids.push((button, row.index));
        y + ROW_H + ROW_GAP
    }
}
