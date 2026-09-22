//! Shared GoldSrc projection/clipping; local state selects the current view.
use crate::shared;
use shared::View;
pub use shared::*;
static mut VIEW: FullView = FullView::new();
static mut SCRATCH: ClipScratch = ClipScratch::new();
#[inline(always)]
pub unsafe fn set_projection_h(h: i32) {
    (*core::ptr::addr_of_mut!(VIEW)).set_projection_h(h);
}
#[inline(always)]
pub fn projection_h() -> i32 {
    unsafe { (&*core::ptr::addr_of!(VIEW)).projection_h() }
}
pub const OFX: i32 = 160;
pub const OFY: i32 = 120;
#[inline(always)]
pub fn close_inv_q12(z: i32) -> i32 {
    unsafe { shared::close_inv_q12(&*core::ptr::addr_of!(VIEW), z) }
}
#[inline(always)]
pub fn project_soft(v: &CVert) -> SVert {
    unsafe { shared::project_soft(&*core::ptr::addr_of!(VIEW), v) }
}
#[inline(always)]
pub fn quad_outside_vertical(c: &[&CVert; 4]) -> bool {
    unsafe { shared::quad_outside_vertical(&*core::ptr::addr_of!(VIEW), c) }
}
#[inline(always)]
pub fn on_visible_boundary(p: &SVert) -> bool {
    unsafe { shared::on_visible_boundary(&*core::ptr::addr_of!(VIEW), p) }
}
/// Consume the result before the next clipping call; rendering is serialized.
#[inline]
pub unsafe fn visible_clip(poly: [&CVert; 3]) -> (*const SVert, usize) {
    shared::visible_clip(
        &*core::ptr::addr_of!(VIEW),
        &mut *core::ptr::addr_of_mut!(SCRATCH),
        poly,
    )
}
#[inline]
pub fn guard_clip(poly: &[SVert], n: usize, out: &mut [SVert; 8]) -> usize {
    unsafe { shared::guard_clip(&mut *core::ptr::addr_of_mut!(SCRATCH), poly, n, out) }
}
