//! What a menu holds, as plain data: the overlay decides it (`overlay::menu_bar`) and each platform
//! draws it, AppKit on macOS (`StatusMenu`) and the overlay's own popup on Windows (`overlay::tray_menu`).

/// A line of the menu. `C` is what choosing an item means to the caller.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuEntry<C> {
    Item { title: String, command: C, enabled: bool, checked: bool, key: Option<MenuKey> },
    /// Greyed-out text that can't be chosen, such as a status line.
    Label(String),
    /// A group's title inside a list.
    Header(String),
    Submenu { title: String, entries: Vec<MenuEntry<C>> },
    Separator,
}

impl<C> MenuEntry<C> {
    /// An item that can be chosen, unchecked and without a shortcut.
    pub fn item(title: impl Into<String>, command: C) -> Self { Self::Item { title: title.into(), command, enabled: true, checked: false, key: None } }

    pub fn key(mut self, shortcut: MenuKey) -> Self {
        if let Self::Item { key, .. } = &mut self { *key = Some(shortcut); }
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        if let Self::Item { checked, .. } = &mut self { *checked = on; }
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        if let Self::Item { enabled, .. } = &mut self { *enabled = on; }
        self
    }
}

/// A shortcut shown beside an item. The menu bar's menu only answers to it while it is open; the
/// app's own shortcuts work the same as always.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuKey { pub key: &'static str, pub command: bool, pub shift: bool, pub option: bool, pub control: bool }

impl MenuKey {
    /// The shortcut as Windows writes it, like Settings › Keys does ("Ctrl \", "Ctrl Shift ↵").
    #[cfg(not(target_os = "macos"))]
    pub fn label(&self) -> String {
        let key = match self.key { "\r" => "↵", " " => "Space", key => key };
        [(self.control, "Ctrl"), (self.option, "Alt"), (self.shift, "Shift"), (self.command, "Win")].into_iter()
            .filter_map(|(on, name)| on.then_some(name)).chain([key]).collect::<Vec<_>>().join(" ")
    }
}
