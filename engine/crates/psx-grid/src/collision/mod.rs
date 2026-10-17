//! Native sector-grid collision.
//!
//! * [`GridCollision`] owns the world queries: [`locate`](GridCollision::locate),
//!   [`line_of_sight`](GridCollision::line_of_sight),
//!   [`contents_at`](GridCollision::contents_at), floor and ceiling heights.
//! * [`GridTraceProvider`] adapts it to `psx_engine::CollisionTraceProvider`.
//!
//! Solid model (matches the transitional PXBSP slabs the motor was tuned on):
//! a floor is a slab of [`SLAB_THICKNESS`] below its surface, a solid ceiling a
//! slab above its surface, wall pieces are zero-thickness vertical quads that
//! block from both sides. A sector without a floor is void: nothing is solid
//! there and bodies fall through. Every room is an independent solid set, so
//! stacked rooms over the same plan cells just add their slabs.
//!
//! Every coordinate is an `i32` runtime unit and there is no 64-bit or float
//! arithmetic. Queries reject (provider) or clamp (line of sight) coordinates
//! beyond [`COORD_LIMIT`] so every Q12 product stays inside `i32`.

mod body;
mod provider;
mod ray;
mod world;

#[cfg(test)]
mod tests;

pub use provider::GridTraceProvider;
pub use world::{GridCollision, Located, COORD_LIMIT, SLAB_THICKNESS};
