// node.over — fusable body (freeze section 12), MultiInputCoincident: `top`
// and `bottom` at the same texel. Porter-Duff "over" with premultiplied
// alpha: out = top + bottom · (1 − top.a). render_scene writes premultiplied
// colour with alpha 0 where nothing was drawn. PARAMS: none.
fn body(top: vec4<f32>, bottom: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    return top + bottom * (1.0 - clamp(top.a, 0.0, 1.0));
}
