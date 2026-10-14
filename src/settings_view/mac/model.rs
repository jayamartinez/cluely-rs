//! Settings › Model: what answers (a subscription or an API key, the one in use marked), then how
//! it answers. An API key's provider, key and model are set in the box under its row.

use gpui::{AnyElement, Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Styled, div, prelude::*, px};

use super::{action, label, link, row, section, status, switch};
use crate::chat::SubscriptionStatus;
use crate::overlay::Overlay;
use crate::settings::{AnswerStyle, Provider};
use crate::settings_view::cli_location::details;
use crate::settings_view::{Picker, listen, plan_label};
use crate::ui_v3::{self, Kind};
use crate::{providers, theme, ui};

/// The logos after "Your API key"; "+ N more" follows for the other providers.
const API_LOGOS: [&str; 4] = ["logos/anthropic.svg", "logos/openai.svg", "logos/googlegemini.svg", "logos/openrouter.svg"];

impl Overlay {
    pub(super) fn mac_model_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut providers: Vec<AnyElement> = [Provider::Codex, Provider::Claude, Provider::ApiKey].into_iter()
            .map(|provider| self.provider_row(provider, cx).into_any_element()).collect();
        if s.provider == Provider::ApiKey { providers.push(self.connection_group(cx).into_any_element()); }
        let mut answers: Vec<AnyElement> = Vec::new();
        if s.provider != Provider::ApiKey {
            answers.push(row("icons/setup-sparkle.svg", "Model", None, div().w(px(super::DROPDOWN_WIDTH)).flex_none().child(self.subscription_model_picker(cx))).into_any_element());
        }
        let styles = [(AnswerStyle::Spoken, "Words I can say aloud"), (AnswerStyle::Standard, "Standard explanations")];
        let style = styles.iter().find(|(style, _)| *style == s.answer_style).map_or(styles[0].1, |(_, text)| *text);
        answers.push(row("icons/type.svg", "Answer style", None, self.mac_dropdown(Picker::AnswerStyle, None, style,
            styles.iter().map(|(style, text)| (format!("{style:?}"), text.to_string())).collect(), &format!("{:?}", s.answer_style),
            |s, v| s.answer_style = if v == "Standard" { AnswerStyle::Standard } else { AnswerStyle::Spoken }, cx)).into_any_element());
        answers.push(row("icons/setup-bulb.svg", "Smart mode", None, switch("smart-mode", s.smart_mode, |s, v| s.smart_mode = v, cx)).into_any_element());
        answers.push(row("icons/setup-bolt.svg", "Answer early", None, switch("speculative-answers", s.speculative_answers, |s, v| s.speculative_answers = v, cx)).into_any_element());
        answers.push(row("icons/setup-chat.svg", "Show answers automatically", None, switch("auto-answer", s.auto_answer, |s, v| s.auto_answer = v, cx)).into_any_element());
        answers.extend(self.cli_row(cx).map(IntoElement::into_any_element));
        div().flex().flex_col().gap(px(30.0))
            .child(section("Answer with", 14.0, providers))
            .child(section("Answers", 14.0, answers))
    }

    /// A way to answer: its logo, name and account, and Use (In use once chosen).
    fn provider_row(&self, provider: Provider, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let s = &self.store.value;
        let chosen = s.provider == provider;
        let subscription = match provider { Provider::Codex => self.codex_status.as_ref(), Provider::Claude => self.claude_status.as_ref(), Provider::ApiKey => None };
        let (title, logo) = match provider {
            Provider::Codex => ("ChatGPT subscription", "logos/openai.svg"),
            Provider::Claude => ("Claude subscription", "logos/claude.svg"),
            Provider::ApiKey => ("Your API key", "icons/setup-key.svg"),
        };
        let detail: AnyElement = match provider {
            Provider::ApiKey => API_LOGOS.iter().fold(div().flex().items_center().gap(px(9.0)).h(px(16.0)), |strip, logo| strip.child(ui::icon(logo, 13.0, theme::muted())))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(format!("+ {} more", providers::PRESETS.len() - API_LOGOS.len())))
                .into_any_element(),
            _ => status(subscription_detail(provider, subscription)),
        };
        let signed_in = subscription.is_some_and(|status| status.signed_in);
        let control: AnyElement = if provider != Provider::ApiKey && subscription.is_some_and(|status| status.installed && !status.signed_in) {
            // Sign in runs the CLI's login in Terminal; clicking the waiting button stops waiting.
            let waiting = self.signing_in == Some(provider);
            action("sign-in", if waiting { "Waiting for Terminal…" } else { "Sign in" }, if chosen { Kind::Primary } else { Kind::Secondary }, cx,
                move |this, window, cx| this.sign_in(provider, window, cx)).when(waiting, |button| button.opacity(0.7)).into_any_element()
        } else if chosen {
            let sign_out = (provider == Provider::Codex && signed_in).then(|| link("sign-out", "Sign out", false)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| { cx.stop_propagation(); this.sign_out_codex(window, cx) })));
            div().flex().items_center().gap(px(12.0)).children(sign_out).child(in_use()).into_any_element()
        } else {
            action(if provider == Provider::ApiKey { "set-up-api" } else { "use-provider" }, if provider == Provider::ApiKey { "Set up…" } else { "Use" },
                Kind::Secondary, cx, move |this, window, cx| this.update_settings(|s| s.provider = provider, window, cx)).into_any_element()
        };
        let tint = if provider == Provider::ApiKey { ui_v3::tile_ink(chosen) } else { theme::logo_ink() };
        div().id(("provider", provider as usize)).flex().items_center().gap(px(14.0)).px(px(12.0)).py(px(10.0)).rounded(px(ui_v3::RADII.card)).border_1().cursor_pointer()
            .border_color(if chosen { theme::bubble_border() } else { gpui::rgba(0x00000000) })
            .when(chosen, |row| row.bg(theme::chosen()))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.update_settings(|s| s.provider = provider, window, cx)))
            .child(ui_v3::tile(40.0, chosen).child(ui::icon(logo, if provider == Provider::ApiKey { 20.0 } else { 16.0 }, tint)))
            .child(label(title, Some(detail)))
            .child(control)
    }

    /// CLI location, for a subscription: where its CLI was found, with Locate… (or Use automatic).
    fn cli_row(&self, cx: &mut Context<Self>) -> Option<Div> {
        let provider = self.store.value.provider;
        let cli = details(provider)?;
        let found = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let error = self.cli_error.as_ref().filter(|(shown_for, _)| *shown_for == provider).map(|(_, message)| message.clone());
        let (text, custom) = match found {
            None => ("Checking…", false),
            Some(st) if !st.installed => ("Not found", false),
            Some(st) if st.custom_location => ("Custom location", true),
            Some(_) => ("Found automatically", false),
        };
        let detail = match error {
            Some(message) => div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::danger()).child(message).into_any_element(),
            None => status(text),
        };
        let control: AnyElement = if custom {
            action("cli-automatic", "Use automatic", Kind::Secondary, cx, move |this, window, cx| this.set_cli_location(provider, String::new(), window, cx)).into_any_element()
        } else {
            let missing = found.is_some_and(|st| !st.installed);
            let page = cli.install_page;
            div().flex().items_center().gap(px(12.0))
                .when(missing, |controls| controls.child(link("cli-install", "How to install ↗", true)
                    .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page)))))
                .child(action("cli-locate", "Locate…", if missing { Kind::Primary } else { Kind::Secondary }, cx, move |this, window, cx| this.locate_cli(provider, window, cx)))
                .into_any_element()
        };
        Some(row("icons/setup-terminal.svg", "CLI location", Some(detail), control))
    }
}

/// "Codex CLI · Plus", or what's missing.
fn subscription_detail(provider: Provider, status: Option<&SubscriptionStatus>) -> String {
    let cli = if provider == Provider::Codex { "Codex CLI" } else { "Claude Code" };
    match status {
        None => format!("{cli} · Checking…"),
        Some(st) if !st.installed => format!("{cli} · Not found"),
        Some(st) if !st.signed_in => format!("{cli} · Not signed in"),
        Some(st) => match st.plan.as_deref() { Some(plan) => format!("{cli} · {}", plan_label(provider, plan).trim_start_matches("ChatGPT ")), None => cli.to_string() },
    }
}

/// The pill on the provider in use.
fn in_use() -> Div {
    div().flex().flex_none().items_center().gap(px(5.0)).px(px(9.0)).py(px(4.0)).rounded_full().bg(theme::accent())
        .child(ui::icon("icons/check.svg", 12.0, theme::accent_ink()))
        .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::BOLD).text_color(theme::accent_ink()).child("In use"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscription_reads_as_its_cli_and_plan_or_what_is_missing() {
        let status = |installed, signed_in, plan: Option<&str>| SubscriptionStatus { installed, signed_in, plan: plan.map(str::to_string), ..SubscriptionStatus::default() };
        assert_eq!(subscription_detail(Provider::Codex, Some(&status(true, true, Some("plus")))), "Codex CLI · Plus");
        assert_eq!(subscription_detail(Provider::Claude, Some(&status(true, true, Some("max_5x")))), "Claude Code · Max 5x");
        assert_eq!(subscription_detail(Provider::Claude, Some(&status(true, false, None))), "Claude Code · Not signed in");
        assert_eq!(subscription_detail(Provider::Codex, Some(&status(false, false, None))), "Codex CLI · Not found");
        assert_eq!(subscription_detail(Provider::Codex, None), "Codex CLI · Checking…");
    }
}
