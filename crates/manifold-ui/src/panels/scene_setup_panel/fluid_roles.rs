use super::*;

impl ScenePanel {
    pub(super) fn build_fluid_role_action(
        &mut self,
        tree: &mut UITree,
        x: f32,
        width: f32,
        mut y: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        match &row.fluid_roles {
            Ok(roles) => {
                for role in roles {
                    tree.add_label(
                        Some(self.content_parent),
                        x,
                        y,
                        width,
                        ROW_H,
                        &role.name,
                        label_style(),
                    );
                    y += ROW_H;
                    let remove_w = ROW_H;
                    let target = tree.add_button_keyed(
                        Some(self.content_parent),
                        x,
                        y,
                        width - remove_w - ROW_GAP,
                        ROW_H,
                        btn_style(),
                        &role.target_label,
                        obj_key(role.source_node_id as usize, OBJ_OFF_FLUID_ROLE_TARGET),
                    );
                    let remove = tree.add_button_keyed(
                        Some(self.content_parent),
                        x + width - remove_w,
                        y,
                        remove_w,
                        ROW_H,
                        btn_style(),
                        "×",
                        obj_key(role.source_node_id as usize, OBJ_OFF_FLUID_ROLE_REMOVE),
                    );
                    tree.set_name(
                        target,
                        format!("scene_setup.fluid_role.{}.target", role.source_node_id),
                    );
                    tree.set_name(
                        remove,
                        format!("scene_setup.fluid_role.{}.remove", role.source_node_id),
                    );
                    self.fluid_role_target_ids
                        .push((target, role.source_node_id));
                    self.fluid_role_remove_ids
                        .push((remove, role.source_node_id));
                    y += ROW_H + ROW_GAP;
                }
            }
            Err(reason) => {
                tree.add_label(
                    Some(self.content_parent),
                    x,
                    y,
                    width,
                    ROW_H,
                    &format!("Fluid roles: {reason}"),
                    label_style(),
                );
                y += ROW_H + ROW_GAP;
            }
        }
        if row.fluid_role_available {
            let button = tree.add_button_keyed(
                Some(self.content_parent),
                x,
                y,
                width,
                ROW_H,
                btn_style(),
                "+ Fluid Role",
                obj_key(row.object_node_id as usize, OBJ_OFF_FLUID_ROLE),
            );
            tree.set_name(button, "scene_setup.properties.add_fluid_role");
            self.object_fluid_role_ids.push((button, row.index));
            y += ROW_H + ROW_GAP;
        }
        y
    }
}
