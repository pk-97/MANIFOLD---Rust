fn body(idx:u32,count:u32,step_dt:f32)->Element {
    var out=buf_grid[idx];
    let area=buf_geometry[idx];
    if (!water_finite1(step_dt)||step_dt<=0.0) {out.mac_velocity=vec4<f32>(bitcast<f32>(0x7fc00000u));return out;}
    for(var a=0u;a<3u;a++) {
        if(area[a]==0.0){out.mac_velocity[a]=0.0;out.mac_valid[a]=0.0;}
        else if(a==1u && out.mac_valid[a]>0.0){out.mac_velocity[a]-=9.81*step_dt;}
    }
    return out;
}
