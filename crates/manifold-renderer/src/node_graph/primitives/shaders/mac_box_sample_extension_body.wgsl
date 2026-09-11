fn mac_extension_sample(pos:vec3<f32>,a:u32)->vec2<f32>{
    var offset=vec3<f32>(0.5);offset[a]=0.0;
    let q=(pos-WATER_ORIGIN)*WATER_INV_H-offset;
    if(!water_q_plausible(q)){return vec2<f32>(0.0);}
    let b=vec3<i32>(floor(q));let f=q-vec3<f32>(b);var dims=vec3<i32>(64);dims[a]=65;
    if(any(b<vec3<i32>(0))||any(b+vec3<i32>(1)>=dims)){return vec2<f32>(0.0);}
    var sum=0.0;
    for(var k=0u;k<8u;k++){
        let bit=vec3<u32>(k&1u,(k>>1u)&1u,(k>>2u)&1u);
        let w=select(vec3<f32>(1.0)-f,f,bit!=vec3<u32>(0u));let weight=w.x*w.y*w.z;
        if(weight==0.0){continue;}
        let e=buf_grid[mac_pad(b+vec3<i32>(bit))];
        if(e.mac_valid[a]<=0.0||!water_finite1(e.mac_valid[a])||!water_finite1(e.mac_velocity[a])){return vec2<f32>(0.0);}
        sum+=weight*e.mac_velocity[a];
    }
    if(!water_finite1(sum)){return vec2<f32>(0.0);}
    return vec2<f32>(sum,1.0);
}
fn body(idx:u32,count:u32,basin_min_x:f32,basin_min_y:f32,basin_min_z:f32,basin_max_x:f32,basin_max_y:f32,basin_max_z:f32,box_min_x:f32,box_min_y:f32,box_min_z:f32,box_max_x:f32,box_max_y:f32,box_max_z:f32)->Element {
    let lo=vec3<f32>(basin_min_x,basin_min_y,basin_min_z);let hi=vec3<f32>(basin_max_x,basin_max_y,basin_max_z);
    let blo=vec3<f32>(box_min_x,box_min_y,box_min_z);let bhi=vec3<f32>(box_max_x,box_max_y,box_max_z);
    var out=buf_grid[idx];let c=mac_pad_coord(idx);
    if(!water_finite3(lo)||!water_finite3(hi)||!water_finite3(blo)||!water_finite3(bhi)||any(lo>=hi)||any(blo>=bhi)){out.mac_velocity=vec4<f32>(bitcast<f32>(0x7fc00000u));return out;}
    for(var a=0u;a<3u;a++){
        var dims=vec3<i32>(64);dims[a]=65;
        if(any(c>=dims)){out.mac_velocity[a]=0.0;out.mac_valid[a]=0.0;continue;}
        let area=buf_geometry[idx][a];
        if(!water_finite1(area)||area<0.0||area>1.0){out.mac_velocity[a]=bitcast<f32>(0x7fc00000u);out.mac_valid[a]=0.0;continue;}
        if(area>0.0){continue;}
        var offset=vec3<f32>(0.5);offset[a]=0.0;
        let pos=WATER_ORIGIN+(vec3<f32>(c)+offset)*WATER_H;
        var mirrored=pos;var sign=1.0;var wall_normal=false;var changed=false;
        for(var k=0u;k<3u;k++){
            if(pos[k]<lo[k]){mirrored[k]=2.0*lo[k]-pos[k];if(k==a){sign=-sign;}changed=true;}
            else if(pos[k]>hi[k]){mirrored[k]=2.0*hi[k]-pos[k];if(k==a){sign=-sign;}changed=true;}
            else if(k==a&&(abs(pos[k]-lo[k])<1e-6||abs(pos[k]-hi[k])<1e-6)){wall_normal=true;}
        }
        if(all(pos>=blo)&&all(pos<=bhi)){
            var distance=1e10;var axis=0u;var plane=0.0;
            for(var k=0u;k<3u;k++){
                if(pos[k]-blo[k]<distance){distance=pos[k]-blo[k];axis=k;plane=blo[k];}
                if(bhi[k]-pos[k]<distance){distance=bhi[k]-pos[k];axis=k;plane=bhi[k];}
            }
            mirrored[axis]=2.0*plane-pos[axis];changed=true;
            if(axis==a){sign=-sign;if(distance<1e-6){wall_normal=true;}}
        }
        if(wall_normal){out.mac_velocity[a]=0.0;out.mac_valid[a]=1.0;continue;}
        if(changed){let sample=mac_extension_sample(mirrored,a);out.mac_velocity[a]=sign*sample.x;out.mac_valid[a]=sample.y;}
        else {out.mac_velocity[a]=0.0;out.mac_valid[a]=0.0;}
    }
    return out;
}
