//! Shared GoldSrc projection/clipping; local state selects the current view.
use crate::shared;
use shared::View;
pub use shared::*;
static mut VIEW: RectView = RectView::new();
static mut SCRATCH: ClipScratch = ClipScratch::new();
#[inline(always)]
pub unsafe fn set_projection_h(h: i32) {
    (*core::ptr::addr_of_mut!(VIEW)).set_projection_h(h);
}
#[inline(always)]
pub fn projection_h() -> i32 {
    unsafe { (&*core::ptr::addr_of!(VIEW)).projection_h() }
}
#[inline(always)]
pub fn ofx() -> i32 {
    unsafe { (&*core::ptr::addr_of!(VIEW)).ofx() }
}
#[inline(always)]
pub fn ofy() -> i32 {
    unsafe { (&*core::ptr::addr_of!(VIEW)).ofy() }
}
#[inline(always)]
pub unsafe fn set_view_rect(x: i32, y: i32, w: i32, h: i32) {
    (*core::ptr::addr_of_mut!(VIEW)).set_view_rect(x, y, w, h);
}
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
