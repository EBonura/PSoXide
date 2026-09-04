struct Probe { current_volume_percent:u8, target_volume_percent:u8, fade_ticks_left:u16, fade_out_stop:bool, routed:bool, stopped:bool }
fn cdda_set_volume(_:u8) {}
impl Probe {
fn release_for_data_reads(&mut self,_:u32) { self.stopped=true; }
    fn tick_fade(&mut self, tick: u32) {
        if self.current_volume_percent == self.target_volume_percent {
            if self.fade_out_stop {
                self.fade_out_stop = false;
                self.release_for_data_reads(tick);
            }
            return;
        }
        let remaining = i32::from(self.fade_ticks_left.max(1));
        let delta = i32::from(self.target_volume_percent) - i32::from(self.current_volume_percent);
        let step = (delta.abs() + remaining - 1) / remaining;
        let next = i32::from(self.current_volume_percent) + step.min(delta.abs()) * delta.signum();
        self.current_volume_percent = next.clamp(0, 100) as u8;
        self.fade_ticks_left = self.fade_ticks_left.saturating_sub(1);
        if self.routed {
            cdda_set_volume(self.current_volume_percent);
        }
    }
}
fn main() {
for (start,target,duration) in [(0,80,60),(80,0,120),(0,40,60),(40,0,120)] {
let mut p=Probe { current_volume_percent:start,target_volume_percent:target,fade_ticks_left:duration,fade_out_stop:target==0,routed:false,stopped:false};
let mut arrived=0;
for tick in 1..=duration+1 { p.tick_fade(tick as u32); if p.current_volume_percent==target && arrived==0 { arrived=tick; } }
println!("{} -> {}: requested {} ticks, reached target after {} ticks",start,target,duration,arrived);
}
}
