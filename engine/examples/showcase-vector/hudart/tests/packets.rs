//! Host check of the guest's packet code: the HUD packets are plain data, so the unit tests in
//! `stance_hud_packets.rs` run here against the generated art.
#![allow(dead_code)]

#[path = "../../../editor-playtest/src/stance_hud_sprites.rs"]
mod hud_sprites;

#[path = "../../../editor-playtest/src/stance_hud_packets.rs"]
mod hud_packets;
