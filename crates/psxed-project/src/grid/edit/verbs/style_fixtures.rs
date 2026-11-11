//! Styles the tests build runs from.
//!
//! The editor ships no vault styles; the style machinery (profiles, rhythm,
//! vaults, piers) is exercised with these fixtures instead. They are test
//! data, not a catalogue.

use super::style::{BayKind, Curve, Pier, Profile, Role, Spandrel, Style, Vault};

/// A ribbed corridor, for a width of 2 sectors (8 clicks): a floor that
/// steps `_/-\_`, a lowered central ceiling panel, and a rhythm of a narrow
/// low rib of one click and a panel of three.
pub fn ribbed_walk() -> Style {
    let mut rib = BayKind::plain("rib", 1);
    rib.inset = 1;
    rib.ceiling_offset = -256;
    rib.wall_role = Some(Role::Accent);
    rib.ceiling_role = Some(Role::Trim);
    rib.floor_role = Some(Role::Trim);
    let mut panel = BayKind::plain("panel", 3);
    panel.wall_role = Some(Role::Wall);
    Style {
        name: "ribbed walk".into(),
        floor: Profile::Points(vec![
            (0, 0),
            (250, 0),
            (375, 128),
            (625, 128),
            (750, 0),
            (1000, 0),
        ]),
        ceiling: Profile::Points(vec![
            (0, 0),
            (250, 0),
            (375, -128),
            (625, -128),
            (750, 0),
            (1000, 0),
        ]),
        headroom: 3072,
        rhythm: vec![rib, panel],
        vault: Vault::None,
        floor_role: Role::Floor,
        ceiling_role: Role::Ceiling,
        wall_role: Role::Wall,
        corner_piers: None,
        chord_clicks: 1,
    }
}

/// A pointed barrel: a pointed barrel with a rib of one click at each bay of
/// eight, a column pair at each rib.
pub fn pointed_barrel() -> Style {
    let mut rib = BayKind::plain("rib", 1);
    rib.ceiling_offset = -128;
    rib.ceiling_role = Some(Role::Trim);
    rib.pier = Some(Pier {
        radius: 192,
        sides: 6,
    });
    Style {
        name: "pointed barrel".into(),
        floor: Profile::Flat,
        ceiling: Profile::Arc {
            curve: Curve::Pointed,
            rise: 1536,
        },
        headroom: 4096,
        rhythm: vec![rib, BayKind::plain("bay", 7)],
        vault: Vault::None,
        floor_role: Role::Floor,
        ceiling_role: Role::Ceiling,
        wall_role: Role::Wall,
        corner_piers: None,
        chord_clicks: 2,
    }
}

/// A groin_walk walk: groin vaults, one square bay after another (bay length
/// 8 clicks, for a width of 2 sectors).
pub fn groin_walk() -> Style {
    Style {
        name: "groin walk".into(),
        floor: Profile::Flat,
        ceiling: Profile::Flat,
        headroom: 2560,
        rhythm: vec![BayKind::plain("bay", 8)],
        vault: Vault::Groin {
            curve: Curve::Round,
            rise: 1024,
        },
        floor_role: Role::Floor,
        ceiling_role: Role::Ceiling,
        wall_role: Role::Wall,
        corner_piers: None,
        chord_clicks: 2,
    }
}

/// A groin_walk bay as a room style: one groin vault over a 16 click square.
pub fn groin_room() -> Style {
    let mut s = groin_walk();
    s.name = "groin_walk room".into();
    s.rhythm = vec![BayKind::plain("bay", 16)];
    s
}

/// A piered_barrel: a low round barrel and thick piers that narrow the passage.
pub fn piered_barrel() -> Style {
    let mut pier = BayKind::plain("pier", 1);
    pier.inset = 1;
    pier.wall_role = Some(Role::Trim);
    pier.pier = Some(Pier {
        radius: 320,
        sides: 6,
    });
    Style {
        name: "piered barrel".into(),
        floor: Profile::Flat,
        ceiling: Profile::Arc {
            curve: Curve::Round,
            rise: 512,
        },
        headroom: 1792,
        rhythm: vec![pier, BayKind::plain("bay", 3)],
        vault: Vault::None,
        floor_role: Role::Floor,
        ceiling_role: Role::Ceiling,
        wall_role: Role::Wall,
        corner_piers: None,
        chord_clicks: 2,
    }
}

/// A dome room: a pointed cupola over a square room on four piers.
pub fn dome_room() -> Style {
    Style {
        name: "dome room".into(),
        floor: Profile::Flat,
        ceiling: Profile::Flat,
        headroom: 2560,
        rhythm: vec![BayKind::plain("bay", 16)],
        vault: Vault::Dome {
            pointed: true,
            rise: 3072,
            spandrel: Spandrel::Sloped { raise: 512 },
        },
        floor_role: Role::Floor,
        ceiling_role: Role::Ceiling,
        wall_role: Role::Wall,
        corner_piers: Some(Pier {
            radius: 256,
            sides: 8,
        }),
        chord_clicks: 1,
    }
}
