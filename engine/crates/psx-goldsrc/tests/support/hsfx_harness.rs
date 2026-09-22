#![allow(dead_code,unused_imports,static_mut_refs)]
use std::{cell::RefCell, panic::{catch_unwind,AssertUnwindSafe}};
mod spu; mod sfx; mod shared; mod old_hl; mod old_cs; mod new_hl; mod new_cs;
#[derive(Clone,Debug,PartialEq,Eq)]
enum Op { Init, Write(u32,u16), Upload(u32,Vec<u8>), State(Vec<u64>), Return(u16) }
thread_local! {static TRACE:RefCell<Vec<Op>>=const {RefCell::new(Vec::new())};}
fn record(op:Op){TRACE.with(|t|t.borrow_mut().push(op));}
fn take()->Vec<Op>{TRACE.with(|t|std::mem::take(&mut*t.borrow_mut()))}
fn audio(rate:u32,blocks:u32)->Vec<u8>{
 let mut p=Vec::from(*b"PSAU");p.extend(1u16.to_le_bytes());p.extend(3u16.to_le_bytes());p.extend((20+16*blocks).to_le_bytes());
 p.extend([1,1,0,0]);p.extend(rate.to_le_bytes());p.extend((28*blocks).to_le_bytes());p.extend(blocks.to_le_bytes());p.extend(u32::MAX.to_le_bytes());
 for n in 0..blocks {p.extend([n as u8;16]);}p
}
fn bank(samples:&[Vec<u8>])->Vec<u8>{
 let mut p=Vec::from(*b"HSFX");p.extend((samples.len() as u32).to_le_bytes());let mut offset=8+8*samples.len();
 for s in samples {p.extend((offset as u32).to_le_bytes());p.extend((s.len() as u32).to_le_bytes());offset+=s.len();}
 for s in samples {p.extend(s);}p
}
macro_rules! snapshot {($m:ident)=>{record(Op::State($m::snapshot()));};}
macro_rules! scenario {($m:ident,$pack:expr)=>{{
 take();unsafe {
 $m::reset();record(Op::Return($m::init_from_pack($pack) as u16));snapshot!($m);
 let dialogue=bank(&(0..105).map(|i|audio([8000,11025,22050,44100,65536][i%5],i as u32%9+1)).collect::<Vec<_>>());
 record(Op::Return($m::load_dialogue_pack(&dialogue) as u16));snapshot!($m);
 $m::set_ear([100,-30,20]);
 for id in 0..100u8 {
  $m::play(id);$m::play_vol(id,id as u16%5);$m::play_world(id,[id as i32*20,10,-10]);
  $m::play_at(id,if id%7==0 {-1}else{id as i32*id as i32});
  record(Op::Return($m::play_voice(id,0)));record(Op::Return($m::voice_ticks(id)));
  record(Op::Return($m::play_voice_world(id,[0,0,0])));
  record(Op::Return($m::play_voice_authored(id,[id as i32*100,0,0],id, id%8|8)));
  $m::play_map(id);$m::play_map_vol(id,0);$m::play_map_world(id,[0,100,0]);
  $m::play_map_authored(id,[200,0,0],80,id%8);
  $m::play_map_loop_world(id,[0,0,0],id as u16%11);
  $m::play_map_loop_authored(id,[0,0,0],id as u16%11,70,2);
  if id%3==0 {$m::stop_map_loop(id as u16%11);}
  snapshot!($m);
 }
 $m::charger_stop();$m::stop_dialogue();snapshot!($m);
 record(Op::Return($m::load_dialogue_pack(b"bad") as u16));snapshot!($m);
 record(Op::Return($m::load_dialogue_pack(&dialogue) as u16));
 $m::play_map_loop_world(2,[0,0,0],99);$m::play_voice(1,1);
 record(Op::Return($m::load_dialogue_pack(&dialogue) as u16));snapshot!($m);
 $m::stop_map_loops();$m::stop_all();snapshot!($m);
 }take()
}};}
macro_rules! malformed {($m:ident,$pack:expr,$dialogue:expr)=>{{
 take();unsafe{$m::reset();$m::init_from_pack(&bank(&[audio(22050,2)]));}take();
 let result=catch_unwind(AssertUnwindSafe(||unsafe{if $dialogue {$m::load_dialogue_pack($pack)} else {$m::init_from_pack($pack)}}));
 unsafe{snapshot!($m);} (result.ok(),take())
}};}
fn main(){
 std::panic::set_hook(Box::new(|_|{}));
 let full=bank(&(0..85).map(|i|audio([8000,11025,22050,44100][i%4],i as u32%7+1)).collect::<Vec<_>>());
 let a=scenario!(old_hl,&full);let b=scenario!(new_hl,&full);assert_eq!(a,b,"HL complete playback/register/state trace");let count_hl=a.len();
 let a=scenario!(old_cs,&full);let b=scenario!(new_cs,&full);assert_eq!(a,b,"CS complete playback/register/state trace");let count_cs=a.len();
 let mut cases=vec![vec![],b"junk".to_vec(),b"HSFX\x01\0\0\0".to_vec(),bank(&[audio(22050,1),b"bad".to_vec(),audio(22050,2)]),bank(&[audio(22050,32511),audio(22050,10)]),bank(&[audio(22050,32512)])];
 let valid=bank(&[audio(22050,1)]);for len in 8..valid.len(){cases.push(valid[..len].to_vec());}
 let mut over=bank(&[audio(22050,1)]);over[8..12].copy_from_slice(&u32::MAX.to_le_bytes());cases.push(over);
 for (i,p) in cases.iter().enumerate(){for dialogue in [false,true]{
  let a=malformed!(old_hl,p,dialogue);let b=malformed!(new_hl,p,dialogue);assert_eq!(a,b,"HL malformed case{i} dialogue{dialogue}");
  let a=malformed!(old_cs,p,dialogue);let b=malformed!(new_cs,p,dialogue);assert_eq!(a,b,"CS malformed case{i} dialogue{dialogue}");
 }}
 assert!(malformed!(old_hl,b"HSFX\x01\0\0\0",false).0.is_none());
 println!("PASS HL {count_hl} and CS {count_cs} ordered register/upload/state observations; {} malformed/partial/bounds cases per bank and game; legacy directory panic reproduced",cases.len());
}
