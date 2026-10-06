//! Settings panel, laid out like the Paper "Settings · Model" artboard. Every change
//! saves immediately; window-level options (capture hiding) also apply immediately.

use gpui::{Context, Div, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, div, prelude::*, px};

use crate::archive::Retention;
use crate::hotkeys::{Action, DEFAULTS};
use crate::overlay::Overlay;
use crate::settings::{AnswerStyle, AudioSource, ClaudeModel, Provider, Settings};
use crate::stt::parakeet::MODEL;
use crate::theme;
use crate::transcript_view::model_size_label;
use crate::ui;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab { #[default] Model, Listening, Keys, Window, History }

impl Tab {
    const ALL: [Tab; 5] = [Tab::Model, Tab::Listening, Tab::Keys, Tab::Window, Tab::History];
    fn label(self) -> &'static str {
        match self { Tab::Model => "Model", Tab::Listening => "Listening", Tab::Keys => "Keys", Tab::Window => "Window", Tab::History => "History" }
    }
}

/// A segmented control over `options`; clicking applies `set` and saves.
fn choice<T: Copy + PartialEq + 'static>(
    name: &'static str, options: &[(T, &'static str)], current: T, set: fn(&mut Settings, T), cx: &mut Context<Overlay>,
) -> Div {
    let mut control = ui::segmented();
    for (index, (value, label)) in options.iter().copied().enumerate() {
        control = control.child(ui::segment((name, index), label, value == current)
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.update_settings(|s| set(s, value), window, cx))));
    }
    control
}

fn field(label: &'static str, control: impl IntoElement) -> Div {
    div().flex().flex_col().gap(px(6.0)).child(div().text_size(px(12.0)).text_color(theme::muted()).child(label)).child(control)
}

fn toggle(name: &'static str, title: &'static str, detail: &'static str, on: bool, set: fn(&mut Settings, bool), cx: &mut Context<Overlay>) -> impl IntoElement {
    div().id(name).flex().items_center().gap(px(12.0)).py(px(8.0)).cursor_pointer()
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.update_settings(|s| set(s, !on), window, cx)))
        .child(div().flex().flex_col().gap(px(1.0)).flex_1()
            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
            .child(div().text_size(px(12.0)).text_color(theme::muted()).child(detail)))
        .child(ui::switch(on))
}

impl Overlay {
    pub fn settings_panel(&self, tab: Tab, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div().flex().items_baseline().gap(px(14.0))
            .child(div().text_size(px(20.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("Settings"));
        for (index, item) in Tab::ALL.into_iter().enumerate() {
            let selected = item == tab;
            tabs = tabs.child(div().id(("tab", index)).cursor_pointer().pb(px(2.0)).text_size(px(13.0))
                .when(selected, |label| label.font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).border_b_2().border_color(theme::accent()))
                .when(!selected, |label| label.text_color(theme::muted()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.open_settings(item, window, cx)))
                .child(item.label()));
        }
        let header = ui::panel_header().child(tabs)
            .child(ui::close_button("close-settings")
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_panels(window, cx))));
        let body = match tab {
            Tab::Model => self.model_tab(cx).into_any_element(),
            Tab::Listening => self.listening_tab(cx).into_any_element(),
            Tab::Keys => self.keys_tab().into_any_element(),
            Tab::Window => self.window_tab(cx).into_any_element(),
            Tab::History => self.history_tab(cx).into_any_element(),
        };
        let mut panel = div().relative().w(px(560.0)).flex().flex_col().rounded(px(18.0)).bg(theme::glass())
            .border_1().border_color(theme::hairline()).overflow_hidden()
            .child(self.hits.mark())
            .child(header)
            .child(div().id("settings-body").flex().flex_col().gap(px(16.0)).px(px(18.0)).py(px(16.0)).max_h(px(470.0)).overflow_y_scroll().child(body));
        if let Some(warning) = self.store.warning {
            panel = panel.child(div().px(px(18.0)).pb(px(12.0)).text_size(px(12.0)).text_color(gpui::rgb(0xffb4a8)).child(warning));
        }
        panel
    }

    fn model_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let providers = [
            (Provider::Codex, "ChatGPT subscription", "Through the official Codex CLI sign-in"),
            (Provider::Claude, "Claude subscription", "Through the official Claude Code sign-in"),
            (Provider::ApiKey, "Your API key", "Anthropic or OpenAI · billed to your account"),
        ];
        let mut list = div().flex().flex_col().gap(px(8.0)).child(ui::section_label("ANSWER WITH"));
        for (index, (provider, title, detail)) in providers.into_iter().enumerate() {
            let selected = s.provider == provider;
            list = list.child(ui::row(("provider", index), title, detail, selected)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.update_settings(|s| s.provider = provider, window, cx)))
                .child(self.connection_badge(provider, index, cx)));
        }
        let model: Div = match s.provider {
            Provider::Codex => {
                let models = self.codex_status.as_ref().map(|st| st.models.clone()).unwrap_or_default();
                if models.is_empty() {
                    field("Model", div().text_size(px(13.0)).text_color(theme::body()).child("Codex default · the list appears once you're signed in"))
                } else {
                    let mut chips = ui::segmented().child(ui::segment("codex-default", "Default", s.codex_model.is_empty())
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.update_settings(|s| s.codex_model.clear(), window, cx))));
                    for (index, (id, name)) in models.into_iter().take(12).enumerate() {
                        let chosen = id.clone();
                        chips = chips.child(ui::segment(("codex-model", index), name, s.codex_model == id)
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { let id = chosen.clone(); this.update_settings(|s| s.codex_model = id, window, cx) })));
                    }
                    field("Model", div().flex().flex_col().gap(px(8.0)).child(chips)
                        .child(div().id("codex-sign-out").cursor_pointer().text_size(px(12.0)).text_color(theme::muted())
                            .hover(|t| t.text_color(theme::body()))
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.sign_out_codex(window, cx)))
                            .child("Sign out of ChatGPT")))
                }
            }
            Provider::Claude => field("Model", choice("claude-model",
                &[(ClaudeModel::Sonnet, "Sonnet"), (ClaudeModel::Opus, "Opus"), (ClaudeModel::Haiku, "Haiku")],
                s.claude_model, |s, v| s.claude_model = v, cx)),
            Provider::ApiKey => self.api_section(cx),
        };
        div().flex().flex_col().gap(px(16.0))
            .child(list)
            .child(model)
            .child(field("Answer style", choice("style",
                &[(AnswerStyle::Spoken, "Words I can say aloud"), (AnswerStyle::Standard, "Standard explanations")],
                s.answer_style, |s, v| s.answer_style = v, cx)))
    }

    /// Trailing status for a provider row: connected account, a Sign in button, or a hint.
    fn connection_badge(&self, provider: Provider, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let text = |label: String, color: gpui::Rgba| div().w(px(150.0)).flex_none().flex().justify_end().text_size(px(12.0)).text_color(color).truncate().child(label).into_any_element();
        let status = match provider {
            Provider::Codex => self.codex_status.as_ref(),
            Provider::Claude => self.claude_status.as_ref(),
            Provider::ApiKey => {
                let s = &self.store.value;
                let ready = crate::providers::preset(&s.api_provider).is_some_and(|p| !p.needs_key || crate::secrets::get(p.id).is_some());
                return if ready { text("Ready".into(), theme::ok()) } else { text("Not set".into(), theme::muted()) };
            }
        };
        match status {
            None => text("Checking…".into(), theme::muted()),
            Some(st) if st.signed_in => text(st.account.clone().unwrap_or("Connected".into()), theme::ok()),
            Some(st) if !st.installed => text("CLI not installed".into(), theme::muted()),
            Some(_) => div().id(("sign-in", index)).flex_none().px(px(10.0)).py(px(5.0)).rounded(px(8.0)).bg(theme::accent()).cursor_pointer()
                .text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink())
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.sign_in(provider, window, cx)))
                .child(if self.signing_in { "Waiting for browser…" } else { "Sign in" }).into_any_element(),
        }
    }

    fn api_section(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut presets = ui::segmented();
        for (index, preset) in crate::providers::PRESETS.iter().enumerate() {
            let id = preset.id;
            presets = presets.child(ui::segment(("api-provider", index), preset.label, s.api_provider == id)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.update_settings(|s| s.api_provider = id.to_string(), window, cx))));
        }
        let Some(preset) = crate::providers::preset(&s.api_provider) else { return field("Provider", presets) };
        let input_box = |input: gpui::Entity<crate::input::TextInput>, id: &'static str| {
            let focus = input.clone();
            div().id(id).flex_1().min_w_0().px(px(10.0)).py(px(7.0)).rounded(px(9.0))
                .bg(theme::field()).border_1().border_color(theme::hairline()).cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, window, cx| window.focus(&focus.focus_handle(cx))))
                .child(input)
        };
        let mut section = div().flex().flex_col().gap(px(16.0)).child(field("Provider", presets));
        if preset.needs_key {
            let saved = crate::secrets::hint(preset.id);
            let page = preset.key_page;
            let mut status = div().flex().items_center().gap(px(12.0)).text_size(px(12.0)).text_color(theme::muted())
                .child(match &saved { Some(hint) => format!("Saved key {hint}"), None => "No key saved".to_string() });
            if saved.is_some() {
                status = status.child(div().id("remove-key").cursor_pointer().text_color(gpui::rgb(0xffb4a8))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.remove_key(cx))).child("Remove"));
            }
            if !page.is_empty() {
                status = status.child(div().id("key-page").cursor_pointer().text_color(theme::accent_soft())
                    .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| cx.open_url(page))).child("Get a key ↗"));
            }
            section = section.child(field("API key", div().flex().flex_col().gap(px(8.0))
                .child(div().flex().items_center().gap(px(8.0))
                    .child(input_box(self.key_input.clone(), "key-box"))
                    .child(div().id("save-key").cursor_pointer().px(px(12.0)).py(px(7.0)).rounded(px(9.0)).bg(theme::accent())
                        .text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink())
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.save_key(cx))).child("Save key")))
                .child(status)));
        } else if preset.local {
            section = section.child(div().text_size(px(12.0)).text_color(theme::muted())
                .child(format!("{} runs on this PC, so no key is needed. Make sure it's running.", preset.label)));
        }
        if preset.base_url.is_empty() {
            section = section.child(field("Base URL (OpenAI-compatible)", input_box(self.base_url_input.clone(), "base-url-box")));
        }
        let mut models = div().flex().flex_wrap().gap(px(6.0));
        let suggestions: Vec<String> = if self.loaded_models.is_empty() {
            preset.suggested_models.iter().map(|m| m.to_string()).collect()
        } else { self.loaded_models.iter().take(40).cloned().collect() };
        for (index, model) in suggestions.into_iter().enumerate() {
            let selected = s.api_model() == model;
            let chosen = model.clone();
            models = models.child(ui::segment(("model", index), model, selected).border_1().border_color(theme::hairline())
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.choose_model(chosen.clone(), window, cx))));
        }
        let load = div().id("load-models").cursor_pointer().px(px(10.0)).py(px(5.0)).rounded(px(8.0)).border_1().border_color(theme::hairline())
            .text_size(px(12.0)).text_color(theme::body())
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.load_models(window, cx)))
            .child(if self.models_loading { "Loading…" } else { "Load models" });
        section = section.child(field("Model", div().flex().flex_col().gap(px(8.0))
            .child(div().flex().items_center().gap(px(8.0)).child(input_box(self.model_input.clone(), "model-box")).child(load))
            .child(models)));
        if let Some(notice) = self.key_notice.clone() {
            section = section.child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice));
        }
        section.child(div().text_size(px(12.0)).text_color(theme::muted())
            .child("Keys are kept in Windows Credential Manager, never in the settings file, and are only sent to the provider you choose."))
    }

    fn listening_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        div().flex().flex_col().gap(px(16.0))
            .child(toggle("transcribe", "Transcribe conversations", "Runs NVIDIA Parakeet on this PC while Live is on. Nothing is sent anywhere.",
                s.transcribe, |s, v| s.transcribe = v, cx))
            .child(field("Local model", self.model_row(cx)))
            .child(field("Listen to", choice("source",
                &[(AudioSource::Both, "Desktop + mic"), (AudioSource::Desktop, "Desktop only"), (AudioSource::Microphone, "Mic only")],
                s.audio_source, |s, v| s.audio_source = v, cx)))
            .child(div().text_size(px(12.0)).text_color(theme::muted())
                .child("Desktop and mic are transcribed separately, so the transcript can tell Me from Them. English only for now."))
    }

    /// The Parakeet model: installed, downloading (with progress and Cancel), or a Download button.
    fn model_row(&self, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, label: SharedString, primary: bool| {
            let base = div().id(id).flex_none().px(px(12.0)).py(px(6.0)).rounded(px(9.0)).cursor_pointer().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).child(label);
            if primary { base.bg(theme::accent()).text_color(theme::accent_ink()) } else { base.border_1().border_color(theme::hairline()).text_color(theme::body()) }
        };
        let trailing: gpui::AnyElement = if let Some(progress) = self.download_progress() {
            div().flex().items_center().gap(px(10.0))
                .child(div().w(px(120.0)).h(px(6.0)).rounded_full().bg(theme::hairline())
                    .child(div().h_full().rounded_full().bg(theme::accent()).w(px(120.0 * progress.clamp(0.0, 1.0) as f32))))
                .child(div().w(px(36.0)).font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(format!("{:.0}%", progress * 100.0)))
                .child(button("cancel-download", "Cancel".into(), false)
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { this.cancel_download(); cx.notify(); })))
                .into_any_element()
        } else if self.model_installed {
            div().text_size(px(12.0)).text_color(theme::ok()).child("Installed").into_any_element()
        } else {
            button("download-model", format!("Download {}", model_size_label(&MODEL)).into(), true)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.download_model(window, cx)))
                .into_any_element()
        };
        let mut row = div().flex().flex_col().gap(px(8.0))
            .child(div().flex().items_center().gap(px(12.0)).px(px(12.0)).py(px(10.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
                .child(div().flex().flex_col().gap(px(1.0)).flex_1().min_w_0()
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Parakeet Realtime EOU 120M"))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child("NVIDIA · on-device · English · detects when a sentence ends")))
                .child(trailing));
        if let Some(notice) = self.model_notice.clone() {
            row = row.child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice));
        }
        row.child(div().flex().flex_wrap().items_center().gap(px(6.0)).text_size(px(12.0)).text_color(theme::muted())
            .child("Downloaded from Hugging Face and verified against a pinned checksum ·")
            .child(div().id("model-license").cursor_pointer().text_color(theme::accent_soft())
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.open_url(MODEL.license_url)))
                .child("NVIDIA Open Model License ↗")))
    }

    fn keys_tab(&self) -> Div {
        let mut list = div().flex().flex_col();
        for (action, _) in DEFAULTS {
            let label = match action {
                Action::Assist => "Assist (screen + conversation)", Action::Live => "Start / stop Live",
                Action::Focus => "Type a question",
                Action::Toggle => "Show / hide overlay", Action::MoveUp => "Move up", Action::MoveDown => "Move down",
                Action::MoveLeft => "Move left", Action::MoveRight => "Move right",
                Action::ScrollUp => "Scroll answer up", Action::ScrollDown => "Scroll answer down",
                Action::Close => "Close settings",
            };
            let taken = self.hotkeys.unavailable.contains(action);
            list = list.child(div().flex().items_center().justify_between().py(px(8.0)).border_b_1().border_color(theme::divider())
                .child(div().text_size(px(13.0)).text_color(theme::text()).child(label))
                .child(div().flex().items_center().gap(px(8.0))
                    .when(taken, |row| row.child(div().text_size(px(12.0)).text_color(gpui::rgb(0xffb4a8)).child("Used by another app")))
                    .child(ui::keycap(self.hotkeys.label(*action)))));
        }
        div().flex().flex_col().gap(px(10.0))
            .child(list)
            .child(div().text_size(px(12.0)).text_color(theme::muted())
                .child("Assist is only claimed during a Live session, move and scroll keys only while the overlay is visible, and Esc only while settings is open, so other apps keep their shortcuts."))
    }

    fn history_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut tab = div().flex().flex_col()
            .child(toggle("save-sessions", "Save sessions", "Keeps each Live session's transcript, questions and answers on this PC.",
                s.save_sessions, |s, v| s.save_sessions = v, cx));
        if s.save_sessions {
            tab = tab
                .child(toggle("save-screenshots", "Save screenshots", "Stores the screen each answer used, so you can see what was on screen.",
                    s.save_screenshots, |s, v| s.save_screenshots = v, cx))
                .child(div().pt(px(8.0)).child(field("Keep sessions for", choice("retention",
                    &[(Retention::Days7, "7 days"), (Retention::Days30, "30 days"), (Retention::Forever, "Forever")],
                    s.keep_sessions, |s, v| s.keep_sessions = v, cx))));
        }
        tab.child(div().flex().gap(px(8.0)).pt(px(14.0))
                .child(div().id("view-history").cursor_pointer().px(px(12.0)).py(px(6.0)).rounded(px(9.0)).bg(theme::accent())
                    .text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink())
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.open_sessions(cx)))
                    .child("Open sessions"))
                .child(div().id("open-archive").cursor_pointer().px(px(12.0)).py(px(6.0)).rounded(px(9.0)).border_1().border_color(theme::hairline())
                    .text_size(px(12.0)).text_color(theme::body())
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.reveal_archive()))
                    .child("Open folder")))
            .child(div().pt(px(10.0)).text_size(px(12.0)).text_color(theme::muted())
                .child("Recordings of other people may need their consent where you live."))
    }

    fn window_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        div().flex().flex_col()
            .child(toggle("hide-capture", "Hide from screen capture", "Keeps the overlay out of screen shares, recordings and screenshots.",
                s.hide_from_capture, |s, v| s.hide_from_capture = v, cx))
            .child(toggle("screen-on-send", "Attach screen to every message", "Takes a fresh screenshot whenever you ask or press Assist.",
                s.screen_on_send, |s, v| s.screen_on_send = v, cx))
            .child(toggle("live-on-launch", "Start Live when the app opens", "Begins listening right away instead of waiting for Start.",
                s.start_live_on_launch, |s, v| s.start_live_on_launch = v, cx))
    }
}
