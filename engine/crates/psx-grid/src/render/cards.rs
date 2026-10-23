//! Far cards: the rooms beyond the drawn radius as upright pictures standing
//! where the rooms stand.
//!
//! A room too far to draw whole is drawn as one *card*: a vertical quad
//! standing at the room's real place (its centre, on its lowest geometry),
//! as wide as the room, as tall as the room is high, always turned to face
//! the eye about the vertical axis. It is depth-sorted into the ordering
//! table at its real depth like any room quad, so a near card slides over a
//! far one when the eye moves sideways, grows when the eye walks towards it,
//! shrinks when it walks away and is covered by whatever stands in front of
//! it. Nothing about a card is fixed to the view.
//!
//! The picture of a card is baked by the cook from the room's own quads, from
//! [`VIEWS`] headings round the room, each a shear-orthographic render with
//! the eye pitched down by [`PITCH_TAN_Q12`]. The runtime shows the heading
//! nearest the direction the eye is in ([`view_toward`], with [`view_keep`]
//! holding the shown one until the eye is well past the change).
//!
//! # Picture and slot
//!
//! A picture is [`TEXELS`] by [`PICTURE_ROWS`] 4-bit texels, texel index 0
//! transparent, with its 16-colour palette in the row below it: together one
//! `16 x 64` halfword rectangle of VRAM, and one 2 KiB disc sector. The cube
//! sky's `1536 x 256` atlas is cut into [`SLOTS`] such rectangles
//! ([`slot_origin`]); a slot holds one view of one room.
//!
//! # Cost
//!
//! A card is four projected corners and one 11-word packet. The draw looks
//! only at the rooms that hold a slot and skips the ones outside the view
//! cone before projecting anything.

use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;
use psx_math::int32::isqrt_i32;

use super::{GridFrame, GridScreen, NEAR, OFF_ANY, OFF_BOTTOM, OFF_LEFT, OFF_RIGHT, OFF_TOP};

/// Headings a room's picture is baked from, `45` degrees apart. View `k`
/// is seen from the direction `(cos, sin)` of `k * 45` degrees in the
/// world's `(x, z)` plane ([`VIEW_COS_Q12`], [`VIEW_SIN_Q12`]): the
/// direction from the card to an eye that sees view `k`.
pub const VIEWS: usize = 8;
/// Texels across a picture.
pub const TEXELS: i32 = 64;
/// Texel rows of a picture. The row below holds its palette.
pub const PICTURE_ROWS: i32 = 63;
/// Rows of a slot, the palette's included.
pub const SLOT_ROWS: i32 = 64;
/// Halfwords across a slot (64 4-bit texels, or one palette).
pub const SLOT_HALFWORDS: u16 = 16;
/// Slots across the atlas (it is 384 halfwords wide).
pub const ATLAS_COLUMNS: usize = 24;
/// Slots down the atlas (it is 256 rows high).
pub const ATLAS_ROWS: usize = 4;
/// Slots in the atlas.
pub const SLOTS: usize = ATLAS_COLUMNS * ATLAS_ROWS;
/// Bytes of a picture with its palette: one disc sector.
pub const SLOT_BYTES: usize = 2048;
/// Slots across one 256 texel texture page.
const SLOTS_PER_PAGE: usize = 4;
/// Most bands a card too tall for one GPU polygon is cut into.
const MAX_BANDS: i32 = 4;
/// Shown view value of a slot that holds nothing yet.
pub const NO_VIEW: u8 = 0xff;
/// Slot value of a room that holds none.
pub const NO_SLOT: u8 = 0xff;

/// `tan` of the pitch the pictures are baked at (the eye above the card,
/// looking down), Q12: about `10` degrees.
pub const PITCH_TAN_Q12: i32 = 722;

/// Heading `k * 45` degrees, `cos` as Q12.
pub const VIEW_COS_Q12: [i16; VIEWS] = [4096, 2896, 0, -2896, -4096, -2896, 0, 2896];
/// Heading `k * 45` degrees, `sin` as Q12.
pub const VIEW_SIN_Q12: [i16; VIEWS] = [0, 2896, 4096, 2896, 0, -2896, -4096, -2896];

/// Words of one card packet: the tag, a texture-window reset and a flat
/// textured quad.
pub const CARD_PACKET_WORDS: usize = 11;
/// GP0 command of a flat textured quad, modulated by its colour.
const TEXTURED_QUAD_COMMAND: u32 = 0x2c00_0000;
/// GP0 command bit of a semi-transparent primitive.
const SEMI_TRANSPARENT: u32 = 0x0200_0000;

/// How a card is mixed with what is behind it.
#[derive(Copy, Clone)]
enum Blend {
    Opaque,
    /// Half the card, half the picture behind.
    Average,
    /// The picture behind plus a quarter of the card.
    AddQuarter,
}

/// The shown view for an eye at `(dx, dz)` from the card: the heading whose
/// direction is nearest, by comparing the offsets with the tangent of the
/// half-step (`tan 22.5` is `0.414`; `2/5` is used).
pub fn view_toward(dx: i32, dz: i32) -> u8 {
    let (a, b) = (dx.abs(), dz.abs());
    if b * 5 < a * 2 {
        if dx > 0 {
            0
        } else {
            4
        }
    } else if a * 5 < b * 2 {
        if dz > 0 {
            2
        } else {
            6
        }
    } else {
        match (dx > 0, dz > 0) {
            (true, true) => 1,
            (false, true) => 3,
            (false, false) => 5,
            (true, false) => 7,
        }
    }
}

/// Whether `view` still serves an eye at `(dx, dz)`: within about `28`
/// degrees of its heading (`tan 28.4` is `0.54`), a little past the
/// `22.5` where [`view_toward`] would change, so an eye on the edge does not
/// flip between two pictures. Offsets are clamped to 2,500 units so the
/// products stay inside 32 bits.
pub fn view_keep(view: u8, dx: i32, dz: i32) -> bool {
    let Some((&cos, &sin)) = VIEW_COS_Q12
        .get(usize::from(view))
        .zip(VIEW_SIN_Q12.get(usize::from(view)))
    else {
        return false;
    };
    let (dx, dz) = (dx.clamp(-2500, 2500), dz.clamp(-2500, 2500));
    let along = dx * i32::from(cos) + dz * i32::from(sin);
    let across = dz * i32::from(cos) - dx * i32::from(sin);
    along > 0 && across.abs() * 100 < along * 54
}

/// Top-left VRAM pixel of `slot`, given the atlas's own origin.
pub fn slot_origin(atlas: (u16, u16), slot: usize) -> (u16, u16) {
    let (column, row) = (slot % ATLAS_COLUMNS, slot / ATLAS_COLUMNS);
    (
        atlas.0 + column as u16 * SLOT_HALFWORDS,
        atlas.1 + (row as i32 * SLOT_ROWS) as u16,
    )
}

/// One room's card, from the level's tables (world runtime units).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Card {
    /// Where the card stands, `[x, z]`.
    pub centre: [i32; 2],
    /// Height of the card's bottom edge.
    pub base: i32,
    /// Height of the card.
    pub height: i32,
    /// Half its width.
    pub half_width: i32,
    /// 0 for a room's card, 1 and up for a layer of cards beyond the rooms'
    /// ([`CardCamera::outer`]): a card that holds everything behind it from
    /// its distance out to the next layer's.
    pub layer: u8,
}

impl Card {
    /// A card from a table row `[x, z, base, height, half width, layer]`;
    /// `None` for a room that has no card (a zero height).
    pub fn from_row(row: &[i16; 6]) -> Option<Self> {
        (row[3] > 0).then_some(Self {
            centre: [i32::from(row[0]), i32::from(row[1])],
            base: i32::from(row[2]),
            height: i32::from(row[3]),
            half_width: i32::from(row[4]),
            layer: row[5].clamp(0, 255) as u8,
        })
    }
}

/// The distances a layer of cards shows between (plan distance, runtime
/// units).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CardBand {
    /// No card of the layer is drawn closer than this.
    pub near: i32,
    /// None is drawn farther than this.
    pub far: i32,
    /// A card within this distance of `far` fades in.
    pub fade: i32,
}

/// Layers of cards beyond the rooms' that [`CardCamera::outer`] describes.
pub const OUTER_LAYERS: usize = 2;

/// Plan distance, runtime units, standing for "no limit" as the far edge of
/// the last layer of cards (finite so sums with it stay inside 32 bits).
pub const CARD_UNLIMITED: i32 = 1 << 20;

/// The bands of the layers of cards beyond the rooms', given the far edge
/// and the fade width of the rooms' cards. Each layer shows from where the
/// one inside it has finished fading in, to twice as far, fading in over
/// twice the width; the last shows to the horizon. The cook and the guest
/// both work these out, so a level need not carry them.
pub const fn outer_bands(far: i32, fade: i32) -> [CardBand; OUTER_LAYERS] {
    let (far1, fade1) = (far * 2, fade * 2);
    [
        CardBand {
            near: far - fade,
            far: far1,
            fade: fade1,
        },
        CardBand {
            near: far1 - fade1,
            far: CARD_UNLIMITED,
            fade: fade1 * 2,
        },
    ]
}

/// The card of each room, borrowed from the manifest.
#[derive(Copy, Clone, Debug)]
pub struct CardTable<'a> {
    rows: &'a [[i16; 6]],
}

impl<'a> CardTable<'a> {
    /// A table over `rows`, one `[x, z, base, height, half width, 0]` per
    /// room.
    pub const fn new(rows: &'a [[i16; 6]]) -> Self {
        Self { rows }
    }

    /// Whether the level has no cards.
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Rooms the table covers.
    pub const fn len(&self) -> usize {
        self.rows.len()
    }

    /// The card of `room`, if it has one.
    pub fn card(&self, room: usize) -> Option<Card> {
        self.rows.get(room).and_then(Card::from_row)
    }
}

/// A slot as the draw sees it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LiveSlot {
    /// The room whose picture the slot holds, or [`u16::MAX`] for none.
    pub room: u16,
    /// The view the slot holds, or [`NO_VIEW`] while nothing has landed.
    pub view: u8,
    /// The slot holds the view the room is changing to, shown over the one
    /// it is leaving (still in another slot of the same room): drawn half
    /// blended with it and a hair nearer, so the change takes two steps.
    pub fade: bool,
}

impl LiveSlot {
    /// A slot that holds nothing.
    pub const EMPTY: Self = Self {
        room: u16::MAX,
        view: NO_VIEW,
        fade: false,
    };
}

/// What a card draw needs besides the cards.
pub struct CardCamera {
    /// The eye, world runtime units.
    pub eye: [i32; 3],
    /// Screen size in pixels.
    pub screen: [i32; 2],
    /// Smallest depth, in GTE `SZ` units, a corner may have.
    pub near_sz: u16,
    /// The view's right and forward axes in plan ([`super::plan_axes`]).
    pub right: [i32; 2],
    pub forward: [i32; 2],
    /// Half the horizontal field of view as `tan` times 4096, with margin.
    pub half_tan_q12: i32,
    /// Cards farther than this (runtime units, plan distance) are not
    /// drawn.
    pub far: i32,
    /// A room that is drawn whole (`skip`) shows its card from `overlap_from`
    /// plan distance, fading in until `hide_inside`, where it is fully there:
    /// the geometry stays until it leaves the drawn set, further out, so the
    /// card is complete before the geometry goes.
    pub hide_inside: i32,
    /// Where the card of a room drawn whole starts to fade in (at most
    /// `hide_inside`; equal means no fade, the card appears at once).
    pub overlap_from: i32,
    /// No card is drawn closer than this plan distance, drawn whole or not
    /// (the rooms there are drawn whole).
    pub hide_all: i32,
    /// A card within this distance of `far` fades in: its modulation colour
    /// ramps from black to neutral over it.
    pub fade: i32,
    /// The distances of the layers of cards beyond the rooms' (`far`,
    /// `hide_all` and `fade` are layer 0's). A layer with `far` 0 has no cards.
    pub outer: [CardBand; OUTER_LAYERS],
    /// Texture-page word of the atlas's first page (`uv_word(0)` of its
    /// slot), and the atlas's top-left VRAM pixel.
    pub tpage: u16,
    pub atlas: (u16, u16),
}

/// Add the sky's atlas page offsets: the tpage word of the page `page`
/// pages to the right of `first`.
#[inline]
fn page_word(first: u16, page: usize) -> u16 {
    first.wrapping_add(page as u16)
}

/// CLUT word of the palette row of `slot`.
#[inline]
pub fn slot_clut_word(atlas: (u16, u16), slot: usize) -> u16 {
    let (x, y) = slot_origin(atlas, slot);
    let y = y + (PICTURE_ROWS as u16);
    ((x / 16) & 0x3f) | (y << 6)
}

/// Draw the card of every slot in `list` (indices into `live`, as
/// `CardStream::draw_list` gives them) whose room `skip` does not name. The
/// GTE must hold the world view (rotation and the translation of the world
/// origin).
///
/// Packets go forward from `out[0]`, or backward from the end of `out` when
/// `reverse`. The frame's `packets` and `packet_words` say how many;
/// `cells_visible` counts the slots looked at and `quads_visible` the cards
/// drawn.
pub fn draw_cards(
    table: &CardTable<'_>,
    live: &[LiveSlot],
    list: &[u8],
    skip: &[bool],
    camera: &CardCamera,
    out: &mut [u32],
    reverse: bool,
) -> GridFrame {
    draw_cards_marked(table, live, list, skip, camera, out, reverse, &mut |_| {})
}

/// [`draw_cards`], calling `mark(room)` for every room whose card it wrote a
/// packet for (a room more than once only when it is drawn in bands, and
/// then once).
#[allow(clippy::too_many_arguments)]
pub fn draw_cards_marked(
    table: &CardTable<'_>,
    live: &[LiveSlot],
    list: &[u8],
    skip: &[bool],
    camera: &CardCamera,
    out: &mut [u32],
    reverse: bool,
    mark: &mut dyn FnMut(u16),
) -> GridFrame {
    let mut frame = GridFrame::default();
    let start = out.as_mut_ptr();
    // SAFETY: one past the end of the slice.
    let end = unsafe { start.add(out.len()) };
    let mut next = if reverse { end } else { start };
    let mut packets = 0u32;
    let mut looked = 0u16;
    let mut drawn = 0u16;
    for &slot in list {
        let slot = usize::from(slot);
        let Some(entry) = live.get(slot) else {
            continue;
        };
        if entry.view == NO_VIEW {
            continue;
        }
        let Some(card) = table.card(usize::from(entry.room)) else {
            continue;
        };
        looked += 1;
        let (dx, dz) = (
            camera.eye[0] - card.centre[0],
            camera.eye[2] - card.centre[1],
        );
        let band = match card.layer {
            0 => CardBand {
                near: camera.hide_all,
                far: camera.far,
                fade: camera.fade,
            },
            layer => camera.outer[usize::from(layer - 1).min(OUTER_LAYERS - 1)],
        };
        if dx.abs() > band.far || dz.abs() > band.far {
            continue;
        }
        // Plan view cone on the card's centre, with its half width as
        // slack: behind the eye or outside the field of view is skipped
        // before anything costly is worked out.
        let (ex, ez) = (-dx, -dz);
        let ahead = (camera.forward[0] * ex + camera.forward[1] * ez) >> 12;
        let across = (camera.right[0] * ex + camera.right[1] * ez) >> 12;
        let slack = card.half_width;
        if ahead < -slack || across.abs() > ((ahead.max(0) * camera.half_tan_q12) >> 12) + slack {
            continue;
        }
        let length = isqrt_i32(dx * dx + dz * dz).max(1);
        if length > band.far {
            continue;
        }
        // A room drawn whole has no card closer than `overlap_from`; from
        // there its card fades in (below), so by `hide_inside` it is complete
        // behind the geometry, which only goes when the room leaves the drawn
        // set, further out.
        let whole = card.layer == 0 && skip.get(usize::from(entry.room)).copied().unwrap_or(false);
        if length < band.near || (whole && length < camera.overlap_from) {
            continue;
        }
        // The card's right: the eye looks along (-dx, -dz), whose right
        // is (dz, -dx).
        let inverse = (1 << 24) / length;
        let rx = (dz * inverse) >> 12;
        let rz = (-dx * inverse) >> 12;
        let wx = (rx * card.half_width) >> 12;
        let wz = (rz * card.half_width) >> 12;
        let (cx, cz) = (card.centre[0], card.centre[1]);
        let (bottom, top) = (card.base, card.base + card.height);
        let corners = [
            (cx - wx, top, cz - wz),
            (cx + wx, top, cz + wz),
            (cx + wx, bottom, cz + wz),
            (cx - wx, bottom, cz - wz),
        ];
        let screen = project4(corners, camera);
        let any = screen[0].code | screen[1].code | screen[2].code | screen[3].code;
        if any & NEAR != 0
            || screen[0].code & screen[1].code & screen[2].code & screen[3].code & OFF_ANY != 0
        {
            continue;
        }
        // Fading in at the far edge, over `fade` units: opaque, then half
        // blended with what is behind it, then a quarter added to it with
        // its brightness ramping up from nothing.
        // The same three steps lead a card in at the near end over a room
        // drawn whole, from `overlap_from` to `hide_inside`.
        let (in_fade, width) = if whole && length < camera.hide_inside {
            (
                length - camera.overlap_from,
                camera.hide_inside - camera.overlap_from,
            )
        } else {
            (band.far - length, band.fade)
        };
        let (brightness, blend) = if in_fade >= width {
            (128, Blend::Opaque)
        } else if in_fade * 2 >= width {
            (128, Blend::Average)
        } else {
            (
                (256 * in_fade / width.max(1)).clamp(0, 128),
                Blend::AddQuarter,
            )
        };
        // A view fading in over the one it replaces is half blended with it.
        let blend = match blend {
            Blend::Opaque if entry.fade => Blend::Average,
            other => other,
        };
        let brightness = brightness as u32;
        if brightness == 0 {
            continue;
        }
        // A card too tall for the GPU's 511 row limit is drawn as a few
        // bands, each its own quad; a band wholly off screen is dropped.
        let bands = if gpu_safe(&screen) {
            1
        } else {
            let (top_y, bottom_y) = screen.iter().fold((i32::MAX, i32::MIN), |(lo, hi), s| {
                let y = i32::from((s.sxy >> 16) as u16 as i16);
                (lo.min(y), hi.max(y))
            });
            ((bottom_y - top_y + 399) / 400).clamp(2, MAX_BANDS)
        };
        let column = slot % ATLAS_COLUMNS;
        let row = slot / ATLAS_COLUMNS;
        let u0 = ((column % SLOTS_PER_PAGE) * TEXELS as usize) as u8;
        let v0 = (row as i32 * SLOT_ROWS) as u8;
        let u1 = u0 + (TEXELS - 1) as u8;
        let tpage = page_word(camera.tpage, column / SLOTS_PER_PAGE);
        let clut = slot_clut_word(camera.atlas, slot);
        let mut card_drawn = false;
        for band in 0..bands {
            let band_screen = if bands == 1 {
                screen
            } else {
                let high = top - card.height * band / bands;
                let low = top - card.height * (band + 1) / bands;
                project4(
                    [
                        (corners[0].0, high, corners[0].2),
                        (corners[1].0, high, corners[1].2),
                        (corners[2].0, low, corners[2].2),
                        (corners[3].0, low, corners[3].2),
                    ],
                    camera,
                )
            };
            if !drawable(&band_screen) {
                continue;
            }
            let sum = u32::from(band_screen[0].sz)
                + u32::from(band_screen[1].sz)
                + u32::from(band_screen[2].sz)
                + u32::from(band_screen[3].sz);
            // The view fading in sorts a step nearer, so it is drawn after the
            // one it blends with.
            let mut otz = (sum >> 4) - u32::from(entry.fade && sum >= 32);
            if card.layer > 0 {
                // A far layer sorts behind everything, however far.
                otz = otz.min(2047);
            }
            if otz == 0 || otz >= 2048 {
                frame.depth_skipped += 1;
                continue;
            }
            // SAFETY: both pointers lie in the packet storage.
            let left = unsafe {
                if reverse {
                    next.offset_from(start)
                } else {
                    end.offset_from(next)
                }
            };
            if left < CARD_PACKET_WORDS as isize {
                frame.overflowed = true;
                break;
            }
            let rows = PICTURE_ROWS - 1;
            let v_high = v0 + (rows * band / bands) as u8;
            let v_low = v0 + (rows * (band + 1) / bands) as u8;
            // SAFETY: room for one packet was just checked.
            unsafe {
                let at = if reverse {
                    next.sub(CARD_PACKET_WORDS)
                } else {
                    next
                };
                write_card(
                    at,
                    &band_screen,
                    [(u0, v_high), (u1, v_high), (u1, v_low), (u0, v_low)],
                    clut,
                    tpage,
                    brightness,
                    blend,
                    otz,
                );
                next = if reverse {
                    at
                } else {
                    at.add(CARD_PACKET_WORDS)
                };
            }
            packets += 1;
            card_drawn = true;
        }
        if frame.overflowed {
            break;
        }
        drawn += u16::from(card_drawn);
        if card_drawn {
            mark(entry.room);
        }
    }
    frame.packets = packets;
    frame.cells_visible = looked;
    frame.quads_visible = drawn;
    // SAFETY: both pointers lie in the packet storage.
    frame.packet_words = unsafe {
        if reverse {
            end.offset_from(next)
        } else {
            next.offset_from(start)
        }
    } as usize;
    frame
}

/// Write one card packet at `at`. The corners are top-left, top-right,
/// bottom-right and bottom-left of the picture.
///
/// The GPU splits a quad `(a, b, c, d)` along `b-c`, so the corners go out as
/// `(1, 0, 2, 3)` to split along the top-left to bottom-right diagonal.
///
/// # Safety
/// `at` must have room for [`CARD_PACKET_WORDS`].
#[inline(always)]
unsafe fn write_card(
    at: *mut u32,
    s: &[GridScreen; 4],
    uv: [(u8, u8); 4],
    clut: u16,
    tpage: u16,
    brightness: u32,
    blend: Blend,
    otz: u32,
) {
    let texel = |k: usize| u32::from(uv[k].0) | (u32::from(uv[k].1) << 8);
    let colour = brightness | brightness << 8 | brightness << 16;
    let (command, tpage) = match blend {
        Blend::Opaque => (TEXTURED_QUAD_COMMAND, tpage),
        // Semi-transparent primitive; the page's blend bits (5 and 6) pick
        // the mix: 0 half and half, 3 a quarter of the card added.
        Blend::Average => (TEXTURED_QUAD_COMMAND | SEMI_TRANSPARENT, tpage & !0x60),
        Blend::AddQuarter => (
            TEXTURED_QUAD_COMMAND | SEMI_TRANSPARENT,
            (tpage & !0x60) | 0x60,
        ),
    };
    // SAFETY: the caller reserved room for the whole packet.
    unsafe {
        at.write(((CARD_PACKET_WORDS as u32 - 1) << 24) | otz);
        // Texture window off: other packets may have left one set.
        at.add(1).write(0xe200_0000);
        at.add(2).write(command | colour);
        at.add(3).write(s[1].sxy);
        at.add(4).write(texel(1) | (u32::from(clut) << 16));
        at.add(5).write(s[0].sxy);
        at.add(6).write(texel(0) | (u32::from(tpage) << 16));
        at.add(7).write(s[2].sxy);
        at.add(8).write(texel(2));
        at.add(9).write(s[3].sxy);
        at.add(10).write(texel(3));
    }
}

/// A GTE vertex from world coordinates the cook proved fit 16 bits.
#[inline(always)]
fn vertex(p: (i32, i32, i32)) -> GteVec3I16 {
    GteVec3I16::new(p.0 as i16, p.1 as i16, p.2 as i16)
}

/// Outcode of a projected corner.
#[inline(always)]
fn screen_of(p: scene::Projected, camera: &CardCamera) -> GridScreen {
    let sx = i32::from(p.sx);
    let sy = i32::from(p.sy);
    let mut code = 0u16;
    if p.sz < camera.near_sz {
        code = NEAR;
    } else {
        if sx < 0 {
            code |= OFF_LEFT;
        }
        if sx >= camera.screen[0] {
            code |= OFF_RIGHT;
        }
        if sy < 0 {
            code |= OFF_TOP;
        }
        if sy >= camera.screen[1] {
            code |= OFF_BOTTOM;
        }
    }
    GridScreen {
        sxy: (p.sx as u16 as u32) | ((p.sy as u16 as u32) << 16),
        sz: p.sz,
        code,
    }
}

/// Project four corners: a triangle of the GTE and one more.
#[inline(always)]
fn project4(corners: [(i32, i32, i32); 4], camera: &CardCamera) -> [GridScreen; 4] {
    let t = scene::project_triangle_scheduled(
        vertex(corners[0]),
        vertex(corners[1]),
        vertex(corners[2]),
    );
    let d = scene::project_vertex_scheduled(vertex(corners[3]));
    [
        screen_of(t[0], camera),
        screen_of(t[1], camera),
        screen_of(t[2], camera),
        screen_of(d, camera),
    ]
}

/// Whether the GPU draws a quad with these corners as given: every corner
/// inside 11 bits and the span under the 1024 by 512 limit.
#[inline(always)]
fn gpu_safe(corners: &[GridScreen; 4]) -> bool {
    let (mut left, mut right, mut top, mut bottom) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for corner in corners {
        let x = i32::from(corner.sxy as u16 as i16);
        let y = i32::from((corner.sxy >> 16) as u16 as i16);
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    left > -1000
        && right < 1000
        && top > -1000
        && bottom < 1000
        && right - left <= 1023
        && bottom - top <= 511
}

/// Whether a card of these corners is worth a packet: nothing at or behind
/// the near plane, not wholly off one screen edge, safe for the GPU.
#[inline(always)]
fn drawable(s: &[GridScreen; 4]) -> bool {
    let any = s[0].code | s[1].code | s[2].code | s[3].code;
    if any & NEAR != 0 {
        return false;
    }
    if s[0].code & s[1].code & s[2].code & s[3].code & OFF_ANY != 0 {
        return false;
    }
    gpu_safe(s)
}

/// Words of one backdrop packet: the tag and a Gouraud quad.
pub const BACKDROP_PACKET_WORDS: usize = 9;

/// The colours of the backdrop behind the cards, `[r, g, b]` each.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Backdrop {
    /// Everything below the horizon.
    pub below: [u8; 3],
    /// The sky at the horizon.
    pub horizon: [u8; 3],
    /// The sky at the top of the screen.
    pub top: [u8; 3],
}

/// Screen `y` (pixels from the top) of the horizon at screen column `x`,
/// for a view with the rotation rows `rotation` (`[0]` right, `[1]` down,
/// `[2]` forward, in world axes with `y` up), projection `focal` and a
/// screen of `screen` pixels. `None` when the view is too steep to say.
pub fn horizon_y(rotation: &[[i16; 3]; 3], focal: i32, screen: [i32; 2], x: i32) -> Option<i32> {
    let down = i32::from(rotation[1][1]);
    if down.abs() < 16 {
        return None;
    }
    let numerator =
        (x - screen[0] / 2) * i32::from(rotation[0][1]) + focal * i32::from(rotation[2][1]);
    Some(screen[1] / 2 - numerator / down)
}

/// Write the backdrop into `out` (at most `2 * BACKDROP_PACKET_WORDS`): a
/// gradient from the horizon colour up to the top colour above the horizon
/// and a flat fill below it. Written after the cards, so it runs before
/// them. Returns `(words, packets)`.
pub fn write_backdrop(
    rotation: &[[i16; 3]; 3],
    focal: i32,
    screen: [i32; 2],
    colours: Backdrop,
    slot: u16,
    out: &mut [u32],
) -> (usize, u32) {
    let (w, h) = (screen[0], screen[1]);
    // A view too steep to place the horizon looks at the sky or the void.
    let looking_up = rotation[2][1] > 0;
    let at = |x: i32| {
        horizon_y(rotation, focal, screen, x)
            .unwrap_or(if looking_up { h } else { 0 })
            .clamp(0, h)
    };
    let (left, right) = (at(0), at(w));
    let mut words = 0usize;
    let mut packets = 0u32;
    let mut put = |corners: [(i32, i32); 4], colour: [[u8; 3]; 4]| {
        if words + BACKDROP_PACKET_WORDS > out.len() {
            return;
        }
        let pack = |c: [u8; 3]| u32::from(c[0]) | u32::from(c[1]) << 8 | u32::from(c[2]) << 16;
        let xy = |(x, y): (i32, i32)| (x as u16 as u32) | ((y as u16 as u32) << 16);
        let o = &mut out[words..words + BACKDROP_PACKET_WORDS];
        o[0] = ((BACKDROP_PACKET_WORDS as u32 - 1) << 24) | u32::from(slot);
        o[1] = 0x3800_0000 | pack(colour[0]);
        o[2] = xy(corners[0]);
        o[3] = pack(colour[1]);
        o[4] = xy(corners[1]);
        o[5] = pack(colour[2]);
        o[6] = xy(corners[2]);
        o[7] = pack(colour[3]);
        o[8] = xy(corners[3]);
        words += BACKDROP_PACKET_WORDS;
        packets += 1;
    };
    if left < h || right < h {
        put([(0, left), (w, right), (0, h), (w, h)], [colours.below; 4]);
    }
    if left > 0 || right > 0 {
        put(
            [(0, 0), (w, 0), (0, left), (w, right)],
            [colours.top, colours.top, colours.horizon, colours.horizon],
        );
    }
    (words, packets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn the_heading_tables_are_unit_vectors_an_eighth_of_a_turn_apart() {
        for k in 0..VIEWS {
            let (c, s) = (i32::from(VIEW_COS_Q12[k]), i32::from(VIEW_SIN_Q12[k]));
            let length = c * c + s * s;
            assert!(
                (length - 4096 * 4096).abs() < 2 * 4096,
                "view {k}: {length}"
            );
            // Two steps on, the sine becomes the old cosine.
            assert_eq!(VIEW_SIN_Q12[(k + 2) % VIEWS], VIEW_COS_Q12[k]);
            assert_eq!(VIEW_COS_Q12[(k + 2) % VIEWS], -VIEW_SIN_Q12[k]);
        }
    }

    #[test]
    fn the_nearest_view_is_the_one_whose_heading_the_eye_is_on() {
        for k in 0..VIEWS {
            // An eye 1,000 units out along heading k, then 15 degrees either
            // side of it: still view k.
            for turn in [-15i32, 0, 15] {
                let angle = (k as f64 * 45.0 + f64::from(turn)).to_radians();
                let (dx, dz) = (
                    (angle.cos() * 1000.0).round() as i32,
                    (angle.sin() * 1000.0).round() as i32,
                );
                assert_eq!(usize::from(view_toward(dx, dz)), k, "view {k} turn {turn}");
            }
        }
    }

    #[test]
    fn a_view_is_kept_a_little_past_where_the_nearest_changes() {
        // 25 degrees from heading 0: view 1 is nearer, view 0 is still kept.
        let angle = 25.0f64.to_radians();
        let (dx, dz) = (
            (angle.cos() * 1000.0).round() as i32,
            (angle.sin() * 1000.0).round() as i32,
        );
        assert_eq!(view_toward(dx, dz), 1);
        assert!(view_keep(0, dx, dz));
        // 35 degrees: no longer.
        let angle = 35.0f64.to_radians();
        let (dx, dz) = (
            (angle.cos() * 1000.0).round() as i32,
            (angle.sin() * 1000.0).round() as i32,
        );
        assert!(!view_keep(0, dx, dz));
        assert!(view_keep(1, dx, dz));
        // Behind the heading is never kept.
        assert!(!view_keep(0, -1000, 0));
    }

    #[test]
    fn slots_tile_the_sky_atlas() {
        assert_eq!(SLOTS, 96);
        assert_eq!(
            SLOT_BYTES,
            usize::from(SLOT_HALFWORDS) * SLOT_ROWS as usize * 2
        );
        assert_eq!(
            usize::from(SLOT_HALFWORDS) * ATLAS_COLUMNS,
            1536 / 4,
            "the columns span the atlas"
        );
        assert_eq!(SLOT_ROWS as usize * ATLAS_ROWS, 256);
        assert_eq!(slot_origin((320, 0), 0), (320, 0));
        assert_eq!(slot_origin((320, 0), 25), (320 + 16, 64));
        assert_eq!(slot_origin((320, 0), 95), (320 + 23 * 16, 192));
        // A palette row starts on a 16 halfword boundary.
        for slot in 0..SLOTS {
            assert_eq!(slot_origin((320, 0), slot).0 % 16, 0);
        }
    }

    #[test]
    fn the_horizon_of_a_level_view_is_the_middle_of_the_screen() {
        // Rows of a view looking along +z with no pitch: right is +x, down is
        // -y, forward is +z (Q12).
        let rotation = [[4096, 0, 0], [0, -4096, 0], [0, 0, 4096]];
        assert_eq!(horizon_y(&rotation, 320, [320, 240], 160), Some(120));
        // Straight up or down has no horizon.
        let up = [[4096, 0, 0], [0, 0, 4096], [0, 4096, 0]];
        assert_eq!(horizon_y(&up, 320, [320, 240], 160), None);
    }

    /// Draw one card standing at `centre` (`[x, z]`) for an eye at `eye` with
    /// the given heading (Q12 sine and cosine of the yaw: yaw 0 looks along
    /// +X); returns the packet words, or `None` when nothing was drawn.
    fn draw_one(
        eye: [i32; 3],
        yaw: (i16, i16),
        card_row: [i16; 6],
        hide_inside: i32,
        skipped: bool,
    ) -> Option<Vec<u32>> {
        draw_one_overlap(eye, yaw, card_row, hide_inside, hide_inside, skipped)
    }

    /// [`draw_one`] with a card that fades in over a room drawn whole between
    /// `overlap_from` and `hide_inside`.
    fn draw_one_overlap(
        eye: [i32; 3],
        yaw: (i16, i16),
        card_row: [i16; 6],
        overlap_from: i32,
        hide_inside: i32,
        skipped: bool,
    ) -> Option<Vec<u32>> {
        use crate::render::{load_view_rotation, plan_axes, view_rotation};
        use psx_gte::math::Vec3I32;
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(320);
        let rotation = view_rotation(0, 0x1000, yaw.0, yaw.1);
        let view = load_view_rotation(
            Vec3I32::new(eye[0] << 12, eye[1] << 12, eye[2] << 12),
            rotation,
        );
        let (right, forward) = plan_axes(&view.rotation.m);
        let rows = [card_row];
        let table = CardTable::new(&rows);
        let live = [LiveSlot {
            room: 0,
            view: 0,
            fade: false,
        }];
        let camera = CardCamera {
            eye,
            screen: [320, 240],
            near_sz: 162,
            right,
            forward,
            half_tan_q12: 4096 * 6 / 5,
            far: 1600,
            hide_inside,
            overlap_from,
            hide_all: 0,
            fade: 240,
            outer: [CardBand::default(); OUTER_LAYERS],
            tpage: 0x0105,
            atlas: (320, 0),
        };
        let mut out = vec![0u32; 64];
        let frame = draw_cards(&table, &live, &[0], &[skipped], &camera, &mut out, false);
        (frame.packets > 0).then(|| out[..frame.packet_words].to_vec())
    }

    #[test]
    fn a_card_ahead_of_the_eye_is_drawn_upright_and_unmirrored() {
        // The eye at the origin looking along +X; a card 800 units ahead,
        // 400 wide and 300 tall standing on the ground 60 below the eye.
        let packet = draw_one([0, 0, 0], (0, 0x1000), [800, 0, -60, 300, 200, 0], 0, false)
            .expect("the card is in view");
        assert_eq!(packet.len(), CARD_PACKET_WORDS);
        let xy = |word: u32| (word as u16 as i16, (word >> 16) as u16 as i16);
        // The words go out as (top-right, top-left, bottom-right,
        // bottom-left); texel columns say which side each is.
        let (tr, tl, br, bl) = (xy(packet[3]), xy(packet[5]), xy(packet[7]), xy(packet[9]));
        let texel = |word: u32| (word as u8, (word >> 8) as u8);
        assert!(texel(packet[4]).0 > texel(packet[6]).0);
        // Facing +X, the card's right (+Z) is the screen's right.
        assert!(tl.0 < tr.0 && bl.0 < br.0, "{tl:?} {tr:?} {bl:?} {br:?}");
        // The top is above the bottom, the horizon (y = 120) between them
        // since the eye sits between the ground and the top.
        assert!(tl.1 < bl.1 && tr.1 < br.1);
        assert!(tl.1 < 120 && bl.1 > 120);
        // And it is centred: the eye looks straight at it.
        assert!(((tl.0 + tr.0) / 2 - 160).abs() <= 2);
    }

    #[test]
    fn the_card_turns_to_face_the_eye_wherever_the_eye_is() {
        // The same card seen from the +Z side (yaw 90 degrees looks along
        // -Z... the card is then at the eye's -Z): the picture is still
        // facing us, left to right.
        let (sin, cos) = (0x1000i16, 0i16);
        // Yaw by a quarter turn: the rotation looks along +Z or -Z; put
        // the card on that side to be in view either way.
        for card_z in [800i16, -800] {
            if let Some(packet) = draw_one(
                [0, 0, 0],
                (sin, cos),
                [0, card_z, -60, 300, 200, 0],
                0,
                false,
            ) {
                let xy = |word: u32| (word as u16 as i16, (word >> 16) as u16 as i16);
                let (tr, tl) = (xy(packet[3]), xy(packet[5]));
                assert!(tl.0 < tr.0, "card at z {card_z}: {tl:?} {tr:?}");
            }
        }
    }

    #[test]
    fn a_room_drawn_whole_keeps_its_card_only_while_the_eye_is_far() {
        let row = [800, 0, -60, 300, 200, 0];
        // Drawn whole, 800 units off, hidden inside 600: shown.
        assert!(draw_one([0, 0, 0], (0, 0x1000), row, 600, true).is_some());
        // Hidden inside 1000: gone.
        assert!(draw_one([0, 0, 0], (0, 0x1000), row, 1000, true).is_none());
        // Not drawn whole: shown whatever the radius.
        assert!(draw_one([0, 0, 0], (0, 0x1000), row, 1000, false).is_some());
    }

    #[test]
    fn a_card_too_tall_for_one_polygon_is_cut_into_bands() {
        // 3,000 units tall at 800: far over 511 rows on screen.
        let packet = draw_one(
            [0, 0, 0],
            (0, 0x1000),
            [800, 0, -1000, 3000, 200, 0],
            0,
            false,
        )
        .expect("the card is in view");
        assert!(packet.len() > CARD_PACKET_WORDS, "{} words", packet.len());
        assert_eq!(packet.len() % CARD_PACKET_WORDS, 0);
        // Each band's picture rows are a slice of the picture, together all
        // of it.
        let mut rows = Vec::new();
        for band in packet.chunks(CARD_PACKET_WORDS) {
            rows.push(((band[6] >> 8) as u8, (band[8] >> 8) as u8));
        }
        assert!(rows.iter().all(|&(top, bottom)| top <= bottom));
    }

    #[test]
    fn a_card_fades_in_over_the_last_stretch_before_the_far_edge() {
        let at = |x: i16| {
            draw_one([0, 0, 0], (0, 0x1000), [x, 0, -60, 300, 200, 0], 0, false)
                .map(|p| (p[2] >> 24, p[2] & 0xff, p[6] >> 16))
        };
        // Well inside: opaque command, full brightness.
        let (command, brightness, _) = at(800).unwrap();
        assert_eq!((command, brightness), (0x2c, 128));
        // Past far - fade/2 = 1480: a quarter added, dim.
        let (command, brightness, tpage) = at(1560).unwrap();
        assert_eq!(command, 0x2e);
        assert!(brightness < 128 && brightness > 0, "{brightness}");
        assert_eq!(tpage & 0x60, 0x60);
        // Between: half and half.
        let (command, brightness, tpage) = at(1400).unwrap();
        assert_eq!((command, brightness, tpage & 0x60), (0x2e, 128, 0));
        // Beyond the far edge: nothing.
        assert!(at(1650).is_none());
    }

    #[test]
    fn the_card_of_a_room_drawn_whole_fades_in_between_two_distances() {
        // Overlap from 500 to 700: the card is absent before 500, then a
        // quarter added, dim; then half and half; then opaque.
        let at = |x: i16| {
            draw_one_overlap(
                [0, 0, 0],
                (0, 0x1000),
                [x, 0, -60, 300, 200, 0],
                500,
                700,
                true,
            )
            .map(|p| (p[2] >> 24, p[2] & 0xff, p[6] >> 16))
        };
        assert!(at(480).is_none());
        let (command, brightness, tpage) = at(530).unwrap();
        assert_eq!(command, 0x2e);
        assert!(brightness > 0 && brightness < 128, "{brightness}");
        assert_eq!(tpage & 0x60, 0x60);
        let (command, brightness, tpage) = at(620).unwrap();
        assert_eq!((command, brightness, tpage & 0x60), (0x2e, 128, 0));
        let (command, brightness, _) = at(720).unwrap();
        assert_eq!((command, brightness), (0x2c, 128));
        // A room not drawn whole shows its card at any distance.
        assert!(draw_one_overlap(
            [0, 0, 0],
            (0, 0x1000),
            [300, 0, -60, 300, 200, 0],
            500,
            700,
            false
        )
        .is_some());
    }

    #[test]
    fn a_card_table_row_with_no_height_is_no_card() {
        assert_eq!(Card::from_row(&[0; 6]), None);
        let card = Card::from_row(&[100, 200, -30, 400, 320, 0]).unwrap();
        assert_eq!(card.centre, [100, 200]);
        assert_eq!(card.half_width, 320);
    }
}
