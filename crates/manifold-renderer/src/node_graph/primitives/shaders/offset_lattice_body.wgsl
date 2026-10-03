fn body(idx: u32, count: u32, e_levelset: f32, offset: f32) -> f32 {
    if offset == 0.0 { return e_levelset; }
    return e_levelset + offset;
}
