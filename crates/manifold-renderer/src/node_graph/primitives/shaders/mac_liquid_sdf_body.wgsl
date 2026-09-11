// Exact capped union of particle spheres, accelerated by existing linked bins.
fn body(idx:u32,count:u32)->f32 {
    let c=vec3<u32>(idx%64u,idx/64u%64u,idx/4096u);
    let center=WATER_ORIGIN+(vec3<f32>(c)+vec3<f32>(0.5))*WATER_H;
    let radius=0.8660254037844386*WATER_H;
    let cutoff=3.0*WATER_H+radius;
    let lo=max(vec3<i32>(0),vec3<i32>(floor((center-vec3<f32>(cutoff)-WATER_ORIGIN)/0.125)));
    let hi=min(vec3<i32>(31),vec3<i32>(floor((center+vec3<f32>(cutoff)-WATER_ORIGIN)/0.125)));
    var phi=3.0*WATER_H;
    let capacity=arrayLength(&buf_particles);
    for(var z=lo.z;z<=hi.z;z++) {for(var y=lo.y;y<=hi.y;y++) {for(var x=lo.x;x<=hi.x;x++) {
        var link=buf_heads[u32(x+32*(y+32*z))];var visited=0u;
        loop {
            if(link==0u){break;}
            if(visited>=capacity || link>capacity || link>arrayLength(&buf_next)){return bitcast<f32>(0x7fc00000u);}
            let i=link-1u;let p=buf_particles[i];
            if(!water_finite1(p.position_mass.w) || p.position_mass.w<0.0){return bitcast<f32>(0x7fc00000u);}
            if(p.position_mass.w>0.0){
                if(!water_finite3(p.position_mass.xyz)){return bitcast<f32>(0x7fc00000u);}
                phi=min(phi,length(center-p.position_mass.xyz)-radius);
            }
            link=buf_next[i];visited++;
        }
    }}}
    let open=buf_geometry[c.x+65u*(c.y+65u*c.z)].w;
    if(!water_finite1(open)||open<0.0||open>1.0){return bitcast<f32>(0x7fc00000u);}
    if(phi<0.5*WATER_H && open==0.0){phi=-0.5*WATER_H;}
    if(abs(phi)<0.005*WATER_H){phi=select(-0.005*WATER_H,0.005*WATER_H,phi>0.0);}
    return phi;
}
