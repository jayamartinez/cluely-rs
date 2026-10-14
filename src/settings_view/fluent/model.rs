//! Settings › Model (Paper "Settings · Model"): what answers (a subscription or your API key, whose
//! setup is the API key card's expander), how (model, style, smart mode, answering early), and the
//! selected subscription's CLI: its account and where it is.

use gpui::{AnyElement, Context, Div, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, div, prelude::*, px};

use super::{
    button, card, card_icon, card_label, chevron, dot_status, error_text, expander, expander_header, expander_row, external, in_use, pill,
    section, status, text_box, title,
};
use crate::overlay::Overlay;
use crate::settings::{AnswerStyle, Provider};
use crate::settings_view::{Picker, cli_location, key_eye, listen, masked, plan_label, reveal, saved_key_label};
use crate::{theme, ui};

/// The API key card's logo strip: these, then "+ N more".
const STRIP: [&str; 4] = ["anthropic", "openai", "gemini", "openrouter"];

/// A provider's logo (the provider ComboBox, the API key card).
fn provider_logo(id: &str) -> &'static str {
    match id {
        "anthropic" => "logos/anthropic.svg", "openai" => "logos/openai.svg", "openrouter" => "logos/openrouter.svg",
        "gemini" => "logos/googlegemini.svg", "xai" => "logos/xai.svg", "groq" => "logos/groq.svg", "deepseek" => "logos/deepseek.svg",
        "mistral" => "logos/mistral.svg", "together" => "logos/together.svg", "ollama" => "logos/ollama.svg", "lmstudio" => "logos/lmstudio.svg",
        _ => "icons/plug.svg",
    }
}

impl Overlay {
    pub(super) fn fluent_model(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut providers = vec![self.fluent_subscription_card(Provider::Codex, cx), self.fluent_subscription_card(Provider::Claude, cx), self.fluent_api_key_card(window, cx)];
        // Sign-in errors; the API key's own notices are in its card.
        if let Some(notice) = self.key_notice.clone().filter(|_| s.provider != Provider::ApiKey) {
            providers.push(div().px(px(16.0)).pt(px(4.0)).text_size(px(12.0)).text_color(theme::accent_soft()).child(notice).into_any_element());
        }
        let choice = self.model_choice();
        let styles = [(AnswerStyle::Spoken, "Words I can say aloud"), (AnswerStyle::Standard, "Standard explanations")];
        let style = styles.iter().find(|(style, _)| *style == s.answer_style).map_or(styles[0].1, |(_, label)| *label);
        let combo_card = |id: &'static str, icon: &'static str, label: &'static str, combo: Div| card(id).child(card_icon(icon)).child(card_label(label, None)).child(combo).into_any_element();
        let answers = vec![
            combo_card("model", "icons/setup-sparkle.svg", "Model",
                self.fluent_combo(choice.picker, None, choice.value, choice.options, &choice.selected, choice.set, 200.0, cx)),
            combo_card("answer-style", "icons/type.svg", "Answer style",
                self.fluent_combo(Picker::AnswerStyle, None, style, styles.iter().map(|(style, label)| (format!("{style:?}"), label.to_string())).collect(),
                    &format!("{:?}", s.answer_style), |s, v| s.answer_style = if v == "Standard" { AnswerStyle::Standard } else { AnswerStyle::Spoken }, 200.0, cx)),
            self.toggle_card("smart-mode", "icons/setup-bulb.svg", "Smart mode", s.smart_mode, |s, v| s.smart_mode = v, cx),
            self.toggle_card("speculative-answers", "icons/setup-bolt.svg", "Answer early", s.speculative_answers, |s, v| s.speculative_answers = v, cx),
            self.toggle_card("auto-answer", "icons/setup-chat.svg", "Show answers automatically", s.auto_answer, |s, v| s.auto_answer = v, cx),
        ];
        div().flex().flex_col().gap(px(24.0))
            .child(title("Model"))
            .child(section("Answer with", providers))
            .child(section("Answers", answers))
            .children(self.fluent_cli_section(cx))
    }

    /// ChatGPT or Claude: the CLI and plan, then Sign in, Use, or In use.
    fn fluent_subscription_card(&self, provider: Provider, cx: &mut Context<Self>) -> AnyElement {
        let (index, title, cli, logo) = match provider {
            Provider::Codex => (0usize, "ChatGPT subscription", "Codex CLI", "logos/openai.svg"),
            _ => (1, "Claude subscription", "Claude Code", "logos/claude.svg"),
        };
        let status_now = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let line = match status_now {
            None => format!("{cli} · Checking…"),
            Some(st) if !st.installed => format!("{cli} · Not found"),
            Some(st) if !st.signed_in => format!("{cli} · Not signed in"),
            // "Plus", not "ChatGPT Plus": the card already says ChatGPT.
            Some(st) => match st.plan.as_deref() {
                Some(plan) => { let plan = plan_label(provider, plan); format!("{cli} · {}", plan.strip_prefix("ChatGPT ").unwrap_or(&plan)) }
                None => cli.to_string(),
            },
        };
        let selected = self.store.value.provider == provider;
        let needs_sign_in = status_now.is_some_and(|st| st.installed && !st.signed_in);
        let waiting = self.signing_in == Some(provider);
        let card = card(("provider", index)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.update_settings(|s| s.provider = provider, window, cx)))
            .child(ui::icon(logo, 20.0, theme::text()))
            .child(card_label(title, Some(status(line).into_any_element())))
            .when(needs_sign_in, |card| card.child(button(("sign-in", index), if waiting { "Waiting for browser…" } else { "Sign in" }, true)
                .when(waiting, |button| button.opacity(0.7))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.sign_in(provider, window, cx) }))))
            .child(if selected { pill("In use").into_any_element() } else {
                button(("use", index), "Use", false)
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.update_settings(|s| s.provider = provider, window, cx) }))
                    .into_any_element()
            });
        if selected { in_use(card).into_any_element() } else { card.into_any_element() }
    }

    /// Your API key: the logo strip, In use / Use / Set up…, and the setup itself while open.
    fn fluent_api_key_card(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.store.value;
        let selected = s.provider == Provider::ApiKey;
        let open = self.fluent_expanded("api-key");
        let ready = crate::providers::preset(&s.api_provider).is_some_and(|p| !p.needs_key || s.saved_keys.contains_key(p.id));
        let more = crate::providers::PRESETS.len() - STRIP.len();
        let strip = div().flex().items_center().gap(px(8.0)).h(px(16.0))
            .children(STRIP.iter().map(|id| ui::icon(provider_logo(id), 12.0, theme::muted())))
            .child(status(format!("+ {more} more · Credential Manager")));
        let action = if selected { pill("In use").into_any_element() } else if ready {
            button("use-api-key", "Use", false)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| { cx.stop_propagation(); this.update_settings(|s| s.provider = Provider::ApiKey, window, cx) }))
                .into_any_element()
        } else {
            button("set-up-api-key", "Set up…", false)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { cx.stop_propagation(); if !this.fluent_expanded("api-key") { this.fluent_toggle("api-key", cx) } }))
                .into_any_element()
        };
        let header = expander_header().id("api-key-header").cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.fluent_toggle("api-key", cx)))
            .child(card_icon("icons/setup-key.svg"))
            .child(card_label("Your API key", Some(div().pt(px(2.0)).child(strip).into_any_element())))
            .child(action)
            .child(chevron(open));
        let shell = expander("api-key", header, open.then(|| self.fluent_api_key_rows(window, cx)));
        if selected { shell.bg(theme::fluent_in_use()).border_color(theme::bubble_border()).into_any_element() } else { shell.into_any_element() }
    }

    /// The API key's setup: provider, key, base URL (custom endpoints) and model id.
    fn fluent_api_key_rows(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = &self.store.value;
        let presets: Vec<(String, String)> = crate::providers::PRESETS.iter().map(|p| (p.id.to_string(), p.label.to_string())).collect();
        let current = crate::providers::preset(&s.api_provider).map_or("Choose a provider", |p| p.label);
        let label = |text: &'static str, line: Option<AnyElement>| card_label(text, line);
        let mut rows = vec![expander_row("api-provider").child(label("Provider", None))
            .child(self.fluent_combo(Picker::ApiProvider, Some(provider_logo), current, presets, &s.api_provider, |s, v| s.api_provider = v, 240.0, cx))
            .into_any_element()];
        let Some(preset) = crate::providers::preset(&s.api_provider) else { return rows };
        if preset.needs_key {
            let saved = s.saved_keys.get(preset.id).cloned();
            let mut line = div().flex().items_center().gap(px(10.0)).text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted());
            if let Some(key) = self.revealed_key(preset.id) {
                line = line.child(reveal::revealed_text(key)).child(self.saved_key_eye(preset.id, cx)).children(self.reveal_countdown(true));
            } else {
                line = line.child(saved_key_label(saved.as_deref()));
                if saved.is_some() {
                    line = line.child(self.saved_key_eye(preset.id, cx))
                        .child(div().id("remove-key").cursor_pointer().text_color(theme::accent_soft())
                            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.remove_key(window, cx))).child("Remove"));
                }
                if !preset.key_page.is_empty() {
                    let page = preset.key_page;
                    line = line.child(div().id("key-page").cursor_pointer().text_color(theme::accent_soft())
                        .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page))).child("Get a key ↗"));
                }
            }
            rows.push(expander_row("api-key-field").py(px(10.0))
                .child(label("API key", Some(line.into_any_element())))
                .child(text_box("key-box", &self.key_input, window, cx).w(px(200.0)).child(key_eye(&self.key_input, cx)))
                .child(button("save-key", "Save", true).on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_key(window, cx))))
                .into_any_element());
        }
        if preset.base_url.is_empty() {
            rows.push(expander_row("api-base-url").child(label("Base URL", None))
                .child(text_box("base-url-box", &self.base_url_input, window, cx).w(px(280.0))).into_any_element());
        }
        let notice = self.key_notice.clone().map(|notice| div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::accent_soft()).child(notice).into_any_element());
        rows.push(expander_row("api-model").py(px(10.0))
            .child(label("Model id", notice))
            .child(text_box("model-box", &self.model_input, window, cx).w(px(200.0)))
            .child(button("load-models", if self.models_loading { "Loading…" } else { "Load models" }, false)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.load_models(window, cx))))
            .into_any_element());
        rows
    }

    /// The selected subscription's CLI: its account (masked until shown) and where it is.
    fn fluent_cli_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        let provider = self.store.value.provider;
        let cli = cli_location::details(provider)?;
        let status_now = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let mut cards = Vec::new();
        if let Some(st) = status_now.filter(|st| st.signed_in) {
            let account = st.account.clone().unwrap_or("Connected".into());
            let shown = if self.reveal_accounts { account } else { masked(&account) };
            let eye = div().id("account-eye").size(px(32.0)).flex_none().flex().items_center().justify_center().rounded(px(4.0)).cursor_pointer()
                .hover(|eye| eye.bg(theme::fluent_selected()))
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { this.reveal_accounts = !this.reveal_accounts; cx.notify(); }))
                .child(ui::icon(if self.reveal_accounts { "icons/eye-off.svg" } else { "icons/eye.svg" }, 16.0, theme::body()));
            cards.push(card("account").child(card_icon("icons/setup-person.svg"))
                .child(card_label("Account", Some(status(shown).font_family(theme::MONO).into_any_element())))
                .child(eye)
                .when(provider == Provider::Codex, |card| card.child(button("sign-out", "Sign out", false)
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.sign_out_codex(window, cx)))))
                .into_any_element());
        }
        let open = self.fluent_expanded("cli-location");
        let line = match status_now {
            None => status("Checking…").into_any_element(),
            Some(st) if !st.installed => dot_status("Not found", theme::placeholder()).into_any_element(),
            Some(st) if st.custom_location => dot_status("Custom location", theme::accent_soft()).into_any_element(),
            Some(_) => dot_status("Found automatically", theme::ok()).into_any_element(),
        };
        let header = expander_header().id("cli-location-header").cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
                this.fluent_toggle("cli-location", cx);
                if this.fluent_expanded("cli-location") { this.fluent_find_cli(provider, window, cx); }
            }))
            .child(card_icon("icons/setup-terminal.svg"))
            .child(card_label("Location", Some(line)))
            .child(chevron(open));
        let body = open.then(|| {
            let custom = status_now.is_some_and(|st| st.custom_location);
            let saved = if provider == Provider::Codex { &self.store.value.codex_path } else { &self.store.value.claude_path };
            let path: SharedString = if custom && !saved.is_empty() { saved.clone().into() } else {
                match (&self.fluent.cli_path, status_now) {
                    (Some((found_for, path)), _) if *found_for == provider => path.clone(),
                    (_, Some(st)) if !st.installed => "Not found on this PC".into(),
                    _ => "Looking…".into(),
                }
            };
            let error = self.cli_error.as_ref().filter(|(shown_for, _)| *shown_for == provider).map(|(_, message)| error_text(message.clone()));
            let page = cli.install_page;
            vec![
                expander_row("cli-path").py(px(10.0))
                    .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
                        .child(div().truncate().font_family(theme::MONO).text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body()).child(path))
                        .children(error))
                    .when(custom, |row| row.child(button("cli-automatic", "Use automatic", false)
                        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.set_cli_location(provider, String::new(), window, cx)))))
                    .child(button("cli-locate", "Locate…", false)
                        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.locate_cli(provider, window, cx))))
                    .into_any_element(),
                expander_row("cli-install").cursor_pointer().hover(|row| row.bg(theme::raised()))
                    .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page)))
                    .child(div().flex_1().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::text()).child("How to install"))
                    .child(external(12.0, theme::body()))
                    .into_any_element(),
            ]
        });
        cards.push(expander("cli-location", header, body).into_any_element());
        Some(section(cli.name, cards))
    }

    /// Look up where the subscription's CLI is (it reads the disk, so off the UI thread).
    fn fluent_find_cli(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let found = cx.background_executor().spawn(async move {
                match provider { Provider::Codex => crate::codex::executable(), _ => crate::claude_cli::ClaudeCli::executable() }
            }).await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(path) = found { this.fluent.cli_path = Some((provider, path.display().to_string().into())); }
                cx.notify();
            });
        }).detach();
    }
}
