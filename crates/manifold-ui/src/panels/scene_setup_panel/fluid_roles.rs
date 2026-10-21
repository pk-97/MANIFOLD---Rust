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
                        crate::param_surface::stable_key(&format!("scene.fluid_role.target.{}", ref_text(&role.source))),
                    );
                    let remove = tree.add_button_keyed(
                        Some(self.content_parent),
                        x + width - remove_w,
                        y,
                        remove_w,
                        ROW_H,
                        btn_style(),
                        "×",
                        crate::param_surface::stable_key(&format!("scene.fluid_role.remove.{}", ref_text(&role.source))),
                    );
                    tree.set_name(
                        target,
                        format!("scene_setup.fluid_role.{}.target", ref_text(&role.source)),
                    );
                    tree.set_name(
                        remove,
                        format!("scene_setup.fluid_role.{}.remove", ref_text(&role.source)),
                    );
                    self.fluid_role_target_ids
                        .push((target, role.source.clone()));
                    self.fluid_role_remove_ids
                        .push((remove, role.source.clone()));
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
                obj_key(self.row_key(&row.node), OBJ_OFF_FLUID_ROLE),
            );
            tree.set_name(button, "scene_setup.properties.add_fluid_role");
            self.object_fluid_role_ids.push((button, row.node.clone()));
            y += ROW_H + ROW_GAP;
        }
        y
    }
}
