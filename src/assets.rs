//! Icons compiled into the binary, so the overlay never reads files at runtime.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const ICONS: &[(&str, &[u8])] = &[
    ("icons/gear.svg", include_bytes!("../assets/icons/gear.svg")),
    ("icons/close.svg", include_bytes!("../assets/icons/close.svg")),
    ("icons/chevron-down.svg", include_bytes!("../assets/icons/chevron-down.svg")),
    ("icons/chevron-up.svg", include_bytes!("../assets/icons/chevron-up.svg")),
    ("icons/eye.svg", include_bytes!("../assets/icons/eye.svg")),
    ("icons/eye-off.svg", include_bytes!("../assets/icons/eye-off.svg")),
    ("icons/minimize.svg", include_bytes!("../assets/icons/minimize.svg")),
    ("icons/maximize.svg", include_bytes!("../assets/icons/maximize.svg")),
    ("icons/window-close.svg", include_bytes!("../assets/icons/window-close.svg")),
    ("icons/lightbulb.svg", include_bytes!("../assets/icons/lightbulb.svg")),
    ("icons/shift.svg", include_bytes!("../assets/icons/shift.svg")),
    ("icons/space.svg", include_bytes!("../assets/icons/space.svg")),
    ("icons/return.svg", include_bytes!("../assets/icons/return.svg")),
    ("icons/tooltip-pointer-up.svg", include_bytes!("../assets/icons/tooltip-pointer-up.svg")),
    ("icons/info.svg", include_bytes!("../assets/icons/info.svg")),
    ("icons/sparkle.svg", include_bytes!("../assets/icons/sparkle.svg")),
    ("icons/image.svg", include_bytes!("../assets/icons/image.svg")),
    ("icons/image-off.svg", include_bytes!("../assets/icons/image-off.svg")),
    ("icons/tab-model.svg", include_bytes!("../assets/icons/tab-model.svg")),
    ("icons/tab-listening.svg", include_bytes!("../assets/icons/tab-listening.svg")),
    ("icons/tab-shortcuts.svg", include_bytes!("../assets/icons/tab-shortcuts.svg")),
    ("icons/tab-window.svg", include_bytes!("../assets/icons/tab-window.svg")),
    ("icons/tab-sessions.svg", include_bytes!("../assets/icons/tab-sessions.svg")),
    ("icons/tab-about.svg", include_bytes!("../assets/icons/tab-about.svg")),
    ("icons/mode-document.svg", include_bytes!("../assets/icons/mode-document.svg")),
    ("icons/mode-briefcase.svg", include_bytes!("../assets/icons/mode-briefcase.svg")),
    ("icons/mode-conversation.svg", include_bytes!("../assets/icons/mode-conversation.svg")),
    ("icons/mode-code.svg", include_bytes!("../assets/icons/mode-code.svg")),
    ("icons/mode-diagram.svg", include_bytes!("../assets/icons/mode-diagram.svg")),
    ("icons/mode-chart.svg", include_bytes!("../assets/icons/mode-chart.svg")),
    ("icons/mode-phone.svg", include_bytes!("../assets/icons/mode-phone.svg")),
    ("icons/mode-graduation-cap.svg", include_bytes!("../assets/icons/mode-graduation-cap.svg")),
    ("icons/mode-people.svg", include_bytes!("../assets/icons/mode-people.svg")),
    ("icons/mode-tag.svg", include_bytes!("../assets/icons/mode-tag.svg")),
    ("icons/mode-headset.svg", include_bytes!("../assets/icons/mode-headset.svg")),
    ("icons/mode-star.svg", include_bytes!("../assets/icons/mode-star.svg")),
    ("icons/mode-lightbulb.svg", include_bytes!("../assets/icons/mode-lightbulb.svg")),
    ("icons/mode-book.svg", include_bytes!("../assets/icons/mode-book.svg")),
    ("icons/mode-folder.svg", include_bytes!("../assets/icons/mode-folder.svg")),
    ("icons/mode-globe.svg", include_bytes!("../assets/icons/mode-globe.svg")),
    ("icons/tab-modes.svg", include_bytes!("../assets/icons/tab-modes.svg")),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    ("icons/more.svg", include_bytes!("../assets/icons/more.svg")),
    ("icons/check.svg", include_bytes!("../assets/icons/check.svg")),
    ("icons/file-drop.svg", include_bytes!("../assets/icons/file-drop.svg")),
    ("icons/warning.svg", include_bytes!("../assets/icons/warning.svg")),
    ("icons/search.svg", include_bytes!("../assets/icons/search.svg")),
    ("icons/spinner.svg", include_bytes!("../assets/icons/spinner.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(name, _)| *name == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(name, _)| name.starts_with(path)).map(|(name, _)| SharedString::from(*name)).collect())
    }
}
