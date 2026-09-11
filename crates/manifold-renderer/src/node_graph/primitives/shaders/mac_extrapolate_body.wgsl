// Read exclusively from the previous layer. Updating validity in place would
// make extrapolation depend on GPU thread order.
fn body(idx: u32, count: u32) -> Element {
    let e_in = buf_in[idx];
    var out = e_in;
    let c = vec3<i32>(i32(idx % 65u), i32((idx / 65u) % 65u), i32(idx / 4225u));
    for (var a=0u; a<3u; a++) {
        var dims=vec3<i32>(64);
        dims[a]=65;
        if (any(c>=dims)) {out.mac_velocity[a]=0.0;out.mac_valid[a]=0.0;continue;}
        if (e_in.mac_valid[a]>0.0) {continue;}
        var sum=0.0;
        var valid_count=0.0;
        for (var axis=0u;axis<3u;axis++) {
            for (var sign=-1;sign<=1;sign+=2) {
                var neighbor=c;
                neighbor[axis]+=sign;
                if (any(neighbor<vec3<i32>(0)) || any(neighbor>=dims)) {continue;}
                let j=u32(neighbor.x)+65u*(u32(neighbor.y)+65u*u32(neighbor.z));
                let value=buf_in[j];
                if (value.mac_valid[a]>0.0) {sum+=value.mac_velocity[a];valid_count+=1.0;}
            }
        }
        if (valid_count>0.0) {out.mac_velocity[a]=sum/valid_count;out.mac_valid[a]=1.0;}
    }
    out.mac_velocity.w=0.0;
    out.mac_valid.w=0.0;
    return out;
}
