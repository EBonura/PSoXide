mod psx_hw { #[path="HARDWARE_PATH"] pub mod sio; }
mod psx_math { pub mod int32 { pub fn isqrt_i32(n:i32)->i32 { n.isqrt() } } }
mod psx_io {
    pub mod sio { pub const DATA:u32=0; pub const STAT:u32=4; pub const MODE:u32=8; pub const CTRL:u32=10; pub const BAUD:u32=14; }
    #[derive(Clone,Copy,PartialEq,Debug)] pub enum Fault { None, Tx, Rx, Ack, AckHeld, Acquire, LateRx }
    #[derive(Debug)] pub struct Model {
        pub now:u64, pub selected:bool, pub step:usize, pub id:u8, pub buttons:u16,
        pub pending:Vec<(u8,u64)>, pub ack_at:u64, pub ack_end:u64,
        pub early:usize, pub sends:usize, pub attempts:usize, pub mid_ctrl:usize,
        pub fault:Fault, pub fault_byte:usize, pub fault_once:bool,
        pub sequence:Vec<(u8,Fault,usize)>,
        pub rx_delay:u64, pub ack_delay:u64, pub ack_width:u64, pub resets:usize, pub tx_errors:usize,
    }
    impl Default for Model { fn default()->Self { Self { now:0,selected:false,step:0,id:0x73,buttons:0,pending:vec![],ack_at:0,ack_end:0,early:0,sends:0,attempts:0,mid_ctrl:0,fault:Fault::None,fault_byte:3,fault_once:false,sequence:vec![],rx_delay:3,ack_delay:12,ack_width:5,resets:0,tx_errors:0 } } }
    static STATE:std::sync::OnceLock<std::sync::Mutex<Model>>=std::sync::OnceLock::new();
    pub fn state()->std::sync::MutexGuard<'static,Model> { STATE.get_or_init(||std::sync::Mutex::new(Model::default())).lock().unwrap() }
    fn fault(m:&Model,kind:Fault,index:usize)->bool { m.fault==kind && m.fault_byte==index && (!m.fault_once || m.attempts==1) }
    pub unsafe fn write16(addr:u32,value:u16) {
        let mut m=state();m.now+=1;
        if addr==sio::CTRL {
            let selected=value&2!=0;
            // UART RX survives deselection; only RESET cancels a late response.
            if value & 0x40 != 0 { assert!(!selected);m.pending.clear();m.resets+=1; }
            if selected && m.selected {m.mid_ctrl+=1;}
            if selected && !m.selected {
                m.attempts+=1;m.step=0;m.ack_at=0;m.ack_end=0;
                if let Some((id,kind,byte))=m.sequence.get(m.attempts-1).copied(){m.id=id;m.fault=kind;m.fault_byte=byte;}
                if fault(&m,Fault::Acquire,0){m.ack_at=m.now;m.ack_end=m.now+100_000;}
            }
            if !selected {m.ack_at=0;m.ack_end=0;}
            m.selected=selected;
        }
    }
    pub unsafe fn write8(addr:u32,value:u8) {
        assert_eq!(addr,sio::DATA);let mut m=state();m.now+=1;
        assert!(m.selected,"DATA sent while deselected");
        if m.step!=0 && m.now<m.ack_end {m.early+=1;}
        let index=m.step;let final_index=if m.id==0x41 {4} else {8};
        if value != match index {0=>1,1=>0x42,_=>0} {m.tx_errors+=1;}
        let rx=match index {0=>0xFF,1=>m.id,2=>0x5A,3=>!(m.buttons as u8),4=>!((m.buttons>>8) as u8),_=>0x80};
        if !fault(&m,Fault::Rx,index) {
            let ready=m.now+if fault(&m,Fault::LateRx,index) {33_820} else {m.rx_delay};
            m.pending.push((rx,ready));
        }
        if m.id!=0xFF && index<final_index && !fault(&m,Fault::Ack,index) {m.ack_at=m.now+m.ack_delay;m.ack_end=m.ack_at+m.ack_width;} else {m.ack_at=0;m.ack_end=0;}
        if fault(&m,Fault::AckHeld,index) && index<final_index {m.ack_end=m.now+100_000;}
        m.step+=1;m.sends+=1;
    }
    pub unsafe fn read32(addr:u32)->u32 {
        assert_eq!(addr,sio::STAT);let mut m=state();m.now+=1;
        let mut stat=if fault(&m,Fault::Tx,m.step) {0} else {1};
        if m.pending.iter().any(|(_,ready)|m.now>=*ready) {stat|=2;}
        if m.ack_at!=0 && m.now>=m.ack_at && m.now<m.ack_end {stat|=1<<7;}
        stat
    }
    pub unsafe fn read8(addr:u32)->u8 {assert_eq!(addr,sio::DATA);let mut m=state();m.now+=1;let i=m.pending.iter().enumerate().filter(|(_,(_,ready))|m.now>=*ready).min_by_key(|(_,(_,ready))|*ready).map(|(i,_)|i).expect("read empty RX");m.pending.remove(i).0}
}
