#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cluely_rs::{assets, hotkeys, input, modes, overlay, text_area};
use gpui::{App, AppContext, Application, Bounds, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, point, px, size};

#[cfg(target_os = "macos")]
gpui::actions!(cluely_rs, [Quit, OpenSettings]);

/// The app menu, so Settings opens with ⌘, and CluelyRS quits with ⌘Q, like any Mac app.
#[cfg(target_os = "macos")]
fn app_menu(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([gpui::KeyBinding::new("cmd-q", Quit, None), gpui::KeyBinding::new("cmd-,", OpenSettings, None)]);
    cluely_rs::settings_window::bind_keys(cx);
    cx.set_menus(vec![gpui::Menu { name: "CluelyRS".into(), items: vec![
        gpui::MenuItem::action("Settings…", OpenSettings),
        gpui::MenuItem::separator(),
        gpui::MenuItem::action("Quit CluelyRS", Quit),
    ] }]);
}

fn main() {
    // Started only to read a file added to a mode (in its own process, so a bad file can't hang the app).
    if let Some(code) = modes::extract::child_main() { std::process::exit(code); }
    Application::new().with_assets(assets::Assets).run(|cx: &mut App| {
        input::bind_keys(cx);
        text_area::bind_keys(cx);
        #[cfg(target_os = "macos")]
        app_menu(cx);
        // Hotkeys must be created on this (the GPUI main) thread so its message loop serves them.
        let (hotkeys, presses) = match hotkeys::Hotkeys::new() {
            Ok(pair) => pair,
            Err(error) => { eprintln!("global shortcuts unavailable: {error}"); cx.quit(); return; }
        };
        let display = cx.primary_display();
        // As wide as Settings › Window › Card width makes it, centred.
        let width = px(cluely_rs::settings::Store::load().value.card_width.window());
        let origin = display.as_ref().map(|display| {
            let bounds = display.bounds();
            point(bounds.origin.x + (bounds.size.width - width) / 2.0, bounds.origin.y + px(24.0))
        }).unwrap_or(point(px(200.0), px(24.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size(width, px(overlay::IDLE_HEIGHT))))),
            titlebar: None,
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::PopUp,
            focus: false,
            show: true,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            display_id: display.map(|display| display.id()),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| cx.new(|cx| overlay::Overlay::new(hotkeys, presses, window, cx)));
        #[cfg(target_os = "macos")]
        if let Ok(handle) = opened {
            cx.on_action(move |_: &OpenSettings, cx| {
                let _ = handle.update(cx, |overlay, window, cx| overlay.open_settings(Default::default(), window, cx));
            });
        }
        if let Err(error) = opened {
            eprintln!("overlay window could not open: {error}");
            cx.quit();
        }
    });
}
