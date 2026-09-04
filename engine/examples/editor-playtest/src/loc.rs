//! Localisation: one Italian column over the cooked English copy.
//!
//! # Why this shape
//!
//! Authored UI text already reaches the screen twice over: the cook writes it
//! into [`psx_level::LevelUiNodeRecord::text`] as `&'static str`, and the UI
//! renderer draws that verbatim unless the node carries a `tag`, in which case
//! it asks the scene through [`psx_engine::Scene::ui_text`] and prefers the
//! answer. That indirection was built for live gameplay text (module names,
//! stat lines) and it is exactly the hook a second language needs, so nothing
//! in `psx-level`, `ui.rs` or the editor's `ui_types.rs` changes to support
//! one. A localisable widget is an authored widget that has been given a
//! `ui.`-prefixed tag.
//!
//! # English is free
//!
//! Only the Italian column lives here. English is whatever the cook already
//! wrote into the node record, so [`translate`] returns `None` for it and the
//! renderer falls through to the authored copy. Adding Italian therefore costs
//! the Italian bytes plus one key and two fat pointers per entry, not two full
//! copies of the game's text. It also means an untranslated string degrades to
//! English on screen instead of to an empty label, which is the right failure
//! for a table that will be filled in over several passes.
//!
//! # Cost per frame
//!
//! [`draw_scene`] resolves a tag once per tagged node per frame. In English --
//! the default, and the only language a player who never opens Settings sees --
//! that resolve costs one load of [`LANGUAGE`] and one branch, because the
//! early return fires before the table is touched. In Italian it costs a
//! binary search over [`ITALIAN`], which is `log2(len)` string compares against
//! short keys. Neither path disturbs the cross-frame UI layout memo: that memo
//! caches resolved *rectangles*, which are a pure function of the const node
//! pool, and live text has always been resolved outside it.
//!
//! # ASCII only
//!
//! Every bitmap face in `psx-font` covers `0x20..0x80` and nothing else, so
//! `LUMINOSITA'` is spelled with a trailing apostrophe rather than an accent.
//! That is standard Italian practice for uppercase text and needs no font
//! work; adding the Latin-1 accented range would mean re-rasterising and
//! re-uploading every atlas the project uses.

/// Languages the project ships. Stored as a `u8` so the setting can be
/// persisted or bound to a project option later without a conversion table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Language {
    /// The authored copy, straight out of the cooked node records.
    English = 0,
    /// The [`ITALIAN`] column, keyed by tag.
    Italian = 1,
}

/// Live language. Single-threaded guest, written only by [`set_language`] from
/// the UI action path and read from the per-frame text resolve.
static mut LANGUAGE: Language = Language::English;

/// The language the next resolved label will draw in.
#[inline]
pub(crate) fn language() -> Language {
    // SAFETY: the guest is single-threaded and cooperatively scheduled; no
    // reference escapes this function and `Language` is a `Copy` byte.
    unsafe { core::ptr::read(core::ptr::addr_of!(LANGUAGE)) }
}

/// Select a language. Takes effect on the next drawn frame -- nothing is
/// cached across the switch, because the text resolve is already per-frame.
pub(crate) fn set_language(next: Language) {
    // SAFETY: as `language`.
    unsafe {
        core::ptr::write(core::ptr::addr_of_mut!(LANGUAGE), next);
    }
}

/// Cycle to the next language. Wired to the Settings scene's language button
/// through `UiAction::Game(LANGUAGE_TOGGLE_ACTION)`.
///
/// A button rather than a project option because `SetOption` clamps its delta
/// to the option range instead of wrapping, so a two-value setting driven that
/// way can be advanced once and never brought back.
pub(crate) fn cycle_language() {
    set_language(match language() {
        Language::English => Language::Italian,
        Language::Italian => Language::English,
    });
}

/// `UiAction::Game` id the Settings language control fires. Chosen clear of the
/// inventory's 200..=220 and the tab bar's 300..=304.
pub(crate) const LANGUAGE_TOGGLE_ACTION: u16 = 400;

/// Tag prefix reserved for localisation keys. Gameplay tags (`boost.`,
/// `inventory.`, `tab.`) never begin with it, so the two tag namespaces cannot
/// collide and the resolver can tell them apart without a table lookup.
pub(crate) const KEY_PREFIX: &str = "ui.";

/// Italian copy, keyed by UI tag. **Must stay sorted by key** --
/// [`translate`] binary-searches it, and `italian_table_is_sorted` fails the
/// build's test run if a new row breaks the order.
///
/// English is deliberately absent: see the module docs.
static ITALIAN: &[(&str, &str)] = &[
    ("ui.credits.artist", "ARTISTA 3D"),
    ("ui.credits.by", "UN GIOCO DI"),
    ("ui.credits.director", "DIRETTORE E LEAD PROGRAMMER"),
    ("ui.credits.music", "MUSICA E SFX"),
    ("ui.credits.title", "CREDITI"),
    ("ui.ending.back", "  MENU PRINCIPALE"),
    ("ui.ending.body", "RESTA SINTONIZZATO PER NUOVI AGGIORNAMENTI."),
    ("ui.ending.head", "GRAZIE PER AVER GIOCATO"),
    ("ui.ending.subject", "DEMO TECNICA CORTEX IGNITION"),
    (
        "ui.hint.sockets",
        "INNESTA I MODULI RECUPERATI\nDALL'INVENTARIO.",
    ),
    ("ui.inventory.assign", "ASSEGNA"),
    ("ui.inventory.atk_spd", "VEL ATK"),
    ("ui.inventory.choose_socket", "SCEGLI UN INNESTO"),
    ("ui.inventory.close", "CHIUDI"),
    ("ui.inventory.collected", "RACCOLTO"),
    ("ui.inventory.defence", "DIFESA"),
    ("ui.inventory.equipped", "EQUIPAGGIATO"),
    ("ui.inventory.final_stats", "VALORI FINALI"),
    ("ui.inventory.horizon", "ORIZZONTE"),
    ("ui.inventory.module_effect", "EFFETTO MODULO"),
    ("ui.inventory.modules", "MODULI"),
    ("ui.inventory.move_spd", "VEL MOV"),
    ("ui.inventory.no_modules", "NESSUN MODULO"),
    ("ui.inventory.none", "NESSUNO"),
    ("ui.inventory.remove", "RIMUOVI"),
    ("ui.inventory.select", "SELEZIONA"),
    ("ui.inventory.select_module", "SCEGLI UN MODULO"),
    ("ui.inventory.target_high_gain", "MIRA // ALTO GUADAGNO"),
    ("ui.inventory.target_stable", "MIRA // STABILE"),
    ("ui.inventory.zenith", "ZENIT"),
    ("ui.item_acquired", "OGGETTO OTTENUTO - "),
    ("ui.loading.continue", "CONTINUA"),
    ("ui.loading.status", "SINCRONIZZAZIONE PROIEZIONE"),
    ("ui.loading.wait", "ATTENDERE"),
    ("ui.menu.credits", "  CREDITI"),
    ("ui.menu.new_game", "  NUOVA PARTITA"),
    ("ui.menu.system", "  SISTEMA"),
    ("ui.module.short.module", "MODULO"),
    ("ui.module.short.rupture", "ROTTURA"),
    ("ui.module.short.shell", "GUSCIO"),
    ("ui.module.short.zenith", "ZENIT"),
    ("ui.prompt.dismiss", "CHIUDI"),
    ("ui.prompt.read", "LEGGI"),
    ("ui.prompt.take", "PRENDI"),
    ("ui.settings.back", "  INDIETRO"),
    ("ui.settings.brightness", "LUMINOSITA'"),
    // "ZONA MORTA STICK" is the literal translation and measures 140px in a
    // 127px column, which wraps to a second line and overprints the row below.
    // The stick is the only deadzone in this menu, so the qualifier goes.
    ("ui.settings.deadzone", "ZONA MORTA"),
    ("ui.settings.language", "LINGUA"),
    ("ui.settings.language.value", "ITALIANO"),
    ("ui.settings.music_volume", "VOLUME MUSICA"),
    ("ui.settings.screen_x", "SCHERMO X"),
    ("ui.settings.screen_y", "SCHERMO Y"),
    ("ui.settings.sfx_volume", "VOLUME EFFETTI"),
    ("ui.settings.title", "IMPOSTAZIONI"),
    ("ui.system.deadzone", "ZONA MORTA"),
    ("ui.system.music", "MUSICA"),
    ("ui.system.return", "TORNA AL TITOLO"),
    ("ui.system.title", "SISTEMA"),
    ("ui.title.tech_demo", "DEMO TECNICA"),
];

/// Runtime copy: the live-language string for `key`, or `english` when the
/// language has no column or the key is not filled in. The gameplay code's
/// hard-coded verbs and labels go through here, so English stays the literal
/// in the source and Italian is one table row.
#[inline]
pub(crate) fn tr(key: &str, english: &'static str) -> &'static str {
    translate(key).unwrap_or(english)
}

/// The Archive panel's close verb.
pub(crate) fn dismiss_action() -> &'static str {
    tr("ui.prompt.dismiss", "DISMISS")
}

/// Prefix of the compact acquisition panel, trailing separator included.
pub(crate) fn item_acquired_prefix() -> &'static str {
    tr("ui.item_acquired", "ITEM ACQUIRED - ")
}

/// A beacon's cooked interaction verb (`READ`, `TAKE`) in the live language.
/// Unknown verbs stay as authored.
pub(crate) fn prompt_verb(verb: &'static str) -> &'static str {
    match verb {
        "READ" => tr("ui.prompt.read", verb),
        "TAKE" => tr("ui.prompt.take", verb),
        _ => verb,
    }
}

/// Cooked message page `index` in the live language: the Italian column when
/// it was authored, the English page otherwise.
pub(crate) fn page_text(index: usize) -> Option<&'static str> {
    use crate::generated::{INTERACTABLE_MESSAGE_PAGES, INTERACTABLE_MESSAGE_PAGES_IT};
    let english = INTERACTABLE_MESSAGE_PAGES.get(index).copied()?;
    if language() == Language::English {
        return Some(english);
    }
    match INTERACTABLE_MESSAGE_PAGES_IT.get(index) {
        Some(italian) if !italian.is_empty() => Some(italian),
        _ => Some(english),
    }
}

/// Module `index`'s name in the live language, English when no Italian was
/// authored.
pub(crate) fn module_name(index: usize, english: &'static str) -> &'static str {
    module_column(index, english, |(name, _)| name)
}

/// Module `index`'s description in the live language.
pub(crate) fn module_description(index: usize, english: &'static str) -> &'static str {
    module_column(index, english, |(_, description)| description)
}

fn module_column(
    index: usize,
    english: &'static str,
    pick: fn(&(&'static str, &'static str)) -> &'static str,
) -> &'static str {
    if language() == Language::English {
        return english;
    }
    match crate::generated::BOOST_MODULES_IT.get(index).map(pick) {
        Some(italian) if !italian.is_empty() => italian,
        _ => english,
    }
}

/// Resolve `tag` in the live language, or `None` to keep the authored English.
///
/// `None` is the answer for every gameplay tag, for every language that has no
/// column, and for a key the current column has not been filled in for yet.
#[inline]
pub(crate) fn translate(tag: &str) -> Option<&'static str> {
    let column = match language() {
        // The authored copy IS the English column. Returning early here is
        // what keeps the default language's per-node cost at one branch.
        Language::English => return None,
        Language::Italian => ITALIAN,
    };
    if !tag.starts_with(KEY_PREFIX) {
        return None;
    }
    let index = column.binary_search_by_key(&tag, |(key, _)| *key).ok()?;
    Some(column[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `translate` binary-searches, so an out-of-order row would make its own
    /// string unreachable AND could hide a neighbour's. Cheaper to assert the
    /// invariant than to debug a label that silently stayed English.
    #[test]
    fn italian_table_is_sorted_and_unique() {
        for pair in ITALIAN.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "ITALIAN is out of order at {:?} -> {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    /// Every key must be in the reserved namespace, or `translate`'s prefix
    /// gate drops it before the search ever runs.
    #[test]
    fn every_key_uses_the_reserved_prefix() {
        for (key, _) in ITALIAN {
            assert!(key.starts_with(KEY_PREFIX), "{key} escapes {KEY_PREFIX}");
        }
    }

    /// The fonts cover printable ASCII only. An accented byte draws as a hole,
    /// so catch it here rather than on a screenshot.
    #[test]
    fn italian_copy_is_printable_ascii() {
        for (key, text) in ITALIAN {
            for byte in text.bytes() {
                assert!(
                    (0x20..0x80).contains(&byte),
                    "{key}: {text:?} has non-printable-ASCII byte {byte:#04x}"
                );
            }
        }
    }

    #[test]
    fn english_keeps_the_authored_copy_and_italian_overrides_it() {
        set_language(Language::English);
        assert_eq!(translate("ui.settings.title"), None);
        set_language(Language::Italian);
        assert_eq!(translate("ui.settings.title"), Some("IMPOSTAZIONI"));
        // A gameplay tag is never a localisation key in either language.
        assert_eq!(translate("boost.stat.horizon"), None);
        // An unfilled key falls back to English rather than to an empty label.
        assert_eq!(translate("ui.not.filled.in"), None);
        set_language(Language::English);
    }

    #[test]
    fn cycling_returns_to_english() {
        set_language(Language::English);
        cycle_language();
        assert_eq!(language(), Language::Italian);
        cycle_language();
        assert_eq!(language(), Language::English);
    }
}
