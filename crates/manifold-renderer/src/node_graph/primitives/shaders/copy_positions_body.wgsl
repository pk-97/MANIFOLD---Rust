// node.copy_positions — coincident InstanceTransform to Vec4Vertex extraction.
fn body(idx: u32, count: u32, e_instances: Element) -> Element2 {
    return Element2(
        e_instances.pos_scale.x,
        e_instances.pos_scale.y,
        e_instances.pos_scale.z,
        1.0,
    );
}
