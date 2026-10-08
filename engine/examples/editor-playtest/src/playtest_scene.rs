use super::*;

const PLAYER_HEALTH_MAX_Q12: i32 = 4096;
const INVENTORY_UI_MODE_MASK: u8 = 0x03;
const INVENTORY_UI_MODULE_SHIFT: u8 = 2;
pub(crate) const INVENTORY_UI_SOCKETS: u8 = 0;
const INVENTORY_UI_MODULES: u8 = 1;
const INVENTORY_UI_ASSIGN: u8 = 2;
const INVENTORY_ITEM_COUNT: u8 = 3;
const GAMEPLAY_SCENE_STATE_NAME: &str = "Gameplay";
const INVENTORY_SCENE_STATE_NAME: &str = "Inventory Overlay";

/// Find the authored Basic 8x8 atlas used by HUD feedback. Cooked font slots
/// are ordered by first use, so a hard-coded index would change whenever an
/// earlier UI node changes face. Projects without Basic retain slot-zero as a
/// safe fallback.
fn damage_font_slot() -> usize {
    UI_FONTS
        .iter()
        .position(|font| core::ptr::eq(*font, DAMAGE_NUMBER_FACE))
        .unwrap_or(0)
}

fn boost_module(id: BoostModuleId) -> Option<&'static psx_level::BoostModuleRecord> {
    id.index().and_then(|index| BOOST_MODULES.get(index))
}

fn boost_module_name(id: BoostModuleId) -> &'static str {
    match (id.index(), boost_module(id)) {
        (Some(index), Some(module)) => crate::loc::module_name(index, module.name),
        _ => crate::loc::tr("ui.inventory.none", "NONE"),
    }
}

/// Socket buttons are only 63 pixels wide at the native 320x240 resolution.
/// These compact names match the three authored Cortex modules; the analysis
/// pane still presents each module's complete authored name.
fn boost_module_socket_name(id: BoostModuleId) -> &'static str {
    use crate::loc::tr;
    match id.index() {
        Some(0) => tr("ui.module.short.rupture", "RUPTURE"),
        Some(1) => tr("ui.module.short.zenith", "ZENITH"),
        Some(2) => tr("ui.module.short.shell", "SHELL"),
        Some(_) => tr("ui.module.short.module", "MODULE"),
        None => tr("ui.inventory.none", "NONE"),
    }
}

struct UiScratch<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl<'a> UiScratch<'a> {
    fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, len: 0 }
    }

    fn push_str(&mut self, text: &str) {
        let remaining = self.bytes.len().saturating_sub(self.len);
        let count = text.len().min(remaining);
        self.bytes[self.len..self.len + count].copy_from_slice(&text.as_bytes()[..count]);
        self.len += count;
    }

    fn push_signed_percent_q12(&mut self, value_q12: i32) {
        let percent = psx_game_runtime::vitality::q12_to_percent(value_q12);
        self.push_str(if percent < 0 { "-" } else { "+" });
        self.push_u32(percent.unsigned_abs());
        self.push_str("%");
    }

    /// Decimal `value`, 32-bit division only. The R3000 has no 64-bit divide,
    /// so HUD numbers stay in `u32`.
    fn push_u32(&mut self, mut value: u32) {
        let mut digits = [0u8; 10];
        let mut count = 0usize;
        loop {
            digits[count] = b'0' + (value % 10) as u8;
            count += 1;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        while count > 0 {
            count -= 1;
            if self.len >= self.bytes.len() {
                return;
            }
            self.bytes[self.len] = digits[count];
            self.len += 1;
        }
    }

    fn finish(self) -> Option<&'a str> {
        core::str::from_utf8(&self.bytes[..self.len]).ok()
    }
}

impl Playtest {
    #[inline]
    fn inventory_ui_mode(&self) -> u8 {
        self.inventory_ui_state & INVENTORY_UI_MODE_MASK
    }

    #[inline]
    fn inventory_module_cursor(&self) -> u8 {
        (self.inventory_ui_state >> INVENTORY_UI_MODULE_SHIFT)
            .min(INVENTORY_ITEM_COUNT.saturating_sub(1))
    }

    #[inline]
    fn set_inventory_ui_mode(&mut self, mode: u8) {
        self.inventory_ui_state =
            (self.inventory_ui_state & !INVENTORY_UI_MODE_MASK) | (mode & INVENTORY_UI_MODE_MASK);
    }

    #[inline]
    fn set_inventory_module_cursor(&mut self, index: u8) {
        self.inventory_ui_state = (self.inventory_ui_state & INVENTORY_UI_MODE_MASK)
            | (index.min(INVENTORY_ITEM_COUNT.saturating_sub(1)) << INVENTORY_UI_MODULE_SHIFT);
    }

    fn first_inventory_item_index(&self) -> Option<u8> {
        (0..INVENTORY_ITEM_COUNT).find(|index| !self.power_up_inventory.item_at(*index).is_none())
    }

    fn requested_gameplay_state(&self, ctx: &mut Ctx) {
        if let Some(state) = crate::generated::SCENE_STATES
            .iter()
            .find(|state| state.name == GAMEPLAY_SCENE_STATE_NAME)
        {
            ctx.request_scene_state(state.id);
        }
    }

    /// Answer one `souls.` text tag. `tag` arrives with the namespace already
    /// stripped, so this is a two-arm compare on the remainder.
    ///
    /// The soul total is drawn every frame while the HUD is up, so it formats
    /// with 32-bit division only; a 64-bit formatter would drag in the
    /// software `__udivdi3` this target has no business calling from a
    /// per-frame path.
    fn souls_ui_text<'a>(&self, tag: &str, scratch: &'a mut [u8]) -> Option<&'a str> {
        let mut out = UiScratch::new(scratch);
        match tag {
            "count" => out.push_u32(self.souls.total()),
            "gain" => {
                // The node is gated by `ui_node_visible`, but a scene that
                // binds this tag without the gate must still not read "+0".
                if !self.souls.showing_recent_gain(self.souls_now()) {
                    return None;
                }
                out.push_str("+");
                out.push_u32(self.souls.recent_gain());
            }
            _ => return None,
        }
        out.finish()
    }

    /// Gameplay tick the frame being composed belongs to. The soul popup is a
    /// deadline compare rather than a countdown, so presentation reads the
    /// same snapshot the rest of the overlay animates from instead of the
    /// tick current at flip time (see `overlay_sim_tick`).
    fn souls_now(&self) -> u32 {
        self.prepared_overlay_sim_tick.as_u32()
    }
}

fn write_stat_value(scratch: &mut [u8], bonus_q12: i32) -> Option<&str> {
    let mut out = UiScratch::new(scratch);
    out.push_signed_percent_q12(bonus_q12);
    out.finish()
}

fn target_ui_value(
    binding: LevelUiValueBinding,
    entities: &RuntimeGameEntities,
    target: Option<usize>,
) -> Option<i32> {
    let health_q12 = |current: u16, maximum: u16| {
        if maximum == 0 {
            0
        } else {
            (i32::from(current) * PLAYER_HEALTH_MAX_Q12) / i32::from(maximum)
        }
    };
    let record = target.and_then(|index| GAME_ENTITIES.get(index));
    match binding {
        LevelUiValueBinding::TargetHealth => {
            Some(target.zip(record).map_or(0, |(index, record)| {
                health_q12(entities.health(index), record.max_health)
            }))
        }
        LevelUiValueBinding::TargetHealthMax => Some(record.map_or(0, |record| {
            if record.max_health == 0 {
                0
            } else {
                PLAYER_HEALTH_MAX_Q12
            }
        })),
        LevelUiValueBinding::TargetHealthSecondary => {
            Some(target.zip(record).map_or(0, |(index, record)| {
                health_q12(
                    entities.health_secondary(index),
                    record.max_health_secondary,
                )
            }))
        }
        LevelUiValueBinding::TargetHealthSecondaryMax => Some(record.map_or(0, |record| {
            if record.max_health_secondary == 0 {
                0
            } else {
                PLAYER_HEALTH_MAX_Q12
            }
        })),
        LevelUiValueBinding::TargetStanceSwapProgress => Some(target.map_or(0, |index| {
            i32::from(entities.stance_swap_progress_q12(index))
        })),
        LevelUiValueBinding::TargetStanceActiveIsZenith => Some(target.map_or(0, |index| {
            i32::from(entities.stance(index) == VitalityChannelId::Two)
        })),
        _ => None,
    }
}

#[cfg(test)]
mod target_ui_tests {
    use super::*;

    #[test]
    fn target_bindings_resolve_live_pools_guard_and_absence() {
        assert!(!GAME_ENTITIES.is_empty(), "cortex fixture has an enemy");
        let mut entities = RuntimeGameEntities::EMPTY;
        entities.spawn_from_records(GAME_ENTITIES);
        let record = &GAME_ENTITIES[0];

        assert_eq!(
            target_ui_value(LevelUiValueBinding::TargetHealth, &entities, Some(0)),
            Some(PLAYER_HEALTH_MAX_Q12)
        );
        assert_eq!(
            target_ui_value(LevelUiValueBinding::TargetHealthMax, &entities, Some(0)),
            Some(PLAYER_HEALTH_MAX_Q12)
        );
        assert_eq!(
            target_ui_value(
                LevelUiValueBinding::TargetStanceActiveIsZenith,
                &entities,
                Some(0)
            ),
            Some(0)
        );

        entities.apply_stance_hit(GAME_ENTITIES, 0, VitalityChannelId::One, 10, 0);
        let expected =
            (i32::from(entities.health(0)) * PLAYER_HEALTH_MAX_Q12) / i32::from(record.max_health);
        assert_eq!(
            target_ui_value(LevelUiValueBinding::TargetHealth, &entities, Some(0)),
            Some(expected)
        );
        assert_eq!(
            target_ui_value(LevelUiValueBinding::TargetHealth, &entities, None),
            Some(0)
        );
    }
}

impl Playtest {
    /// The cooked BSP world, then the sky that shares its farthest OT slot.
    fn draw_world_and_sky(
        &mut self,
        camera: WorldCamera,
        room_record: Option<&LevelRoomRecord>,
        sim_tick: SimTick,
        primitive_packets: &mut PrimitivePacketArena<'_>,
        ot: &mut OtFrame<'_, OT_DEPTH>,
    ) {
        let material_tick = self.gameplay_tick(sim_tick).as_u32();
        let mut visible_sky_aperture = false;
        if let Some(bsp) = self.bsp.as_mut() {
            telemetry::stage_begin(telemetry::stage::ROOM);
            sort_probe_class(SORT_CLASS_WORLD);
            let cinematic_visibility = (self.opening.active() && !self.opening.gameplay_camera())
                .then(|| {
                    let player = self.motor.position();
                    RoomPoint::new(player.x, player.y + 32, player.z)
                });
            visible_sky_aperture = bsp.draw(
                camera,
                cinematic_visibility,
                material_tick,
                &self.destructibles,
                primitive_packets,
                ot,
            );
            telemetry::stage_end(telemetry::stage::ROOM);
        }

        // Sky shares the farthest OT slot with the maximum-depth PXBSP packet.
        // OT insertion prepends, so inserting the sky after PXBSP makes DMA
        // execute the sky first and keeps even a slot-2047 wall in front.
        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::SKY);
            sort_probe_class(SORT_CLASS_SKY);
            draw_scene_sky(
                room_record.sky,
                camera,
                material_tick,
                visible_sky_aperture,
                primitive_packets,
                ot,
            );
            telemetry::stage_end(telemetry::stage::SKY);
            sort_probe_class(SORT_CLASS_WORLD);
        }
    }
}

/// Arena words a present-queue frame reserves for its recorded overlay
/// (HUD, panels, damage numbers, fades).
const PRESENT_OVERLAY_WORDS: usize = 1024;

/// Whether a frame's successor, built while this frame waits in the present
/// queue, would reach its paired-arena fence late: in or after the world
/// pass, not in the character passes ahead of it.
///
/// `used_slots` is what this frame took from the arena of `capacity` slots;
/// `queued` says whether it reserved its overlay words (a double-buffered
/// frame did not, so they are added as a queued frame would take them).
/// `world_words` is the world pass's packet words. The successor draws the
/// world first when it fits in the slots this frame leaves free, with the
/// same eighth to spare as `BspRuntime::fits_before_fence`; otherwise it
/// draws everything else first, and the fence lands late only if that fits.
fn queued_successor_fences_late(
    capacity: usize,
    used_slots: usize,
    queued: bool,
    world_words: usize,
) -> bool {
    let overlay_slots = PRESENT_OVERLAY_WORDS / PRIMITIVE_PACKET_SLOT_WORDS;
    let used = if queued {
        used_slots
    } else {
        used_slots + overlay_slots
    };
    let free = capacity.saturating_sub(used);
    if world_words + world_words / 8 <= free * PRIMITIVE_PACKET_SLOT_WORDS {
        return true;
    }
    let world_slots = world_words.div_ceil(PRIMITIVE_PACKET_SLOT_WORDS);
    used.saturating_sub(world_slots + overlay_slots) <= free
}

/// Frames in a row whose packets must fit beside a queued frame before the
/// present queue is offered again. Leaving the queue drains it (most of a
/// frame), so a scene at the edge must not flip between the two paths.
const PRESENT_QUEUE_REENTRY_FRAMES: u16 = 30;
/// A stay in the queue shorter than this counts as a false start and
/// doubles the next re-entry wait, up to [`PRESENT_QUEUE_MAX_BACKOFF`]
/// doublings.
const PRESENT_QUEUE_SHORT_STAY_FRAMES: u16 = 60;
const PRESENT_QUEUE_MAX_BACKOFF: u8 = 4;

impl Playtest {
    /// Decide whether the next frame goes through the present queue.
    ///
    /// A queued frame cannot start drawing until its VBlank kick, so the
    /// next frame's paired-arena fence waits longer than it would in the
    /// double-buffered path. That only pays when the fence lands late
    /// (`fits`, see [`queued_successor_fences_late`]). With the player and
    /// one enemy it does; with two melee enemies the fence lands in the
    /// character passes, where the queue alone was 1.6% slower, so those
    /// frames go double-buffered. The queue is left at once, and re-entered
    /// after [`PRESENT_QUEUE_REENTRY_FRAMES`] fitting frames, twice as many
    /// after each stay shorter than [`PRESENT_QUEUE_SHORT_STAY_FRAMES`].
    fn choose_present_queue(&mut self, fits: bool) {
        if !self.present_queue_held_off {
            if fits {
                self.present_queue_frames = self.present_queue_frames.saturating_add(1);
            } else {
                self.present_queue_backoff =
                    if self.present_queue_frames < PRESENT_QUEUE_SHORT_STAY_FRAMES {
                        (self.present_queue_backoff + 1).min(PRESENT_QUEUE_MAX_BACKOFF)
                    } else {
                        0
                    };
                self.present_queue_held_off = true;
                self.present_queue_frames = 0;
            }
        } else if !fits {
            self.present_queue_frames = 0;
        } else {
            self.present_queue_frames = self.present_queue_frames.saturating_add(1);
            if self.present_queue_frames
                >= PRESENT_QUEUE_REENTRY_FRAMES << self.present_queue_backoff
            {
                self.present_queue_held_off = false;
                self.present_queue_frames = 0;
            }
        }
    }

    /// Hand the state the last render prepared to `render_overlay`, which
    /// draws it once that frame is presented.
    fn commit_overlay_state(&mut self) {
        self.overlay_camera = self.prepared_overlay_camera;
        self.overlay_sim_tick = self.prepared_overlay_sim_tick;
        self.overlay_poi_panel_frame = self.prepared_poi_panel_frame;
        self.overlay_poi_page_type_frame = self.prepared_poi_page_type_frame;
    }
}

impl Scene for Playtest {
    fn render_submission(&self) -> RenderSubmission {
        if cfg!(feature = "present-queue") && !self.present_queue_held_off {
            RenderSubmission::PresentQueue
        } else {
            RenderSubmission::QueuedDoubleBuffered
        }
    }

    fn take_gameplay_sfx_events(&mut self) -> u32 {
        core::mem::take(&mut self.gameplay_sfx_events)
    }

    #[cfg(feature = "cd-stream-bench")]
    fn with_streamed_ui_sfx_sample(
        &mut self,
        index: usize,
        consume: &mut dyn FnMut(&[u8]),
    ) -> bool {
        with_streamed_ui_sfx_sample(index, consume)
    }

    /// Lend the uploaded HUD font to the flow driver so front-end UI
    /// scenes (the cooked Main Menu) draw their labels and buttons with
    /// the same glyphs the in-game HUD uses.
    fn ui_font(&self) -> Option<&FontAtlas> {
        self.ui_fonts[0].as_ref()
    }

    fn ui_font_at(&self, index: u8) -> Option<&FontAtlas> {
        self.ui_fonts
            .get(index as usize)
            .and_then(|font| font.as_ref())
    }

    fn ui_texture(&self, asset_id: AssetId) -> Option<UiTextureSlot> {
        let asset = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture)?;
        // Streamed UI images carry empty baked bytes; they are already in
        // VRAM (loaded on menu entry), so look up the existing slot rather
        // than re-parsing empty bytes through `ensure_ui_texture_uploaded`.
        let slot = if asset.bytes.is_empty() {
            find_room_texture_vram_slot(asset.id)?
        } else {
            ensure_ui_texture_uploaded(asset.id, asset.bytes)?
        };
        Some(UiTextureSlot {
            clut_word: slot.clut_word,
            tpage_word: slot.tpage_word,
            texture_window: slot.texture_window,
            texture_width: slot.texture_width,
            texture_height: slot.texture_height,
        })
    }

    fn ui_value(&self, binding: LevelUiValueBinding) -> Option<i32> {
        if let Some(value) = target_ui_value(
            binding,
            &self.game_entities,
            self.combat_target_entity_index(),
        ) {
            return Some(value);
        }
        let horizon = self.player_vitality.pool(VitalityChannelId::One);
        let zenith = self.player_vitality.pool(VitalityChannelId::Two);
        let health_q12 = |current: u16, maximum: u16| {
            if maximum == 0 {
                PLAYER_HEALTH_MAX_Q12
            } else {
                (i32::from(current) * PLAYER_HEALTH_MAX_Q12) / i32::from(maximum)
            }
        };
        match binding {
            LevelUiValueBinding::PlayerHealth => {
                Some(health_q12(horizon.current(), horizon.maximum()))
            }
            LevelUiValueBinding::PlayerHealthMax => Some(PLAYER_HEALTH_MAX_Q12),
            LevelUiValueBinding::PlayerHealthSecondary => {
                Some(health_q12(zenith.current(), zenith.maximum()))
            }
            LevelUiValueBinding::PlayerHealthSecondaryMax => Some(PLAYER_HEALTH_MAX_Q12),
            // Stance-relative readings. A bar bound to these follows whichever
            // pool is live rather than a fixed colour, so the HUD keeps saying
            // "this is the one taking damage" across a swap.
            LevelUiValueBinding::PlayerStanceActiveHealth => {
                let pool = self.player_vitality.pool(self.player_stance.active());
                Some(health_q12(pool.current(), pool.maximum()))
            }
            LevelUiValueBinding::PlayerStanceActiveHealthMax => Some(PLAYER_HEALTH_MAX_Q12),
            LevelUiValueBinding::PlayerStanceInactiveHealth => {
                let pool = self.player_vitality.pool(self.player_stance.inactive());
                Some(health_q12(pool.current(), pool.maximum()))
            }
            LevelUiValueBinding::PlayerStanceInactiveHealthMax => Some(PLAYER_HEALTH_MAX_Q12),
            LevelUiValueBinding::PlayerStanceSwapProgress => Some(i32::from(
                self.player_stance
                    .swap_progress_q12(&self.player_stance_config),
            )),
            LevelUiValueBinding::PlayerStanceActiveIsZenith => Some(i32::from(
                self.player_stance.active() == VitalityChannelId::Two,
            )),
            LevelUiValueBinding::PlayerStanceActiveBroken => Some(i32::from(
                self.player_stance.is_broken(self.player_stance.active()),
            )),
            LevelUiValueBinding::PlayerStanceInactiveBroken => Some(i32::from(
                self.player_stance.is_broken(self.player_stance.inactive()),
            )),
            LevelUiValueBinding::PlayerHealthEmptyInfluence => {
                Some(i32::from(horizon.polarity().empty_q12))
            }
            LevelUiValueBinding::PlayerHealthFullInfluence => {
                Some(i32::from(horizon.polarity().full_q12))
            }
            LevelUiValueBinding::PlayerHealthSecondaryEmptyInfluence => {
                Some(i32::from(zenith.polarity().empty_q12))
            }
            LevelUiValueBinding::PlayerHealthSecondaryFullInfluence => {
                Some(i32::from(zenith.polarity().full_q12))
            }
            LevelUiValueBinding::PlayerStamina => Some(self.motor.stamina_q12()),
            LevelUiValueBinding::PlayerStaminaMax => Some(self.motor_config().stamina_max_q12),
            _ => None,
        }
    }

    fn ui_text<'a>(&self, tag: &str, scratch: &'a mut [u8]) -> Option<&'a str> {
        // Localisation first: it is a single load and branch in English (see
        // `loc::translate`), and it answers only its own `ui.` namespace, so
        // the gameplay tags below are reached with one extra byte compare.
        if let Some(translated) = crate::loc::translate(tag) {
            return Some(translated);
        }
        use crate::loc::tr;
        // Souls answers ahead of the boost-menu setup below, which resolves a
        // loadout slot and the whole vitality modifier stack before it reaches
        // its match. A HUD label is drawn every frame; it must not pay the
        // inventory screen's setup to print a number.
        if let Some(souls_tag) = tag.strip_prefix("souls.") {
            return self.souls_ui_text(souls_tag, scratch);
        }
        let selected = BoostSlotId::from_index(self.selected_power_up_slot);
        let selected_item = self.selected_power_up_item;
        let slotted_item = self.power_up_loadout.module(selected);
        let inventory_mode = self.inventory_ui_mode();
        let detail_item = match inventory_mode {
            INVENTORY_UI_MODULES => self
                .power_up_inventory
                .item_at(self.inventory_module_cursor()),
            INVENTORY_UI_ASSIGN => selected_item,
            _ => slotted_item,
        };
        let detail = boost_module(detail_item);
        let previewing_module =
            matches!(inventory_mode, INVENTORY_UI_MODULES | INVENTORY_UI_ASSIGN)
                && detail.is_some();
        // The socket view reports the complete configured loadout, including
        // the inactive stance. Otherwise assigning a Zenith module while in
        // Horizon would misleadingly report +0% immediately after assignment.
        let modifiers = self
            .power_up_loadout
            .modifiers(&self.player_vitality, BOOST_MODULES);
        let module_bonus = |stat| {
            detail
                .and_then(|module| module.percentages.get(stat))
                .map(|percent| i32::from(*percent).saturating_mul(4096) / 100)
                .unwrap_or(0)
        };
        let stat_value = |stat, final_value| {
            if previewing_module {
                module_bonus(stat)
            } else {
                final_value
            }
        };
        match tag {
            "boost.horizon.empty" => Some(boost_module_socket_name(
                self.power_up_loadout.module(BoostSlotId::HorizonEmpty),
            )),
            "boost.horizon.full" => Some(boost_module_socket_name(
                self.power_up_loadout.module(BoostSlotId::HorizonFull),
            )),
            "boost.zenith.empty" => Some(boost_module_socket_name(
                self.power_up_loadout.module(BoostSlotId::ZenithEmpty),
            )),
            "boost.zenith.full" => Some(boost_module_socket_name(
                self.power_up_loadout.module(BoostSlotId::ZenithFull),
            )),
            "inventory.item.0" => Some(boost_module_name(self.power_up_inventory.item_at(0))),
            "inventory.item.1" => Some(boost_module_name(self.power_up_inventory.item_at(1))),
            "inventory.item.2" => Some(boost_module_name(self.power_up_inventory.item_at(2))),
            "inventory.empty" => Some(tr("ui.inventory.no_modules", "NO MODULES")),
            "boost.assignment.prompt" => Some(tr("ui.inventory.choose_socket", "CHOOSE A SOCKET")),
            "boost.control.primary" => Some(match self.inventory_ui_mode() {
                INVENTORY_UI_MODULES => tr("ui.inventory.select", "SELECT"),
                INVENTORY_UI_ASSIGN => tr("ui.inventory.assign", "ASSIGN"),
                _ => tr("ui.inventory.modules", "MODULES"),
            }),
            "boost.control.remove" => Some(tr("ui.inventory.remove", "REMOVE")),
            "boost.control.back" => Some(if self.inventory_ui_mode() == INVENTORY_UI_ASSIGN {
                tr("ui.inventory.modules", "MODULES")
            } else {
                tr("ui.inventory.close", "CLOSE")
            }),
            "boost.inventory.selected.name" => Some(detail_item.index().zip(detail).map_or(
                tr("ui.inventory.select_module", "SELECT A MODULE"),
                |(index, module)| crate::loc::module_name(index, module.name),
            )),
            "boost.inventory.selected.stat" => Some(
                detail_item
                    .index()
                    .zip(detail)
                    .map_or("", |(index, module)| {
                        crate::loc::module_description(index, module.description)
                    }),
            ),
            "boost.inventory.selected.count" => {
                if previewing_module {
                    Some(tr("ui.inventory.collected", "COLLECTED"))
                } else if !detail_item.is_none() {
                    Some(tr("ui.inventory.equipped", "EQUIPPED"))
                } else {
                    Some("")
                }
            }
            "boost.selected.name" => Some(
                detail_item
                    .index()
                    .zip(detail)
                    .map_or(tr("ui.inventory.none", "NONE"), |(index, module)| {
                        crate::loc::module_name(index, module.name)
                    }),
            ),
            "boost.selected.effect" => Some(detail.map_or("", |module| module.effect_summary)),
            "boost.selected.base" => Some(if previewing_module {
                tr("ui.inventory.module_effect", "MODULE EFFECT")
            } else {
                tr("ui.inventory.final_stats", "FINAL STATS")
            }),
            "boost.stat.horizon" => write_stat_value(
                scratch,
                stat_value(
                    psx_level::boost_stat::HORIZON_ATTACK,
                    i32::from(modifiers.horizon_damage_q12) - 4096,
                ),
            ),
            "boost.stat.zenith" => write_stat_value(
                scratch,
                stat_value(
                    psx_level::boost_stat::ZENITH_ATTACK,
                    i32::from(modifiers.zenith_damage_q12) - 4096,
                ),
            ),
            "boost.stat.defence" => write_stat_value(
                scratch,
                stat_value(
                    psx_level::boost_stat::DEFENCE,
                    4096 - i32::from(modifiers.incoming_damage_q12),
                ),
            ),
            "boost.stat.movement" => write_stat_value(
                scratch,
                stat_value(
                    psx_level::boost_stat::MOVEMENT_SPEED,
                    i32::from(modifiers.movement_speed_q12) - 4096,
                ),
            ),
            "boost.stat.attack_speed" => write_stat_value(
                scratch,
                stat_value(
                    psx_level::boost_stat::ATTACK_SPEED,
                    i32::from(modifiers.attack_speed_q12) - 4096,
                ),
            ),
            "boost.remove" => Some(if slotted_item.is_none() {
                ""
            } else {
                tr("ui.inventory.remove", "REMOVE")
            }),
            "boost.selected.pole" => Some(match selected.pole() {
                psx_game_runtime::vitality::VitalityPole::Empty => {
                    tr("ui.inventory.target_high_gain", "TARGET // HIGH GAIN")
                }
                psx_game_runtime::vitality::VitalityPole::Full => {
                    tr("ui.inventory.target_stable", "TARGET // STABLE")
                }
            }),
            _ => None,
        }
    }

    fn ui_node_visible(&self, tag: &str) -> bool {
        // Ahead of the loadout resolve below, for the same reason
        // `souls_ui_text` runs ahead of the boost setup in `ui_text`.
        if tag == "souls.gain" {
            return self.souls.showing_recent_gain(self.souls_now());
        }
        let selected = BoostSlotId::from_index(self.selected_power_up_slot);
        match tag {
            "inventory.item.0" => !self.power_up_inventory.item_at(0).is_none(),
            "inventory.item.1" => !self.power_up_inventory.item_at(1).is_none(),
            "inventory.item.2" => !self.power_up_inventory.item_at(2).is_none(),
            "inventory.empty" => self.power_up_inventory.is_empty(),
            "boost.assignment.prompt" => !self.selected_power_up_item.is_none(),
            "boost.remove" => !self.power_up_loadout.module(selected).is_none(),
            "boost.control.remove" => self.inventory_ui_mode() != INVENTORY_UI_MODULES,
            "prompt.cross.runtime" => false,
            // The runtime draws the compact, target-anchored dual vitality
            // stack. Keep the authored node as a font-order/editor preview
            // anchor, but never layer its old large bars over gameplay.
            "target.hud" => false,
            // The runtime overlay now owns the complete player HUD, including
            // the stance names inside its moving bars. Legacy authored labels
            // with these tags otherwise remain underneath the translucent dial
            // and leak through its hollow centre during a swap.
            "stance.horizon.active" | "stance.zenith.active" => false,
            _ => true,
        }
    }

    fn ui_node_focusable(&self, tag: &str) -> bool {
        if !self.inventory_overlay_active {
            return true;
        }
        match tag {
            "boost.horizon.empty"
            | "boost.horizon.full"
            | "boost.zenith.empty"
            | "boost.zenith.full" => self.inventory_ui_mode() != INVENTORY_UI_MODULES,
            "inventory.item.0" | "inventory.item.1" | "inventory.item.2" => {
                self.inventory_ui_mode() == INVENTORY_UI_MODULES
            }
            // Tabs remain shoulder-button destinations, but do not steal the
            // d-pad cursor from the two-stage socket/module flow.
            "tab.player.selected" | "tab.system" | "boost.remove" => false,
            _ => true,
        }
    }

    fn preferred_ui_focus_action(&self) -> Option<u16> {
        if !self.inventory_overlay_active {
            return None;
        }
        if self.inventory_ui_mode() == INVENTORY_UI_MODULES {
            let requested = self.inventory_module_cursor();
            let index = if self.power_up_inventory.item_at(requested).is_none() {
                self.first_inventory_item_index()?
            } else {
                requested
            };
            Some(210 + u16::from(index))
        } else {
            Some(200 + u16::from(self.selected_power_up_slot.min(3)))
        }
    }

    fn game_ui_focus_changed(&mut self, id: u16) {
        match id {
            200..=203 => self.selected_power_up_slot = (id - 200) as u8,
            210..=212 => self.set_inventory_module_cursor((id - 210) as u8),
            _ => {}
        }
    }

    fn game_ui_action(&mut self, id: u16, _ctx: &mut Ctx) {
        if id == crate::loc::LANGUAGE_TOGGLE_ACTION {
            crate::loc::cycle_language();
            return;
        }
        if let Some(item_index) = match id {
            210 => Some(0),
            211 => Some(1),
            212 => Some(2),
            _ => None,
        } {
            if self.inventory_ui_mode() != INVENTORY_UI_MODULES {
                return;
            }
            let module = self.power_up_inventory.item_at(item_index);
            if !module.is_none() {
                self.set_inventory_module_cursor(item_index);
                // The socket was chosen before entering the module list, so
                // choosing the module completes the assignment: no third
                // press back on the socket.
                let slot = BoostSlotId::from_index(self.selected_power_up_slot);
                if self
                    .power_up_inventory
                    .assign(&mut self.power_up_loadout, slot, module)
                {
                    self.selected_power_up_item = BoostModuleId::NONE;
                    self.set_inventory_ui_mode(INVENTORY_UI_SOCKETS);
                } else {
                    self.selected_power_up_item = module;
                    self.set_inventory_ui_mode(INVENTORY_UI_ASSIGN);
                }
            }
            return;
        }

        if id == 220 {
            let slot = BoostSlotId::from_index(self.selected_power_up_slot);
            if self
                .power_up_inventory
                .assign(&mut self.power_up_loadout, slot, BoostModuleId::NONE)
            {
                self.selected_power_up_item = BoostModuleId::NONE;
            }
            return;
        }

        let slot = match id {
            200 => Some(BoostSlotId::HorizonEmpty),
            201 => Some(BoostSlotId::HorizonFull),
            202 => Some(BoostSlotId::ZenithEmpty),
            203 => Some(BoostSlotId::ZenithFull),
            _ => None,
        };
        if let Some(slot) = slot {
            self.selected_power_up_slot = slot as u8;
            if self.inventory_ui_mode() == INVENTORY_UI_ASSIGN
                && !self.selected_power_up_item.is_none()
                && self.power_up_inventory.assign(
                    &mut self.power_up_loadout,
                    slot,
                    self.selected_power_up_item,
                )
            {
                self.selected_power_up_item = BoostModuleId::NONE;
                self.set_inventory_ui_mode(INVENTORY_UI_SOCKETS);
            } else if self.inventory_ui_mode() == INVENTORY_UI_SOCKETS
                && self.first_inventory_item_index().is_some()
            {
                self.set_inventory_ui_mode(INVENTORY_UI_MODULES);
            }
        }
    }

    fn game_ui_cancel(&mut self, ctx: &mut Ctx) -> bool {
        if !self.inventory_overlay_active {
            return false;
        }
        if self.inventory_ui_mode() == INVENTORY_UI_ASSIGN {
            self.selected_power_up_item = BoostModuleId::NONE;
            self.set_inventory_ui_mode(INVENTORY_UI_MODULES);
        } else {
            self.selected_power_up_item = BoostModuleId::NONE;
            self.set_inventory_ui_mode(INVENTORY_UI_SOCKETS);
            self.requested_gameplay_state(ctx);
        }
        true
    }

    fn game_ui_square(&mut self, _ctx: &mut Ctx) -> bool {
        if !self.inventory_overlay_active || self.inventory_ui_mode() == INVENTORY_UI_MODULES {
            return false;
        }
        let slot = BoostSlotId::from_index(self.selected_power_up_slot);
        let _ =
            self.power_up_inventory
                .assign(&mut self.power_up_loadout, slot, BoostModuleId::NONE);
        true
    }

    fn on_flow_state_entered(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
        self.inventory_overlay_active = crate::generated::SCENE_STATES.iter().any(|candidate| {
            candidate.id == state.id && candidate.name == INVENTORY_SCENE_STATE_NAME
        });
        if self.inventory_overlay_active {
            self.selected_power_up_item = BoostModuleId::NONE;
            self.set_inventory_ui_mode(INVENTORY_UI_SOCKETS);
        }
    }

    /// Gameplay and each UI scene use distinct resource-set keys so the flow
    /// driver fires `on_exit_state`/`on_enter_state` across menu-to-menu and
    /// menu-to-gameplay boundaries. Gameplay overlays share the gameplay key
    /// and the selector-preserving cooked font pack. Streamed UI image VRAM is scoped
    /// to the active front-end scene so a splash/logo screen does not keep its
    /// texture resident beside every main-menu strip.
    fn state_resource_key(&self, state: SceneStateRef) -> u32 {
        if state.has_gameplay() {
            GAMEPLAY_RESOURCE_KEY
        } else if state.ui_scene != psx_level::UI_SCENE_NONE {
            MENU_RESOURCE_KEY.saturating_add(u32::from(state.ui_scene).saturating_add(1))
        } else {
            MENU_RESOURCE_KEY
        }
    }

    /// Acquire the cooked font set without changing selector positions between
    /// front-end and gameplay-backed UI. The old HUD-only pack caused pause
    /// labels above slot one to fall back silently to the italic default face.
    fn on_enter_state(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
        assert!(
            acquire_ui_fonts(UI_FONTS, &mut self.ui_fonts),
            "UI font VRAM pack failed"
        );
        // Streamed UI images live only in menu states. Menu entry uploads any
        // already-cached active-scene images but does not read the disc; those
        // reads are stepped after boot by `update_ui_resources` so real hardware
        // can render a first frame before menu preloading starts. Gameplay entry
        // frees previous menu VRAM (see `on_exit_state`). The sky panorama is
        // gameplay-scoped, so it is the mirror image: loaded on gameplay entry
        // and freed on gameplay exit.
        #[cfg(feature = "cd-stream-bench")]
        if state.has_gameplay() {
            load_streamed_sky_from_cd();
        } else {
            note_menu_ui_scene_entered();
            let _ = load_ui_images_for_scene(state.ui_scene);
        }
        let _ = state;
    }

    /// Release the menu's streamed UI images when leaving a menu state so the
    /// gameplay room textures reclaim that VRAM. Font ownership is switched by
    /// `on_enter_state`, which replaces the menu pack with the HUD-only pack.
    fn on_exit_state(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
        if state.has_gameplay() {
            // A gameplay-to-front-end handoff is the other safe save boundary.
            // Gameplay overlays share the gameplay resource key, so opening the
            // pause/inventory menu does not trigger a card write here.
            self.snapshot_resume_position();
            self.flush_poi_save();
            // Re-anchor the animation epoch on the next gameplay entry
            // (see `gameplay_epoch` in main.rs).
            self.gameplay_epoch_set = false;
            self.clear_actor_pose_snapshots();
            release_gameplay_vram();
            // The BSP material table caches VRAM slot words and is latched once
            // resolved. `Scene::init` runs at boot, not per gameplay entry, so
            // this runtime survives the release above; drop the bindings with
            // the slots they name.
            if let Some(bsp) = self.bsp.as_mut() {
                bsp.invalidate_materials();
            }
            #[cfg(feature = "cd-stream-bench")]
            {
                self.unload_runtime_models();
                self.gameplay_asset_arena_active = false;
                // Re-establish valid empty UI-cache metadata over the union
                // before the incoming front-end scene begins streaming.
                retire_menu_ui_cache();
            }
        }
        if !state.has_gameplay() {
            release_ui_images();
        }
        let _ = state;
    }

    /// Apply front-end settings chosen before Play. Screen-position options
    /// shift the whole rendered scene through the display window.
    fn apply_options(
        &mut self,
        options: &[psx_level::LevelOptionDef],
        values: &[i32],
        ctx: &mut Ctx,
    ) {
        let mut screen_offset = self.screen_offset;
        for (option, value) in options.iter().zip(values) {
            if option.id == SCREEN_OFFSET_X_OPTION_ID {
                screen_offset.0 = (*value).clamp(-128, 127) as i16;
            } else if option.id == SCREEN_OFFSET_Y_OPTION_ID {
                screen_offset.1 = (*value).clamp(-128, 127) as i16;
            } else if option.id == SFX_VOLUME_OPTION_ID {
                let percent = (*value).clamp(0, SFX_VOLUME_MAX) as u16;
                let volume = psx_spu::Volume::linear(percent, SFX_VOLUME_MAX as u16);
                psx_spu::set_main_volume(volume, volume);
            } else if option.id == ANALOG_DEADZONE_OPTION_ID {
                self.analog_deadzone =
                    (*value).clamp(ANALOG_DEADZONE_MIN.into(), ANALOG_DEADZONE_MAX.into()) as i16;
            } else if option.id == BRIGHTNESS_OPTION_ID {
                self.brightness_level = (*value).clamp(1, i32::from(BRIGHTNESS_LEVELS)) as u8;
            }
        }
        if screen_offset != self.screen_offset {
            self.screen_offset = screen_offset;
            ctx.gpu().set_display(
                DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240)
                    .with_offset(screen_offset),
            );
        }
    }

    fn render_post_process(&mut self, ctx: &mut Ctx) {
        let fade = self.opening.fade();
        let (gpu, fb) = ctx.gpu_and_buffers();
        draw_brightness_overlay(gpu, self.brightness_level);
        draw_opening_fade(gpu, fb, fade);
    }

    fn init(&mut self, _ctx: &mut Ctx) {
        self.init_gameplay();
    }

    fn loading_update(&mut self, ctx: &mut Ctx) -> bool {
        // Hide the blocking initial directory scan behind the authored loading
        // screen rather than spending the first live gameplay update on it.
        if !self.poi_save_load_attempted {
            self.poi_save_load_attempted = true;
            self.ensure_poi_save_loaded();
        }
        self.step_streaming_jobs(ctx);
        self.initial_world_ready()
    }

    /// Real load progress for the authored loading scene's bar: the
    /// persistent model pack dominates the load. The engine pins the bar full
    /// once `loading_update` reports ready.
    fn loading_progress_q12(&self) -> i32 {
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            4096
        }
        #[cfg(feature = "cd-stream-bench")]
        {
            if !self.runtime_models_loaded {
                // A failed persistent load never resumes, so leaving the bar
                // parked at whatever fraction it reached reads as "still
                // working". Empty and stuck is the honest signal, and it is the
                // only one this screen can give without authored error UI.
                if persistent_assets_arena().failed() {
                    return 0;
                }
                return persistent_assets_arena().progress_q12().saturating_mul(3) / 8;
            }
            // Persistent assets span 0..1536; the texture/upload tail is the
            // rest, pinned to 4096 by the engine once `loading_update`
            // reports fully ready.
            1536
        }
    }

    /// Re-upload the loading scene's streamed images into VRAM from
    /// the front-end RAM cache (filled by the contiguous menu
    /// preload). Never touches the CD: the laser belongs to the world
    /// stream during loading.
    fn prepare_loading_assets(&mut self, scene: u16) {
        #[cfg(feature = "cd-stream-bench")]
        {
            if self.gameplay_asset_arena_active {
                return;
            }
            let loading_images_ready = scene == psx_level::UI_SCENE_NONE
                || (menu_ui_cache_ready() && load_ui_images_for_scene(scene));
            // The loading images are now in VRAM; this is the overlay
            // handoff point (`FrontEndGameplayOverlay`): gameplay assets own
            // the cache's RAM from here.
            if loading_images_ready {
                retire_menu_ui_cache();
                persistent_assets_arena_mut().reset_for_scene_load();
                self.gameplay_asset_arena_active = true;
            }
        }
        #[cfg(not(feature = "cd-stream-bench"))]
        let _ = scene;
    }

    fn update_ui_resources(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
        #[cfg(feature = "cd-stream-bench")]
        if !state.has_gameplay() {
            service_menu_ui_images(state.ui_scene);
        }
        let _ = state;
    }

    /// Hold the menu CD-DA until every front-end UI image is resident, so the
    /// front-end (intro/menu/settings) never reads the CD while music plays.
    fn cinematic_active(&self) -> bool {
        self.opening.active()
    }

    fn gameplay_menu_blocked(&self) -> bool {
        self.opening.active() || self.initial_world_message_active()
    }

    fn combat_music_active(&self) -> bool {
        self.combat_music.engaged
    }

    fn front_end_assets_ready(&self) -> bool {
        menu_ui_cache_ready()
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let physical = (ctx.pad, ctx.pad_prev);
        self.duel_pad(ctx);
        if self.duel.active && self.duel.finished {
            self.game_entities.advance_defeated_animations(1);
            self.refresh_actor_pose_snapshots(ctx);
            ctx.pad = physical.0;
            ctx.pad_prev = physical.1;
            return;
        }
        self.player_poise.tick(1);
        self.combat_flow.tick(
            1,
            matches!(self.anim_state, PlayerAnim::Stun | PlayerAnim::HitReact),
        );
        self.update_gameplay(ctx);
        self.tick_poi_presentation();
        // This tail runs after every intentional early return in
        // `update_gameplay`: freeze final actor state once, then run combat
        // from the same snapshots the next body/equipment render consumes.
        self.refresh_actor_pose_snapshots(ctx);
        if !self.opening.active() && !self.initial_world_message_active() {
            self.resolve_enemy_melee(ctx);
            self.resolve_player_melee(ctx);
        }
        self.duel_observe(ctx);
        ctx.pad = physical.0;
        ctx.pad_prev = physical.1;
    }

    fn render(&mut self, ctx: &mut Ctx) {
        let camera = self.render_camera;
        self.resolve_poi_floors();
        self.prepared_overlay_camera = camera;
        self.prepared_overlay_sim_tick = self.gameplay_tick(ctx.sim_tick);
        self.snapshot_poi_presentation_for_render();

        #[cfg(feature = "fps-overlay")]
        {
            // One presented frame per render() call; measure against the
            // gameplay-anchored tick so the readout is cadence-true.
            let now = self.prepared_overlay_sim_tick.as_u32();
            let gap = now.wrapping_sub(self.fps_last_tick).min(255) as u8;
            if self.fps_window_frames > 0 {
                self.fps_worst_gap = self.fps_worst_gap.max(gap);
            }
            self.fps_last_tick = now;
            self.fps_window_frames = self.fps_window_frames.saturating_add(1);
            if now.wrapping_sub(self.fps_window_start) >= 60 {
                self.fps_display = self.fps_window_frames;
                self.fps_display_worst = self.fps_worst_gap;
                self.fps_window_start = now;
                self.fps_window_frames = 0;
                self.fps_worst_gap = 0;
            }
        }
        let render_scratch = frame_render_scratch();
        // This frame's table and packets never overlap the previous frame's,
        // which the GPU may still be walking: see PACKET_FRAMES.
        let (mut ot, mut primitive_packets) = unsafe {
            let frames = &mut *core::ptr::addr_of_mut!(PACKET_FRAMES);
            let ot = &mut *core::ptr::addr_of_mut!(OT[frames.next_frame()]);
            (
                OtFrame::begin(ot),
                PrimitivePacketArena::new_paired(&mut render_scratch.primitive_packets, frames),
            )
        };
        self.queued_head = core::ptr::null();
        let present_hook = ctx.present_queue_hook();
        if let Some(hook) = present_hook {
            // The walk continues past slot 0 into the overlay the runner
            // records for this frame.
            unsafe { ot.end_with_chain(hook) };
        }

        let room_record = ROOMS.get(self.room_index.to_usize());
        // Which world objects the cooked BSP lets the passes below draw.
        let mut world_object_visibility = WorldObjectVisibility::ALL;
        if let Some(bsp) = self.bsp.as_mut() {
            telemetry::stage_begin(telemetry::stage::ROOM);
            world_object_visibility = bsp.visible_world_objects(camera, &self.destructibles);
            telemetry::stage_end(telemetry::stage::ROOM);
        }
        // The cooked BSP world is the frame's largest block of packets and
        // needs it contiguous. When it fits beside the frame the GPU may still
        // be reading, it goes first and nothing waits. When it does not, it
        // goes last, so the paired-arena fence (the wait for that frame's
        // walk) comes late in the frame, when the walk has usually finished,
        // instead of costing most of a draw up front. Characters and props
        // emit their packets too quickly to push the fence back themselves.
        let world_first = self
            .bsp
            .as_ref()
            .is_none_or(|bsp| bsp.fits_before_fence(&primitive_packets));
        if world_first {
            self.draw_world_and_sky(
                camera,
                room_record,
                ctx.sim_tick,
                &mut primitive_packets,
                &mut ot,
            );
        }

        let mut world = begin_world_render_pass(&mut ot, &mut render_scratch.world_commands);

        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::FAR_VISTA);
            draw_far_vista_ring(
                camera,
                room_record.far_vista,
                room_surface_options(room_record),
                &mut primitive_packets,
                &mut world,
            );
            telemetry::stage_end(telemetry::stage::FAR_VISTA);
        }

        if USES_PXBSP {
            let mut total_instance_stats = ModelInstanceDrawStats::default();

            // Live entity poses: instances bound to game entities
            // render where the entity runtime moved them (phase 3).
            let mut entity_poses =
                psx_engine::FixedScratch::<ModelInstancePoseOverride, MAX_GAME_ENTITIES>::new();
            self.game_entity_pose_overrides(&mut entity_poses);
            let entity_poses = entity_poses.as_slice();

            // Draw the singleton metadata room's ordinary gameplay content
            // directly in world space while the BSP renderer above owns only
            // static brush surfaces.
            if let (Some(room_record), Some(lighting)) =
                (room_record, self.current_room_lighting(camera))
            {
                let actor_options = pxbsp_actor_surface_options(room_record)
                    .with_material_animation(
                        self.gameplay_tick(ctx.sim_tick).as_u32(),
                        ctx.video_hz.as_u16(),
                    );
                let instance_stats = self.draw_room_world_content(
                    self.room_index,
                    &camera,
                    actor_options,
                    &lighting,
                    entity_poses,
                    world_object_visibility,
                    ctx,
                    &mut primitive_packets,
                    &mut world,
                );
                accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
            }

            // Player draws through the same compact model path as
            // placed model instances.
            // `character` by reference: copying the whole RuntimeCharacter
            // (688 B) into the tuple cost a memcpy per frame.
            if let (Some(character), Some(player_pose)) =
                (self.character.as_ref(), self.player_actor_pose)
            {
                {
                    // Diagnostic: the model's rendered forward (local +Z through the
                    // rotation the draw uses), to compare with the motor's facing.
                    let m = player_pose.pose().rotation().m;
                    telemetry::counter(
                        telemetry::counter::PLAYER_RENDER_FORWARD_X_Q12_BIASED,
                        (m[0][2] as i32 + 4096) as u32,
                    );
                    telemetry::counter(
                        telemetry::counter::PLAYER_RENDER_FORWARD_Z_Q12_BIASED,
                        (m[2][2] as i32 + 4096) as u32,
                    );
                }
                let player = self.motor.position();
                let player_lighting = self.current_room_lighting(camera);
                let actor_options = current_actor_surface_options(self.room_index);
                telemetry::stage_begin(telemetry::stage::PLAYER);
                sort_probe_class(SORT_CLASS_PLAYER);
                #[cfg(feature = "actor-shadows-projected")]
                {
                    draw_player_projected_shadow(
                        player_pose,
                        player.y,
                        &camera,
                        actor_options,
                        &self.model_faces[..self.model_face_count],
                        &self.model_parts[..self.model_part_count],
                        &self.model_vertices[..self.model_vertex_count],
                        &mut primitive_packets,
                        &mut world,
                    );
                }
                #[cfg(not(feature = "actor-shadows-projected"))]
                if !cfg!(feature = "actor-shadows-off") {
                    if let Some(shadow_material) = self.shadow_material {
                        draw_actor_shadow(
                            player.x,
                            player.y,
                            player.z,
                            actor_shadow_radius(character.radius),
                            &camera,
                            actor_options,
                            shadow_material,
                            &mut primitive_packets,
                            &mut world,
                        );
                    }
                }
                // A free camera backed into a wall collapses its arm into the
                // player's body; drawing her from inside fills the screen with
                // near-plane slivers and dash wireframe streaks. Hide her (and
                // what she holds) until the arm is clear again.
                // Only for the follow camera: the intro shots and debug
                // sweeps render from elsewhere.
                let follow = self.camera.position();
                let camera_in_player = camera.position.x == follow.x
                    && camera.position.y == follow.y
                    && camera.position.z == follow.z
                    && self.camera.distance() < self.camera_config().min_distance;
                let player_lighting = player_lighting.filter(|_| !camera_in_player);
                let stance_crystal_material = self
                    .stance_cluts
                    .crystal_material(character, self.player_stance.active());
                let player_draw =
                    player_lighting.map_or(PlayerModelDrawStats::default(), |lighting| {
                        let phase_assembly = player_phase_assembly(
                            self.player_stance,
                            &self.player_stance_config,
                            player,
                            player_phase_height(character),
                        )
                        .map(|effect| effect.with_crystal_material(stance_crystal_material));
                        let stance_clut = self
                            .stance_cluts
                            .player_override_clut(character, self.player_stance.active());
                        draw_player(
                            self.room_index,
                            character,
                            player_pose,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            ctx.sim_tick,
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            phase_assembly,
                            &mut self.player_dash_assembly,
                            stance_clut,
                            Some(player_stance_lit_tint(self.player_stance.active())),
                            &mut primitive_packets,
                            &mut world,
                        )
                    });
                // Share the stance burst/reassembly clock, but keep scarf packets
                // outside the body's finish fade so its stance hue persists.
                if !camera_in_player && player_lighting.is_some() {
                    self.player_scarf.draw_crystal_with_dash(
                        camera,
                        stance_rgb(self.player_stance.active()),
                        player_phase_assembly(
                            self.player_stance,
                            &self.player_stance_config,
                            player,
                            player_phase_height(character),
                        )
                        .filter(|_| {
                            matches!(
                                self.player_dash_assembly.visual(ctx.sim_tick),
                                psx_game_runtime::model_rendering::DashWireVisual::Solid
                            )
                        }),
                        self.player_dash_assembly.visual(ctx.sim_tick),
                        actor_options,
                        stance_crystal_material,
                        &mut primitive_packets,
                        &mut world,
                    );
                }
                telemetry::stage_end(telemetry::stage::PLAYER);
                emit_model_counters(
                    player_draw.stats,
                    telemetry::counter::PLAYER_PROJECTED_VERTICES,
                    telemetry::counter::PLAYER_SUBMITTED_TRIS,
                    telemetry::counter::PLAYER_CULLED_TRIS,
                    telemetry::counter::PLAYER_DROPPED_TRIS,
                );
                telemetry::counter(
                    telemetry::counter::PLAYER_BOUNDS_TESTS,
                    player_draw.bounds_tests as u32,
                );
                telemetry::counter(
                    telemetry::counter::PLAYER_BOUNDS_CULLED,
                    player_draw.bounds_culled as u32,
                );
                telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                let equipment_stats = if player_draw.bounds_culled != 0 {
                    EquipmentDrawStats::default()
                } else {
                    player_lighting.map_or(EquipmentDrawStats::default(), |lighting| {
                        draw_player_equipment(
                            self.anim_state,
                            crate::model_rendering::equipment_wire_q12(
                                self.anim_state,
                                player_pose.pose().phase_q12(),
                                player_pose.pose().animation().frame_count(),
                            ),
                            player_pose,
                            &self.models,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            &self.clips,
                            ctx.sim_tick,
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            &mut primitive_packets,
                            &mut world,
                        )
                    })
                };
                telemetry::stage_end(telemetry::stage::EQUIPMENT);
                telemetry::counter(
                    telemetry::counter::EQUIPMENT_DRAWS,
                    equipment_stats.draws as u32,
                );
                if equipment_stats.draws > 0 && !self.weapon_attach_reported {
                    // First frame of this life where the equipped weapon
                    // resolved to its socket pose and submitted: one
                    // PLAYER_WEAPON_ATTACHMENTS event per (re)spawn.
                    self.weapon_attach_reported = true;
                    telemetry::counter(telemetry::counter::PLAYER_WEAPON_ATTACHMENTS, 1);
                }
                emit_model_counters(
                    equipment_stats.stats,
                    telemetry::counter::EQUIPMENT_PROJECTED_VERTICES,
                    telemetry::counter::EQUIPMENT_SUBMITTED_TRIS,
                    telemetry::counter::EQUIPMENT_CULLED_TRIS,
                    telemetry::counter::EQUIPMENT_DROPPED_TRIS,
                );
            }

            if self.character.is_some() {
                if let (Some(room_record), Some(lighting)) =
                    (room_record, self.current_room_lighting(camera))
                {
                    let actor_options = pxbsp_actor_surface_options(room_record)
                        .with_material_animation(
                            self.gameplay_tick(ctx.sim_tick).as_u32(),
                            ctx.video_hz.as_u16(),
                        );
                    telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                    let _ = draw_instance_equipment(
                        self.room_index,
                        &self.instance_actor_poses,
                        MAX_EQUIPMENT_DRAWS,
                        self.gameplay_tick(ctx.sim_tick),
                        ctx.video_hz,
                        &camera,
                        actor_options,
                        &lighting,
                        &self.models,
                        &self.model_faces[..self.model_face_count],
                        &self.model_parts[..self.model_part_count],
                        &self.model_vertices[..self.model_vertex_count],
                        &self.clips,
                        &mut primitive_packets,
                        &mut world,
                    );
                    telemetry::stage_end(telemetry::stage::EQUIPMENT);
                }
            }
            sort_probe_class(SORT_CLASS_WORLD);

            let _ = self.draw_archive_beacons_world(
                camera,
                self.gameplay_tick(ctx.sim_tick),
                world_object_visibility,
                &mut primitive_packets,
                &mut world,
            );
            self.draw_hook_beacons_world(camera, ctx.sim_tick, &mut primitive_packets, &mut world);
            self.draw_vitality_circles_world(
                camera,
                self.gameplay_tick(ctx.sim_tick),
                &mut primitive_packets,
                &mut world,
            );

            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_DRAWS,
                total_instance_stats.draws as u32,
            );
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_BOUNDS_TESTS,
                total_instance_stats.bounds_tests as u32,
            );
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_BOUNDS_CULLED,
                total_instance_stats.bounds_culled as u32,
            );
            emit_model_counters(
                total_instance_stats.stats,
                telemetry::counter::MODEL_INSTANCE_PROJECTED_VERTICES,
                telemetry::counter::MODEL_INSTANCE_SUBMITTED_TRIS,
                telemetry::counter::MODEL_INSTANCE_CULLED_TRIS,
                telemetry::counter::MODEL_INSTANCE_DROPPED_TRIS,
            );
        }

        let world_command_len = world.command_len();
        telemetry::stage_begin(telemetry::stage::WORLD_FLUSH);
        world.flush();
        telemetry::stage_end(telemetry::stage::WORLD_FLUSH);
        sort_probe_class(SORT_CLASS_SPRITE);
        let _ = self.draw_particle_emitters(
            camera,
            self.gameplay_tick(ctx.sim_tick),
            &mut ot,
            &mut primitive_packets,
        );
        let _ = self.draw_combat_projectiles(camera, &mut ot, &mut primitive_packets);
        // The same authored blade capsules own enemy trails and damage.
        for (index, entity) in GAME_ENTITIES.iter().enumerate() {
            if entity.room != self.room_index {
                continue;
            }
            let Some(attack) = self
                .deferred_enemy_attacks
                .as_slice()
                .iter()
                .copied()
                .find(|attack| attack.entity() == index && !attack.is_ranged())
            else {
                continue;
            };
            let Some(pose) = self
                .instance_actor_poses
                .get(entity.model_instance as usize)
                .copied()
                .flatten()
            else {
                continue;
            };
            let first = entity.combat_capsule_first.to_usize();
            let end = first + usize::from(entity.combat_capsule_count);
            let Some(capsules) = COMBAT_CAPSULES.get(first..end) else {
                continue;
            };
            let projector =
                PROP_PARTICLE_GTE_PROJECT_ENABLED.then(|| LoadedWorldCameraGte::load(camera));
            for capsule in capsules
                .iter()
                .take(psx_level::MAX_CHARACTER_COMBAT_CAPSULES)
            {
                let _ = psx_game_runtime::particles::draw_melee_window_trail(
                    capsule,
                    attack.action(),
                    pose.pose(),
                    self.game_entities.stance(index) == VitalityChannelId::Two,
                    camera,
                    projector,
                    self.effect_depth_range(entity.room),
                    &mut ot,
                    &mut primitive_packets,
                );
            }
        }
        let _ = self.draw_player_water_wade_splash(
            camera,
            self.gameplay_tick(ctx.sim_tick),
            &mut ot,
            &mut primitive_packets,
        );

        if !world_first {
            self.draw_world_and_sky(
                camera,
                room_record,
                ctx.sim_tick,
                &mut primitive_packets,
                &mut ot,
            );
        }
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVES,
            primitive_packets.len() as u32,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVE_REMAINING,
            primitive_packets.remaining() as u32,
        );
        telemetry::counter(telemetry::counter::WORLD_COMMANDS, world_command_len as u32);
        if present_hook.is_some() {
            // Words for the runner to record the overlay into. The whole
            // reservation is committed: the recording links its nodes by
            // address, and a descending frame would slide a shorter prefix.
            let words = PRESENT_OVERLAY_WORDS.min(primitive_packets.remaining_words())
                / PRIMITIVE_PACKET_SLOT_WORDS
                * PRIMITIVE_PACKET_SLOT_WORDS;
            if let Some(mut reservation) = primitive_packets.reserve_packet_words(words) {
                let overlay = reservation.words_mut().as_mut_ptr();
                if reservation.commit(words, 1).is_some() {
                    self.queued_head = ot.submit_head();
                    self.queued_overlay = overlay;
                    self.queued_overlay_words = words;
                }
            }
        }
        let world_words = self
            .bsp
            .as_ref()
            .map_or(0, |bsp| bsp.last_world_packet_words());
        self.choose_present_queue(queued_successor_fences_late(
            primitive_packets.capacity(),
            primitive_packets.used_slots(),
            present_hook.is_some(),
            world_words,
        ));
        // The next frame builds beside this one while its list is walked.
        primitive_packets.finish_paired_frame();
        // Submission is deliberately split from packet preparation. The app
        // runner first presents the previous queued frame and clears the new
        // back buffer, then calls submit_render below.
        let _ = ot;
    }

    fn take_queued_frame(&mut self, _ctx: &mut Ctx) -> Option<QueuedFrame> {
        let head = core::mem::replace(&mut self.queued_head, core::ptr::null());
        if head.is_null() {
            return None;
        }
        self.commit_overlay_state();
        Some(QueuedFrame {
            head,
            overlay: self.queued_overlay,
            overlay_words: self.queued_overlay_words,
        })
    }

    fn submit_render(&mut self, ctx: &mut Ctx) {
        self.commit_overlay_state();
        telemetry::stage_begin(telemetry::stage::OT_SUBMIT);
        // SAFETY: `render` built OT[built] this frame from PACKET_FRAMES' paired
        // scratch and static packets. Neither is touched again until the
        // runner has waited this walk out: it drains channel 2 before
        // `render_overlay` and the flip, and the next frame builds into the
        // other table and the other end of the scratch.
        unsafe {
            let built = (*core::ptr::addr_of!(PACKET_FRAMES)).built_frame();
            psx_gpu::chain::submit_async_raw(
                ctx.gpu_dma(),
                (*core::ptr::addr_of!(OT[built])).submit_head(),
            );
        }
        telemetry::stage_end(telemetry::stage::OT_SUBMIT);
    }

    fn render_overlay(&mut self, ctx: &mut Ctx) {
        let gpu = ctx.gpu();
        let camera = self.overlay_camera;
        let overlay_tick = self.overlay_sim_tick;

        if let Some(room_record) = ROOMS.get(self.room_index.to_usize()) {
            draw_room_atmosphere_overlay(gpu, room_record, overlay_tick);
        }

        if self.opening.active() {
            if !self.opening.gameplay_camera() {
                if let Some(font) = self.ui_fonts[0].as_ref() {
                    draw_opening_skip(gpu, font, self.opening.skip_progress());
                }
            }
            return;
        }

        #[cfg(feature = "collision-debug-overlay")]
        if self.show_collision_debug {
            self.draw_collision_debug_overlay(gpu, camera);
        }

        self.draw_hook_selection(gpu, camera);
        if self.hook_selected.is_none()
            && self.player_has_ranged_weapon()
            && self.ranged_ready.aiming()
            && self.player_stance.active() == VitalityChannelId::Two
        {
            let center = if self.is_locked() {
                let [x, y, z] = self.ranged_target();
                camera.project_world(RoomPoint::new(x, y, z))
            } else {
                Some(ProjectedVertex::new(
                    camera.projection.screen_x,
                    camera.projection.screen_y,
                    1,
                ))
            };
            if let Some(center) = center {
                draw_target_reticle(gpu, center, overlay_tick, self.player_stance.active());
            }
        } else if let Some(target) = self.lock_target_indicator_position() {
            draw_lock_target_indicator(
                gpu,
                target,
                camera,
                overlay_tick,
                self.player_stance.active(),
            );
        }

        // Damage numbers sit above the world and below the panels: they
        // are combat feedback, so a message box that is up should cover
        // them rather than compete with them.
        // `FontAtlas` is `Copy`, so take the handle by value: holding a
        // borrow of `self.ui_fonts` here would block the `&mut` the pool
        // needs to retire its own expired slots.
        if let Some(font) = self.ui_fonts[damage_font_slot()] {
            let room = self.room_index;
            let _drawn = self.damage_numbers.draw(&font, camera, room, overlay_tick);
        }

        // Hard lock wraps the existing reticle with dual vitality arcs.
        // Soft targets keep their compact vitality stack beside the model.
        if !self.inventory_overlay_active {
            if let (Some(font), Some(target_index)) =
                (self.ui_fonts[0].as_ref(), self.combat_target_entity_index())
            {
                if let Some(record) = GAME_ENTITIES.get(target_index) {
                    let [x, y, z] = self.game_entities.position(target_index);
                    let head = RoomPoint::new(x, y.saturating_add(i32::from(record.height)), z);
                    if let Some(projected) = camera.project_world(head) {
                        let active = self.game_entities.stance(target_index);
                        let health_share = |channel| {
                            let (current, maximum) = match channel {
                                VitalityChannelId::One => {
                                    (self.game_entities.health(target_index), record.max_health)
                                }
                                VitalityChannelId::Two => (
                                    self.game_entities.health_secondary(target_index),
                                    record.max_health_secondary,
                                ),
                            };
                            if maximum == 0 {
                                0
                            } else {
                                ((u32::from(current) * 4096) / u32::from(maximum)) as u16
                            }
                        };
                        if self.is_locked() {
                            if let Some(center) = self
                                .lock_target_indicator_position()
                                .and_then(|p| camera.project_world(p))
                            {
                                draw_lock_target_readout(
                                    gpu,
                                    center,
                                    active,
                                    health_share(VitalityChannelId::One),
                                    health_share(VitalityChannelId::Two),
                                );
                            }
                        } else {
                            draw_enemy_vitality_hud(
                                gpu,
                                font,
                                projected.sx.saturating_add(40).clamp(4, SCREEN_W - 80),
                                projected.sy.clamp(4, SCREEN_H - 20),
                                active,
                                health_share(active),
                                health_share(active.other()),
                                self.game_entities.stance_swap_progress_q12(target_index),
                            );
                        }
                    }
                }
            }
        }

        // One runtime-owned cluster draws the mutation charge and both health
        // pools. The previous implementation layered a moving rectangular bar
        // beneath an authored slanted bar, leaving both visible at rest.
        if !self.inventory_overlay_active {
            if let Some(font) = self.ui_fonts[0].as_ref() {
                if self.player_has_ranged_weapon() {
                    draw_combat_energy(
                        gpu,
                        font,
                        self.combat_flow.energy,
                        self.hook_attached.map(|_| self.combat_flow.air_left),
                    );
                }
                const VITALITY_Q12_ONE: u16 = 4096;
                let config = self.player_stance_config;
                let active = self.player_stance.active();
                let share = |channel| {
                    let pool = self.player_vitality.pool(channel);
                    if pool.maximum() == 0 {
                        0
                    } else {
                        ((u32::from(pool.current()) * u32::from(VITALITY_Q12_ONE))
                            / u32::from(pool.maximum())) as u16
                    }
                };
                let elapsed = self.player_stance.swap_elapsed_ticks();
                let cooldown_remaining = self.player_stance.swap_cooldown();
                let cooldown_progress_q12 = swap_cooldown_display_progress_q12(
                    config.swap_cooldown_ticks,
                    cooldown_remaining,
                );
                let echo_elapsed = if elapsed != u16::MAX
                    && elapsed >= config.swap_cooldown_ticks
                    && elapsed < config.swap_cooldown_ticks.saturating_add(13)
                {
                    Some(elapsed - config.swap_cooldown_ticks)
                } else {
                    None
                };
                draw_player_vitality_hud(
                    gpu,
                    font,
                    active,
                    share(active),
                    share(active.other()),
                    self.player_stance.swap_progress_q12(&config),
                    cooldown_progress_q12,
                    echo_elapsed,
                );
            }
        }

        if let Some(font) = self.ui_fonts[0].as_ref() {
            if self.duel.active {
                let label = match self.duel.outcome {
                    1 => "DUEL: PLAYER WINS",
                    2 => "DUEL: ENEMY WINS",
                    3 => "DUEL: DOUBLE KO",
                    5 => "DUEL: TIME LIMIT",
                    6 => "DUEL: NO PROGRESS",
                    _ => "AI DUEL: BOTH STANCES",
                };
                font.draw_text(8, 66, label, (240, 220, 150));
                font.draw_text(8, 78, "PRESS A BUTTON TO TAKE OVER", (200, 200, 200));
            } else if GAME_ENTITIES
                .iter()
                .any(|r| r.flags & psx_level::game_entity_flags::TRAINING != 0)
            {
                font.draw_text(8, 66, "SELECT+L2: AI DUEL", (180, 190, 200));
            }
        }

        #[cfg(feature = "fps-overlay")]
        if let Some(font) = self.ui_fonts[0].as_ref() {
            draw_fps_overlay(gpu, font, self.fps_display, self.fps_display_worst);
        }

        let cross_prompt = UI_NODES
            .iter()
            .find(|node| node.tag == "prompt.cross.runtime")
            .and_then(|node| self.ui_texture(node.texture_asset));

        if self.character.is_some() {
            // EXPLOSION PROBE (diagnostic): overlay the player's skinned-vertex
            // capture pages. Feature-gated -- probe builds only, never the
            // shipping game or perf-measurement builds.
            #[cfg(feature = "vert-debug-overlay")]
            if let Some(font) = self.ui_fonts[0].as_ref() {
                draw_player_vert_debug(font);
            }
        }

        if let Some(font) = self.ui_fonts[0].as_ref() {
            if self.poi_closing {
                let variant = match self.poi_messages.active().map(|message| message.source()) {
                    Some(psx_game_runtime::poi::MessageSource::World) => MessagePanelVariant::World,
                    _ => MessagePanelVariant::PointOfInterest,
                };
                let source = match self.poi_messages.active().map(|message| message.source()) {
                    Some(psx_game_runtime::poi::MessageSource::PointOfInterest(index)) => {
                        Some(usize::from(index))
                    }
                    _ => self.active_interactable,
                };
                let action = source
                    .and_then(|index| INTERACTABLES.get(index))
                    .map(|interactable| crate::loc::prompt_verb(interactable.prompt))
                    .unwrap_or("READ");
                psx_engine::ui::draw_dismissing_message_panel(
                    gpu,
                    font,
                    variant,
                    !self.acquired_module.is_none(),
                    action,
                    overlay_tick.as_u32() as u16,
                    self.overlay_poi_panel_frame,
                    cross_prompt,
                );
            } else if let Some(module) = self
                .acquired_module
                .index()
                .and_then(|index| BOOST_MODULES.get(index))
            {
                draw_acquired_module(
                    gpu,
                    font,
                    self.acquired_module.index().map_or(module.name, |index| {
                        crate::loc::module_name(index, module.name)
                    }),
                    overlay_tick.as_u32() as u16,
                    self.overlay_poi_panel_frame,
                    self.overlay_poi_page_type_frame,
                    cross_prompt,
                );
            } else if let Some(message) = self.poi_messages.active() {
                if let Some(page_text) = crate::loc::page_text(message.page() as usize) {
                    let variant = match message.source() {
                        psx_game_runtime::poi::MessageSource::PointOfInterest(_) => {
                            MessagePanelVariant::PointOfInterest
                        }
                        psx_game_runtime::poi::MessageSource::World => MessagePanelVariant::World,
                    };
                    let page = MessagePageMeta::new(
                        message.page_offset().min(u8::MAX as u16) as u8,
                        message.page_count().min(u8::MAX as u16) as u8,
                    );
                    match message.source() {
                        psx_game_runtime::poi::MessageSource::PointOfInterest(index) => {
                            let action = crate::loc::prompt_verb(
                                INTERACTABLES
                                    .get(usize::from(index))
                                    .map(|interactable| interactable.prompt)
                                    .unwrap_or("READ"),
                            );
                            draw_expanding_poi_message(
                                gpu,
                                font,
                                action,
                                page_text,
                                page,
                                overlay_tick.as_u32() as u16,
                                self.overlay_poi_panel_frame,
                                self.overlay_poi_page_type_frame,
                                cross_prompt,
                            );
                        }
                        psx_game_runtime::poi::MessageSource::World => draw_message_page(
                            gpu,
                            font,
                            page_text,
                            variant,
                            page,
                            overlay_tick.as_u32() as u16,
                            self.overlay_poi_page_type_frame,
                            cross_prompt,
                        ),
                    }
                }
            } else if let Some(message) = self.message_overlay {
                draw_interactable_message(
                    gpu,
                    font,
                    message.title,
                    message.body,
                    self.overlay_poi_page_type_frame,
                    cross_prompt,
                );
            } else if let Some(index) = self.active_interactable {
                if let Some(interactable) = INTERACTABLES.get(index) {
                    draw_interaction_prompt_animated(
                        gpu,
                        font,
                        crate::loc::prompt_verb(interactable.prompt),
                        overlay_tick.as_u32() as u16,
                        cross_prompt,
                    );
                }
            }
        }
    }
}

impl Playtest {
    #[allow(clippy::too_many_arguments)]
    fn draw_room_world_content(
        &self,
        room: RoomIndex,
        camera: &WorldCamera,
        actor_options: WorldSurfaceOptions,
        lighting: &RuntimeRoomLighting,
        entity_poses: &[ModelInstancePoseOverride],
        world_object_visibility: WorldObjectVisibility,
        ctx: &Ctx,
        primitive_packets: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) -> ModelInstanceDrawStats {
        draw_water(
            room,
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        telemetry::stage_begin(telemetry::stage::IMAGE_PROPS);
        box_prop_profile_begin(telemetry::stage::BOX_PROPS);
        draw_box_props(
            BOX_PROPS,
            BOX_PROP_SURFACES,
            &self.box_props,
            room,
            |index| {
                world_object_visibility.typed_visible(
                    WORLD_OBJECTS,
                    psx_level::world_object_kind::BOX_PROP,
                    index,
                )
            },
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::BOX_PROPS);
        psx_game_runtime::cylinder_props::draw_cylinder_props::<
            _,
            OT_DEPTH,
            { !CYLINDER_PROPS.is_empty() },
        >(
            CYLINDER_PROPS,
            CYLINDER_PROP_SURFACES,
            room,
            |index| {
                world_object_visibility.typed_visible(
                    WORLD_OBJECTS,
                    psx_level::world_object_kind::CYLINDER_PROP,
                    index,
                )
            },
            camera,
            actor_options,
            lighting,
            prop_texture_slot,
            primitive_packets,
            world,
        );
        psx_game_runtime::arch_props::draw_arch_props(
            ARCH_PROPS,
            ARCH_PROP_SURFACES,
            room,
            |index| {
                world_object_visibility.typed_visible(
                    WORLD_OBJECTS,
                    psx_level::world_object_kind::ARCH_PROP,
                    index,
                )
            },
            camera,
            actor_options,
            lighting,
            prop_texture_slot,
            primitive_packets,
            world,
        );
        box_prop_profile_begin(telemetry::stage::BOX_PROP_DEBRIS);
        draw_box_prop_floor_debris(
            BOX_PROPS,
            &self.box_props,
            room,
            |index| {
                world_object_visibility.typed_visible(
                    WORLD_OBJECTS,
                    psx_level::world_object_kind::BOX_PROP,
                    index,
                )
            },
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::BOX_PROP_DEBRIS);
        box_prop_profile_begin(telemetry::stage::BOX_PROP_SHARDS);
        draw_box_prop_break_events(
            BOX_PROPS,
            &self.box_props,
            room,
            |index| {
                world_object_visibility.typed_visible(
                    WORLD_OBJECTS,
                    psx_level::world_object_kind::BOX_PROP,
                    index,
                )
            },
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::BOX_PROP_SHARDS);
        if room == self.room_index {
            if let Some(bsp) = self.bsp.as_ref() {
                bsp.draw_destructible_fragments(camera, actor_options, primitive_packets, world);
            }
        }
        box_prop_profile_begin(telemetry::stage::IMAGE_CARDS);
        draw_image_props(
            IMAGE_PROPS,
            room,
            |index| {
                world_object_visibility.typed_visible(
                    WORLD_OBJECTS,
                    psx_level::world_object_kind::IMAGE_PROP,
                    index,
                )
            },
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::IMAGE_CARDS);
        telemetry::stage_end(telemetry::stage::IMAGE_PROPS);
        telemetry::stage_begin(telemetry::stage::MODEL_INSTANCES);
        sort_probe_class(SORT_CLASS_MODEL);
        #[cfg(feature = "actor-shadows-projected")]
        {
            draw_model_instance_projected_shadows(
                room,
                &self.instance_actor_poses,
                camera,
                actor_options,
                if USES_PXBSP {
                    // psx-numeric-allow-next-line: per-instance visibility bitmask, see the parameter
                    u64::from(self.bsp_instance_visible_mask)
                } else {
                    // psx-numeric-allow-next-line: per-instance visibility bitmask, all instances visible
                    u64::MAX
                },
                &self.model_faces[..self.model_face_count],
                &self.model_parts[..self.model_part_count],
                &self.model_vertices[..self.model_vertex_count],
                primitive_packets,
                world,
            );
        }
        #[cfg(not(feature = "actor-shadows-projected"))]
        if !cfg!(feature = "actor-shadows-off") {
            if let Some(shadow_material) = self.shadow_material {
                draw_model_instance_shadows(
                    room,
                    camera,
                    actor_options,
                    shadow_material,
                    &self.models,
                    entity_poses,
                    &self.instance_actor_poses,
                    if USES_PXBSP {
                        // psx-numeric-allow-next-line: per-instance visibility bitmask, see the parameter
                        u64::from(self.bsp_instance_visible_mask)
                    } else {
                        // psx-numeric-allow-next-line: per-instance visibility bitmask, all instances visible
                        u64::MAX
                    },
                    primitive_packets,
                    world,
                );
            }
        }
        let stats = draw_model_instances(
            room,
            &self.game_entities,
            &self.stance_cluts,
            &self.instance_actor_poses,
            self.gameplay_tick(ctx.sim_tick),
            ctx.video_hz,
            camera,
            actor_options,
            lighting,
            &self.model_faces[..self.model_face_count],
            &self.model_parts[..self.model_part_count],
            &self.model_vertices[..self.model_vertex_count],
            primitive_packets,
            world,
        );
        telemetry::stage_end(telemetry::stage::MODEL_INSTANCES);
        sort_probe_class(SORT_CLASS_WORLD);
        stats
    }
}

// Primitive classes for PSoXide's depth-sort probe (`--sort-log`).
const SORT_CLASS_WORLD: u32 = 1;
const SORT_CLASS_MODEL: u32 = 2;
const SORT_CLASS_SPRITE: u32 = 4;
const SORT_CLASS_PLAYER: u32 = 5;
const SORT_CLASS_SKY: u32 = 7;

/// Tag the projections that follow with a primitive class for PSoXide's
/// depth-sort probe (`sort-probe` measurement builds only; nothing
/// otherwise).
#[inline(always)]
fn sort_probe_class(class: u32) {
    #[cfg(feature = "sort-probe")]
    // SAFETY: emulator-only port in Expansion Region 2 (PSoXide telemetry
    // slice + 0x28); retail hardware ignores the write.
    unsafe {
        core::ptr::write_volatile(0x1F80_2F28 as *mut u32, class);
    }
    #[cfg(not(feature = "sort-probe"))]
    let _ = class;
}
