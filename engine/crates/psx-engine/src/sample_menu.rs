//! A ready-to-run sample title menu, authored as data.
//!
//! The menu *engine* already exists: [`crate::ui::draw_scene`] paints a pool of
//! [`LevelUiNodeRecord`]s and [`crate::game_app::GameApp`] drives focus
//! navigation, button activation, and scene transitions. A menu is therefore
//! not code you write but **data you author**: a node pool, a scene table, and
//! a flow graph. This module is a worked example of that data, public so a game
//! can use it directly or copy it as a starting point.
//!
//! # What it builds
//!
//! A title screen (scene id 1) with a centred title label and three stacked
//! buttons -- Play, Options, Quit -- plus an Options sub-screen (scene id 2)
//! with a single Back button. Wire it up with:
//!
//! ```ignore
//! let mut app = GameApp::new(
//!     &sample_menu::FLOW,
//!     sample_menu::SCENES,
//!     sample_menu::NODES,
//!     &[],            // no options table for this sample
//!     &mut gameplay,  // your Scene
//! );
//! ```
//!
//! Then call `app.init` / `app.update` / `app.render` each frame as usual. The
//! render path needs a [`FontAtlas`](psx_font::FontAtlas); upload one of the
//! built-in [`psx_font::fonts`] once and pass it to `draw_scene`.
//!
//! # Coordinates
//!
//! Buttons are parented to the canvas (node 0) and centre-anchored, so their
//! `x` is an offset from the canvas centre and a button of `width` W sits at
//! `-W/2` to read as horizontally centred. The title label is centre-anchored
//! and centre-aligned within its own width.

use psx_level::{
    ui_node_flags, AssetId, GameFlow, FlowState, LevelUiAction, LevelUiNodeKind,
    LevelUiNodeRecord, LevelUiScene, LevelUiValueBinding, UiNodeIndex, UI_OPTION_NONE,
};

/// Standard PS1 framebuffer width the sample is laid out for.
const CANVAS_W: u16 = 320;
/// Standard PS1 framebuffer height the sample is laid out for.
const CANVAS_H: u16 = 240;

/// Button width in canvas pixels. Centre-anchored, so the button's `x` is
/// `-BUTTON_W / 2` to centre it horizontally.
const BUTTON_W: u16 = 120;
/// Button height in canvas pixels.
const BUTTON_H: u16 = 22;

/// Anchor nibble for "centre of parent" (factors (1, 1) in
/// [`crate::ui`]'s `anchor_factors`).
const ANCHOR_CENTER: u16 = 4;
/// Text-align nibble for centre, shifted into [`ui_node_flags::TEXT_ALIGN_MASK`].
const ALIGN_CENTER: u16 = 1 << ui_node_flags::TEXT_ALIGN_SHIFT;

/// Root canvas. Every other node's anchor/layout resolves against this.
const CANVAS: LevelUiNodeRecord = LevelUiNodeRecord {
    parent: None,
    kind: LevelUiNodeKind::Canvas,
    x: 0,
    y: 0,
    width: CANVAS_W,
    height: CANVAS_H,
    color: [0, 0, 0],
    background: [0, 0, 0],
    accent: [0, 0, 0],
    value: LevelUiValueBinding::ConstantQ12(0),
    max: LevelUiValueBinding::ConstantQ12(1),
    texture_asset: AssetId(u16::MAX),
    text: "",
    tag: "",
    action: LevelUiAction::Back,
    option: UI_OPTION_NONE,
    flags: 0,
    font: 0,
};

/// Font-table index for the large title font. The caller uploads a font table
/// where index 1 is a big/display face; if it only uploads one font, the
/// renderer falls back to index 0, so this stays correct either way.
pub const TITLE_FONT: u8 = 1;
/// Font-table index for the small body font used by buttons (the default).
pub const BODY_FONT: u8 = 0;

/// A centre-anchored, centre-aligned text label of the given width, drawn with
/// font-table index `font` (see [`LevelUiNodeRecord::font`]).
const fn label(
    y: i16,
    width: u16,
    text: &'static str,
    color: [u8; 3],
    font: u8,
) -> LevelUiNodeRecord {
    LevelUiNodeRecord {
        parent: Some(UiNodeIndex::new(0)),
        kind: LevelUiNodeKind::Label,
        x: -(width as i16) / 2,
        y,
        width,
        height: 16,
        color,
        background: [0, 0, 0],
        accent: [0, 0, 0],
        value: LevelUiValueBinding::ConstantQ12(0),
        max: LevelUiValueBinding::ConstantQ12(1),
        texture_asset: AssetId(u16::MAX),
        text,
        tag: "",
        action: LevelUiAction::Back,
        option: UI_OPTION_NONE,
        flags: ANCHOR_CENTER | ALIGN_CENTER,
        font,
    }
}

/// A focusable, centre-anchored button with a centred caption.
const fn button(y: i16, text: &'static str, action: LevelUiAction) -> LevelUiNodeRecord {
    LevelUiNodeRecord {
        parent: Some(UiNodeIndex::new(0)),
        kind: LevelUiNodeKind::Button,
        x: -(BUTTON_W as i16) / 2,
        y,
        width: BUTTON_W,
        height: BUTTON_H,
        color: [40, 48, 64],
        background: [0, 0, 0],
        accent: [236, 240, 248],
        value: LevelUiValueBinding::ConstantQ12(0),
        max: LevelUiValueBinding::ConstantQ12(1),
        texture_asset: AssetId(u16::MAX),
        text,
        tag: "",
        action,
        option: UI_OPTION_NONE,
        flags: ANCHOR_CENTER | ALIGN_CENTER,
        font: 0,
    }
}

/// Shared node pool. Title scene occupies `[0, 5)`; options scene `[5, 7)`.
///
/// Buttons stack downward from above centre; `y` is relative to the canvas
/// centre because of [`ANCHOR_CENTER`].
pub static NODES: &[LevelUiNodeRecord] = &[
    // --- Title scene (nodes 0..5) ---
    CANVAS,
    label(-70, 200, "PSOXIDE", [248, 224, 96], TITLE_FONT),
    button(-20, "PLAY", LevelUiAction::StartGameplay),
    button(10, "OPTIONS", LevelUiAction::GotoScene { scene: 2 }),
    button(40, "QUIT", LevelUiAction::Game { id: QUIT_ACTION_ID }),
    // --- Options scene (nodes 5..7) ---
    CANVAS,
    button(40, "BACK", LevelUiAction::Back),
];

/// Game-action id fired by the Quit button. The host `Scene` decides what
/// "quit" means (return to a launcher, power-off screen, ...); the menu engine
/// just forwards the id.
pub const QUIT_ACTION_ID: u16 = 1;

/// Scene table: maps scene ids to their slice of [`NODES`].
pub static SCENES: &[LevelUiScene] = &[
    LevelUiScene {
        id: 1,
        name: "title",
        node_first: 0,
        node_count: 5,
    },
    LevelUiScene {
        id: 2,
        name: "options",
        node_first: 5,
        node_count: 2,
    },
];

/// Flow graph. Entry state shows the title scene; the Options button jumps to
/// the options scene; Play enters gameplay.
pub static FLOW: GameFlow = GameFlow {
    states: &[
        FlowState::UiScene { scene: 1 },
        FlowState::UiScene { scene: 2 },
        FlowState::Gameplay,
    ],
    entry: 0,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{SimTick, VideoHz, VisualFrame};
    use crate::game_app::GameApp;
    use crate::scene::{Ctx, Scene};
    use psx_gpu::framebuf::FrameBuffer;
    use psx_pad::{button, ButtonState, PadState};

    /// Minimal gameplay scene: records whether `init` ran, so the "Play
    /// enters gameplay" path is observable without a GPU.
    #[derive(Default)]
    struct DummyScene {
        inited: bool,
    }
    impl Scene for DummyScene {
        fn init(&mut self, _ctx: &mut Ctx) {
            self.inited = true;
        }
        fn update(&mut self, _ctx: &mut Ctx) {}
        fn render(&mut self, _ctx: &mut Ctx) {}
    }

    fn test_ctx() -> Ctx {
        Ctx {
            sim_tick: SimTick::ZERO,
            visual_frame: VisualFrame::ZERO,
            video_hz: VideoHz::NTSC,
            pad: PadState::NONE,
            pad_prev: PadState::NONE,
            fb: FrameBuffer::new(320, 240),
        }
    }

    fn press(ctx: &mut Ctx, b: u16) {
        ctx.pad_prev = PadState::NONE;
        ctx.pad.buttons = ButtonState::from_bits(b);
    }

    // --- Structural validity: the authored data is internally consistent ---

    #[test]
    fn scene_node_ranges_are_in_bounds_and_cover_the_pool() {
        let mut covered = 0usize;
        for scene in SCENES {
            let end = scene.node_first as usize + scene.node_count as usize;
            assert!(end <= NODES.len(), "scene {} runs past the pool", scene.id);
            covered += scene.node_count as usize;
        }
        // Title (5) + options (2) = the whole 7-node pool.
        assert_eq!(covered, NODES.len());
    }

    #[test]
    fn title_label_uses_the_title_font_buttons_use_the_body_font() {
        // The title label is node 1; it requests the large title font, while
        // the buttons (nodes 2..5) keep the default body font. This is the
        // multi-font path: one scene, two fonts.
        assert_eq!(NODES[1].kind, LevelUiNodeKind::Label);
        assert_eq!(NODES[1].font, TITLE_FONT);
        for button in &NODES[2..5] {
            assert_eq!(button.kind, LevelUiNodeKind::Button);
            assert_eq!(button.font, BODY_FONT);
        }
    }

    #[test]
    fn title_scene_has_three_focusable_buttons() {
        let title = &SCENES[0];
        let first = title.node_first as usize;
        let end = first + title.node_count as usize;
        let buttons = NODES[first..end]
            .iter()
            .filter(|n| crate::ui::is_focusable(n.kind))
            .count();
        assert_eq!(buttons, 3, "Play / Options / Quit");
    }

    // --- Behavioural: the real GameApp drives the authored data correctly ---

    #[test]
    fn focus_seeds_to_play_and_dpad_walks_the_buttons() {
        let mut scene = DummyScene::default();
        let mut app = GameApp::new(&FLOW, SCENES, NODES, &[], &mut scene);
        let mut ctx = test_ctx();
        app.init(&mut ctx);

        // First idle update seeds focus to the top button (Play, node 2).
        app.update(&mut ctx);
        assert_eq!(app.cursor.focused_node(), Some(2usize));

        // Down walks Play -> Options -> Quit, then clamps at Quit.
        press(&mut ctx, button::DOWN);
        app.update(&mut ctx);
        assert_eq!(app.cursor.focused_node(), Some(3usize));
        press(&mut ctx, button::DOWN);
        app.update(&mut ctx);
        assert_eq!(app.cursor.focused_node(), Some(4usize));
        press(&mut ctx, button::DOWN);
        app.update(&mut ctx);
        assert_eq!(
            app.cursor.focused_node(),
            Some(4usize),
            "clamps at the last button"
        );
    }

    #[test]
    fn cross_on_play_enters_gameplay() {
        let mut scene = DummyScene::default();
        let mut app = GameApp::new(&FLOW, SCENES, NODES, &[], &mut scene);
        let mut ctx = test_ctx();
        app.init(&mut ctx);

        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert!(app.gameplay.inited, "Play (StartGameplay) inits the scene");
    }
}
