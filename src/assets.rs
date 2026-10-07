//! Icons compiled into the binary, so the overlay never reads files at runtime.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const ICONS: &[(&str, &[u8])] = &[
    ("icons/gear.svg", include_bytes!("../assets/icons/gear.svg")),
    ("icons/close.svg", include_bytes!("../assets/icons/close.svg")),
    ("icons/history.svg", include_bytes!("../assets/icons/history.svg")),
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
    ("icons/send.svg", include_bytes!("../assets/icons/send.svg")),
    ("icons/tooltip-pointer.svg", include_bytes!("../assets/icons/tooltip-pointer.svg")),
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
