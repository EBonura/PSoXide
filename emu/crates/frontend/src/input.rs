//! Host keyboard and gamepad routing.
//!
//! The router discovers every connected controller, assigns host devices to
//! either PlayStation controller port, and emits both an all-device summary for
//! UI/freecam shortcuts and independent guest samples for ports 1 and 2.

use std::collections::BTreeMap;

use emulator_core::{button, Bus};

#[cfg(not(target_arch = "wasm32"))]
use gilrs::{Axis, Button, Event, EventType, GamepadId, Gilrs};
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;

/// Stable identifier used by the routing panel for keyboard input.
pub(crate) const KEYBOARD_DEVICE_ID: &str = "keyboard";

/// Left-stick deadzone for the digital D-pad proxy path.
const STICK_DEADZONE: f32 = 0.3;
/// Analog trigger activation threshold on native gamepads.
#[cfg(not(target_arch = "wasm32"))]
const TRIGGER_THRESHOLD: f32 = 0.5;
/// The Select+Start menu chord.
const CHORD_MASK: u16 = button::SELECT | button::START;

/// A host device's guest-controller destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PsxPort {
    /// Device is visible to the emulator UI but not to the guest.
    #[default]
    Off,
    /// PlayStation controller port 1.
    One,
    /// PlayStation controller port 2.
    Two,
}

impl PsxPort {
    /// Compact label used by the controller panel.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::One => "Port 1",
            Self::Two => "Port 2",
        }
    }
}

/// One row in the controller-routing panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputDeviceInfo {
    /// Stable router identifier.
    pub(crate) id: String,
    /// Host-provided display name.
    pub(crate) name: String,
    /// Current guest port assignment.
    pub(crate) port: PsxPort,
    /// Whether the guest sees an Analog DualShock instead of an original pad.
    pub(crate) analog: bool,
}

#[derive(Clone, Debug)]
struct RoutingEntry {
    name: String,
    port: PsxPort,
    analog: bool,
}

/// Host-device assignments shared by the native and browser backends.
#[derive(Clone, Debug)]
struct RoutingTable {
    devices: BTreeMap<String, RoutingEntry>,
    generation: u64,
}

impl Default for RoutingTable {
    fn default() -> Self {
        let mut devices = BTreeMap::new();
        devices.insert(
            KEYBOARD_DEVICE_ID.to_string(),
            RoutingEntry {
                name: "Keyboard".to_string(),
                port: PsxPort::One,
                analog: false,
            },
        );
        Self {
            devices,
            generation: 1,
        }
    }
}

impl RoutingTable {
    fn connect(&mut self, id: String, name: String) {
        if let Some(existing) = self.devices.get_mut(&id) {
            existing.name = name;
            return;
        }

        let controller_count = self
            .devices
            .keys()
            .filter(|device_id| device_id.as_str() != KEYBOARD_DEVICE_ID)
            .count();
        let port = if controller_count == 0 {
            PsxPort::One
        } else if !self.port_occupied(PsxPort::Two) {
            PsxPort::Two
        } else {
            PsxPort::Off
        };
        if port != PsxPort::Off {
            self.clear_port(port, &id);
        }
        self.devices.insert(
            id,
            RoutingEntry {
                name,
                port,
                // Modern host controllers should appear as a DualShock in
                // analog mode. Guests such as Quake, VoXide, and HL-PSX use
                // both sticks immediately; the routing panel still permits an
                // explicit original-digital-pad profile for early games.
                analog: true,
            },
        );
        self.bump();
    }

    fn disconnect(&mut self, id: &str) {
        if id == KEYBOARD_DEVICE_ID || self.devices.remove(id).is_none() {
            return;
        }
        if !self.port_occupied(PsxPort::One) {
            if let Some(keyboard) = self.devices.get_mut(KEYBOARD_DEVICE_ID) {
                keyboard.port = PsxPort::One;
            }
        }
        self.bump();
    }

    fn set_port(&mut self, id: &str, port: PsxPort) -> bool {
        let Some(previous) = self.devices.get(id).map(|entry| entry.port) else {
            return false;
        };
        if previous == port {
            return false;
        }
        if port != PsxPort::Off {
            self.clear_port(port, id);
        }
        if let Some(entry) = self.devices.get_mut(id) {
            entry.port = port;
        }
        self.bump();
        true
    }

    fn set_analog(&mut self, id: &str, analog: bool) -> bool {
        let Some(entry) = self.devices.get_mut(id) else {
            return false;
        };
        if entry.analog == analog {
            return false;
        }
        entry.analog = analog;
        self.bump();
        true
    }

    fn toggle_analog(&mut self, id: &str) -> bool {
        let Some(entry) = self.devices.get_mut(id) else {
            return false;
        };
        entry.analog = !entry.analog;
        let analog = entry.analog;
        self.bump();
        analog
    }

    fn port_for(&self, id: &str) -> PsxPort {
        self.devices
            .get(id)
            .map(|entry| entry.port)
            .unwrap_or(PsxPort::Off)
    }

    fn keyboard_port(&self) -> PsxPort {
        self.port_for(KEYBOARD_DEVICE_ID)
    }

    fn profile_for(&self, port: PsxPort) -> Option<bool> {
        self.devices
            .values()
            .find(|entry| entry.port == port)
            .map(|entry| entry.analog)
    }

    fn port_occupied(&self, port: PsxPort) -> bool {
        self.devices.values().any(|entry| entry.port == port)
    }

    fn clear_port(&mut self, port: PsxPort, except_id: &str) {
        for (id, entry) in &mut self.devices {
            if id != except_id && entry.port == port {
                entry.port = PsxPort::Off;
            }
        }
    }

    fn devices(&self) -> Vec<InputDeviceInfo> {
        self.devices
            .iter()
            .map(|(id, entry)| InputDeviceInfo {
                id: id.clone(),
                name: entry.name.clone(),
                port: entry.port,
                analog: entry.analog,
            })
            .collect()
    }

    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Buttons and sticks routed to one guest port this frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RoutedPadInput {
    /// Held PlayStation button bits.
    pub(crate) mask: u16,
    /// Left stick in host coordinates.
    pub(crate) left_stick: (f32, f32),
    /// Right stick in host coordinates.
    pub(crate) right_stick: (f32, f32),
}

impl RoutedPadInput {
    fn merge(&mut self, mask: u16, left_stick: (f32, f32), right_stick: (f32, f32)) {
        self.mask |= mask;
        if self.left_stick == (0.0, 0.0) {
            self.left_stick = left_stick;
        }
        if self.right_stick == (0.0, 0.0) {
            self.right_stick = right_stick;
        }
    }
}

/// Gamepad hotplug notification surfaced to the frontend toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputNotice {
    /// A controller became available.
    Connected {
        /// User-facing controller name.
        name: String,
        /// Mapping source summary.
        mapping: String,
    },
    /// A previously tracked controller disappeared.
    Disconnected {
        /// User-facing name captured before disconnect.
        name: String,
    },
}

impl InputNotice {
    /// Short status-line text for the frontend toast.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Connected { name, mapping } => format!("Gamepad connected: {name} ({mapping})"),
            Self::Disconnected { name } => format!("Gamepad disconnected: {name}"),
        }
    }
}

/// Per-frame gamepad summary returned by [`InputRouter::poll`].
#[derive(Clone, Debug, Default)]
pub(crate) struct InputFrame {
    /// Hotplug notices drained this frame.
    pub(crate) notices: Vec<InputNotice>,
    /// All connected gamepads merged for menu and freecam shortcuts.
    pub(crate) pad1_mask: u16,
    /// Routed guest input for controller port 1.
    pub(crate) port1: RoutedPadInput,
    /// Routed guest input for controller port 2.
    pub(crate) port2: RoutedPadInput,
    /// Rising edges of the all-device merged mask.
    pub(crate) pressed_mask: u16,
    /// Rising edge of Select+Start on any controller.
    pub(crate) toggle_menu: bool,
    /// Rising edge of a host Analog/Mode button.
    pub(crate) analog_button: bool,
    /// Menu navigation edge.
    pub(crate) menu_up: bool,
    /// Menu navigation edge.
    pub(crate) menu_down: bool,
    /// Menu navigation edge.
    pub(crate) menu_left: bool,
    /// Menu navigation edge.
    pub(crate) menu_right: bool,
    /// Menu confirmation edge.
    pub(crate) menu_confirm: bool,
    /// Menu cancellation edge.
    pub(crate) menu_back: bool,
    /// First active controller's left stick, for freecam.
    pub(crate) left_stick: (f32, f32),
    /// First active controller's right stick, for freecam.
    pub(crate) right_stick: (f32, f32),
}

fn rising_edges(previous: u16, now: u16) -> u16 {
    now & !previous
}

fn finish_frame(
    frame: &mut InputFrame,
    previous_mask: &mut u16,
    previous_chord: &mut bool,
    mut mask: u16,
) {
    let raw_mask = mask;
    let chord = (mask & CHORD_MASK) == CHORD_MASK;
    frame.toggle_menu = chord && !*previous_chord;
    if chord {
        mask &= !CHORD_MASK;
        frame.port1.mask &= !CHORD_MASK;
        frame.port2.mask &= !CHORD_MASK;
    }
    frame.pressed_mask = rising_edges(*previous_mask, raw_mask);
    frame.menu_up = frame.pressed_mask & button::UP != 0;
    frame.menu_down = frame.pressed_mask & button::DOWN != 0;
    frame.menu_left = frame.pressed_mask & button::LEFT != 0;
    frame.menu_right = frame.pressed_mask & button::RIGHT != 0;
    frame.menu_confirm = frame.pressed_mask & button::CROSS != 0;
    frame.menu_back = frame.pressed_mask & button::CIRCLE != 0;
    frame.pad1_mask = mask;
    *previous_mask = raw_mask;
    *previous_chord = chord;
}

fn configure_port(bus: &mut Bus, port: PsxPort, analog: Option<bool>) {
    match (port, analog) {
        (PsxPort::One, None) => bus.detach_pad_port1(),
        (PsxPort::One, Some(false)) => bus.attach_original_digital_pad_port1(),
        (PsxPort::One, Some(true)) => {
            bus.attach_digital_pad_port1();
            let _ = bus.force_port1_analog_mode();
        }
        (PsxPort::Two, None) => bus.detach_pad_port2(),
        (PsxPort::Two, Some(false)) => bus.attach_original_digital_pad_port2(),
        (PsxPort::Two, Some(true)) => {
            bus.attach_digital_pad_port2();
            let _ = bus.force_port2_analog_mode();
        }
        (PsxPort::Off, _) => {}
    }
}

/// Methods that are identical for the native and browser router structs.
macro_rules! routing_api {
    () => {
        /// Whether at least one physical gamepad is connected.
        pub(crate) fn is_connected(&self) -> bool {
            !self.pads.is_empty()
        }

        /// Comma-separated connected gamepad names.
        pub(crate) fn connected_names(&self) -> String {
            let mut names: Vec<&str> = self.pads.values().map(|pad| pad.name.as_str()).collect();
            names.sort_unstable();
            names.join(", ")
        }

        /// Current rows for the controller-routing panel.
        pub(crate) fn devices(&self) -> Vec<InputDeviceInfo> {
            self.routing.devices()
        }

        /// Move a host device to Off, port 1, or port 2.
        pub(crate) fn set_device_port(&mut self, id: &str, port: PsxPort) -> bool {
            self.routing.set_port(id, port)
        }

        /// Select original Digital or Analog DualShock identity.
        pub(crate) fn set_device_analog(&mut self, id: &str, analog: bool) -> bool {
            self.routing.set_analog(id, analog)
        }

        /// Toggle the keyboard's configured controller identity.
        pub(crate) fn toggle_keyboard_analog(&mut self) -> bool {
            self.routing.toggle_analog(KEYBOARD_DEVICE_ID)
        }

        /// Port currently receiving keyboard input.
        pub(crate) fn keyboard_port(&self) -> PsxPort {
            self.routing.keyboard_port()
        }

        /// Monotonic stamp changed by any route or mode edit.
        pub(crate) fn routing_generation(&self) -> u64 {
            self.routing.generation
        }

        /// Apply current Digital/Analog controller identities to a new layout.
        pub(crate) fn apply_layout(&self, bus: &mut Bus) {
            configure_port(bus, PsxPort::One, self.routing.profile_for(PsxPort::One));
            configure_port(bus, PsxPort::Two, self.routing.profile_for(PsxPort::Two));
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
struct TrackedPad {
    name: String,
    route_id: String,
    previous_mode_button: bool,
}

/// Central native input router. One instance is polled once per frame.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct InputRouter {
    gilrs: Option<Gilrs>,
    pads: HashMap<GamepadId, TrackedPad>,
    routing: RoutingTable,
    previous_mask: u16,
    previous_chord: bool,
    pending_notices: Vec<InputNotice>,
}

#[cfg(not(target_arch = "wasm32"))]
impl InputRouter {
    /// Initialise gilrs and enumerate controllers already connected.
    pub(crate) fn new() -> Self {
        let gilrs = match Gilrs::new() {
            Ok(gilrs) => Some(gilrs),
            Err(error) => {
                eprintln!("[input] gilrs init failed: {error} - keyboard only");
                None
            }
        };
        let mut routing = RoutingTable::default();
        let mut pads = HashMap::new();
        let mut pending_notices = Vec::new();
        if let Some(gilrs) = gilrs.as_ref() {
            for (id, gamepad) in gilrs.gamepads().filter(|(_, pad)| pad.is_connected()) {
                let name = gamepad.name().to_string();
                let route_id = format!("native:{id:?}");
                let mapping = native_mapping_name(&gamepad);
                routing.connect(route_id.clone(), name.clone());
                pending_notices.push(InputNotice::Connected {
                    name: name.clone(),
                    mapping: mapping.to_string(),
                });
                pads.insert(
                    id,
                    TrackedPad {
                        name,
                        route_id,
                        previous_mode_button: false,
                    },
                );
            }
        }
        Self {
            gilrs,
            pads,
            routing,
            previous_mask: 0,
            previous_chord: false,
            pending_notices,
        }
    }

    /// Drain hotplug events and sample every connected controller.
    pub(crate) fn poll(&mut self) -> InputFrame {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return InputFrame::default();
        };
        let mut frame = InputFrame {
            notices: std::mem::take(&mut self.pending_notices),
            ..InputFrame::default()
        };

        while let Some(Event { id, event, .. }) = gilrs.next_event() {
            match event {
                EventType::Connected => {
                    let gamepad = gilrs.gamepad(id);
                    let name = gamepad.name().to_string();
                    let route_id = format!("native:{id:?}");
                    let mapping = native_mapping_name(&gamepad).to_string();
                    self.routing.connect(route_id.clone(), name.clone());
                    self.pads.insert(
                        id,
                        TrackedPad {
                            name: name.clone(),
                            route_id,
                            previous_mode_button: false,
                        },
                    );
                    frame.notices.push(InputNotice::Connected { name, mapping });
                }
                EventType::Disconnected => {
                    if let Some(tracked) = self.pads.remove(&id) {
                        self.routing.disconnect(&tracked.route_id);
                        frame
                            .notices
                            .push(InputNotice::Disconnected { name: tracked.name });
                    }
                }
                _ => {}
            }
        }

        let mut merged_mask = 0;
        let mut first_sticks = None;
        for (id, tracked) in &mut self.pads {
            let gamepad = gilrs.gamepad(*id);
            if !gamepad.is_connected() {
                continue;
            }
            let mask = sample_native_pad(&gamepad);
            let left = (
                gamepad.value(Axis::LeftStickX),
                gamepad.value(Axis::LeftStickY),
            );
            let right = (
                gamepad.value(Axis::RightStickX),
                gamepad.value(Axis::RightStickY),
            );
            merged_mask |= mask;
            first_sticks.get_or_insert((left, right));
            match self.routing.port_for(&tracked.route_id) {
                PsxPort::One => frame.port1.merge(mask, left, right),
                PsxPort::Two => frame.port2.merge(mask, left, right),
                PsxPort::Off => {}
            }

            let mode_down = gamepad.is_pressed(Button::Mode);
            if mode_down && !tracked.previous_mode_button {
                self.routing.toggle_analog(&tracked.route_id);
                frame.analog_button = true;
            }
            tracked.previous_mode_button = mode_down;
        }
        if let Some((left, right)) = first_sticks {
            frame.left_stick = left;
            frame.right_stick = right;
        }
        finish_frame(
            &mut frame,
            &mut self.previous_mask,
            &mut self.previous_chord,
            merged_mask,
        );
        frame
    }

    routing_api!();
}

#[cfg(not(target_arch = "wasm32"))]
fn native_mapping_name(gamepad: &gilrs::Gamepad<'_>) -> &'static str {
    if gamepad.mapping_source() == gilrs::MappingSource::None {
        "raw HID"
    } else {
        "SDL mapping"
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sample_native_pad(gamepad: &gilrs::Gamepad<'_>) -> u16 {
    let mut mask = 0;
    for (pressed, bit) in [
        (gamepad.is_pressed(Button::South), button::CROSS),
        (gamepad.is_pressed(Button::East), button::CIRCLE),
        (gamepad.is_pressed(Button::West), button::SQUARE),
        (gamepad.is_pressed(Button::North), button::TRIANGLE),
        (gamepad.is_pressed(Button::LeftTrigger), button::L1),
        (gamepad.is_pressed(Button::RightTrigger), button::R1),
        (
            gamepad.is_pressed(Button::LeftTrigger2)
                || gamepad.value(Axis::LeftZ) > TRIGGER_THRESHOLD,
            button::L2,
        ),
        (
            gamepad.is_pressed(Button::RightTrigger2)
                || gamepad.value(Axis::RightZ) > TRIGGER_THRESHOLD,
            button::R2,
        ),
        (gamepad.is_pressed(Button::LeftThumb), button::L3),
        (gamepad.is_pressed(Button::RightThumb), button::R3),
        (gamepad.is_pressed(Button::DPadUp), button::UP),
        (gamepad.is_pressed(Button::DPadDown), button::DOWN),
        (gamepad.is_pressed(Button::DPadLeft), button::LEFT),
        (gamepad.is_pressed(Button::DPadRight), button::RIGHT),
        (gamepad.is_pressed(Button::Select), button::SELECT),
        (gamepad.is_pressed(Button::Start), button::START),
    ] {
        if pressed {
            mask |= bit;
        }
    }
    let x = gamepad.value(Axis::LeftStickX);
    let y = gamepad.value(Axis::LeftStickY);
    add_stick_dpad(&mut mask, x, y, true);
    mask
}

#[cfg(target_arch = "wasm32")]
struct WebTrackedPad {
    name: String,
    route_id: String,
    previous_mode_button: bool,
}

/// Browser Gamepad API router.
#[cfg(target_arch = "wasm32")]
pub(crate) struct InputRouter {
    pads: BTreeMap<u32, WebTrackedPad>,
    routing: RoutingTable,
    previous_mask: u16,
    previous_chord: bool,
}

#[cfg(target_arch = "wasm32")]
impl Default for InputRouter {
    fn default() -> Self {
        Self {
            pads: BTreeMap::new(),
            routing: RoutingTable::default(),
            previous_mask: 0,
            previous_chord: false,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl InputRouter {
    /// Construct the web gamepad router.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Enumerate and sample every controller exposed by the browser.
    pub(crate) fn poll(&mut self) -> InputFrame {
        let connected = connected_web_gamepads();
        let mut frame = InputFrame::default();

        let removed: Vec<u32> = self
            .pads
            .keys()
            .filter(|index| !connected.contains_key(index))
            .copied()
            .collect();
        for index in removed {
            if let Some(tracked) = self.pads.remove(&index) {
                self.routing.disconnect(&tracked.route_id);
                frame
                    .notices
                    .push(InputNotice::Disconnected { name: tracked.name });
            }
        }
        for (index, gamepad) in &connected {
            if !self.pads.contains_key(index) {
                let name = gamepad.id();
                let route_id = format!("web:{index}");
                self.routing.connect(route_id.clone(), name.clone());
                self.pads.insert(
                    *index,
                    WebTrackedPad {
                        name: name.clone(),
                        route_id,
                        previous_mode_button: false,
                    },
                );
                frame.notices.push(InputNotice::Connected {
                    name,
                    mapping: "web standard mapping".to_string(),
                });
            }
        }

        let mut merged_mask = 0;
        let mut first_sticks = None;
        for (index, gamepad) in connected {
            let Some(tracked) = self.pads.get_mut(&index) else {
                continue;
            };
            let (mask, left, right, mode_down) = sample_web_pad(&gamepad);
            merged_mask |= mask;
            first_sticks.get_or_insert((left, right));
            match self.routing.port_for(&tracked.route_id) {
                PsxPort::One => frame.port1.merge(mask, left, right),
                PsxPort::Two => frame.port2.merge(mask, left, right),
                PsxPort::Off => {}
            }
            if mode_down && !tracked.previous_mode_button {
                self.routing.toggle_analog(&tracked.route_id);
                frame.analog_button = true;
            }
            tracked.previous_mode_button = mode_down;
        }
        if let Some((left, right)) = first_sticks {
            frame.left_stick = left;
            frame.right_stick = right;
        }
        finish_frame(
            &mut frame,
            &mut self.previous_mask,
            &mut self.previous_chord,
            merged_mask,
        );
        frame
    }

    routing_api!();
}

#[cfg(target_arch = "wasm32")]
fn connected_web_gamepads() -> BTreeMap<u32, web_sys::Gamepad> {
    use wasm_bindgen::JsCast;

    let mut connected = BTreeMap::new();
    let Some(window) = web_sys::window() else {
        return connected;
    };
    let Ok(gamepads) = window.navigator().get_gamepads() else {
        return connected;
    };
    for slot in 0..gamepads.length() {
        let value = gamepads.get(slot);
        if value.is_null() || value.is_undefined() {
            continue;
        }
        let gamepad: web_sys::Gamepad = value.unchecked_into();
        if gamepad.connected() {
            connected.insert(gamepad.index(), gamepad);
        }
    }
    connected
}

#[cfg(target_arch = "wasm32")]
fn sample_web_pad(gamepad: &web_sys::Gamepad) -> (u16, (f32, f32), (f32, f32), bool) {
    use wasm_bindgen::JsCast;

    let buttons = gamepad.buttons();
    let pressed = |index: u32| {
        let value = buttons.get(index);
        !value.is_null()
            && !value.is_undefined()
            && value.unchecked_into::<web_sys::GamepadButton>().pressed()
    };
    let mut mask = 0;
    for (index, bit) in [
        (0, button::CROSS),
        (1, button::CIRCLE),
        (2, button::SQUARE),
        (3, button::TRIANGLE),
        (4, button::L1),
        (5, button::R1),
        (6, button::L2),
        (7, button::R2),
        (8, button::SELECT),
        (9, button::START),
        (10, button::L3),
        (11, button::R3),
        (12, button::UP),
        (13, button::DOWN),
        (14, button::LEFT),
        (15, button::RIGHT),
    ] {
        if pressed(index) {
            mask |= bit;
        }
    }
    let axes = gamepad.axes();
    let axis = |index: u32| -> f32 {
        let value = axes.get(index).as_f64().unwrap_or(0.0) as f32;
        if value.abs() < 0.15 {
            0.0
        } else {
            value
        }
    };
    let left = (axis(0), -axis(1));
    let right = (axis(2), -axis(3));
    add_stick_dpad(&mut mask, left.0, left.1, true);
    (mask, left, right, pressed(16))
}

fn add_stick_dpad(mask: &mut u16, x: f32, y: f32, positive_y_is_up: bool) {
    let y = if positive_y_is_up { y } else { -y };
    if y > STICK_DEADZONE {
        *mask |= button::UP;
    }
    if y < -STICK_DEADZONE {
        *mask |= button::DOWN;
    }
    if x < -STICK_DEADZONE {
        *mask |= button::LEFT;
    }
    if x > STICK_DEADZONE {
        *mask |= button::RIGHT;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rising_edges_reports_only_new_presses() {
        assert_eq!(
            rising_edges(0, button::LEFT | button::CROSS),
            button::LEFT | button::CROSS
        );
        assert_eq!(rising_edges(button::LEFT, button::LEFT), 0);
        assert_eq!(rising_edges(button::LEFT, 0), 0);
        assert_eq!(
            rising_edges(button::LEFT, button::LEFT | button::RIGHT),
            button::RIGHT
        );
    }

    #[test]
    fn first_controller_takes_port_one_and_second_takes_port_two() {
        let mut routes = RoutingTable::default();
        routes.connect("pad-a".to_string(), "Pad A".to_string());
        assert_eq!(routes.port_for(KEYBOARD_DEVICE_ID), PsxPort::Off);
        assert_eq!(routes.port_for("pad-a"), PsxPort::One);
        assert_eq!(routes.profile_for(PsxPort::One), Some(true));

        routes.connect("pad-b".to_string(), "Pad B".to_string());
        assert_eq!(routes.port_for("pad-b"), PsxPort::Two);
    }

    #[test]
    fn assignments_are_unique_and_disconnect_restores_keyboard() {
        let mut routes = RoutingTable::default();
        routes.connect("pad-a".to_string(), "Pad A".to_string());
        routes.connect("pad-b".to_string(), "Pad B".to_string());
        assert!(routes.set_port("pad-b", PsxPort::One));
        assert_eq!(routes.port_for("pad-a"), PsxPort::Off);
        assert_eq!(routes.port_for("pad-b"), PsxPort::One);
        routes.disconnect("pad-b");
        assert_eq!(routes.port_for(KEYBOARD_DEVICE_ID), PsxPort::One);
    }

    #[test]
    fn analog_choice_is_part_of_the_port_profile() {
        let mut routes = RoutingTable::default();
        assert_eq!(routes.profile_for(PsxPort::One), Some(false));
        assert!(routes.set_analog(KEYBOARD_DEVICE_ID, true));
        assert_eq!(routes.profile_for(PsxPort::One), Some(true));
    }

    #[test]
    fn controller_profiles_rebuild_both_bus_ports() {
        use emulator_core::pad::PadMode;

        let mut bus = Bus::new_without_bios();
        configure_port(&mut bus, PsxPort::One, Some(false));
        configure_port(&mut bus, PsxPort::Two, Some(true));
        assert_eq!(bus.port1_pad_mode(), Some(PadMode::Digital));
        assert_eq!(bus.port2_pad_mode(), Some(PadMode::Analog));

        configure_port(&mut bus, PsxPort::One, None);
        configure_port(&mut bus, PsxPort::Two, None);
        assert_eq!(bus.port1_pad_mode(), None);
        assert_eq!(bus.port2_pad_mode(), None);
    }
}
