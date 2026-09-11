// node.displace_copies — weighted point displacement of InstanceTransform.
fn body(
    idx: u32,
    count: u32,
    e_instances: Element,
    e_weights: f32,
    amount: f32,
    direction_x: f32,
    direction_y: f32,
    direction_z: f32,
) -> Element {
    if amount == 0.0 || e_instances.pos_scale.w == 0.0 {
        return e_instances;
    }
    let offset = amount * e_weights * vec3<f32>(direction_x, direction_y, direction_z);
    return Element(
        vec4<f32>(e_instances.pos_scale.xyz + offset, e_instances.pos_scale.w),
        e_instances.rot,
    );
}
