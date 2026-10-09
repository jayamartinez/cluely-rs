//! Settings › Model: where the selected subscription's CLI is, when that needs attention. A CLI
//! that wasn't found offers How to install and Locate… (the system file picker; there is no text
//! field). A file the user chose shows "Custom location" with Use automatic. A CLI found
//! automatically, or a chosen file that's gone while another is found, shows nothing (`cli_path`).

use std::path::Path;

use gpui::{Context, Div, FontWeight, MouseButton, ParentElement, PathPromptOptions, Styled, Window, div, prelude::*, px};

use super::{ROW_INSET, button, listen};
use crate::overlay::Overlay;
use crate::settings::{Provider, Settings};
use crate::theme;

/// What differs between the two subscription CLIs.
#[derive(Clone, Copy)]
struct CliDetails {
    name: &'static str,
    install_page: &'static str,
    /// The same rules the automatic search applies to a file.
    check: fn(&Path) -> Result<(), String>,
    set: fn(&mut Settings, String),
}

fn details(provider: Provider) -> Option<CliDetails> {
    match provider {
        Provider::Codex => Some(CliDetails {
            name: "Codex CLI",
            install_page: "https://developers.openai.com/codex/cli",
            check: crate::codex::check_location,
            set: |s, path| s.codex_path = path,
        }),
        Provider::Claude => Some(CliDetails {
            name: "Claude Code",
            install_page: "https://code.claude.com/docs/en/setup",
            check: crate::claude_cli::ClaudeCli::check_location,
            set: |s, path| s.claude_path = path,
        }),
        Provider::ApiKey => None,
    }
}

impl Overlay {
    /// The row under the providers for the selected subscription, when its CLI wasn't found or a
    /// chosen file is in use.
    pub(super) fn cli_location_row(&self, cx: &mut Context<Self>) -> Option<Div> {
        let provider = self.store.value.provider;
        let cli = details(provider)?;
        let status = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() }?;
        let (detail, action) = if !status.installed {
            let page = cli.install_page;
            let install = div().id("cli-install").cursor_pointer().text_size(px(12.0)).text_color(theme::accent_soft())
                .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page)))
                .child("How to install ↗");
            let locate = button("cli-locate", "Locate…", true)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.locate_cli(provider, window, cx)));
            (install.into_any_element(), locate)
        } else if status.custom_location {
            let custom = div().text_size(px(12.0)).text_color(theme::muted()).child("Custom location");
            let automatic = button("cli-automatic", "Use automatic", false)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.set_cli_location(provider, String::new(), window, cx)));
            (custom.into_any_element(), automatic)
        } else {
            return None;
        };
        let error = self.cli_error.as_ref().filter(|(shown_for, _)| *shown_for == provider)
            .map(|(_, message)| div().text_size(px(11.0)).text_color(gpui::rgb(0xffb4a8)).child(message.clone()));
        let title = div().text_size(px(13.0)).text_color(theme::text())
            .when(cfg!(not(target_os = "macos")), |title| title.font_weight(FontWeight::SEMIBOLD))
            .child(cli.name);
        // Inside the provider list (8 px apart); the extra 8 px matches the gap to what follows.
        Some(div().flex().items_center().gap(px(16.0)).px(px(ROW_INSET)).pt(px(8.0))
            .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0().child(title).child(detail))
            .child(div().flex().flex_col().items_end().gap(px(4.0)).flex_none().child(action).children(error)))
    }

    /// Pick the CLI with the system file picker, check it like the search would, and use it.
    fn locate_cli(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cli) = details(provider) else { return };
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            // Reads the file's metadata (and a launcher's text), so off the UI thread.
            let checked = cx.background_executor().spawn(async move {
                (cli.check)(&path)?;
                path.into_os_string().into_string().map_err(|_| "That location can't be saved.".to_string())
            }).await;
            let _ = this.update_in(cx, |this, window, cx| match checked {
                Ok(path) => this.set_cli_location(provider, path, window, cx),
                Err(message) => {
                    this.cli_error = Some((provider, message.into()));
                    cx.notify();
                }
            });
        }).detach();
    }

    /// Save the CLI's location (empty: automatic), then check the subscription again with it.
    fn set_cli_location(&mut self, provider: Provider, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cli) = details(provider) else { return };
        self.cli_error = None;
        self.update_settings(|s| (cli.set)(s, path), window, cx);
        // A running Codex app-server is still the old file; stopping it lets the check start the new one.
        let codex = (provider == Provider::Codex).then(|| self.codex.clone());
        cx.spawn_in(window, async move |this, cx| {
            if let Some(codex) = codex {
                cx.background_executor().spawn(async move { codex.shutdown() }).await;
            }
            let _ = this.update_in(cx, |this, window, cx| this.refresh_subscriptions(window, cx));
        }).detach();
    }
}
