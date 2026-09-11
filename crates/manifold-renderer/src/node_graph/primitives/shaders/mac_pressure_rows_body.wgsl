fn body(idx:u32,count:u32)->Element2 {
    let c=mac_cell_coord(idx);
    let phi=buf_phi[idx];
    var lower=vec3<f32>(0.0);var upper=vec3<f32>(0.0);var diag=0.0;var flux=0.0;
    if(!water_finite1(phi)){return Element2(vec4<f32>(bitcast<f32>(0x7fc00000u)),vec4<f32>(0.0));}
    if(phi>=0.0){return Element2(vec4<f32>(0.0),vec4<f32>(0.0));}
    for(var a=0u;a<3u;a++){
        for(var s=-1;s<=1;s+=2){
            var face=c;if(s==1){face[a]+=1;}
            let f=mac_pad(face);let area=buf_geometry[f][a];
            if(!water_finite1(area)||area<0.0||area>1.0){return Element2(vec4<f32>(bitcast<f32>(0x7fc00000u)),vec4<f32>(0.0));}
            if(area==0.0){continue;}
            flux+=f32(s)*area*buf_grid[f].mac_velocity[a];
            var other=c;other[a]+=s;
            var other_phi=WATER_H;if(mac_inside(other)){other_phi=buf_phi[mac_cell(other)];}
            if(!water_finite1(other_phi)){return Element2(vec4<f32>(bitcast<f32>(0x7fc00000u)),vec4<f32>(0.0));}
            if(other_phi<0.0){diag+=area;if(s<0){lower[a]=area;}else{upper[a]=area;}}
            else {diag+=area*mac_ghost(phi,other_phi);}
        }
    }
    // A and rhs are both scaled by h². q remains dt*p/rho.
    return Element2(vec4<f32>(lower,diag),vec4<f32>(upper,-flux*WATER_H));
}
