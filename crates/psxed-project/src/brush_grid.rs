//! Names shared with the bonnie-32 brush importer binary.
//!
//! The grid render pack, its collision derivation and the raw-level bench
//! path that lived here are gone: grid worlds cook through
//! [`crate::grid::cook`]. What remains goes with the importer binary.

/// Material name for a texture nothing else can supply: bonnie draws a
/// checkerboard for an empty reference, and `_USER` textures are embedded
/// indexed images the importer does not decode.
pub const FALLBACK_TEXTURE: &str = "__fallback";

/// Material resource name for a bonnie texture reference.
pub fn texture_key(pack: &str, name: &str) -> String {
    if name.is_empty() || pack == "_USER" {
        FALLBACK_TEXTURE.to_string()
    } else {
        name.to_string()
    }
}
