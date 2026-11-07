//! How well each room's card lines up with the room's own geometry.
//!
//! Usage: `card_fit <project.ron> <out dir> [room ...]`
//!        `card_fit <project.ron> <out dir> eye=X,Y,Z [sectors]`
//!
//! With `eye=X,Y,Z` (runtime units) it instead lists every room whose centre is within
//! `sectors` (12) of that eye, with how far the eye is above the room's lowest geometry and how its
//! card and the geometry compare from there, for the two kinds of card.
//!
//! For each room named (the tallest, the widest and the most complex ones when none are) and
//! each distance, an eye is put out along the card's heading, at the eye height the cards are
//! baked for, and again higher and turned 22.5 degrees off the heading. The room's geometry
//! and its card are drawn into masks the size of the game's screen and compared, for the
//! shear orthographic cards the game had and for the perspective ones it has now. Prints the
//! mismatch in pixels and writes an overlay (white both, green geometry only, magenta card
//! only) for each case of the nominal eye.

use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};
use psxed_project::grid::cook::cards::{bake_room, bake_room_ortho, fit, EyeHeight};
use psxed_project::playtest::{cook_for_cards, room_card_eye};
use psxed_project::ProjectDocument;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: card_fit <project.ron> <out dir> [room ...]");
        std::process::exit(2);
    }
    let path = Path::new(&args[0]);
    let text = std::fs::read_to_string(path).expect("read the project");
    let project = ProjectDocument::from_ron_str(&text).expect("parse the project");
    let root = path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let out = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&out).expect("create the output directory");
    let scene = project.active_scene();
    let world = scene.world.grid().expect("a grid world");
    let inputs = cook_for_cards(&project, world, &root).expect("cook the level");
    let (cooked, albedo) = (inputs.cooked, inputs.albedo);
    let sector = i32::from(cooked.level.sector_units);
    let eye = room_card_eye(&world.stream.flood, sector, inputs.spawn_floor_y);
    println!(
        "eye: {:.0} units out ({:.1} sectors), height {:?}",
        eye.distance,
        eye.distance / sector as f32,
        eye.height
    );
    let rooms = cooked.level.rooms.len();
    let ortho: Vec<_> = (0..rooms)
        .map(|r| bake_room_ortho(&cooked, &albedo, r))
        .collect();
    let persp: Vec<_> = (0..rooms)
        .map(|r| bake_room(&cooked, &albedo, r, eye))
        .collect();
    if let Some(eye_arg) = args.get(2).and_then(|a| a.strip_prefix("eye=")) {
        let c: Vec<f32> = eye_arg.split(',').filter_map(|v| v.parse().ok()).collect();
        let reach = args
            .get(3)
            .and_then(|a| a.parse::<f32>().ok())
            .unwrap_or(12.0)
            * sector as f32;
        let at = [c[0], c[1], c[2]];
        println!("eye {at:?}; room, plan distance (sectors), eye above the room's lowest geometry (sectors), shear mismatch/top/bottom, perspective mismatch/top/bottom");
        for room in 0..rooms {
            let (Some(old), Some(new)) = (ortho[room].as_ref(), persp[room].as_ref()) else {
                continue;
            };
            let (dx, dz) = (new.centre[0] as f32 - at[0], new.centre[1] as f32 - at[2]);
            let length = (dx * dx + dz * dz).sqrt();
            if length > reach || length < 3.0 * sector as f32 {
                continue;
            }
            let a = fit::fit_room(&cooked, &albedo, room, old, at);
            let b = fit::fit_room(&cooked, &albedo, room, new, at);
            let (Some(a), Some(b)) = (a, b) else { continue };
            println!(
                "{room:4} {:5.1} {:6.1} | {:5.2} {:5.1} {:5.1} | {:5.2} {:5.1} {:5.1}   geometry px {} card px {}",
                length / sector as f32,
                (at[1] - room_low(&cooked, room)) / sector as f32,
                a.mismatch, a.top, a.bottom, b.mismatch, b.top, b.bottom, b.geometry, b.card
            );
        }
        return;
    }
    let chosen: Vec<usize> = if args.len() > 2 {
        args[2..].iter().filter_map(|a| a.parse().ok()).collect()
    } else {
        let mut by_height: Vec<usize> = (0..rooms).filter(|&r| persp[r].is_some()).collect();
        by_height.sort_by_key(|&r| std::cmp::Reverse(persp[r].as_ref().map_or(0, |c| c.height)));
        let mut by_width = by_height.clone();
        by_width.sort_by_key(|&r| std::cmp::Reverse(persp[r].as_ref().map_or(0, |c| c.half_width)));
        let mut pick = Vec::new();
        for r in by_height.iter().take(3).chain(by_width.iter().take(3)) {
            if !pick.contains(r) {
                pick.push(*r);
            }
        }
        pick
    };
    println!("room  d(sect) eye  heading   | shear: mismatch top bottom left right | perspective: mismatch top bottom left right   (pixels, screen 320x240)");
    let mut totals = [(0.0f32, 0usize); 2];
    for &room in &chosen {
        let (Some(old), Some(new)) = (ortho[room].as_ref(), persp[room].as_ref()) else {
            continue;
        };
        let low = new.base as f32; // the card's base stands at the room's lowest view, only used for labels
        let _ = low;
        for &distance in &[11.0f32, 12.0, 14.0] {
            for (label, extra_height, turn) in [
                ("nominal", 0.0f32, 0.0f32),
                ("+2 sect", 2.0 * sector as f32, 0.0),
                ("22.5deg", 0.0, 22.5f32.to_radians()),
            ] {
                // The eye's own height: the card's nominal eye over the room's lowest
                // geometry. The room's lowest geometry is the card's base plus what the bake
                // lowered it by; take the room's own floor from its triangles via the fit.
                let ground = room_low(&cooked, room);
                let nominal_y = match eye.height {
                    EyeHeight::World(y) => y,
                    EyeHeight::AboveRoom(h) => ground + h,
                };
                let angle = turn;
                let (cx, cz) = (new.centre[0] as f32, new.centre[1] as f32);
                let d = distance * sector as f32;
                let at = [
                    cx + angle.cos() * d,
                    nominal_y + extra_height,
                    cz + angle.sin() * d,
                ];
                let a = fit::fit_room(&cooked, &albedo, room, old, at);
                let b = fit::fit_room(&cooked, &albedo, room, new, at);
                let (Some(a), Some(b)) = (a, b) else { continue };
                println!(
                    "{room:4}  {distance:5.1}  {label}  | {:5.2} {:5.1} {:5.1} {:5.1} {:5.1} | {:5.2} {:5.1} {:5.1} {:5.1} {:5.1}",
                    a.mismatch, a.top, a.bottom, a.left, a.right, b.mismatch, b.top, b.bottom, b.left, b.right
                );
                if label == "nominal" {
                    totals[0].0 += a.mismatch;
                    totals[0].1 += 1;
                    totals[1].0 += b.mismatch;
                    totals[1].1 += 1;
                    for (name, fit) in [("shear", &a), ("persp", &b)] {
                        let mut image = RgbImage::new(fit::SCREEN[0] as u32, fit::SCREEN[1] as u32);
                        for (i, p) in fit.overlay.iter().enumerate() {
                            image.put_pixel(
                                (i % fit::SCREEN[0]) as u32,
                                (i / fit::SCREEN[0]) as u32,
                                Rgb(*p),
                            );
                        }
                        let file = out.join(format!("fit-room{room:03}-{distance:.0}-{name}.png"));
                        image.save(&file).expect("write the overlay");
                    }
                }
            }
        }
    }
    println!(
        "mean mismatch at the nominal eye: shear {:.3}, perspective {:.3}",
        totals[0].0 / totals[0].1.max(1) as f32,
        totals[1].0 / totals[1].1.max(1) as f32
    );
}

/// The lowest point of the room's cooked geometry.
fn room_low(cooked: &psxed_project::grid::cook::CookedLevel, room: usize) -> f32 {
    let table = &cooked.tables[room];
    let origin = cooked.level.rooms[room].origin;
    table
        .vertices
        .iter()
        .map(|v| (origin[1] + i32::from(v.y)) as f32)
        .fold(f32::INFINITY, f32::min)
}
