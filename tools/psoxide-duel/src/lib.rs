//! Headless host duel harness for the Cortex combat rules.
//!
//! Any two fighters of a project (the training enemy, or Aletha cooked as an
//! entity with the enemy's AI persona) fight in an arena on the host, each
//! driven by the enemy tactical AI or by a scripted pad through the
//! possession seam. The rules, the motor, the projectiles and the AI are the
//! game's own code; see [`arena`] for what the arena adds and what it leaves
//! out. A seed sweep runs in seconds, without a disc build; the disc duel
//! (`psxed-mcp`'s `duel-baseline`) stays the end-to-end gate.

pub mod arena;
pub mod fighter;
pub mod project;
pub mod sweep;
