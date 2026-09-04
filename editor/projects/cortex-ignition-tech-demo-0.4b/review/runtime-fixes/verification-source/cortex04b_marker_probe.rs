use psxed_project::{ProjectDocument,NodeKind,NodeId,playtest::cook_to_dir};
fn main(){
 let source=std::path::PathBuf::from("editor/projects/cortex-ignition-tech-demo-0.4b").canonicalize().unwrap();
 let root=std::path::Path::new("/tmp/cortex04b-debug/marker-probe");std::fs::create_dir_all(root).unwrap();
 for name in ["assets","source_assets"] { if !root.join(name).exists(){std::os::unix::fs::symlink(source.join(name),root.join(name)).unwrap();}}
 let mut p=ProjectDocument::load_from_path(source.join("project.ron")).unwrap();
 let scene=p.active_scene_mut();let player=scene.node_mut(ron::from_str("(2)").unwrap()).unwrap();player.transform.translation=[-10624.,576.,-20736.];player.transform.rotation_degrees=[0.;3];
 if let NodeKind::World{world_message,..}=&mut scene.node_mut(NodeId::ROOT).unwrap().kind {*world_message=None;}
 p.save_to_path(root.join("project.ron")).unwrap();let r=cook_to_dir(&p,root,&root.join("baked/generated")).unwrap();assert!(r.is_ok(),"{:?}",r.errors);
}
