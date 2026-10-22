//! Host cooker for `psx-vector`: an SVG subset in, a PSVG asset out.
//!
//! Supported SVG: `svg`, `g`, `path` (M L H V C S Q T A Z, absolute and
//! relative), `rect` (with rounded corners), `circle`, `ellipse`, `polygon`,
//! `polyline`, `line`; solid fills, `linearGradient` fills with any number of
//! stops (`objectBoundingBox` or `userSpaceOnUse`, `gradientTransform`,
//! `href` stop inheritance); `fill-rule`; strokes with width, join, cap and
//! miter limit; `transform` lists; `viewBox`; `opacity`. Anything else is an
//! error that names the element, never a silent skip.
//!
//! PS1 extensions are plain attributes: `data-blend` (`add`, `sub`,
//! `quarter`, `avg`) puts a shape on the GPU's semi-transparency modes, and
//! `data-pivot="x y"` sets a group's default rotation centre.
//!
//! Groups are the contract between art and code: the nearest ancestor-or-self
//! with an `id` owns a shape, and the id is the runtime's lookup name.
//!
//! The cook is deterministic: the same SVG and options give the same bytes.

mod cook;
mod geom;
mod paint;
mod path;
pub mod raster;
mod stroke;
mod tri;
mod xml;

pub use cook::{
    cook, Budget, CookError, CookOptions, Cooked, GroupReport, RefColor, RefShape, Report,
};
pub use paint::{FillRule, Gradient, Rgb};
pub use tri::{triangulate, IPt, ITri, TriError};
