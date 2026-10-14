//! CluelyRS's own menu, from the menu bar icon on macOS and the tray icon on Windows, laid out like
//! the Paper artboards "macOS · Menu bar icon, open while Live is on" and "Tray v2 · Windows · 2
//! Right-click menu": what Live is doing, the overlay, Live, the modes, Settings and Sessions, the
//! icon itself (and the Dock and setup on macOS), updates and Quit. macOS draws it with AppKit
//! (`platform::StatusMenu`), Windows in popups of the overlay's own (`tray_menu`); this decides what
//! it holds and carries out what is chosen. Chosen items reach the overlay through a channel, as the
//! global shortcuts do (`hotkeys`).

#[cfg(target_os = "macos")]
use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::rc::Rc;
use std::time::Instant;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{Context, Window};

use super::Overlay;
use crate::hotkeys::{Action, DEFAULTS};
use crate::modes::ModeStore;
use crate::platform::{self, MenuEntry, MenuKey};
#[cfg(target_os = "macos")]
use crate::platform::{Alert, StatusMenu};
use crate::settings_view::Tab;

/// What a menu item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Command {
    ToggleOverlay, ToggleLive, Mode(String), EditModes, Settings, Sessions,
    #[cfg(target_os = "macos")]
    ToggleDock,
    HideIcon, CheckForUpdates,
    #[cfg(target_os = "macos")]
    RunSetup,
    Quit,
}

pub(super) type Entry = MenuEntry<Command>;

/// The modes as the switcher lists them: each group's title (none for General) and its modes' ids
/// and names.
type Sections = Vec<(Option<String>, Vec<(String, String)>)>;

/// What the menu shows, copied from the overlay: AppKit builds the menu as it opens, outside any
/// GPUI update, so it can't read the overlay itself (Windows copies it as the menu opens).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct MenuState {
    live_since: Option<Instant>,
    modes: Sections,
    active_mode: String,
    #[cfg(target_os = "macos")]
    show_in_dock: bool,
    /// Whether there is a data folder to keep sessions in.
    sessions: bool,
    /// The Show / hide overlay shortcut as Settings writes it ("⌘ \", "Ctrl \"), and whether another app owns it.
    toggle_label: String,
    toggle_unavailable: bool,
}

/// macOS: the menu bar icon, its menu and what the menu shows.
#[cfg(target_os = "macos")]
pub(super) struct MenuBar {
    state: Rc<RefCell<MenuState>>,
    menu: StatusMenu,
    commands: UnboundedSender<Command>,
    /// Shown though Settings hides it: at launch another app owned the Show / hide shortcut and
    /// CluelyRS wasn't in the Dock, so the icon was the only way back. "Hide Anyway" clears it.
    rescued: bool,
}

/// Windows: the tray icon. Its menu is drawn when it opens (`tray_menu`), from the overlay as it is then.
#[cfg(windows)]
pub(super) struct MenuBar {
    icon: platform::TrayIcon,
    pub(super) commands: UnboundedSender<Command>,
}

impl Overlay {
    /// Put up the icon (unless Settings › Window turned it off) and carry out its menu's commands.
    #[cfg(target_os = "macos")]
    pub(super) fn start_menu_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (sender, commands) = unbounded();
        let state = Rc::new(RefCell::new(self.menu_state()));
        let (shown, asked, send, native) = (state.clone(), state.clone(), sender.clone(), self.native);
        let menu = StatusMenu::new(move || entries(&shown.borrow(), native.is_some_and(platform::is_visible), Instant::now()), move |command| {
            // AppKit calls this from its own handling of the menu bar click (the status bar isn't
            // a GPUI window), never inside a GPUI update, so the alert's modal loop can run right
            // here: GPUI tasks that run meanwhile find nothing borrowed. The alert is copied out
            // of the state first, so the overlay can update the state while the alert is up.
            if command == Command::HideIcon {
                let alert = hide_alert(&asked.borrow());
                if !platform::confirm(&alert) { return; }
            }
            let _ = send.unbounded_send(command);
        });
        let Some(menu) = menu else { return };
        let rescued = { let state = state.borrow(); state.toggle_unavailable && !state.show_in_dock };
        self.menu_bar = Some(MenuBar { state, menu, commands: sender, rescued });
        self.serve_menu(commands, window, cx);
    }

    /// Put up the tray icon (unless Settings › Window turned it off), open its menu on a right click
    /// and carry out the menu's commands. A left click shows or hides the overlay.
    #[cfg(windows)]
    pub(super) fn start_menu_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((icon, mut events)) = platform::TrayIcon::new() else { return };
        let (sender, commands) = unbounded();
        self.menu_bar = Some(MenuBar { icon, commands: sender });
        self.serve_menu(commands, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update_in(cx, |this, window, cx| this.tray_event(event, window, cx)).is_err() { break; }
            }
        }).detach();
    }

    fn serve_menu(&mut self, mut commands: UnboundedReceiver<Command>, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(command) = commands.next().await {
                if this.update_in(cx, |this, window, cx| this.menu_command(command, window, cx)).is_err() { break; }
            }
        }).detach();
        // Everything the menu shows changes through the overlay, which notifies when it does.
        cx.observe_self(|this, _| this.sync_menu_bar()).detach();
        self.sync_menu_bar();
    }

    /// Copy what the menu shows, and show or hide the icon and its Live dot.
    #[cfg(target_os = "macos")]
    fn sync_menu_bar(&mut self) {
        let state = self.menu_state();
        let Some(bar) = &mut self.menu_bar else { return };
        let shown = self.store.value.show_menu_bar_icon || bar.rescued;
        bar.menu.set_live(state.live_since.is_some());
        bar.menu.set_shown(shown);
        *bar.state.borrow_mut() = state;
    }

    /// Show or hide the tray icon, its Live badge, and the status its tooltip gives. The icon is
    /// never shown against Settings, even when the Show / hide shortcut is taken (unlike macOS):
    /// opening CluelyRS again always shows the overlay.
    #[cfg(windows)]
    fn sync_menu_bar(&mut self) {
        let Some(bar) = &mut self.menu_bar else { return };
        let tip = format!("CluelyRS\n{}", live_status(self.live_since, &self.modes.active().name, Instant::now()));
        bar.icon.set_live(self.live_since.is_some());
        bar.icon.set_tooltip(&tip);
        bar.icon.set_shown(self.store.value.show_menu_bar_icon);
    }

    pub(super) fn menu_state(&self) -> MenuState {
        MenuState { live_since: self.live_since, modes: mode_sections(&self.modes), active_mode: self.modes.active().id.clone(),
            #[cfg(target_os = "macos")]
            show_in_dock: self.store.value.show_in_dock,
            sessions: self.archive.is_some(),
            toggle_label: self.hotkeys.label(Action::Toggle), toggle_unavailable: self.hotkeys.unavailable.contains(&Action::Toggle) }
    }

    /// A left click shows or hides the overlay, as the Toggle shortcut does; showing also brings it
    /// back on screen, as opening CluelyRS again does. A right click opens the menu.
    #[cfg(windows)]
    fn tray_event(&mut self, event: platform::TrayEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            platform::TrayEvent::Click if self.native.is_some_and(platform::is_visible) => self.handle(Action::Toggle, window, cx),
            platform::TrayEvent::Click => self.relaunched(cx),
            platform::TrayEvent::Menu(x, y) => self.open_tray_menu((x, y), cx),
        }
    }

    fn menu_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        // During first-run setup the overlay stays hidden: anything but Quit brings setup forward.
        #[cfg(windows)]
        if command != Command::Quit && self.raise_setup(cx) { return; }
        match command {
            Command::ToggleOverlay => self.handle(Action::Toggle, window, cx),
            Command::ToggleLive => {
                let live = self.live_since.is_none();
                // Live never starts unseen: the overlay shows that it's on.
                if live { self.show(); }
                self.set_live(live, window, cx);
            }
            Command::Mode(id) => self.activate_mode(&id, cx),
            #[cfg(target_os = "macos")]
            Command::EditModes => self.open_settings(Tab::Modes, window, cx),
            #[cfg(windows)]
            Command::EditModes => self.open_modes_window(None, cx),
            Command::Settings => {
                // Windows opens Settings under the overlay, so the overlay must be on screen.
                #[cfg(windows)]
                self.show();
                self.open_settings(Tab::default(), window, cx);
            }
            Command::Sessions => self.open_sessions(cx),
            #[cfg(target_os = "macos")]
            Command::ToggleDock => self.update_settings(|s| s.show_in_dock = !s.show_in_dock, window, cx),
            // macOS asked before sending this (`start_menu_bar`).
            #[cfg(target_os = "macos")]
            Command::HideIcon => {
                if let Some(bar) = &mut self.menu_bar { bar.rescued = false; }
                self.update_settings(|s| s.show_menu_bar_icon = false, window, cx);
            }
            #[cfg(windows)]
            Command::HideIcon => self.ask_hide_tray_icon(None, cx),
            Command::CheckForUpdates => self.check_for_updates_from_menu(window, cx),
            // Disabled while Live is on, as Settings › About's button is; Live may have started since the menu opened.
            #[cfg(target_os = "macos")]
            Command::RunSetup => if self.live_since.is_none() { self.open_onboarding(window, cx) },
            // `on_app_quit` stops Live first.
            #[cfg(target_os = "macos")]
            Command::Quit => cx.quit(),
            // Asks first while Live is on, then stops Live so the session is saved.
            #[cfg(windows)]
            Command::Quit => self.quit(window, cx),
        }
    }

    /// Settings › Window › Show menu bar icon. Turning it off while CluelyRS isn't in the Dock
    /// either asks first, as the menu's "Hide Menu Bar Icon…" does. This runs inside GPUI's event
    /// handling, where a modal alert would re-enter GPUI, so the alert waits for the next turn of
    /// the main queue (`platform::confirm_later`) and its answer comes back as a menu command.
    #[cfg(target_os = "macos")]
    pub(crate) fn set_menu_bar_icon(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bar) = self.menu_bar.as_ref().filter(|_| !shown && !self.store.value.show_in_dock) {
            let commands = bar.commands.clone();
            platform::confirm_later(hide_alert(&bar.state.borrow()), move || { let _ = commands.unbounded_send(Command::HideIcon); });
            return;
        }
        self.update_settings(|s| s.show_menu_bar_icon = shown, window, cx);
    }

    /// Settings › Window › Show tray icon. Turning it off asks first, as the menu's "Hide tray icon…" does.
    #[cfg(windows)]
    pub(crate) fn set_menu_bar_icon(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        if shown { self.update_settings(|s| s.show_menu_bar_icon = true, window, cx); } else { self.ask_hide_tray_icon(None, cx); }
    }
}

fn mode_sections(store: &ModeStore) -> Sections {
    store.sections().into_iter()
        .map(|(group, modes)| (group.map(str::to_string), modes.into_iter().map(|mode| (mode.id.clone(), mode.name.clone())).collect()))
        .collect()
}

/// macOS menus capitalise each word of an item; Windows only the first ("Show overlay").
fn title(macos: &'static str, windows: &'static str) -> &'static str { if cfg!(target_os = "macos") { macos } else { windows } }

/// The menu, top to bottom. On macOS, setup changes what Live uses, so "Run Setup Again…" waits
/// until Live is off; Windows runs setup again from Settings › About.
pub(super) fn entries(state: &MenuState, overlay_visible: bool, now: Instant) -> Vec<Entry> {
    let mut overlay = MenuEntry::item(if overlay_visible { title("Hide Overlay", "Hide overlay") } else { title("Show Overlay", "Show overlay") }, Command::ToggleOverlay);
    if let Some(key) = toggle_key() { overlay = overlay.key(key); }
    let settings = MenuEntry::item("Settings…", Command::Settings);
    let quit = MenuEntry::item("Quit CluelyRS", Command::Quit);
    #[cfg(target_os = "macos")]
    let (settings, quit) = (settings.key(MenuKey { key: ",", command: true, ..MenuKey::default() }), quit.key(MenuKey { key: "q", command: true, ..MenuKey::default() }));
    let mut entries = vec![
        MenuEntry::Label(status_line(state, now)),
        MenuEntry::Separator,
        overlay,
        MenuEntry::item(if state.live_since.is_some() { "Stop Live" } else { "Start Live" }, Command::ToggleLive),
        MenuEntry::Submenu { title: "Mode".into(), entries: mode_entries(state) },
        MenuEntry::Separator,
        settings,
        MenuEntry::item("Sessions…", Command::Sessions).enabled(state.sessions),
        MenuEntry::Separator,
    ];
    #[cfg(target_os = "macos")]
    entries.push(MenuEntry::item("Show in Dock", Command::ToggleDock).checked(state.show_in_dock));
    entries.extend([
        MenuEntry::item(title("Hide Menu Bar Icon…", "Hide tray icon…"), Command::HideIcon),
        MenuEntry::Separator,
        MenuEntry::item(title("Check for Updates…", "Check for updates…"), Command::CheckForUpdates),
    ]);
    #[cfg(target_os = "macos")]
    entries.push(MenuEntry::item("Run Setup Again…", Command::RunSetup).enabled(state.live_since.is_none()));
    entries.extend([MenuEntry::Separator, quit]);
    entries
}

/// "Live · Interview · 12 min", or "Live is off".
fn status_line(state: &MenuState, now: Instant) -> String {
    let mode = state.modes.iter().flat_map(|(_, modes)| modes).find(|(id, _)| *id == state.active_mode).map_or("", |(_, name)| name.as_str());
    live_status(state.live_since, mode, now)
}

fn live_status(live_since: Option<Instant>, mode: &str, now: Instant) -> String {
    let Some(since) = live_since else { return "Live is off".into() };
    format!("Live · {mode} · {} min", now.saturating_duration_since(since).as_secs() / 60)
}

/// The Mode submenu: General, then each group under its title, a check on the active mode, and
/// "Edit Modes…" (Settings › Modes).
fn mode_entries(state: &MenuState) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (group, modes) in &state.modes {
        if let Some(group) = group { entries.push(MenuEntry::Header(group.clone())); }
        entries.extend(modes.iter().map(|(id, name)| MenuEntry::item(name.clone(), Command::Mode(id.clone())).checked(*id == state.active_mode)));
    }
    entries.extend([MenuEntry::Separator, MenuEntry::item(title("Edit Modes…", "Edit modes…"), Command::EditModes)]);
    entries
}

/// The Show / hide overlay shortcut, written beside Show Overlay.
fn toggle_key() -> Option<MenuKey> {
    DEFAULTS.iter().find(|(action, _)| *action == Action::Toggle).and_then(|(_, accelerator)| menu_key(accelerator))
}

/// A shortcut from `hotkeys::DEFAULTS` ("super+Backslash") as a menu writes it; `None` for keys
/// none of the menu's shortcuts use.
fn menu_key(accelerator: &str) -> Option<MenuKey> {
    let mut key = MenuKey::default();
    for part in accelerator.split('+') {
        match part {
            "super" => key.command = true,
            "shift" => key.shift = true,
            "alt" => key.option = true,
            "control" => key.control = true,
            "Backslash" => key.key = "\\",
            "Quote" => key.key = "'",
            "Enter" => key.key = "\r",
            "Space" => key.key = " ",
            _ => return None,
        }
    }
    (!key.key.is_empty()).then_some(key)
}

/// What "Hide Menu Bar Icon…" asks. Without the icon, the way back is the Show / hide shortcut
/// and the overlay's gear. When another app owns that shortcut and CluelyRS isn't in the Dock,
/// there would be none, so keeping the icon becomes the default.
#[cfg(target_os = "macos")]
fn hide_alert(state: &MenuState) -> Alert {
    let shortcut = &state.toggle_label;
    if state.toggle_unavailable && !state.show_in_dock {
        return Alert { title: "Keep the menu bar icon?".into(),
            message: format!("{shortcut} is used by another app and CluelyRS isn't in the Dock, so without this icon there would be no way back to Settings."),
            default_button: "Keep Icon", other_button: "Hide Anyway", default_goes_ahead: false };
    }
    Alert { title: "Hide the menu bar icon?".into(),
        message: format!("To get back to CluelyRS, press {shortcut} to show the overlay, then open Settings from its gear."),
        default_button: "Hide Icon", other_button: "Cancel", default_goes_ahead: true }
}

/// What "Hide tray icon…" asks (Paper "Tray v2 · Windows · 2 Right-click menu · Hide tray icon…"),
/// always the same one question with Hide icon as the default: the way back is the Show / hide
/// shortcut, then Settings from the overlay's gear, or opening CluelyRS again. When another app
/// owns the shortcut, a warning says so, but hiding is still the user's call.
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HideDialog {
    pub(super) title: &'static str,
    /// The Show / hide shortcut's keys, each drawn as a keycap.
    pub(super) keys: Vec<String>,
    pub(super) warning: Option<String>,
    pub(super) hide: &'static str,
    pub(super) cancel: &'static str,
}

#[cfg(windows)]
pub(super) fn hide_dialog(state: &MenuState) -> HideDialog {
    let shortcut = &state.toggle_label;
    HideDialog { title: "Hide the tray icon?", keys: shortcut.split(' ').map(str::to_string).collect(),
        warning: state.toggle_unavailable.then(|| format!("{shortcut} is in use by another app right now")), hide: "Hide icon", cancel: "Cancel" }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use global_hotkey::hotkey::{Code, HotKey, Modifiers};

    use super::*;

    fn state() -> MenuState {
        MenuState { modes: mode_sections(&ModeStore::at(None)), active_mode: "interview".into(), sessions: true, toggle_label: title("⌘ \\", "Ctrl \\").into(), ..MenuState::default() }
    }

    /// Each line as text: an item's title (✓ when checked, ⌀ when disabled), "—" for a separator.
    fn lines(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|entry| match entry {
            MenuEntry::Item { title, checked, enabled, .. } => format!("{}{title}{}", if *checked { "✓ " } else { "" }, if *enabled { "" } else { " ⌀" }),
            MenuEntry::Label(text) => format!("({text})"),
            MenuEntry::Header(title) => format!("[{title}]"),
            MenuEntry::Submenu { title, .. } => format!("{title} ▸"),
            MenuEntry::Separator => "—".into(),
        }).collect()
    }

    fn find<'a>(entries: &'a [Entry], wanted: &str) -> &'a Entry {
        entries.iter().find(|entry| matches!(entry, MenuEntry::Item { title, .. } if title == wanted)).unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_idle_menu_offers_to_show_the_overlay_and_start_live() {
        let now = Instant::now();
        let menu = entries(&state(), false, now);
        assert_eq!(lines(&menu), ["(Live is off)", "—", "Show Overlay", "Start Live", "Mode ▸", "—", "Settings…", "Sessions…", "—", "Show in Dock",
            "Hide Menu Bar Icon…", "—", "Check for Updates…", "Run Setup Again…", "—", "Quit CluelyRS"]);
        let command = |title| match find(&menu, title) { MenuEntry::Item { command, .. } => command.clone(), _ => unreachable!() };
        assert_eq!([command("Show Overlay"), command("Start Live"), command("Settings…"), command("Show in Dock"), command("Hide Menu Bar Icon…"), command("Run Setup Again…"), command("Quit CluelyRS")],
            [Command::ToggleOverlay, Command::ToggleLive, Command::Settings, Command::ToggleDock, Command::HideIcon, Command::RunSetup, Command::Quit]);
        let key = |title| match find(&menu, title) { MenuEntry::Item { key, .. } => *key, _ => unreachable!() };
        assert_eq!(key("Settings…"), Some(MenuKey { key: ",", command: true, ..MenuKey::default() }));
        assert_eq!(key("Quit CluelyRS"), Some(MenuKey { key: "q", command: true, ..MenuKey::default() }));
        assert_eq!(key("Start Live"), None, "Live has no shortcut, on purpose");
    }

    /// Windows: the same menu in Windows' sentence case, without Show in Dock or ⌘ keys.
    #[cfg(windows)]
    #[test]
    fn the_idle_tray_menu_offers_to_show_the_overlay_and_start_live() {
        let menu = entries(&state(), false, Instant::now());
        assert_eq!(lines(&menu), ["(Live is off)", "—", "Show overlay", "Start Live", "Mode ▸", "—", "Settings…", "Sessions…", "—",
            "Hide tray icon…", "—", "Check for updates…", "—", "Quit CluelyRS"]);
        let command = |title| match find(&menu, title) { MenuEntry::Item { command, .. } => command.clone(), _ => unreachable!() };
        assert_eq!([command("Show overlay"), command("Start Live"), command("Settings…"), command("Hide tray icon…"), command("Check for updates…"), command("Quit CluelyRS")],
            [Command::ToggleOverlay, Command::ToggleLive, Command::Settings, Command::HideIcon, Command::CheckForUpdates, Command::Quit]);
        let key = |title| match find(&menu, title) { MenuEntry::Item { key, .. } => *key, _ => unreachable!() };
        assert_eq!(key("Show overlay").map(|key| key.label()).as_deref(), Some("Ctrl \\"), "written as Settings › Keys writes it");
        assert!(["Settings…", "Quit CluelyRS", "Start Live"].iter().all(|title| key(title).is_none()), "only Show overlay has a global shortcut");
    }

    #[test]
    fn during_live_the_menu_says_so_and_offers_to_stop_it() {
        let now = Instant::now();
        let live = MenuState { live_since: Some(now - Duration::from_secs(12 * 60 + 59)), sessions: false, ..state() };
        let menu = entries(&live, true, now);
        assert_eq!(lines(&menu)[..5], ["(Live · Interview · 12 min)", "—", title("Hide Overlay", "Hide overlay"), "Stop Live", "Mode ▸"]);
        assert!(lines(&menu).contains(&"Sessions… ⌀".to_string()), "no data folder to keep sessions in");
        #[cfg(target_os = "macos")]
        assert!(lines(&menu).contains(&"Run Setup Again… ⌀".to_string()), "setup waits until Live is off");
        #[cfg(target_os = "macos")]
        assert!(lines(&entries(&MenuState { show_in_dock: true, ..live }, true, now)).contains(&"✓ Show in Dock".to_string()));
    }

    #[test]
    fn the_mode_submenu_groups_the_modes_and_checks_the_active_one() {
        let menu = entries(&state(), false, Instant::now());
        let MenuEntry::Submenu { entries: modes, .. } = &menu[4] else { panic!("{:?}", menu[4]) };
        let lines = lines(modes);
        assert_eq!(lines[..4], ["General", "[Looking for work]", "✓ Interview", "Behavioral Interview"]);
        assert_eq!(lines.iter().filter(|line| line.starts_with('[')).count(), 3);
        assert_eq!(lines.iter().filter(|line| line.starts_with('✓')).count(), 1);
        assert_eq!(lines[lines.len() - 2..], ["—", title("Edit Modes…", "Edit modes…")]);
        assert!(modes.contains(&MenuEntry::item("Lecture", Command::Mode("lecture".into()))));
        assert_eq!(modes.last(), Some(&MenuEntry::item(title("Edit Modes…", "Edit modes…"), Command::EditModes)));
    }

    /// Show Overlay's shortcut is the global Toggle shortcut, whatever `DEFAULTS` makes it.
    #[test]
    fn show_overlay_shows_the_toggle_shortcut() {
        let menu = entries(&state(), false, Instant::now());
        let MenuEntry::Item { key: Some(key), .. } = find(&menu, title("Show Overlay", "Show overlay")) else { panic!("Show Overlay has no shortcut") };
        let mut modifiers = Modifiers::empty();
        for (on, modifier) in [(key.command, Modifiers::SUPER), (key.shift, Modifiers::SHIFT), (key.option, Modifiers::ALT), (key.control, Modifiers::CONTROL)] {
            if on { modifiers |= modifier; }
        }
        let code = match key.key { "\\" => Code::Backslash, "'" => Code::Quote, "\r" => Code::Enter, " " => Code::Space, other => panic!("{other:?}") };
        let toggle = DEFAULTS.iter().find(|(action, _)| *action == Action::Toggle).unwrap().1;
        assert_eq!(HotKey::new(Some(modifiers), code), toggle.parse::<HotKey>().unwrap());
        assert_eq!(menu_key("control+alt+ArrowUp"), None, "keys the menu doesn't use");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hiding_the_icon_asks_and_defaults_to_keeping_it_when_there_would_be_no_way_back() {
        let usual = hide_alert(&state());
        assert_eq!((usual.title.as_str(), usual.default_button, usual.other_button, usual.default_goes_ahead), ("Hide the menu bar icon?", "Hide Icon", "Cancel", true));
        assert_eq!(usual.message, "To get back to CluelyRS, press ⌘ \\ to show the overlay, then open Settings from its gear.");
        let stuck = hide_alert(&MenuState { toggle_unavailable: true, ..state() });
        assert_eq!((stuck.title.as_str(), stuck.default_button, stuck.other_button, stuck.default_goes_ahead), ("Keep the menu bar icon?", "Keep Icon", "Hide Anyway", false));
        assert!(stuck.message.starts_with("⌘ \\ is used by another app"), "{}", stuck.message);
        // The Dock icon is a way back too.
        assert_eq!(hide_alert(&MenuState { toggle_unavailable: true, show_in_dock: true, ..state() }), usual);
    }

    /// Windows asks the same question either way, Hide icon first; a taken shortcut only adds a warning.
    #[cfg(windows)]
    #[test]
    fn hiding_the_tray_icon_asks_once_and_warns_when_the_shortcut_is_taken() {
        let usual = hide_dialog(&state());
        assert_eq!(usual, HideDialog { title: "Hide the tray icon?", keys: vec!["Ctrl".into(), "\\".into()], warning: None, hide: "Hide icon", cancel: "Cancel" });
        let taken = hide_dialog(&MenuState { toggle_unavailable: true, ..state() });
        assert_eq!(taken.warning.as_deref(), Some("Ctrl \\ is in use by another app right now"));
        assert_eq!(HideDialog { warning: None, ..taken }, usual, "same title, keys and buttons");
    }
}
