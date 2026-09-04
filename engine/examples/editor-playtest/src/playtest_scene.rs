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
        let scaled = i64::from(value_q12).saturating_mul(100);
        let rounded = if scaled < 0 {
            (scaled - 2048) / 4096
        } else {
            (scaled + 2048) / 4096
        };
        if rounded < 0 {
            self.push_str("-");
        } else {
            self.push_str("+");
        }
        self.push_u64(rounded.unsigned_abs());
        self.push_str("%");
    }

    /// Decimal `value`, 32-bit division only. The R3000 has no 64-bit divide,
    /// so a per-frame number must not go through `push_u64`.
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

    fn push_u64(&mut self, mut value: u64) {
        let mut digits = [0u8; 20];
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
    /// with 32-bit division only: `UiScratch::push_u64` would drag in the
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

impl Scene for Playtest {
    fn render_submission(&self) -> RenderSubmission {
        RenderSubmission::Queued
    }

    fn take_gameplay_sfx_events(&mut self) -> u16 {
        core::mem::take(&mut self.gameplay_sfx_events)
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
    fn apply_options(&mut self, options: &[psx_level::LevelOptionDef], values: &[i32]) {
        for (option, value) in options.iter().zip(values) {
            if option.id == SCREEN_OFFSET_X_OPTION_ID {
                let offset_px = (*value).clamp(-128, 127) as i16;
                psx_gpu::set_screen_h_offset(offset_px, psx_gpu::Resolution::R320X240);
            } else if option.id == SCREEN_OFFSET_Y_OPTION_ID {
                let offset_px = (*value).clamp(-128, 127) as i16;
                psx_gpu::set_screen_v_offset(
                    offset_px,
                    psx_gpu::VideoMode::Ntsc,
                    psx_gpu::Resolution::R320X240,
                );
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
    }

    fn render_post_process(&mut self, _ctx: &mut Ctx) {
        draw_brightness_overlay(self.brightness_level);
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
    /// initial room ring dominates the load, so it spans 0..3072; the
    /// texture/upload tail takes the last quarter. The engine pins the
    /// bar full once `loading_update` reports ready.
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
            let count = self.resident_desired_count.min(STREAMED_ROOM_SLOT_COUNT);
            if count == 0 {
                return 1536;
            }
            let mut resident = 0usize;
            let mut i = 0usize;
            while i < count {
                let room = self.resident_desired[i];
                if room != INVALID_ROOM_INDEX && streamed_room_is_resident(room) {
                    resident += 1;
                }
                i += 1;
            }
            // Persistent assets span 0..1536, rooms span 1536..3840;
            // the texture/upload tail is the last
            // stretch, pinned to 4096 by the engine once
            // `loading_update` reports fully ready.
            (1536 + (resident as i32).saturating_mul(2304) / count as i32).min(4096)
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
            // handoff point (`FrontEndGameplayOverlay`): gameplay assets and
            // room draws own the cache's RAM from here. Claims are reset so any
            // rooms built before the handoff (menu-time bootstrap)
            // refill their quads instead of trusting bytes the menu
            // preload may have overwritten.
            if loading_images_ready {
                retire_menu_ui_cache();
                persistent_assets_arena_mut().reset_for_scene_load();
                prebuilt_quads_arena().reset_claims();
                self.gameplay_asset_arena_active = true;
                self.prewarm_active_room_window_quads();
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
    fn combat_music_active(&self) -> bool {
        self.combat_music.engaged
    }

    fn front_end_assets_ready(&self) -> bool {
        menu_ui_cache_ready()
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.update_gameplay(ctx);
        // This tail runs after every intentional early return in
        // `update_gameplay`: freeze final actor state once, then run combat
        // from the same snapshots the next body/equipment render consumes.
        self.refresh_actor_pose_snapshots(ctx);
        self.resolve_enemy_melee(ctx);
        self.resolve_player_melee(ctx);
    }

    fn render(&mut self, ctx: &mut Ctx) {
        let camera = self.render_camera;
        self.resolve_poi_floors();
        self.advance_poi_presentation_frame();
        self.prepared_overlay_camera = camera;
        self.prepared_overlay_sim_tick = self.gameplay_tick(ctx.sim_tick);
        self.prepared_poi_panel_frame = self.poi_panel_frame;
        self.prepared_poi_page_type_frame = self.poi_page_type_frame;

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
        let post_cross_debug = POST_CROSS_RENDER_DEBUG_LOGS && self.post_cross_debug_frames != 0;
        let post_cross_detail = post_cross_debug
            && self.post_cross_debug_frames == RUNTIME_SCHEDULE.post_cross_render_debug_frames;
        let mut post_cross_logged_end = false;
        if post_cross_debug {
            debug_log_post_cross_render_start(
                self.room_index,
                camera,
                self.visibility.result.visible_room_mask(),
                self.active_room_mask(),
                self.current_collision_room.is_some(),
            );
        }

        let mut ot = unsafe { OtFrame::begin(&mut OT) };
        let render_scratch = frame_render_scratch();
        let mut primitive_packets =
            PrimitivePacketArena::new(&mut render_scratch.primitive_packets);

        let room_record = ROOMS.get(self.room_index.to_usize());
        // The cooked BSP replaces only static grid surfaces. It writes its
        // tagged packets into the same arena/OT used below, after which the
        // ordinary actor, equipment, effect, and overlay passes continue.
        let bsp_material_tick = self.gameplay_tick(ctx.sim_tick).as_u32();
        let mut visible_sky_aperture = false;
        let mut world_object_visibility = WorldObjectVisibility::ALL;
        if let Some(bsp) = self.bsp.as_mut() {
            telemetry::stage_begin(telemetry::stage::ROOM);
            world_object_visibility =
                bsp.visible_world_objects(camera, WORLD_OBJECTS, &self.destructibles);
            visible_sky_aperture = bsp.draw(
                camera,
                bsp_material_tick,
                &self.destructibles,
                &mut primitive_packets,
                &mut ot,
            );
            telemetry::stage_end(telemetry::stage::ROOM);
        }

        // Sky shares the farthest OT slot with the maximum-depth PXBSP packet.
        // OT insertion prepends, so inserting the sky after PXBSP makes DMA
        // execute the sky first and keeps even a slot-2047 wall in front.
        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::SKY);
            draw_scene_sky(
                room_record.sky,
                camera,
                bsp_material_tick,
                visible_sky_aperture,
                &mut primitive_packets,
                &mut ot,
            );
            telemetry::stage_end(telemetry::stage::SKY);
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

        if self.current_collision_room.is_some() || self.bsp.is_some() {
            let mut total_instance_stats = ModelInstanceDrawStats::default();
            let mut room_active_chunks = 0u32;
            let mut room_cached_draws = 0u32;
            let mut room_uncached_draws = 0u32;
            let mut room_cache_cells = 0u32;
            let mut room_cache_vertices = 0u32;
            let mut room_cache_surfaces = 0u32;
            let mut room_cache_fallback_draws = 0u32;
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            let mut room_visibility_fallback_draws = 0u32;
            #[cfg(not(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            )))]
            let room_visibility_fallback_draws = 0u32;
            let mut room_active_chunk_mask = RuntimeDebugMask::EMPTY;
            // This mask describes streamed grid chunks, not the resident BSP.
            // BSP draw proof remains the shared primitive/GPU command counters.
            let mut room_drawn_chunk_mask = RuntimeDebugMask::EMPTY;
            #[cfg(feature = "world-grid-visible")]
            let mut room_visible_cells = 0u32;
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            let mut room_range_culled_cells = 0u32;
            #[cfg(all(feature = "world-grid-visible", feature = "vis-full-active-chunks"))]
            let room_range_culled_cells = 0u32;
            #[cfg(feature = "world-grid-visible")]
            let mut room_stats_total = GridVisibilityStats::default();
            #[cfg(feature = "room-surface-profile")]
            let mut room_surface_packets = 0u32;
            #[cfg(feature = "room-surface-profile")]
            let mut room_surface_commands = 0u32;

            // Live entity poses: instances bound to game entities
            // render where the entity runtime moved them (phase 3).
            let mut entity_poses =
                psx_engine::FixedScratch::<ModelInstancePoseOverride, MAX_GAME_ENTITIES>::new();
            self.game_entity_pose_overrides(&mut entity_poses);
            let entity_poses = entity_poses.as_slice();

            // PXBSP has no ActiveRuntimeRoom: that type owns parsed PSXW
            // render/collision payloads. Draw the singleton metadata room's
            // ordinary gameplay content directly in world space while the BSP
            // renderer above owns only static brush surfaces.
            if self.bsp.is_some() {
                if let (Some(room_record), Some(lighting)) =
                    (room_record, self.current_room_lighting(camera))
                {
                    let room_options = pxbsp_surface_options(room_record).with_material_animation(
                        self.gameplay_tick(ctx.sim_tick).as_u32(),
                        ctx.video_hz.as_u16(),
                    );
                    let actor_options = pxbsp_actor_surface_options(room_record)
                        .with_material_animation(
                            self.gameplay_tick(ctx.sim_tick).as_u32(),
                            ctx.video_hz.as_u16(),
                        );
                    let instance_stats = self.draw_room_world_content(
                        self.room_index,
                        &camera,
                        &self.materials[..self.material_count],
                        room_options,
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
            }

            let active_draw_order = active_room_draw_order(
                &self.window.rooms,
                camera,
                &self.visibility.result,
                self.room_index,
                cached_room_draw_order_mode(),
            );
            for &active_slot in &active_draw_order {
                if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                    continue;
                }
                let active_slot = active_slot as usize;
                let Some(active) = self.window.rooms[active_slot] else {
                    continue;
                };
                let draws_room = self.portal_visibility_draws_room(active.index);
                if post_cross_detail {
                    debug_log_post_cross_render_room(active_slot, active, draws_room);
                }
                if !draws_room {
                    continue;
                }
                room_active_chunks = room_active_chunks.saturating_add(1);
                let chunk_mask = room_index_debug_mask(active.index);
                room_active_chunk_mask |= chunk_mask;
                if active.surface_cache.ready {
                    room_cache_cells =
                        room_cache_cells.saturating_add(active.surface_cache.cell_count as u32);
                    room_cache_vertices = room_cache_vertices
                        .saturating_add(active.surface_cache.vertex_count as u32);
                    room_cache_surfaces = room_cache_surfaces
                        .saturating_add(active.surface_cache.surface_count as u32);
                }
                let materials = active_room_materials(&active);
                let Some(room_record) = ROOMS.get(active.index.to_usize()) else {
                    continue;
                };
                let room_options = room_surface_options(room_record).with_material_animation(
                    self.gameplay_tick(ctx.sim_tick).as_u32(),
                    ctx.video_hz.as_u16(),
                );
                // Actors clear the surface they stand on; see actor_surface_options.
                let actor_options = actor_surface_options(room_record).with_material_animation(
                    self.gameplay_tick(ctx.sim_tick).as_u32(),
                    ctx.video_hz.as_u16(),
                );
                let room_camera = camera_for_room(camera, active);
                let lighting = RuntimeRoomLighting {
                    room_index: active.index,
                    ambient: Rgb8::from_array(active.ambient_rgb),
                    camera: room_camera,
                    fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
                    fog_rgb: Rgb8::from_array(room_record.fog_rgb),
                    fog_near: room_record.fog_near,
                    fog_far: room_record.fog_far,
                    lights: room_light_slice(LIGHTS, active.index),
                };
                #[cfg(feature = "room-surface-profile")]
                let room_packet_start = primitive_packets.len();
                #[cfg(feature = "room-surface-profile")]
                let room_command_start = world.command_len();
                telemetry::stage_begin(telemetry::stage::ROOM);
                if self.bsp.is_none() {
                    #[cfg(feature = "world-grid-visible")]
                    {
                        #[cfg(feature = "vis-full-active-chunks")]
                        {
                            let stats = if active.surface_cache.ready {
                                room_cached_draws = room_cached_draws.saturating_add(1);
                                if let Some((
                                    cached_cells,
                                    cached_cell_vertices,
                                    cached_vertices,
                                    cached_surfaces,
                                )) =
                                    room_surface_cache_slices(active.index, active.surface_cache)
                                {
                                    let vertex_count = cached_vertices.len();
                                    let room_projection = room_projection_arena();
                                    let projected_indices =
                                        &mut room_projection.indices[..vertex_count];
                                    let projected_vertices =
                                        &mut room_projection.vertices[..vertex_count];
                                    let projected_depths =
                                        &mut room_projection.depths[..vertex_count];
                                    let cell_scratch = cell_scratch_arena();
                                    let accepted_cell_indices = &mut cell_scratch.indices[..];
                                    let accepted_cell_depths = &mut cell_scratch.depths[..];
                                    generated::draw_project_cached_room!(
                                        &lighting,
                                        draw_indexed_cached_room_vertex_lit_all_cells,
                                        [
                                            cached_cells,
                                            cached_cell_vertices,
                                            cached_vertices,
                                            cached_surfaces,
                                            projected_indices,
                                            projected_vertices,
                                            projected_depths,
                                            accepted_cell_indices,
                                            accepted_cell_depths,
                                            materials,
                                        ],
                                        [
                                            &room_camera,
                                            room_options,
                                            cached_room_depth_mode(),
                                            cached_room_subdivision_mode(),
                                            ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                            active.sector_size,
                                            active.index == self.visibility.root,
                                            Some(prebuilt_room_quads_for(active.index)),
                                            &mut primitive_packets,
                                            &mut world,
                                        ]
                                    )
                                } else {
                                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                                    room_cache_fallback_draws =
                                        room_cache_fallback_draws.saturating_add(1);
                                    if let Some(render_room) = active.render() {
                                        room_drawn_chunk_mask |= chunk_mask;
                                        draw_room_vertex_lit(
                                            render_room,
                                            materials,
                                            &lighting,
                                            &room_camera,
                                            room_options,
                                            &mut primitive_packets,
                                            &mut world,
                                        );
                                    }
                                    GridVisibilityStats::default()
                                }
                            } else {
                                room_uncached_draws = room_uncached_draws.saturating_add(1);
                                if active_surface_cache_failed(active.surface_cache) {
                                    room_cache_fallback_draws =
                                        room_cache_fallback_draws.saturating_add(1);
                                }
                                if let Some(render_room) = active.render() {
                                    room_drawn_chunk_mask |= chunk_mask;
                                    draw_room_vertex_lit(
                                        render_room,
                                        materials,
                                        &lighting,
                                        &room_camera,
                                        room_options,
                                        &mut primitive_packets,
                                        &mut world,
                                    );
                                }
                                GridVisibilityStats::default()
                            };
                            room_visible_cells =
                                room_visible_cells.saturating_add(stats.cells_drawn as u32);
                            if stats.cells_drawn > 0 || stats.surfaces_considered > 0 {
                                room_drawn_chunk_mask |= chunk_mask;
                            }
                            accumulate_grid_visibility_stats(&mut room_stats_total, stats);
                        }
                        #[cfg(not(feature = "vis-full-active-chunks"))]
                        {
                            let player = self.motor.position();
                            let portal_cell_window = self.portal_cell_window(active.index);
                            // The player's own room anchors its per-cell PVS at
                            // the player; a far room admitted by the portal walk
                            // anchors at the portal that admitted it (the
                            // doorway-eye view). Rooms with no usable anchor
                            // draw every cell through the cached path below --
                            // NEVER a silent skip (the arch-door regression).
                            let window_visibility_anchor = if active.index == self.room_index {
                                Some(player)
                            } else {
                                self.portal_entry_anchor(active.index, active.sector_size)
                            };
                            telemetry::stage_begin(telemetry::stage::ROOM_VISIBLE_LIST);
                            let visible_cells_result = match window_visibility_anchor {
                                Some(window_anchor) => {
                                    let visibility_anchor = RoomPoint::new(
                                        window_anchor.x.saturating_sub(active.offset_x),
                                        window_anchor.y,
                                        window_anchor.z.saturating_sub(active.offset_z),
                                    );
                                    self.cached_precomputed_visible_cells(
                                        active_slot,
                                        active.index,
                                        active.width,
                                        active.depth,
                                        active.sector_size,
                                        visibility_anchor,
                                        active.offset_x,
                                        active.offset_z,
                                        window_anchor,
                                        room_camera,
                                        ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES
                                            && !self.player_moved_last_tick
                                            && self.camera_turning_last_tick
                                            && active.surface_cache.ready,
                                    )
                                }
                                None => None,
                            };
                            telemetry::stage_end(telemetry::stage::ROOM_VISIBLE_LIST);
                            let stats = if let Some((cells, range_culled)) = visible_cells_result {
                                room_range_culled_cells =
                                    room_range_culled_cells.saturating_add(range_culled as u32);
                                room_visible_cells =
                                    room_visible_cells.saturating_add(cells.len() as u32);
                                if active.surface_cache.ready {
                                    room_cached_draws = room_cached_draws.saturating_add(1);
                                    if let Some((
                                        cached_cells,
                                        cached_cell_vertices,
                                        cached_vertices,
                                        cached_surfaces,
                                    )) = room_surface_cache_slices(
                                        active.index,
                                        active.surface_cache,
                                    ) {
                                        let vertex_count = cached_vertices.len();
                                        let room_projection = room_projection_arena();
                                        let projected_indices =
                                            &mut room_projection.indices[..vertex_count];
                                        let projected_vertices =
                                            &mut room_projection.vertices[..vertex_count];
                                        let projected_depths =
                                            &mut room_projection.depths[..vertex_count];
                                        let cell_scratch = cell_scratch_arena();
                                        let accepted_cell_indices = &mut cell_scratch.indices[..];
                                        let accepted_cell_depths = &mut cell_scratch.depths[..];
                                        generated::draw_project_cached_room!(
                                            &lighting,
                                            draw_indexed_cached_room_vertex_lit_visible_cells,
                                            [
                                                cached_cells,
                                                cached_cell_vertices,
                                                cached_vertices,
                                                cached_surfaces,
                                                projected_indices,
                                                projected_vertices,
                                                projected_depths,
                                                accepted_cell_indices,
                                                accepted_cell_depths,
                                                active.depth,
                                                active.sector_size,
                                                materials,
                                            ],
                                            [
                                                &room_camera,
                                                room_options,
                                                cached_room_depth_mode(),
                                                cached_room_subdivision_mode(),
                                                cells,
                                                ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                                portal_cell_window,
                                                Some(prebuilt_room_quads_for(active.index)),
                                                &mut primitive_packets,
                                                &mut world,
                                            ]
                                        )
                                    } else {
                                        room_uncached_draws = room_uncached_draws.saturating_add(1);
                                        if let Some(render_room) = active.render() {
                                            draw_room_vertex_lit_visible_cells(
                                                render_room,
                                                materials,
                                                &lighting,
                                                &room_camera,
                                                room_options,
                                                cells,
                                                ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                                &mut primitive_packets,
                                                &mut world,
                                            )
                                        } else {
                                            GridVisibilityStats::default()
                                        }
                                    }
                                } else {
                                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                                    if active_surface_cache_failed(active.surface_cache) {
                                        room_cache_fallback_draws =
                                            room_cache_fallback_draws.saturating_add(1);
                                    }
                                    if let Some(render_room) = active.render() {
                                        draw_room_vertex_lit_visible_cells(
                                            render_room,
                                            materials,
                                            &lighting,
                                            &room_camera,
                                            room_options,
                                            cells,
                                            ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                            &mut primitive_packets,
                                            &mut world,
                                        )
                                    } else {
                                        GridVisibilityStats::default()
                                    }
                                }
                            } else {
                                // No usable anchor or no PVS data for this room.
                                // Draw EVERY cell through the cached path -- it
                                // works for streamed rooms whose full render data
                                // is not resident (active.render() == None), which
                                // the old uncached-only fallback silently skipped
                                // (the arch-door black-room regression).
                                room_visibility_fallback_draws =
                                    room_visibility_fallback_draws.saturating_add(1);
                                if active.surface_cache.ready {
                                    if let Some((
                                        cached_cells,
                                        cached_cell_vertices,
                                        cached_vertices,
                                        cached_surfaces,
                                    )) = room_surface_cache_slices(
                                        active.index,
                                        active.surface_cache,
                                    ) {
                                        room_cached_draws = room_cached_draws.saturating_add(1);
                                        let vertex_count = cached_vertices.len();
                                        let room_projection = room_projection_arena();
                                        let projected_indices =
                                            &mut room_projection.indices[..vertex_count];
                                        let projected_vertices =
                                            &mut room_projection.vertices[..vertex_count];
                                        let projected_depths =
                                            &mut room_projection.depths[..vertex_count];
                                        let cell_scratch = cell_scratch_arena();
                                        let accepted_cell_indices = &mut cell_scratch.indices[..];
                                        let accepted_cell_depths = &mut cell_scratch.depths[..];
                                        generated::draw_project_cached_room!(
                                            &lighting,
                                            draw_indexed_cached_room_vertex_lit_all_cells,
                                            [
                                                cached_cells,
                                                cached_cell_vertices,
                                                cached_vertices,
                                                cached_surfaces,
                                                projected_indices,
                                                projected_vertices,
                                                projected_depths,
                                                accepted_cell_indices,
                                                accepted_cell_depths,
                                                materials,
                                            ],
                                            [
                                                &room_camera,
                                                room_options,
                                                cached_room_depth_mode(),
                                                cached_room_subdivision_mode(),
                                                ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                                active.sector_size,
                                                // Lateral-cull cells in EVERY no-anchor
                                                // fallback room, not just the root: the
                                                // AABB test is the same conservative
                                                // margin bound the root room already
                                                // trusts, and 3-4 of ~5 drawn
                                                // rooms take this path per frame. Cells
                                                // it rejects are off-screen, so output
                                                // pixels are unchanged; only the
                                                // projection + surface walk for them is
                                                // skipped.
                                                true,
                                                Some(prebuilt_room_quads_for(active.index)),
                                                &mut primitive_packets,
                                                &mut world,
                                            ]
                                        )
                                    } else {
                                        room_uncached_draws = room_uncached_draws.saturating_add(1);
                                        if let Some(render_room) = active.render() {
                                            draw_room_vertex_lit(
                                                render_room,
                                                materials,
                                                &lighting,
                                                &room_camera,
                                                room_options,
                                                &mut primitive_packets,
                                                &mut world,
                                            );
                                        }
                                        GridVisibilityStats::default()
                                    }
                                } else {
                                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                                    if let Some(render_room) = active.render() {
                                        draw_room_vertex_lit(
                                            render_room,
                                            materials,
                                            &lighting,
                                            &room_camera,
                                            room_options,
                                            &mut primitive_packets,
                                            &mut world,
                                        );
                                    }
                                    GridVisibilityStats::default()
                                }
                            };
                            if stats.cells_drawn > 0 || stats.surfaces_considered > 0 {
                                room_drawn_chunk_mask |= chunk_mask;
                            }
                            accumulate_grid_visibility_stats(&mut room_stats_total, stats);
                        }
                    }
                    #[cfg(not(feature = "world-grid-visible"))]
                    {
                        room_uncached_draws = room_uncached_draws.saturating_add(1);
                        if active_surface_cache_failed(active.surface_cache) {
                            room_cache_fallback_draws = room_cache_fallback_draws.saturating_add(1);
                        }
                        if let Some(render_room) = active.render() {
                            room_drawn_chunk_mask |= chunk_mask;
                            draw_room_vertex_lit(
                                render_room,
                                materials,
                                &lighting,
                                &room_camera,
                                room_options,
                                &mut primitive_packets,
                                &mut world,
                            );
                        }
                    }
                }
                telemetry::stage_end(telemetry::stage::ROOM);
                #[cfg(feature = "room-surface-profile")]
                {
                    room_surface_packets = room_surface_packets.saturating_add(
                        primitive_packets.len().saturating_sub(room_packet_start) as u32,
                    );
                    room_surface_commands = room_surface_commands.saturating_add(
                        world.command_len().saturating_sub(room_command_start) as u32,
                    );
                }
                let instance_stats = self.draw_room_world_content(
                    active.index,
                    &room_camera,
                    materials,
                    room_options,
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
            if let (Some(character), Some(player_pose)) = (self.character, self.player_actor_pose) {
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
                let actor_options =
                    current_actor_surface_options(self.room_index, self.bsp.is_some());
                telemetry::stage_begin(telemetry::stage::PLAYER);
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
                let player_draw =
                    player_lighting.map_or(PlayerModelDrawStats::default(), |lighting| {
                        let tint_sweep = player_stance_tint_sweep(
                            self.player_stance,
                            &self.player_stance_config,
                            player,
                            character.height,
                            &camera,
                        );
                        draw_player(
                            self.room_index,
                            &character,
                            player_pose,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            ctx.sim_tick,
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            tint_sweep,
                            &mut primitive_packets,
                            &mut world,
                        )
                    });
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
                let mut instance_equipment_remaining = MAX_EQUIPMENT_DRAWS;
                if self.bsp.is_some() {
                    if let (Some(room_record), Some(lighting)) =
                        (room_record, self.current_room_lighting(camera))
                    {
                        let actor_options = pxbsp_actor_surface_options(room_record)
                            .with_material_animation(
                                self.gameplay_tick(ctx.sim_tick).as_u32(),
                                ctx.video_hz.as_u16(),
                            );
                        telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                        let equipment_stats = draw_instance_equipment(
                            self.room_index,
                            &self.instance_actor_poses,
                            instance_equipment_remaining,
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
                        instance_equipment_remaining = instance_equipment_remaining
                            .saturating_sub(equipment_stats.draws as usize);
                        telemetry::stage_end(telemetry::stage::EQUIPMENT);
                    }
                }
                for &active_slot in &active_draw_order {
                    if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                        continue;
                    }
                    let Some(active) = self.window.rooms[active_slot as usize] else {
                        continue;
                    };
                    if !self.portal_visibility_draws_room(active.index) {
                        continue;
                    }
                    let room_camera = camera_for_room(camera, active);
                    let Some(room_record) = ROOMS.get(active.index.to_usize()) else {
                        continue;
                    };
                    let actor_options = room_surface_options(room_record);
                    let lighting = RuntimeRoomLighting {
                        room_index: active.index,
                        ambient: Rgb8::from_array(active.ambient_rgb),
                        camera: room_camera,
                        fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
                        fog_rgb: Rgb8::from_array(room_record.fog_rgb),
                        fog_near: room_record.fog_near,
                        fog_far: room_record.fog_far,
                        lights: room_light_slice(LIGHTS, active.index),
                    };
                    // Enemy weapons ride their instances' live poses. Bodies
                    // were submitted once with their room content above; the
                    // same per-face OT depth sorts this later equipment pass
                    // against body, player and room surfaces.
                    telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                    let equipment_stats = draw_instance_equipment(
                        active.index,
                        &self.instance_actor_poses,
                        instance_equipment_remaining,
                        self.gameplay_tick(ctx.sim_tick),
                        ctx.video_hz,
                        &room_camera,
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
                    instance_equipment_remaining =
                        instance_equipment_remaining.saturating_sub(equipment_stats.draws as usize);
                    telemetry::stage_end(telemetry::stage::EQUIPMENT);
                }
            }

            let _ = self.draw_archive_beacons_world(
                camera,
                self.gameplay_tick(ctx.sim_tick),
                world_object_visibility,
                &mut primitive_packets,
                &mut world,
            );
            self.draw_vitality_circles_world(
                camera,
                self.gameplay_tick(ctx.sim_tick),
                &mut primitive_packets,
                &mut world,
            );

            telemetry::counter(telemetry::counter::ROOM_ACTIVE_CHUNKS, room_active_chunks);
            emit_room_chunk_mask(
                telemetry::counter::ROOM_ACTIVE_CHUNK_MASK_LO,
                telemetry::counter::ROOM_ACTIVE_CHUNK_MASK_HI,
                room_active_chunk_mask,
            );
            emit_room_chunk_mask(
                telemetry::counter::ROOM_DRAWN_CHUNK_MASK_LO,
                telemetry::counter::ROOM_DRAWN_CHUNK_MASK_HI,
                room_drawn_chunk_mask,
            );
            let debug_view = self.active_room_selection_view();
            emit_player_map_debug(
                self.room_index,
                self.motor.position(),
                self.motor.yaw().as_q12(),
                RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
                self.visibility.camera_global,
                yaw_q12_from_basis(debug_view.sin_yaw, debug_view.cos_yaw),
                debug_view.sin_yaw,
                debug_view.cos_yaw,
                debug_view.sin_pitch,
                debug_view.cos_pitch,
            );
            self.emit_portal_visibility_counters();
            #[cfg(feature = "cd-stream-bench")]
            if !USES_PXBSP {
                let room_streams = room_streams_arena();
                telemetry::counter(
                    telemetry::counter::ROOM_STREAM_RESIDENT_SLOTS,
                    room_streams.resident_slot_count() as u32,
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_LO,
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_HI,
                    room_streams.loading_room_mask(),
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_LO,
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_HI,
                    room_streams.resident_room_mask(),
                );
            }
            telemetry::counter(telemetry::counter::ROOM_CACHED_DRAWS, room_cached_draws);
            telemetry::counter(telemetry::counter::ROOM_UNCACHED_DRAWS, room_uncached_draws);
            telemetry::counter(telemetry::counter::ROOM_CACHE_CELLS, room_cache_cells);
            telemetry::counter(telemetry::counter::ROOM_CACHE_VERTICES, room_cache_vertices);
            telemetry::counter(telemetry::counter::ROOM_CACHE_SURFACES, room_cache_surfaces);
            telemetry::counter(
                telemetry::counter::ROOM_CACHE_FALLBACK_DRAWS,
                room_cache_fallback_draws,
            );
            telemetry::counter(
                telemetry::counter::ROOM_VISIBILITY_FALLBACK_DRAWS,
                room_visibility_fallback_draws,
            );
            telemetry::counter(
                telemetry::counter::ROOM_CHUNKS_CONSIDERED,
                self.visibility.candidates as u32,
            );
            telemetry::counter(
                telemetry::counter::ROOM_CHUNK_CACHE_SKIPS,
                self.window.cache_skips as u32,
            );
            #[cfg(feature = "world-grid-visible")]
            {
                telemetry::counter(telemetry::counter::ROOM_VISIBLE_CELLS, room_visible_cells);
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_RANGE_CULLED,
                    room_range_culled_cells,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_CONSIDERED,
                    room_stats_total.cells_considered as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_DRAWN,
                    room_stats_total.cells_drawn as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_CULLED,
                    room_stats_total.cells_frustum_culled as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_SURFACES_CONSIDERED,
                    room_stats_total.surfaces_considered as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_PROJECTED_VERTICES,
                    room_stats_total.projected_vertices as u32,
                );
            }
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_DRAWS,
                total_instance_stats.draws as u32,
            );
            #[cfg(feature = "room-surface-profile")]
            {
                telemetry::counter(
                    telemetry::counter::ROOM_SURFACE_PACKETS,
                    room_surface_packets,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_SURFACE_COMMANDS,
                    room_surface_commands,
                );
            }
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
            if post_cross_debug {
                debug_log_post_cross_render_end(
                    self.room_index,
                    room_active_chunk_mask,
                    room_drawn_chunk_mask,
                    primitive_packets.len(),
                    primitive_packets.remaining(),
                    world.command_len(),
                );
                post_cross_logged_end = true;
            }
        }

        if post_cross_debug && !post_cross_logged_end {
            debug_log_post_cross_render_end(
                self.room_index,
                RuntimeDebugMask::EMPTY,
                RuntimeDebugMask::EMPTY,
                primitive_packets.len(),
                primitive_packets.remaining(),
                world.command_len(),
            );
        }
        if post_cross_debug {
            self.post_cross_debug_frames = self.post_cross_debug_frames.saturating_sub(1);
        }

        let world_command_len = world.command_len();
        telemetry::stage_begin(telemetry::stage::WORLD_FLUSH);
        world.flush();
        telemetry::stage_end(telemetry::stage::WORLD_FLUSH);
        let _ = self.draw_particle_emitters(
            camera,
            self.gameplay_tick(ctx.sim_tick),
            &mut ot,
            &mut primitive_packets,
        );
        let _ = self.draw_combat_projectiles(camera, &mut ot, &mut primitive_packets);
        let _ = self.draw_player_water_wade_splash(
            camera,
            self.gameplay_tick(ctx.sim_tick),
            &mut ot,
            &mut primitive_packets,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVES,
            primitive_packets.len() as u32,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVE_REMAINING,
            primitive_packets.remaining() as u32,
        );
        telemetry::counter(telemetry::counter::WORLD_COMMANDS, world_command_len as u32);
        // Submission is deliberately split from packet preparation. The app
        // runner first presents the previous queued frame and clears the new
        // back buffer, then calls submit_render below.
        let _ = ot;
    }

    fn submit_render(&mut self, _ctx: &mut Ctx) {
        self.overlay_camera = self.prepared_overlay_camera;
        self.overlay_sim_tick = self.prepared_overlay_sim_tick;
        self.overlay_poi_panel_frame = self.prepared_poi_panel_frame;
        self.overlay_poi_page_type_frame = self.prepared_poi_page_type_frame;
        telemetry::stage_begin(telemetry::stage::OT_SUBMIT);
        let ot_in_flight = unsafe { OtFrame::resume(&mut OT) }.submit_async();
        telemetry::stage_end(telemetry::stage::OT_SUBMIT);
        ot_in_flight.detach();
    }

    fn render_overlay(&mut self, _ctx: &mut Ctx) {
        let camera = self.overlay_camera;
        let overlay_tick = self.overlay_sim_tick;

        if let Some(room_record) = ROOMS.get(self.room_index.to_usize()) {
            draw_room_atmosphere_overlay(room_record, overlay_tick);
        }

        #[cfg(feature = "collision-debug-overlay")]
        if self.show_collision_debug {
            self.draw_collision_debug_overlay(camera);
        }

        if let Some(target) = self.lock_target_indicator_position() {
            draw_lock_target_indicator(
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

        // The target vitality stack follows the player HUD's two-ribbon
        // grammar and swap motion, but stays compact and omits the player's
        // cooldown diamond. Anchor its shared left edge to the right of the
        // target rather than covering the model from above.
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
                        draw_enemy_vitality_hud(
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

        // One runtime-owned cluster draws the mutation charge and both health
        // pools. The previous implementation layered a moving rectangular bar
        // beneath an authored slanted bar, leaving both visible at rest.
        if !self.inventory_overlay_active {
            if let Some(font) = self.ui_fonts[0].as_ref() {
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

        #[cfg(feature = "fps-overlay")]
        if let Some(font) = self.ui_fonts[0].as_ref() {
            draw_fps_overlay(font, self.fps_display, self.fps_display_worst);
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
            if let Some(module) = self
                .acquired_module
                .index()
                .and_then(|index| BOOST_MODULES.get(index))
            {
                draw_acquired_module(
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
                    font,
                    message.title,
                    message.body,
                    self.overlay_poi_page_type_frame,
                    cross_prompt,
                );
            } else if let Some(index) = self.active_interactable {
                if let Some(interactable) = INTERACTABLES.get(index) {
                    draw_interaction_prompt_animated(
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
        materials: &[WorldRenderMaterial],
        room_options: WorldSurfaceOptions,
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
        telemetry::stage_begin(telemetry::stage::ENTITY_MARKERS);
        draw_entity_markers(
            ENTITIES,
            room,
            materials,
            camera,
            room_options,
            primitive_packets,
            world,
        );
        telemetry::stage_end(telemetry::stage::ENTITY_MARKERS);
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
        #[cfg(feature = "actor-shadows-projected")]
        {
            draw_model_instance_projected_shadows(
                room,
                &self.instance_actor_poses,
                camera,
                actor_options,
                if self.bsp.is_some() {
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
                    if self.bsp.is_some() {
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
        stats
    }
}
