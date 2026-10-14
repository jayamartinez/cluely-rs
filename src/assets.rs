//! Icons (and setup's logos and app tile) compiled into the binary, so the overlay never reads files at runtime.

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
    ("icons/restore.svg", include_bytes!("../assets/icons/restore.svg")),
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
    ("icons/update.svg", include_bytes!("../assets/icons/update.svg")),
    ("icons/check-circle.svg", include_bytes!("../assets/icons/check-circle.svg")),
    ("icons/alert-circle.svg", include_bytes!("../assets/icons/alert-circle.svg")),
    ("icons/laptop-minimal.svg", include_bytes!("../assets/icons/laptop-minimal.svg")),
    ("icons/arrow-right.svg", include_bytes!("../assets/icons/arrow-right.svg")),
    ("icons/key-round.svg", include_bytes!("../assets/icons/key-round.svg")),
    ("icons/message-square.svg", include_bytes!("../assets/icons/message-square.svg")),
    ("icons/cloud.svg", include_bytes!("../assets/icons/cloud.svg")),
    ("icons/sliders.svg", include_bytes!("../assets/icons/sliders.svg")),
    ("icons/terminal.svg", include_bytes!("../assets/icons/terminal.svg")),
    ("icons/circle-user.svg", include_bytes!("../assets/icons/circle-user.svg")),
    ("icons/send.svg", include_bytes!("../assets/icons/send.svg")),
    ("icons/plug.svg", include_bytes!("../assets/icons/plug.svg")),
    ("icons/link.svg", include_bytes!("../assets/icons/link.svg")),
    ("icons/server.svg", include_bytes!("../assets/icons/server.svg")),
    ("icons/lock.svg", include_bytes!("../assets/icons/lock.svg")),
    ("icons/mic-off.svg", include_bytes!("../assets/icons/mic-off.svg")),
    ("icons/download.svg", include_bytes!("../assets/icons/download.svg")),
    ("icons/zap.svg", include_bytes!("../assets/icons/zap.svg")),
    ("icons/archive.svg", include_bytes!("../assets/icons/archive.svg")),
    ("icons/hourglass.svg", include_bytes!("../assets/icons/hourglass.svg")),
    ("icons/panel-bottom.svg", include_bytes!("../assets/icons/panel-bottom.svg")),
    ("icons/radio.svg", include_bytes!("../assets/icons/radio.svg")),
    ("icons/sparkles.svg", include_bytes!("../assets/icons/sparkles.svg")),
    ("icons/mic.svg", include_bytes!("../assets/icons/mic.svg")),
    ("icons/monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
    ("icons/volume-2.svg", include_bytes!("../assets/icons/volume-2.svg")),
    ("icons/type.svg", include_bytes!("../assets/icons/type.svg")),
    ("icons/move.svg", include_bytes!("../assets/icons/move.svg")),
    ("icons/chevrons-up-down.svg", include_bytes!("../assets/icons/chevrons-up-down.svg")),
    ("icons/shield.svg", include_bytes!("../assets/icons/shield.svg")),
    // Setup (`onboarding_window`): provider and service logos, the app tile.
    ("logos/anthropic.svg", include_bytes!("../assets/logos/anthropic.svg")),
    ("logos/claude.svg", include_bytes!("../assets/logos/claude.svg")),
    ("logos/deepgram.svg", include_bytes!("../assets/logos/deepgram.svg")),
    ("logos/googlegemini.svg", include_bytes!("../assets/logos/googlegemini.svg")),
    ("logos/ollama.svg", include_bytes!("../assets/logos/ollama.svg")),
    ("logos/openai.svg", include_bytes!("../assets/logos/openai.svg")),
    ("logos/openrouter.svg", include_bytes!("../assets/logos/openrouter.svg")),
    ("app-icon/app-icon.svg", include_bytes!("../assets/app-icon/app-icon.svg")),
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
