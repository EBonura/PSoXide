fn reset(id:u8,fault:psx_io::Fault,byte:usize,once:bool) {
    *psx_io::state()=psx_io::Model {id,fault,fault_byte:byte,fault_once:once,..Default::default()};
}
fn main() {
    use psx_io::Fault;
    let mut failures=0;
    let mut check=|name:&str,ok:bool,detail:String|{println!("{} {name}: {detail}",if ok {"PASS"} else {"FAIL"});if !ok {failures+=1;}};
    for id in [0x41,0x73,0xF3] {
        reset(id,Fault::None,3,false);let p=poll_port1();let m=psx_io::state();
        check(&format!("ready-{id:02x}"),m.early==0&&m.mid_ctrl==0&&p.buttons.bits()==0&&!m.selected,format!("mode={:?} early_tx={} sends={} attempts={} mid_ctrl={}",p.mode,m.early,m.sends,m.attempts,m.mid_ctrl));
    }
    for (name,rx_delay,ack_delay,ack_width) in [("early-ack",12,3,3),("coincident-ack",3,3,5)] {
        reset(0x73,Fault::None,3,false);
        {let mut m=psx_io::state();m.rx_delay=rx_delay;m.ack_delay=ack_delay;m.ack_width=ack_width;}
        let p=poll_port1();let m=psx_io::state();
        check(name,p.mode==PadMode::Analog&&p.buttons.bits()==0&&m.attempts==1&&m.sends==9&&m.early==0&&m.tx_errors==0,format!("mode={:?} attempts={} early={} tx_errors={}",p.mode,m.attempts,m.early,m.tx_errors));
    }
    reset(0x73,Fault::LateRx,3,true);let p=poll_port1();{
        let m=psx_io::state();
        check("late-rx-abort-reset",p.mode==PadMode::Analog&&p.buttons.bits()==0&&m.attempts==2&&m.resets==1&&m.pending.is_empty()&&m.tx_errors==0,format!("mode={:?} buttons={:04x} attempts={} resets={} pending={} tx_errors={}",p.mode,p.buttons.bits(),m.attempts,m.resets,m.pending.len(),m.tx_errors));
    }
    for fault in [Fault::Tx,Fault::Rx,Fault::Ack,Fault::AckHeld] {
        for byte in [2,3,4,5,6,7] {
            reset(0x73,fault,byte,false);let p=poll_port1();let m=psx_io::state();
            check(&format!("reject-{fault:?}-{byte}"),p.mode==PadMode::Unknown&&!m.selected,format!("mode={:?} sends={} attempts={}",p.mode,m.sends,m.attempts));
        }
        reset(0x73,fault,3,true);let p=poll_port1();let m=psx_io::state();
        check(&format!("recover-{fault:?}"),p.mode==PadMode::Analog&&m.attempts==2&&m.early==0,format!("mode={:?} sends={} attempts={} early_tx={}",p.mode,m.sends,m.attempts,m.early));
    }
    for fault in [Fault::Tx,Fault::Rx] {
        for byte in [0,1,8] {
            reset(0x73,fault,byte,false);let p=poll_port1();let m=psx_io::state();
            check(&format!("reject-{fault:?}-{byte}"),p.mode==PadMode::Unknown&&!m.selected,format!("mode={:?} sends={} attempts={}",p.mode,m.sends,m.attempts));
        }
    }
    reset(0x73,Fault::Acquire,0,true);let p=poll_port1();{
        let m=psx_io::state();check("acquisition-recovers",p.mode==PadMode::Analog&&m.attempts==2&&m.sends==9,format!("mode={:?} sends={} attempts={}",p.mode,m.sends,m.attempts));
    }
    reset(0x73,Fault::None,3,false);
    psx_io::state().sequence=vec![(0xFF,Fault::None,0),(0x73,Fault::Rx,3),(0xFF,Fault::None,0),(0xFF,Fault::None,0)];
    let p=poll_port1();check("mixed-absence-partial-is-unknown",p.mode==PadMode::Unknown,format!("mode={:?}",p.mode));
    reset(0x73,Fault::None,3,false);
    for id in [0x41,0x73,0x41,0x73] {
        psx_io::state().id=id;let p=poll_port1();
        check(&format!("mode-switch-{id:02x}"),p.id_low==id&&p.buttons.bits()==0,format!("mode={:?} buttons={:04x}",p.mode,p.buttons.bits()));
    }
    reset(0xFF,Fault::None,3,false);let p=poll_port1();check("disconnected",p.mode==PadMode::Disconnected,format!("mode={:?}",p.mode));
    psx_io::state().id=0x73;let p=poll_port1();check("reconnected",p.mode==PadMode::Analog&&p.buttons.bits()==0,format!("mode={:?}",p.mode));
    #[cfg(has_collection_input)] {
        collection_input::reset_test_state();
        reset(0x41,Fault::None,3,false);let before=collection_input::poll_buttons();
        psx_io::state().id=0x73;let analog=collection_input::poll_buttons();
        check("collection-toggle-no-edge",analog.bits()&!before.bits()==0,format!("before={:04x} after={:04x}",before.bits(),analog.bits()));
        // Deliberate user Select opens credits, unlike an invented fault bit.
        psx_io::state().buttons=button::SELECT;let select=collection_input::poll_buttons();
        check("collection-user-select",select.bits()&!analog.bits()&button::SELECT!=0,format!("buttons={:04x}",select.bits()));
        // A failed release poll must not create a fake release. The following
        // real Cross must still produce its own edge and let credits return.
        for kind in [Fault::Tx,Fault::Rx,Fault::Ack] {
            psx_io::state().fault=kind;psx_io::state().fault_byte=3;psx_io::state().buttons=0;
            let rejected=collection_input::poll_buttons();
            check(&format!("collection-hold-on-{kind:?}"),rejected==select,format!("last={:04x} accepted={:04x}",select.bits(),rejected.bits()));
            psx_io::state().fault=Fault::None;psx_io::state().buttons=button::CROSS;
            let cross=collection_input::poll_buttons();
            check(&format!("credits-cross-after-{kind:?}"),cross.bits()&!rejected.bits()&button::CROSS!=0,format!("buttons={:04x}",cross.bits()));
            psx_io::state().buttons=button::SELECT;let _=collection_input::poll_buttons();
        }
    }
    if failures!=0 {eprintln!("{failures} contract failures");std::process::exit(1);}
}
