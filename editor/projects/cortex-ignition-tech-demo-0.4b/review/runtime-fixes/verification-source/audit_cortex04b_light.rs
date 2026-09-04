use psxed_project::{ProjectDocument,NodeKind};
use psxed_project::brush_light::*;
use psx_bsp::{pxbsp_resident::PxbspResidentMap,SliceReader};
fn main(){
 let root=std::path::Path::new("editor/projects/cortex-ignition-tech-demo-0.4b");
 let p=ProjectDocument::load_from_path(root.join("project.ron")).unwrap();let scene=p.active_scene();
 let lights:Vec<_>=scene.nodes().iter().filter_map(|n|if let NodeKind::PointLight{color,intensity,radius}=&n.kind{Some(BrushPointLight{position:n.transform.translation.map(f64::from),radius:*radius as f64*1024.,intensity_q8:(*intensity as f64*256.).round() as u16,color:*color})}else{None}).collect();
 let brushes:Vec<_>=scene.brushes.iter().filter(|b|b.contents.is_solid()).cloned().collect();let occluders=brush_occluder_planes(&brushes);
 let data=std::fs::read(root.join("baked/generated/brush_world.pxbsp")).unwrap(); let mut map=PxbspResidentMap::with_capacity(data.len());map.load(0,&mut SliceReader::new(&data)).unwrap();
 let mut diffs=Vec::new();
 for i in 0..map.faces().len(){let f=map.faces().get(i).unwrap();let material=map.materials().get(f.texture as usize).unwrap();if material.flags & psx_bsp::pxbsp::material_flags::SKY_APERTURE != 0 {continue;} let tint=material.tint;let vs:Vec<_>=(0..f.vertex_count).map(|j|map.vertices().get((f.first_vertex+j as i32) as usize).unwrap()).collect();if vs.len()<3{continue;}
 let point=|v:psx_bsp::Vertex|[v.position.x as f64*16.,v.position.y as f64*16.,v.position.z as f64*16.];
 let a=point(vs[0]);let b=point(vs[1]);let c=point(vs[2]);let u=[b[0]-a[0],b[1]-a[1],b[2]-a[2]];let v=[c[0]-a[0],c[1]-a[1],c[2]-a[2]];let n=[u[1]*v[2]-u[2]*v[1],u[2]*v[0]-u[0]*v[2],u[0]*v[1]-u[1]*v[0]];let len=n.iter().map(|a|a*a).sum::<f64>().sqrt();if len==0.{continue;}let n=n.map(|v|v/len);
 for v in vs{let pos=point(v);let rgb=[v.light as u8,(v.light>>8) as u8,(v.light>>16) as u8];let col=lit_point_color(pos,n,tint,[32;3],&lights,&occluders);let other=lit_point_color(pos,n.map(|v|-v),tint,[32;3],&lights,&occluders);let err=|x:[u8;3]| (0..3).map(|i|rgb[i].abs_diff(x[i]) as u32).sum::<u32>();let error=err(other);diffs.push((error,pos,rgb,col,other));}
 }
 assert!(diffs.iter().all(|v|v.0==0), "cooked RGB diverges from authored-space evaluation"); diffs.sort_by_key(|x|std::cmp::Reverse(x.0));println!("{} vertices, {} differ > 6 total RGB",diffs.len(),diffs.iter().filter(|v|v.0>6).count());for v in diffs.iter().take(20){println!("{:?}",v);}
}
