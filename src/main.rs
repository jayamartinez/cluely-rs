#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cluely_rs::{assets, hotkeys, input, overlay};
use gpui::{App, AppContext, Application, Bounds, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, point, px, size};

#[cfg(target_os = "macos")]
gpui::actions!(cluely_rs, [Quit]);

/// The app menu, so CluelyRS can be quit with ⌘Q or from the menu bar like any Mac app.
#[cfg(target_os = "macos")]
fn app_menu(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([gpui::KeyBinding::new("cmd-q", Quit, None)]);
    cx.set_menus(vec![gpui::Menu { name: "CluelyRS".into(), items: vec![gpui::MenuItem::action("Quit CluelyRS", Quit)] }]);
}

fn main() {
    Application::new().with_assets(assets::Assets).run(|cx: &mut App| {
        input::bind_keys(cx);
        #[cfg(target_os = "macos")]
        app_menu(cx);
        // Hotkeys must be created on this (the GPUI main) thread so its message loop serves them.
        let (hotkeys, presses) = match hotkeys::Hotkeys::new() {
            Ok(pair) => pair,
            Err(error) => { eprintln!("global shortcuts unavailable: {error}"); cx.quit(); return; }
        };
        let display = cx.primary_display();
        let origin = display.as_ref().map(|display| {
            let bounds = display.bounds();
            point(bounds.origin.x + (bounds.size.width - px(600.0)) / 2.0, bounds.origin.y + px(24.0))
        }).unwrap_or(point(px(200.0), px(24.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size(px(600.0), px(120.0))))),
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
        if let Err(error) = cx.open_window(options, |window, cx| cx.new(|cx| overlay::Overlay::new(hotkeys, presses, window, cx))) {
            eprintln!("overlay window could not open: {error}");
            cx.quit();
        }
    });
}
