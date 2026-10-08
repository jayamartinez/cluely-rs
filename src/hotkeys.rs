//! Global shortcuts. Registered on the GPUI main thread so its Win32 message loop
//! also pumps the hidden hotkey window; presses are forwarded through a channel.

use std::collections::HashMap;

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Assist,
    Live,
    /// Jump into the composer to type without clicking; pressed while typing there, jump back out.
    Focus,
    Toggle,
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    ScrollUp,
    ScrollDown,
    /// Close settings.
    Close,
}

impl Action {
    /// Movement and scrolling shadow common editor shortcuts, so they are only
    /// claimed while the overlay is on screen.
    pub fn only_while_visible(self) -> bool {
        matches!(self, Self::MoveUp | Self::MoveDown | Self::MoveLeft | Self::MoveRight | Self::ScrollUp | Self::ScrollDown)
    }

    /// The overlay never takes keyboard focus, so Esc must be global — but only while a
    /// panel is open and on screen, so it never swallows Esc in other apps otherwise.
    pub fn only_while_panel(self) -> bool { matches!(self, Self::Close) }

    /// Ctrl+Enter sends messages in many apps, so Assist is only claimed during a Live session.
    pub fn only_while_live(self) -> bool { matches!(self, Self::Assist) }

    #[cfg(target_os = "macos")]
    fn arrow(self) -> Option<crate::platform::Arrow> {
        use crate::platform::Arrow;
        match self {
            Self::MoveUp | Self::ScrollUp => Some(Arrow::Up),
            Self::MoveDown | Self::ScrollDown => Some(Arrow::Down),
            Self::MoveLeft => Some(Arrow::Left),
            Self::MoveRight => Some(Arrow::Right),
            _ => None,
        }
    }

    fn always(self) -> bool { !self.only_while_visible() && !self.only_while_panel() && !self.only_while_live() }
}

#[cfg(not(target_os = "macos"))]
pub const DEFAULTS: &[(Action, &str)] = &[
    (Action::Assist, "control+Enter"),
    (Action::Live, "control+shift+Enter"),
    (Action::Focus, "control+shift+Space"),
    (Action::Toggle, "control+Backslash"),
    (Action::MoveUp, "control+alt+ArrowUp"),
    (Action::MoveDown, "control+alt+ArrowDown"),
    (Action::MoveLeft, "control+alt+ArrowLeft"),
    (Action::MoveRight, "control+alt+ArrowRight"),
    (Action::ScrollUp, "control+alt+shift+ArrowUp"),
    (Action::ScrollDown, "control+alt+shift+ArrowDown"),
    (Action::Close, "Escape"),
];

/// macOS: Command for CluelyRS's own chords, as Mac apps use it; moving and scrolling keep
/// Control+Option, which Mac apps rarely claim.
#[cfg(target_os = "macos")]
pub const DEFAULTS: &[(Action, &str)] = &[
    (Action::Assist, "super+Enter"),
    (Action::Live, "super+shift+Enter"),
    (Action::Focus, "super+shift+Space"),
    (Action::Toggle, "super+Backslash"),
    (Action::MoveUp, "control+alt+ArrowUp"),
    (Action::MoveDown, "control+alt+ArrowDown"),
    (Action::MoveLeft, "control+alt+ArrowLeft"),
    (Action::MoveRight, "control+alt+ArrowRight"),
    (Action::ScrollUp, "control+alt+shift+ArrowUp"),
    (Action::ScrollDown, "control+alt+shift+ArrowDown"),
    (Action::Close, "Escape"),
];

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    bindings: Vec<(Action, HotKey)>,
    by_id: HashMap<u32, Action>,
    visible_registered: bool,
    panel_registered: bool,
    live_registered: bool,
    /// Shortcuts another application already owns; reported instead of failing startup.
    pub unavailable: Vec<Action>,
}

impl Hotkeys {
    pub fn new() -> anyhow::Result<(Self, UnboundedReceiver<Action>)> {
        let manager = GlobalHotKeyManager::new()?;
        let mut bindings = Vec::new();
        for (action, accelerator) in DEFAULTS {
            bindings.push((*action, accelerator.parse::<HotKey>()?));
        }
        let by_id: HashMap<u32, Action> = bindings.iter().map(|(action, key)| (key.id(), *action)).collect();
        let (sender, receiver) = unbounded();
        let lookup = by_id.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            // macOS tracks held movement from these events rather than the live keyboard state.
            #[cfg(target_os = "macos")]
            if let Some(arrow) = lookup.get(&event.id).and_then(|action| action.arrow()) {
                crate::platform::set_arrow_held(arrow, event.state == HotKeyState::Pressed);
            }
            if event.state == HotKeyState::Pressed && let Some(action) = lookup.get(&event.id) {
                let _ = sender.unbounded_send(*action);
            }
        }));
        let mut hotkeys = Self { manager, bindings, by_id, visible_registered: false, panel_registered: false, live_registered: false, unavailable: Vec::new() };
        hotkeys.register(Action::always);
        Ok((hotkeys, receiver))
    }

    fn register(&mut self, include: impl Fn(Action) -> bool) {
        for (action, key) in &self.bindings {
            if include(*action) && self.manager.register(*key).is_err() && !self.unavailable.contains(action) {
                self.unavailable.push(*action);
            }
        }
    }

    pub fn set_overlay_visible(&mut self, visible: bool) {
        if visible == self.visible_registered { return; }
        self.visible_registered = visible;
        self.set_group(Action::only_while_visible, visible);
    }

    pub fn set_live(&mut self, live: bool) {
        if live == self.live_registered { return; }
        self.live_registered = live;
        self.set_group(Action::only_while_live, live);
    }

    pub fn set_panel_open(&mut self, open: bool) {
        if open == self.panel_registered { return; }
        self.panel_registered = open;
        self.set_group(Action::only_while_panel, open);
    }

    fn set_group(&mut self, member: fn(Action) -> bool, on: bool) {
        if on {
            self.register(member);
        } else {
            for (action, key) in &self.bindings {
                if member(*action) { let _ = self.manager.unregister(*key); }
            }
        }
    }

    pub fn label(&self, action: Action) -> String {
        DEFAULTS.iter().find(|(item, _)| *item == action).map(|(_, text)| pretty(text)).unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn action(&self, id: u32) -> Option<Action> { self.by_id.get(&id).copied() }
}

#[cfg(not(target_os = "macos"))]
fn pretty(accelerator: &str) -> String {
    accelerator.split('+').map(|part| match part {
        "control" => "Ctrl", "alt" => "Alt", "shift" => "Shift", "Enter" => "↵", "Backslash" => "\\",
        "ArrowUp" => "↑", "ArrowDown" => "↓", "ArrowLeft" => "←", "ArrowRight" => "→", "Escape" => "Esc", other => other,
    }).collect::<Vec<_>>().join(" ")
}

/// Mac menus write chords as symbols: "⌘ ⇧ ↵" (spaces separate the keys; `ui::keys` draws them).
#[cfg(target_os = "macos")]
fn pretty(accelerator: &str) -> String {
    accelerator.split('+').map(|part| match part {
        "super" => "⌘", "control" => "⌃", "alt" => "⌥", "shift" => "⇧", "Enter" => "↵", "Backslash" => "\\",
        "ArrowUp" => "↑", "ArrowDown" => "↓", "ArrowLeft" => "←", "ArrowRight" => "→", "Escape" => "Esc", other => other,
    }).collect::<Vec<_>>().join(" ")
}
