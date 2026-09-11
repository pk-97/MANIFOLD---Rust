fn body(idx:u32,count:u32)->Element {
    var out=buf_grid[idx];let c=mac_pad_coord(idx);
    for(var a=0u;a<3u;a++){
        out.mac_valid[a]=0.0;
        let area=buf_geometry[idx][a];
        if(area==0.0){out.mac_velocity[a]=0.0;continue;}
        var lo=c;lo[a]-=1;let hi=c;
        var lp=WATER_H;var hp=WATER_H;var lq=0.0;var hq=0.0;
        if(mac_inside(lo)){let i=mac_cell(lo);lp=buf_phi[i];lq=bitcast<f32>(buf_pressure[i]);}
        if(mac_inside(hi)){let i=mac_cell(hi);hp=buf_phi[i];hq=bitcast<f32>(buf_pressure[i]);}
        var grad=0.0;
        if(lp<0.0 && hp<0.0){grad=hq-lq;}
        else if(lp<0.0){grad=-lq*mac_ghost(lp,hp);}
        else if(hp<0.0){grad=hq*mac_ghost(hp,lp);}
        else {continue;}
        out.mac_velocity[a]-=grad/WATER_H;out.mac_valid[a]=1.0;
    }
    out.mac_velocity.w=0.0;out.mac_valid.w=0.0;return out;
}
