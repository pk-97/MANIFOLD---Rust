// Shared indexing only; the separate graph atoms own each operation.
fn mac_cell(c: vec3<i32>) -> u32 { return u32(c.x+64*(c.y+64*c.z)); }
fn mac_pad(c: vec3<i32>) -> u32 { return u32(c.x+65*(c.y+65*c.z)); }
fn mac_cell_coord(i:u32)->vec3<i32>{return vec3<i32>(i32(i%64u),i32(i/64u%64u),i32(i/4096u));}
fn mac_pad_coord(i:u32)->vec3<i32>{return vec3<i32>(i32(i%65u),i32(i/65u%65u),i32(i/4225u));}
fn mac_inside(c:vec3<i32>)->bool{return all(c>=vec3<i32>(0))&&all(c<vec3<i32>(64));}
fn mac_ghost(liquid:f32,air:f32)->f32{return 1.0+min(air/(-liquid),25.0);}
