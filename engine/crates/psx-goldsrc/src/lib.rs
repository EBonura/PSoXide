//! Shared GoldSrc port mechanisms with caller-owned state and policy.
// Host unit tests in the moved gameplay modules use `Vec`, `Box` and `std::vec`.
#![cfg_attr(not(test), no_std)]
#![feature(optimize_attribute)]

pub mod chunk_stream;
pub mod ground_logic;
pub mod hitbox_logic;
pub mod hsfx;
pub mod ladder_logic;
pub mod model_variant;
pub mod ordering;
pub mod pickup_logic;
pub mod pushable;
pub mod pvs_faces;
pub mod render;
pub mod route_follow;
pub mod semantic_input;
pub mod telemetry;
pub mod texture_animation;
pub mod viewmodel;
pub mod visibility_logic;
