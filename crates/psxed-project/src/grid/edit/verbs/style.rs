//! Styles: a cross profile, a bay rhythm, a vault and materials by role, as
//! data a project can carry (RON) and Corridor, Room and Cathedral accept.
//!
//! Everything a style says is integer. A [`Profile`] gives a height offset at
//! each click across the width; heights are snapped to the 64 unit lattice.
//! A [`BayKind`] is a stretch of the length with its own offsets, inset,
//! materials and an optional pier pair; the rhythm repeats along the run. A
//! [`Vault`] shapes the ceiling of a whole bay (groin, dome, coffer) from
//! heights on the fine leaves; barrels are just arc profiles.
//!
//! Resolution, honestly: the finest leaf is one click (256 grid units, a
//! quarter sector) and heights step by 64 units, so a curve is a polyline
//! with one vertex per click. A 4 sector bay (16 clicks) therefore has 16
//! chords across a barrel and 8 rings in a dome.

use serde::{Deserialize, Serialize};

use crate::grid::units::{snap_to, FINE_UNITS};
use crate::ResourceId;

use super::opening::{intrados, ArchShape};

/// Round or pointed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Curve {
    /// A half ellipse.
    Round,
    /// Two arcs meeting at the crown.
    Pointed,
}

impl Curve {
    fn shape(self) -> ArchShape {
        match self {
            Self::Round => ArchShape::Round,
            Self::Pointed => ArchShape::Pointed,
        }
    }
}

/// A height offset across the width of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Profile {
    /// No offset.
    Flat,
    /// `(permille across the width, offset in grid units)` joined by straight
    /// slopes, sorted by position; flat beyond the first and last point.
    Points(Vec<(i32, i32)>),
    /// Zero at both sides, `rise` in the middle: a barrel.
    Arc {
        /// Curve.
        curve: Curve,
        /// Height at the middle.
        rise: i32,
    },
}

impl Profile {
    /// The offset at lattice point `k` of `0..=width_clicks`, on the 64 unit
    /// lattice.
    pub fn sample(&self, width_clicks: i32, k: i32) -> i32 {
        let w = width_clicks.max(1);
        match self {
            Self::Flat => 0,
            Self::Arc { curve, rise } => {
                let u = k * 256 - w * 128;
                intrados(curve.shape(), 0, *rise, w * 256, u)
            }
            Self::Points(points) => {
                let p = i64::from(k) * 1000 / i64::from(w);
                let p = p as i32;
                let (Some(first), Some(last)) = (points.first(), points.last()) else {
                    return 0;
                };
                if p <= first.0 {
                    return snap_to(first.1, FINE_UNITS);
                }
                if p >= last.0 {
                    return snap_to(last.1, FINE_UNITS);
                }
                for pair in points.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    if p >= a.0 && p <= b.0 && b.0 > a.0 {
                        let num = i64::from(b.1 - a.1) * i64::from(p - a.0);
                        let den = i64::from(b.0 - a.0);
                        let r = if num >= 0 {
                            (num + den / 2) / den
                        } else {
                            -((-num + den / 2) / den)
                        };
                        return snap_to(a.1 + r as i32, FINE_UNITS);
                    }
                }
                0
            }
        }
    }
}

/// A material role, bound to real materials at the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Walking surface.
    Floor,
    /// Walls.
    Wall,
    /// Ceiling and vault.
    Ceiling,
    /// Dark edge trim, ribs.
    Trim,
    /// A contrasting accent.
    Accent,
    /// Open ground outside.
    Ground,
}

/// Materials by role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    /// Walking surface.
    pub floor: Option<ResourceId>,
    /// Walls.
    pub wall: Option<ResourceId>,
    /// Ceiling and vault.
    pub ceiling: Option<ResourceId>,
    /// Trim and ribs.
    pub trim: Option<ResourceId>,
    /// Accent.
    pub accent: Option<ResourceId>,
    /// Outside ground.
    pub ground: Option<ResourceId>,
}

impl Roles {
    /// The material of `role`.
    pub fn get(&self, role: Role) -> Option<ResourceId> {
        match role {
            Role::Floor => self.floor,
            Role::Wall => self.wall,
            Role::Ceiling => self.ceiling,
            Role::Trim => self.trim,
            Role::Accent => self.accent,
            Role::Ground => self.ground,
        }
    }

    /// The six distinct Graybox materials of a project by role: warm stone
    /// floor, light frieze walls, wood ceiling, dark trim, metal accent and
    /// moss ground (see `graybox_set`). A role whose material the project
    /// lacks is `None`.
    pub fn graybox(project: &crate::ProjectDocument) -> Self {
        let find = |name: &str| {
            project
                .resources
                .iter()
                .find(|r| r.name == name && matches!(r.data, crate::ResourceData::Material(_)))
                .map(|r| r.id)
        };
        Self {
            floor: find("Graybox / Warm Stone 64"),
            wall: find("Graybox / Light Frieze 64"),
            ceiling: find("Graybox / Wood 64"),
            trim: find("Graybox / Dark Trim 64"),
            accent: find("Graybox / Metal 64"),
            ground: find("Graybox / Moss 64"),
        }
    }
}

/// A column stood at the bay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pier {
    /// Shaft radius.
    pub radius: i32,
}

/// One stretch of the rhythm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BayKind {
    /// Name, for the editor.
    pub name: String,
    /// Length along the run, in clicks.
    pub length: i32,
    /// Clicks taken off each side: a rib that narrows the passage.
    pub inset: i32,
    /// Added to the floor profile, grid units.
    pub floor_offset: i32,
    /// Added to the ceiling profile, grid units.
    pub ceiling_offset: i32,
    /// Floor profile scale, percent.
    pub floor_scale_pct: i32,
    /// Ceiling profile scale, percent.
    pub ceiling_scale_pct: i32,
    /// Floor material role (the style's when `None`).
    pub floor_role: Option<Role>,
    /// Ceiling material role.
    pub ceiling_role: Option<Role>,
    /// Wall material role.
    pub wall_role: Option<Role>,
    /// A column pair at the start of the bay, one each side, if any.
    pub pier: Option<Pier>,
}

impl BayKind {
    /// A plain bay of `length` clicks.
    pub fn plain(name: &str, length: i32) -> Self {
        Self {
            name: name.to_string(),
            length,
            inset: 0,
            floor_offset: 0,
            ceiling_offset: 0,
            floor_scale_pct: 100,
            ceiling_scale_pct: 100,
            floor_role: None,
            ceiling_role: None,
            wall_role: None,
            pier: None,
        }
    }
}

/// How the corner zones of a dome bay are shaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spandrel {
    /// Flat at the spring line.
    Flat,
    /// Rising towards the corners by `raise`.
    Sloped {
        /// Height at the corner above the spring line.
        raise: i32,
    },
}

/// The ceiling shape of a whole bay, computed from heights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vault {
    /// None: the profiles shape the ceiling (a barrel is an arc profile).
    None,
    /// Two crossing barrels: the higher of the two at every point.
    Groin {
        /// Curve of both barrels.
        curve: Curve,
        /// Crown height above the spring line.
        rise: i32,
    },
    /// A dome over the bay: a hemisphere (or pointed) inscribed in it.
    Dome {
        /// Pointed rather than hemispherical.
        pointed: bool,
        /// Crown height above the spring line.
        rise: i32,
        /// The corners outside the circle.
        spandrel: Spandrel,
    },
    /// A stepped recess: each ring inwards is one step higher.
    Coffer {
        /// Height of one step.
        depth: i32,
        /// Number of steps.
        steps: i32,
    },
}

fn arc(curve: Curve, rise: i32, width: i32, k: i32) -> i32 {
    Profile::Arc { curve, rise }.sample(width, k)
}

fn isqrt(n: i64) -> i64 {
    if n <= 0 {
        return 0;
    }
    let mut x = (n as f64).sqrt() as i64;
    while x * x > n {
        x -= 1;
    }
    while (x + 1) * (x + 1) <= n {
        x += 1;
    }
    x
}

impl Vault {
    /// Ceiling height above the spring line at lattice point `(x, z)` of a
    /// bay `w` by `l` clicks, on the 64 unit lattice.
    pub fn offset(&self, w: i32, l: i32, x: i32, z: i32) -> i32 {
        match self {
            Self::None => 0,
            Self::Groin { curve, rise } => arc(*curve, *rise, w, x).max(arc(*curve, *rise, l, z)),
            Self::Dome {
                pointed,
                rise,
                spandrel,
            } => {
                // Half click units, so the centre is a whole number.
                let (dx, dz) = (i64::from((2 * x - w).abs()), i64::from((2 * z - l).abs()));
                let radius = i64::from(w.min(l));
                let r2 = dx * dx + dz * dz;
                if r2 <= radius * radius {
                    let r = isqrt(r2);
                    let h = if *pointed {
                        intrados(
                            ArchShape::Pointed,
                            0,
                            *rise,
                            (2 * radius * 128) as i32,
                            (r * 128) as i32,
                        )
                    } else {
                        (i64::from(*rise) * isqrt(radius * radius - r2) / radius) as i32
                    };
                    snap_to(h, FINE_UNITS)
                } else {
                    match spandrel {
                        Spandrel::Flat => 0,
                        Spandrel::Sloped { raise } => {
                            let corner =
                                isqrt(i64::from(w) * i64::from(w) + i64::from(l) * i64::from(l));
                            let r = isqrt(r2);
                            let span = (corner - radius).max(1);
                            snap_to((i64::from(*raise) * (r - radius) / span) as i32, FINE_UNITS)
                        }
                    }
                }
            }
            Self::Coffer { depth, steps } => {
                let ring = x.min(w - x).min(z).min(l - z).max(0);
                depth * ring.min(*steps)
            }
        }
    }
}

/// A style: profiles, rhythm, vault and roles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    /// Name.
    pub name: String,
    /// Floor cross profile.
    pub floor: Profile,
    /// Ceiling cross profile (offsets from the headroom line).
    pub ceiling: Profile,
    /// Clear height at the zero of the profiles, grid units.
    pub headroom: i32,
    /// Repeating bays along the run.
    pub rhythm: Vec<BayKind>,
    /// A vault over each bay.
    pub vault: Vault,
    /// Default floor role.
    pub floor_role: Role,
    /// Default ceiling role.
    pub ceiling_role: Role,
    /// Default wall role.
    pub wall_role: Role,
    /// Four piers at the corners of a vaulted room, if any.
    pub corner_piers: Option<Pier>,
}

impl Style {
    /// Total length of one rhythm cycle, clicks.
    pub fn cycle(&self) -> i32 {
        self.rhythm
            .iter()
            .map(|b| b.length.max(1))
            .sum::<i32>()
            .max(1)
    }

    /// The bay at `along` clicks, with where it starts and its length.
    pub fn bay_at(&self, along: i32) -> (&BayKind, i32, i32) {
        let cycle = self.cycle();
        let cycles = along.div_euclid(cycle);
        let mut at = along.rem_euclid(cycle);
        let mut start = cycles * cycle;
        for b in &self.rhythm {
            let len = b.length.max(1);
            if at < len {
                return (b, start, len);
            }
            at -= len;
            start += len;
        }
        // An empty rhythm is one plain bay: callers refuse it before.
        (&self.rhythm[0], start, self.rhythm[0].length.max(1))
    }

    /// Check the numbers.
    pub fn check(&self) -> Result<(), String> {
        if self.rhythm.is_empty() {
            return Err("a style needs at least one bay".into());
        }
        if self.headroom < 768 {
            return Err("headroom is under the body".into());
        }
        for b in &self.rhythm {
            if b.length < 1 || b.inset < 0 {
                return Err(format!("bay {}: length and inset must be positive", b.name));
            }
        }
        Ok(())
    }

    /// The style as RON text.
    pub fn to_ron(&self) -> String {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .expect("a style serialises")
    }

    /// A style from RON text.
    pub fn from_ron(text: &str) -> Result<Self, String> {
        ron::from_str(text).map_err(|e| e.to_string())
    }
}

/// Plain: flat floor and ceiling, one bay.
pub fn plain() -> Style {
    Style {
        name: "plain".into(),
        floor: Profile::Flat,
        ceiling: Profile::Flat,
        headroom: 3072,
        rhythm: vec![BayKind::plain("panel", 4)],
        vault: Vault::None,
        floor_role: Role::Floor,
        ceiling_role: Role::Ceiling,
        wall_role: Role::Wall,
        corner_piers: None,
    }
}

/// The sci-fi rib corridor, for a width of 2 sectors (8 clicks): a floor that
/// steps `_/-\_`, a lowered central ceiling panel, and a rhythm of a narrow
/// low rib of one click and a panel of three.
pub fn scifi_rib() -> Style {
    let mut rib = BayKind::plain("rib", 1);
    rib.inset = 1;
    rib.ceiling_offset = -256;
    rib.wall_role = Some(Role::Accent);
    rib.ceiling_role = Some(Role::Trim);
    rib.floor_role = Some(Role::Trim);
    let mut panel = BayKind::plain("panel", 3);
    panel.wall_role = Some(Role::Wall);
    Style {
        name: "scifi rib".into(),
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
    }
}

/// A gothic nave: a pointed barrel with a rib of one click at each bay of
/// eight, a column pair at each rib.
pub fn gothic_nave() -> Style {
    let mut rib = BayKind::plain("rib", 1);
    rib.ceiling_offset = -128;
    rib.ceiling_role = Some(Role::Trim);
    rib.pier = Some(Pier { radius: 192 });
    Style {
        name: "gothic nave".into(),
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
    }
}

/// A cloister walk: groin vaults, one square bay after another (bay length
/// 8 clicks, for a width of 2 sectors).
pub fn cloister() -> Style {
    Style {
        name: "cloister".into(),
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
    }
}

/// A cloister bay as a room style: one groin vault over a 16 click square.
pub fn cloister_room() -> Style {
    let mut s = cloister();
    s.name = "cloister room".into();
    s.rhythm = vec![BayKind::plain("bay", 16)];
    s
}

/// A crypt: a low round barrel and thick piers that narrow the passage.
pub fn crypt() -> Style {
    let mut pier = BayKind::plain("pier", 1);
    pier.inset = 1;
    pier.wall_role = Some(Role::Trim);
    pier.pier = Some(Pier { radius: 320 });
    Style {
        name: "crypt".into(),
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
    }
}

/// A domed chamber: a pointed cupola over a square room on four piers.
pub fn domed_chamber() -> Style {
    Style {
        name: "domed chamber".into(),
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
        corner_piers: Some(Pier { radius: 256 }),
    }
}

/// Every built-in style.
pub fn built_in() -> Vec<Style> {
    vec![
        plain(),
        scifi_rib(),
        gothic_nave(),
        cloister(),
        crypt(),
        domed_chamber(),
    ]
}
