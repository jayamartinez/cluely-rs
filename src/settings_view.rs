//! Settings panel, laid out like the Paper "Settings" v2 artboards: short labels, dropdown
//! pickers, toggles on the right. Every change saves immediately; window-level options
//! (capture hiding) and listening options apply to the running session.


use gpui::{Context, Div, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, deferred, div, prelude::*, px};

use crate::archive::Retention;
use crate::chat::SubscriptionStatus;
use crate::hotkeys::{Action, DEFAULTS};
use crate::overlay::Overlay;
use crate::settings::{AnswerStyle, ClaudeModel, Provider, Settings, SttProvider};
use crate::stt::deepgram;
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

/// The dropdowns in Settings; at most one is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picker { CodexModel, ClaudeModel, AnswerStyle, ApiProvider, ApiModel, SttProvider, Mic, Desktop }

/// Taller lists scroll.
const MENU_MAX_HEIGHT: f32 = 300.0;

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
    div().flex().flex_col().gap(px(6.0)).flex_1().min_w_0().child(div().text_size(px(12.0)).text_color(theme::muted()).child(label)).child(control)
}

/// A clickable switch bound to a boolean setting.
fn switch(name: &'static str, on: bool, set: fn(&mut Settings, bool), cx: &mut Context<Overlay>) -> impl IntoElement {
    div().id(name).flex_none().cursor_pointer()
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { cx.stop_propagation(); this.update_settings(|s| set(s, !on), window, cx) }))
        .child(ui::switch(on))
}

fn button(id: &'static str, label: impl Into<SharedString>, primary: bool) -> gpui::Stateful<Div> {
    let base = div().id(id).flex_none().px(px(12.0)).py(px(6.0)).rounded(px(9.0)).cursor_pointer().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).child(label.into());
    if primary { base.bg(theme::accent()).text_color(theme::accent_ink()) } else { base.border_1().border_color(theme::hairline()).text_color(theme::body()) }
}

/// "plus" → "ChatGPT Plus", "max_5x" → "Max 5x": the plan as the CLI reports it, made readable.
fn plan_label(provider: Provider, plan: &str) -> String {
    let words: Vec<String> = plan.split(['_', '-', ' ']).filter(|w| !w.is_empty()).map(|word| {
        let mut chars = word.chars();
        match chars.next() { Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(), None => String::new() }
    }).collect();
    let plan = words.join(" ");
    match (provider, plan.strip_prefix("Chatgpt")) {
        (_, Some(rest)) => format!("ChatGPT{rest}"),
        (Provider::Codex, None) => format!("ChatGPT {plan}"),
        (_, None) => plan,
    }
}

/// Everything after the first character hidden, including the domain.
fn masked(account: &str) -> String {
    let mut chars = account.chars();
    match chars.next() { Some(first) => format!("{first}{}", "•".repeat(account.chars().count().clamp(6, 15))), None => "••••••".into() }
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
        let close = ui::round_button("close-settings").size(px(28.0)).border_1().border_color(theme::hairline())
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_panels(window, cx)))
            .child(ui::icon("icons/close.svg", 12.0, theme::body()));
        let header = ui::panel_header().child(tabs).child(close);
        let body = match tab {
            Tab::Model => self.model_tab(cx).into_any_element(),
            Tab::Listening => self.listening_tab(cx).into_any_element(),
            Tab::Keys => self.keys_tab().into_any_element(),
            Tab::Window => self.window_tab(cx).into_any_element(),
            Tab::History => self.history_tab(cx).into_any_element(),
        };
        let mut panel = div().relative().w(px(560.0)).flex().flex_col().rounded(px(18.0)).bg(theme::glass())
            .border_1().border_color(theme::hairline()).overflow_hidden()
            .child(header)
            .child(div().id("settings-body").flex().flex_col().px(px(18.0)).pt(px(14.0)).pb(px(20.0)).max_h(px(470.0)).overflow_y_scroll().child(body));
        if let Some(warning) = self.store.warning {
            panel = panel.child(div().px(px(18.0)).pb(px(12.0)).text_size(px(12.0)).text_color(gpui::rgb(0xffb4a8)).child(warning));
        }
        panel.child(self.hits.mark())
    }

    pub(crate) fn toggle_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        self.open_picker = if self.open_picker == Some(picker) { None } else { Some(picker) };
        cx.notify();
    }

    pub(crate) fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.open_picker.take().is_some() { cx.notify(); }
    }

    /// A dropdown bound to a string-valued choice. `options` are (id, label); `set` gets the id.
    fn dropdown(&self, picker: Picker, value: impl Into<SharedString>, options: Vec<(String, String)>, selected: &str,
        set: fn(&mut Settings, String), cx: &mut Context<Self>) -> Div {
        let open = self.open_picker == Some(picker);
        let mut face = ui::picker(("picker", picker as usize), value, open).relative()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| { cx.stop_propagation(); this.toggle_picker(picker, cx) }));
        let mut wrapper = div().relative().w_full();
        if open {
            // The face records where it is, so the list's "click outside" ignores clicks on it
            // (the face's own handler closes the list).
            // The list hangs 40 px below the face, the same width, up to its scroll limit. Its
            // area is reserved now so the window region shows it in the first frame.
            let face_bounds = self.picker_face.clone();
            let hits = self.hits.clone();
            let list_height = px((options.len() as f32 * 34.0 + 12.0).min(MENU_MAX_HEIGHT + 12.0));
            face = face.child(gpui::canvas(move |bounds, _, _| {
                face_bounds.set(Some(bounds));
                hits.reserve(gpui::Bounds::new(gpui::point(bounds.left(), bounds.bottom() + px(4.0)), gpui::size(bounds.size.width, list_height)));
            }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
            let face_bounds = self.picker_face.clone();
            let mut list = ui::menu().id("menu").relative().w_full().max_h(px(MENU_MAX_HEIGHT)).overflow_y_scroll()
                .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if face_bounds.get().is_some_and(|face| face.contains(&event.position)) { return; }
                    this.close_picker(cx);
                }));
            for (index, (id, label)) in options.into_iter().enumerate() {
                let chosen = id.clone();
                list = list.child(ui::menu_item(("item", index), label, id == selected)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_picker(cx);
                        let value = chosen.clone();
                        this.update_settings(|s| set(s, value), window, cx);
                    })));
            }
            // Floats over whatever follows: painted after the tree (`deferred`), and marked so
            // the overlay's window region and mouse hit-testing cover it.
            list = list.child(self.hits.mark());
            wrapper = wrapper.child(deferred(div().absolute().top(px(40.0)).left_0().w_full().child(list)));
        }
        wrapper.child(face)
    }

    fn model_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let detail = |provider: Provider, cli: &str, status: Option<&SubscriptionStatus>| match status.and_then(|st| st.plan.as_deref()).filter(|_| status.is_some_and(|st| st.signed_in)) {
            Some(plan) => format!("{cli} · {}", plan_label(provider, plan)),
            None => cli.to_string(),
        };
        let providers = [
            (Provider::Codex, "ChatGPT subscription", detail(Provider::Codex, "Codex CLI", self.codex_status.as_ref())),
            (Provider::Claude, "Claude subscription", detail(Provider::Claude, "Claude Code", self.claude_status.as_ref())),
            (Provider::ApiKey, "Your API key", "Anthropic, OpenAI, OpenRouter, Gemini, Ollama…".to_string()),
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
                let mut options = vec![(String::new(), "Default".to_string())];
                options.extend(models.iter().cloned());
                let value = models.iter().find(|(id, _)| *id == s.codex_model).map(|(_, name)| name.clone())
                    .unwrap_or_else(|| if s.codex_model.is_empty() { "Default".into() } else { s.codex_model.clone() });
                field("Model", self.dropdown(Picker::CodexModel, value, options, &s.codex_model, |s, v| s.codex_model = v, cx))
            }
            Provider::Claude => {
                let options = [(ClaudeModel::Sonnet, "Sonnet"), (ClaudeModel::Opus, "Opus"), (ClaudeModel::Haiku, "Haiku")];
                let value = options.iter().find(|(m, _)| *m == s.claude_model).map(|(_, l)| *l).unwrap_or("Sonnet");
                field("Model", self.dropdown(Picker::ClaudeModel, value, options.iter().map(|(m, l)| (m.id().to_string(), l.to_string())).collect(), s.claude_model.id(),
                    |s, v| s.claude_model = match v.as_str() { "opus" => ClaudeModel::Opus, "haiku" => ClaudeModel::Haiku, _ => ClaudeModel::Sonnet }, cx))
            }
            Provider::ApiKey => self.api_model_field(cx),
        };
        let styles = [(AnswerStyle::Spoken, "Words I can say aloud"), (AnswerStyle::Standard, "Standard explanations")];
        let style_value = styles.iter().find(|(st, _)| *st == s.answer_style).map(|(_, l)| *l).unwrap_or("Words I can say aloud");
        let style = field("Answer style", self.dropdown(Picker::AnswerStyle, style_value,
            styles.iter().map(|(st, l)| (format!("{st:?}"), l.to_string())).collect(), &format!("{:?}", s.answer_style),
            |s, v| s.answer_style = if v == "Standard" { AnswerStyle::Standard } else { AnswerStyle::Spoken }, cx));
        let mut tab = div().flex().flex_col().gap(px(16.0)).child(list).child(div().flex().gap(px(12.0)).child(model).child(style));
        if s.provider == Provider::ApiKey { tab = tab.child(self.api_key_section(cx)); }
        tab
    }

    /// Trailing status for a provider row: the account (masked until clicked), a Sign in
    /// button, or a hint.
    fn connection_badge(&self, provider: Provider, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let text = |label: String, color: gpui::Rgba| div().flex_none().text_size(px(12.0)).text_color(color).child(label).into_any_element();
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
            Some(st) if st.signed_in => {
                let account = st.account.clone().unwrap_or("Connected".into());
                let shown = if self.reveal_accounts { account.clone() } else { masked(&account) };
                let mut badge = div().flex().items_center().gap(px(6.0));
                if self.reveal_accounts && provider == Provider::Codex {
                    badge = badge.child(div().id(("sign-out", index)).cursor_pointer().text_size(px(12.0)).text_color(theme::muted()).hover(|t| t.text_color(theme::body()))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.sign_out_codex(window, cx) }))
                        .child("Sign out"));
                }
                badge.child(div().id(("account", index)).cursor_pointer().flex().items_center().gap(px(6.0))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.reveal_accounts = !this.reveal_accounts; cx.notify(); }))
                        .child(div().font_family(theme::MONO).text_size(px(12.0)).text_color(theme::ok()).px(px(8.0)).py(px(3.0)).rounded(px(6.0)).bg(theme::raised()).child(shown))
                        .child(ui::icon(if self.reveal_accounts { "icons/eye-off.svg" } else { "icons/eye.svg" }, 14.0, theme::muted())))
                    .into_any_element()
            }
            Some(st) if !st.installed => text("CLI not installed".into(), theme::muted()),
            Some(_) => div().id(("sign-in", index)).flex_none().px(px(10.0)).py(px(5.0)).rounded(px(8.0)).bg(theme::accent()).cursor_pointer()
                .text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink())
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { cx.stop_propagation(); this.sign_in(provider, window, cx) }))
                .child(if self.signing_in { "Waiting for browser…" } else { "Sign in" }).into_any_element(),
        }
    }

    /// Model for "Your API key": a picker over loaded or suggested models, plus a free-text id.
    fn api_model_field(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let suggestions: Vec<String> = match crate::providers::preset(&s.api_provider) {
            Some(preset) if self.loaded_models.is_empty() => preset.suggested_models.iter().map(|m| m.to_string()).collect(),
            Some(_) => self.loaded_models.iter().take(60).cloned().collect(),
            None => Vec::new(),
        };
        let value = if s.api_model().is_empty() { "Choose a model".to_string() } else { s.api_model().to_string() };
        let options = suggestions.into_iter().map(|m| (m.clone(), m)).collect();
        field("Model", self.dropdown(Picker::ApiModel, value, options, s.api_model(), |s, v| { s.api_models.insert(s.api_provider.clone(), v); }, cx))
    }

    fn api_key_section(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let presets: Vec<(String, String)> = crate::providers::PRESETS.iter().map(|p| (p.id.to_string(), p.label.to_string())).collect();
        let current = crate::providers::preset(&s.api_provider).map(|p| p.label).unwrap_or("Choose a provider");
        let provider = field("Provider", self.dropdown(Picker::ApiProvider, current, presets, &s.api_provider, |s, v| s.api_provider = v, cx));
        let Some(preset) = crate::providers::preset(&s.api_provider) else { return provider };
        let input_box = |input: gpui::Entity<crate::input::TextInput>, id: &'static str| {
            let focus = input.clone();
            div().id(id).flex_1().min_w_0().px(px(10.0)).py(px(7.0)).rounded(px(9.0))
                .bg(theme::field()).border_1().border_color(theme::hairline()).cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, window, cx| window.focus(&focus.focus_handle(cx))))
                .child(input)
        };
        let mut section = div().flex().flex_col().gap(px(16.0));
        let mut top = div().flex().gap(px(12.0)).child(provider);
        if preset.needs_key {
            let saved = crate::secrets::hint(preset.id);
            let mut status = div().flex().items_center().gap(px(12.0)).text_size(px(12.0)).text_color(theme::muted())
                .child(match &saved { Some(hint) => format!("Saved key {hint}"), None => "No key saved".to_string() });
            if saved.is_some() {
                status = status.child(div().id("remove-key").cursor_pointer().text_color(gpui::rgb(0xffb4a8))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.remove_key(cx))).child("Remove"));
            }
            if !preset.key_page.is_empty() {
                let page = preset.key_page;
                status = status.child(div().id("key-page").cursor_pointer().text_color(theme::accent_soft())
                    .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| cx.open_url(page))).child("Get a key ↗"));
            }
            top = top.child(field("API key", div().flex().flex_col().gap(px(6.0))
                .child(div().flex().items_center().gap(px(8.0))
                    .child(input_box(self.key_input.clone(), "key-box"))
                    .child(button("save-key", "Save", true).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.save_key(cx)))))
                .child(status)));
        }
        section = section.child(top);
        if preset.base_url.is_empty() {
            section = section.child(field("Base URL (OpenAI-compatible)", input_box(self.base_url_input.clone(), "base-url-box")));
        }
        let mut model_row = div().flex().items_center().gap(px(8.0)).child(input_box(self.model_input.clone(), "model-box"))
            .child(button("load-models", if self.models_loading { "Loading…" } else { "Load models" }, false)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.load_models(window, cx))));
        if let Some(notice) = self.key_notice.clone() {
            model_row = model_row.child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice));
        }
        section.child(field("Custom model id", model_row))
    }

    fn listening_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let transcribe = div().flex().items_center().gap(px(12.0))
            .child(div().flex().flex_col().gap(px(1.0)).flex_1()
                .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Transcribe conversations"))
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child("On this PC, while Live is on")))
            .child(switch("transcribe", s.transcribe, |s, v| s.transcribe = v, cx));
        let devices = self.devices.clone().unwrap_or_default();
        let device_options = |names: &[String], default: Option<&String>| {
            let mut options = vec![(String::new(), match default { Some(name) => format!("Default · {name}"), None => "Default".to_string() })];
            options.extend(names.iter().map(|name| (name.clone(), name.clone())));
            options
        };
        let value = |chosen: &str, default: Option<&String>| if chosen.is_empty() {
            match default { Some(name) => format!("Default · {name}"), None => if self.devices_loading { "Loading devices…".into() } else { "Default".into() } }
        } else { chosen.to_string() };
        let device_column = |label: &'static str, name: &'static str, on: bool, set_on: fn(&mut Settings, bool), picker: Picker, chosen: &str,
            names: &[String], default: Option<&String>, set: fn(&mut Settings, String), cx: &mut Context<Self>| {
            div().flex().flex_col().gap(px(6.0)).flex_1().min_w_0()
                .child(div().flex().items_center().justify_between().h(px(20.0))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child(label))
                    .child(switch(name, on, set_on, cx)))
                .child(self.dropdown(picker, value(chosen, default), device_options(names, default), chosen, set, cx))
        };
        let columns = div().flex().gap(px(12.0))
            .child(device_column("Microphone", "listen-mic", s.listen_mic, |s, v| s.listen_mic = v, Picker::Mic, &s.mic_device,
                &devices.microphones, devices.default_microphone.as_ref(), |s, v| s.mic_device = v, cx))
            .child(device_column("Desktop audio", "listen-desktop", s.listen_desktop, |s, v| s.listen_desktop = v, Picker::Desktop, &s.desktop_device,
                &devices.playback, devices.default_playback.as_ref(), |s, v| s.desktop_device = v, cx));
        let providers = [(SttProvider::Parakeet, "Parakeet Realtime · on this PC"), (SttProvider::Deepgram, "Deepgram · cloud")];
        let current = providers.iter().find(|(p, _)| *p == s.stt_provider).map(|(_, l)| *l).unwrap_or("Parakeet Realtime · on this PC");
        let with = field("Transcribe with", self.dropdown(Picker::SttProvider, current,
            providers.iter().map(|(p, l)| (format!("{p:?}"), l.to_string())).collect(), &format!("{:?}", s.stt_provider),
            |s, v| s.stt_provider = if v == "Deepgram" { SttProvider::Deepgram } else { SttProvider::Parakeet }, cx));
        let provider_row = match s.stt_provider {
            SttProvider::Parakeet => self.model_row(cx),
            SttProvider::Deepgram => self.deepgram_key_row(cx),
        };
        div().flex().flex_col().gap(px(16.0)).child(transcribe).child(with).child(provider_row).child(columns)
    }

    /// The Deepgram API key: paste and save, or the saved key's hint with Remove.
    fn deepgram_key_row(&self, cx: &mut Context<Self>) -> Div {
        let focus = self.key_input.clone();
        let saved = crate::secrets::hint(deepgram::PROVIDER_ID);
        let mut status = div().flex().items_center().gap(px(12.0)).text_size(px(12.0)).text_color(theme::muted())
            .child(match &saved { Some(hint) => format!("Saved key {hint} · audio is sent to Deepgram while Live is on"), None => "No key saved".to_string() });
        if saved.is_some() {
            status = status.child(div().id("remove-key").cursor_pointer().text_color(gpui::rgb(0xffb4a8))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.remove_key(cx))).child("Remove"));
        }
        status = status.child(div().id("key-page").cursor_pointer().text_color(theme::accent_soft())
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.open_url("https://console.deepgram.com/"))).child("Get a key ↗"));
        let mut row = div().flex().flex_col().gap(px(6.0))
            .child(div().flex().items_center().gap(px(8.0))
                .child(div().id("key-box").flex_1().min_w_0().px(px(10.0)).py(px(7.0)).rounded(px(9.0))
                    .bg(theme::field()).border_1().border_color(theme::hairline()).cursor_text()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, window, cx| window.focus(&focus.focus_handle(cx))))
                    .child(self.key_input.clone()))
                .child(button("save-key", "Save", true).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.save_key(cx)))))
            .child(status);
        if let Some(notice) = self.key_notice.clone() {
            row = row.child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice));
        }
        field("Deepgram API key", row)
    }

    /// The Parakeet model: installed, downloading (with progress and Cancel), or a Download button.
    fn model_row(&self, cx: &mut Context<Self>) -> Div {
        let trailing: gpui::AnyElement = if let Some(progress) = self.download_progress() {
            div().flex().items_center().gap(px(10.0))
                .child(div().w(px(120.0)).h(px(6.0)).rounded_full().bg(theme::hairline())
                    .child(div().h_full().rounded_full().bg(theme::accent()).w(px(120.0 * progress.clamp(0.0, 1.0) as f32))))
                .child(div().w(px(36.0)).font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(format!("{:.0}%", progress * 100.0)))
                .child(button("cancel-download", "Cancel", false).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { this.cancel_download(); cx.notify(); })))
                .into_any_element()
        } else if self.model_installed {
            div().text_size(px(12.0)).text_color(theme::ok()).child("Installed").into_any_element()
        } else {
            button("download-model", format!("Download {}", model_size_label(&MODEL)), true)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.download_model(window, cx)))
                .into_any_element()
        };
        let mut row = div().flex().flex_col().gap(px(6.0))
            .child(div().flex().items_center().gap(px(12.0)).px(px(12.0)).py(px(10.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
                .child(div().flex().flex_col().gap(px(1.0)).flex_1().min_w_0()
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Parakeet Realtime · English"))
                    .child(div().id("model-license").cursor_pointer().text_size(px(12.0)).text_color(theme::muted()).truncate()
                        .hover(|t| t.text_color(theme::accent_soft()))
                        .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.open_url(MODEL.license_url)))
                        .child(format!("{} · NVIDIA Open Model License", model_size_label(&MODEL)))))
                .child(trailing));
        if let Some(notice) = self.model_notice.clone() {
            row = row.child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice));
        }
        row
    }

    /// Enumerate audio devices off the UI thread for the Listening tab's pickers.
    pub(crate) fn load_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.devices_loading { return; }
        self.devices_loading = true;
        cx.spawn_in(window, async move |this, cx| {
            let devices = cx.background_executor().spawn(async move { crate::audio::list_devices() }).await;
            let _ = this.update(cx, |this, cx| { this.devices = Some(devices); this.devices_loading = false; cx.notify(); });
        }).detach();
    }

    /// Size the saved sessions folder off the UI thread for the History tab.
    pub(crate) fn refresh_archive_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.archive.as_ref().map(|archive| archive.root().to_path_buf()) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let bytes = cx.background_executor().spawn(async move { folder_bytes(&root) }).await;
            let _ = this.update(cx, |this, cx| { this.archive_bytes = Some(bytes); cx.notify(); });
        }).detach();
    }

    fn keys_tab(&self) -> Div {
        let mut list = div().flex().flex_col();
        for (action, _) in DEFAULTS {
            let label = match action {
                Action::Live => "Start / stop Live", Action::Focus => "Type a question", Action::Assist => "Assist",
                Action::Toggle => "Show / hide overlay", Action::MoveUp => "Move up", Action::MoveDown => "Move down",
                Action::MoveLeft => "Move left", Action::MoveRight => "Move right",
                Action::ScrollUp => "Scroll answer up", Action::ScrollDown => "Scroll answer down",
                Action::Close => "Close settings",
            };
            let taken = self.hotkeys.unavailable.contains(action);
            list = list.child(ui::setting_row(label, div().flex().items_center().gap(px(8.0))
                .when(taken, |row| row.child(div().text_size(px(12.0)).text_color(gpui::rgb(0xffb4a8)).child("Used by another app")))
                .child(ui::keycap(self.hotkeys.label(*action)))).h(px(40.0)));
        }
        list
    }

    fn history_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut rows = div().flex().flex_col()
            .child(ui::setting_row("Save sessions", switch("save-sessions", s.save_sessions, |s, v| s.save_sessions = v, cx)));
        if s.save_sessions {
            rows = rows
                .child(ui::setting_row("Save screenshots", switch("save-screenshots", s.save_screenshots, |s, v| s.save_screenshots = v, cx)))
                .child(ui::setting_row("Keep for", choice("retention",
                    &[(Retention::Days7, "7 days"), (Retention::Days30, "30 days"), (Retention::Forever, "Forever")],
                    s.keep_sessions, |s, v| s.keep_sessions = v, cx)).h(px(52.0)).border_b_0());
        }
        let size = self.archive_bytes.map(|bytes| match bytes {
            0 => "Nothing saved yet".to_string(),
            b if b < 1_000_000 => format!("{} KB on this PC", (b as f64 / 1e3).ceil()),
            b => format!("{:.0} MB on this PC", b as f64 / 1e6),
        }).unwrap_or_default();
        rows.child(div().flex().items_center().justify_between().pt(px(10.0))
            .child(div().flex().gap(px(8.0))
                .child(button("view-history", "Open sessions", true).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.open_sessions(cx))))
                .child(button("open-archive", "Open folder", false).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.reveal_archive()))))
            .child(div().text_size(px(12.0)).text_color(theme::muted()).child(size)))
    }

    fn window_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        div().flex().flex_col()
            .child(ui::setting_row("Hide from screen capture", switch("hide-capture", s.hide_from_capture, |s, v| s.hide_from_capture = v, cx)))
            .child(ui::setting_row("Attach a screenshot to every message", switch("screen-on-send", s.screen_on_send, |s, v| s.screen_on_send = v, cx)))
            .child(ui::setting_row("Start Live when the app opens", switch("live-on-launch", s.start_live_on_launch, |s, v| s.start_live_on_launch = v, cx)).border_b_0())
    }
}

/// Total size of a folder tree (sessions and their screenshots).
fn folder_bytes(root: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() { pending.push(entry.path()); } else { total += meta.len(); }
        }
    }
    total
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_and_accounts_read_as_intended() {
        assert_eq!(plan_label(Provider::Codex, "plus"), "ChatGPT Plus");
        assert_eq!(plan_label(Provider::Codex, "chatgpt_pro"), "ChatGPT Pro");
        assert_eq!(plan_label(Provider::Claude, "max_5x"), "Max 5x");
        assert_eq!(masked("martinezjay404@gmail.com"), format!("m{}", "•".repeat(15)));
        assert_eq!(masked("ab"), "a••••••");
        assert_eq!(masked(""), "••••••");
    }

    #[test]
    fn folder_size_counts_every_file() {
        let dir = std::env::temp_dir().join(format!("cluelyrs-size-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("a.bin"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("inner").join("b.bin"), [0u8; 5]).unwrap();
        assert_eq!(folder_bytes(&dir), 15);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
