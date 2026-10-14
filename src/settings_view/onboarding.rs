//! The pages of macOS setup (`onboarding_window`), laid out like the Paper "Redesign v3 · Onboarding"
//! macOS artboards and drawn from the overlay's own settings and actions, so every choice saves and
//! applies as it does in Settings. Each page is a title, one thing to do, and one main button.

use gpui::{
    AnyElement, App, Context, Div, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString, Styled,
    WeakEntity, Window, div, prelude::*, px,
};

use super::model_form::provider_logo;
use super::{Picker, listen, plan_label};
use crate::audio::Source;
use crate::chat::SubscriptionStatus;
use crate::hotkeys::Action;
use crate::onboarding_window::{Meter, MeterState, Nav, OnboardingWindow, PageState, Permissions};
use crate::overlay::Overlay;
use crate::platform::{Access, PrivacyPane};
use crate::settings::{OnboardingStep as Step, Provider, SttProvider};
use crate::stt::{deepgram, parakeet};
use crate::transcript_view::model_size_label;
use crate::ui_v3::{self, Kind};
use crate::{providers, theme, ui};

/// A click that acts on the setup window rather than the overlay.
fn on_setup(setup: &WeakEntity<OnboardingWindow>, f: impl Fn(&mut OnboardingWindow, &mut Window, &mut Context<OnboardingWindow>) + 'static)
    -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let setup = setup.clone();
    move |_, window, cx| { let _ = setup.update(cx, |this, cx| f(this, window, cx)); }
}

fn text(size: f32, line: f32, color: gpui::Rgba, content: impl Into<SharedString>) -> Div {
    div().text_size(px(size)).line_height(px(line)).text_color(color).child(content.into())
}

fn allowed() -> Div {
    div().flex().flex_none().items_center().gap(px(6.0))
        .child(ui::icon("icons/setup-circle-check.svg", 16.0, theme::ok()))
        .child(text(13.0, 16.0, theme::ok(), "Allowed").font_weight(FontWeight::SEMIBOLD))
}

/// A meter row's status: heard (a check), or why it can't hear (an alert).
fn meter_status(meter: &Meter) -> Option<AnyElement> {
    match &meter.state {
        MeterState::Running(..) if meter.hearing() => Some(ui::icon("icons/setup-circle-check.svg", 16.0, theme::ok()).into_any_element()),
        MeterState::Failed(_) => Some(ui::icon("icons/setup-circle-alert.svg", 16.0, theme::text()).into_any_element()),
        _ => None,
    }
}

impl Overlay {
    pub(crate) fn onboarding_page(&self, step: Step, state: &PageState, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> AnyElement {
        match step {
            Step::Permissions => self.permissions_page(state.permissions, setup),
            Step::Model => self.answer_page(setup, cx),
            Step::Transcription => self.listening_page(state, setup, cx),
            _ => welcome(setup),
        }
    }

    /// The microphone and Screen & System Audio Recording: Grant each, then Allowed. Screen
    /// Recording applies after a relaunch, so once asked it offers Quit & Reopen.
    fn permissions_page(&self, permissions: &Permissions, setup: &WeakEntity<OnboardingWindow>) -> AnyElement {
        let row = |icon: &'static str, title: &'static str, detail: Div, control: AnyElement| div().flex().items_center().gap(px(14.0)).p(px(16.0))
            .child(ui_v3::tile(44.0, false).child(ui::icon(icon, 22.0, theme::body())))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0))
                .child(text(14.0, 20.0, theme::text(), title).font_weight(FontWeight::SEMIBOLD)).child(detail))
            .child(control);
        let grant = |id: &'static str, f: fn(&mut OnboardingWindow, &mut Context<OnboardingWindow>)| ui_v3::button(id, "Grant", Kind::Secondary).px(px(16.0)).py(px(7.0))
            .on_mouse_down(MouseButton::Left, on_setup(setup, move |this, _, cx| f(this, cx))).into_any_element();
        let mic = match permissions.mic {
            Access::Allowed => row("icons/setup-mic.svg", "Microphone", text(12.0, 16.0, theme::muted(), "Your voice"), allowed().into_any_element()),
            // macOS doesn't ask twice: Grant opens the pane in System Settings.
            Access::Denied => row("icons/setup-mic.svg", "Microphone", text(12.0, 16.0, theme::danger(), "Turned off in System Settings"),
                grant("grant-mic", |this, cx| this.open_privacy(PrivacyPane::Microphone, cx))),
            Access::NotAsked => row("icons/setup-mic.svg", "Microphone", text(12.0, 16.0, theme::muted(), "Your voice"), grant("grant-mic", |this, cx| this.allow_microphone(cx))),
        };
        let restart = permissions.screen_asked && !permissions.screen_allowed;
        let screen_detail = match (&permissions.relaunch_error, restart) {
            (Some(error), _) => text(12.0, 16.0, theme::danger(), error.clone()),
            (None, true) => text(12.0, 16.0, theme::accent_soft(), "Reopen to finish"),
            (None, false) => text(12.0, 16.0, theme::muted(), "Screenshots and what they say"),
        };
        let screen_control = if permissions.screen_allowed { allowed().into_any_element() }
            else if restart { ui_v3::button("reopen", "Quit & Reopen", Kind::Primary).px(px(14.0)).py(px(7.0))
                .on_mouse_down(MouseButton::Left, on_setup(setup, |this, _, cx| this.quit_and_reopen(cx))).into_any_element() }
            else { grant("grant-screen", |this, cx| this.allow_screen_recording(cx)) };
        let screen = row("icons/setup-monitor.svg", "Screen & System Audio", screen_detail, screen_control);
        // Quit & Reopen is the main button then; Continue steps aside.
        let next = if restart { ui_v3::big_button("continue", "Continue without restarting", Kind::Secondary) } else { ui_v3::big_button("continue", "Continue", Kind::Primary) };
        ui_v3::setup_page(div().gap(px(36.0)).child(ui_v3::heading("Let CluelyRS hear and see", "macOS asks once for each."))
                .child(ui_v3::rows_box(vec![mic.into_any_element(), screen.into_any_element()])),
            div().child(next.on_mouse_down(MouseButton::Left, on_setup(setup, |this, window, cx| this.navigate(Nav::Next, window, cx)))), false).into_any_element()
    }

    /// ChatGPT, Claude or an API key; the chosen one's account (or key and model) under them.
    fn answer_page(&self, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.store.value.provider;
        let card = |provider: Provider, logo: AnyElement, title: &'static str, detail: String, cx: &mut Context<Self>| {
            ui_v3::answer_card(("answer", provider as usize), logo, title, detail, chosen == provider)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.update_settings(|s| s.provider = provider, window, cx)))
        };
        let cards = div().flex().gap(px(12.0))
            .child(card(Provider::Codex, ui::icon("logos/openai.svg", 24.0, theme::text()).into_any_element(), "ChatGPT", "Your subscription".into(), cx))
            .child(card(Provider::Claude, ui::icon("logos/claude.svg", 24.0, theme::logo_ink()).into_any_element(), "Claude", "Your subscription".into(), cx))
            .child(card(Provider::ApiKey, ui::icon("icons/setup-key.svg", 22.0, ui_v3::tile_ink(chosen == Provider::ApiKey)).into_any_element(), "API key",
                format!("{} providers", providers::PRESETS.len()), cx));
        let below = match chosen {
            Provider::ApiKey => self.api_form(cx).into_any_element(),
            provider => self.account(provider, cx).into_any_element(),
        };
        ui_v3::setup_page(div().gap(px(32.0)).child(ui_v3::heading("Choose what answers", "Use a plan you already have, or your own key."))
                .child(div().w(px(504.0)).flex().flex_col().items_center().gap(px(12.0)).child(cards).child(below)),
            div().child(ui_v3::big_button("continue", "Continue", Kind::Primary)
                .on_mouse_down(MouseButton::Left, on_setup(setup, |this, window, cx| this.navigate(Nav::Next, window, cx)))), false).into_any_element()
    }

    /// A subscription's account: signed in (and Sign out), Sign in (the CLI's login in Terminal), or
    /// where to get its CLI.
    fn account(&self, provider: Provider, cx: &mut Context<Self>) -> Div {
        let status: Option<&SubscriptionStatus> = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let cli = super::cli_location::details(provider).expect("subscriptions have a CLI");
        let alert = || ui::icon("icons/setup-circle-alert.svg", 16.0, theme::muted()).into_any_element();
        let error = self.key_notice.clone().or_else(|| self.cli_error.as_ref().filter(|(shown_for, _)| *shown_for == provider).map(|(_, message)| message.clone()));
        match status {
            None => ui_v3::account_line(super::about::spinner(), "Checking your account…", div()),
            Some(st) if !st.installed => {
                let page = cli.install_page;
                ui_v3::account_line(alert(), error.unwrap_or_else(|| format!("{} isn't installed", cli.name).into()), div().flex().items_center().gap(px(12.0))
                    .child(link("cli-install", "How to install ↗", cx, move |_, _, cx| cx.open_url(page)))
                    .child(ui_v3::button("cli-locate", "Locate…", Kind::Secondary)
                        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.locate_cli(provider, window, cx)))))
            }
            Some(st) if st.signed_in => {
                let who = st.account.clone().unwrap_or_else(|| "Signed in".into());
                let who = match st.plan.as_deref() { Some(plan) => format!("{who} · {}", plan_label(provider, plan).trim_start_matches("ChatGPT ")), None => who };
                // Only the Codex CLI signs out from here, as in Settings.
                let sign_out = (provider == Provider::Codex).then(|| link("sign-out", "Sign out", cx, |this, window, cx| this.sign_out_codex(window, cx)));
                ui_v3::account_line(ui::icon("icons/setup-circle-check.svg", 16.0, theme::ok()), who, div().children(sign_out))
            }
            Some(_) => {
                // Sign in runs the CLI's login in Terminal; clicking the waiting button stops waiting.
                let waiting = self.signing_in == Some(provider);
                ui_v3::account_line(alert(), error.unwrap_or_else(|| if waiting { "Finish signing in in Terminal".into() } else { "Not signed in".into() }),
                    ui_v3::button("sign-in", if waiting { "Waiting…" } else { "Sign in" }, Kind::Primary).when(waiting, |button| button.opacity(0.7))
                        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.sign_in(provider, window, cx))))
            }
        }
    }

    /// An API key's provider (a dropdown), its key (or endpoint) and the model, typed.
    fn api_form(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let preset = providers::preset(&s.api_provider);
        let open = self.open_picker == Some(Picker::ApiProvider);
        let face = div().id("provider-face").w(px(168.0)).flex_none().child(ui_v3::form_field(40.0).cursor_pointer().when(open, |field| field.border_color(theme::accent()))
            .child(ui::icon(provider_logo(&s.api_provider), 15.0, theme::text()))
            .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::MEDIUM).text_color(theme::text())
                .child(preset.map_or("Provider", |preset| preset.label.trim_end_matches(" (local)"))))
            .child(ui::icon("icons/chevron-down.svg", 12.0, theme::muted())));
        let presets = providers::PRESETS.iter().map(|preset| (preset.id.to_string(), preset.label.to_string())).collect();
        let provider = self.dropdown_with(Picker::ApiProvider, face, 168.0, 44.0, Some(provider_logo), presets, &s.api_provider, |s, v| s.api_provider = v, cx);
        let mut form = div().w(px(504.0)).flex().flex_col().gap(px(8.0));
        let Some(preset) = preset else { return form.child(provider) };
        let second: AnyElement = if preset.local {
            ui_v3::form_field(40.0).flex_1().child(text(12.0, 16.0, theme::muted(), "Runs on this computer · no key needed")).into_any_element()
        } else if preset.base_url.is_empty() {
            self.input_field("base-url-field", &self.base_url_input, None, "Base URL", None, 40.0).into_any_element()
        } else {
            self.key_field(preset.id, cx)
        };
        form = form.child(div().flex().gap(px(8.0)).child(provider).child(second));
        if preset.base_url.is_empty() && !preset.local && preset.needs_key { form = form.child(self.key_field(preset.id, cx)); }
        form.child(self.input_field("model-field", &self.model_input, Some("icons/setup-sparkle.svg"), "Model id", None, 40.0))
            .children(self.key_notice.clone().map(|notice| text(12.0, 16.0, theme::accent_soft(), notice).px(px(2.0))))
    }

    /// The key: saved (its hint, In Keychain; clicking replaces it) or a field to paste it, saved
    /// with Return or Save.
    fn key_field(&self, provider: &'static str, cx: &mut Context<Self>) -> AnyElement {
        match self.store.value.saved_keys.get(provider).filter(|_| !self.replacing_key()) {
            Some(hint) => div().id("saved-key").flex_1().min_w_0().cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.replace_key(cx)))
                .child(ui_v3::form_field(40.0)
                    .child(text(12.0, 16.0, theme::text(), if hint.is_empty() { "••••••••".to_string() } else { hint.clone() }).flex_1().min_w_0().truncate())
                    .child(ui::icon("icons/setup-circle-check.svg", 14.0, theme::ok()))
                    .child(text(12.0, 16.0, theme::ok(), "In Keychain")))
                .into_any_element(),
            None => self.input_field("key-field", &self.key_input, None, "",
                Some(link("save-key", "Save", cx, |this, window, cx| this.save_key(window, cx)).text_color(theme::accent_soft()).into_any_element()), 40.0)
                .into_any_element(),
        }
    }

    /// A text field on setup's form: an optional icon, the input, and a word on the right.
    #[allow(clippy::too_many_arguments)]
    fn input_field(&self, id: &'static str, input: &gpui::Entity<crate::input::TextInput>, icon: Option<&'static str>, hint: &'static str,
        trailing: Option<AnyElement>, height: f32) -> gpui::Stateful<Div> {
        let focus = input.clone();
        div().id(id).flex_1().min_w_0().cursor_text()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&focus.focus_handle(cx)))
            .child(ui_v3::form_field(height)
                .when_some(icon, |field, icon| field.child(ui::icon(icon, 15.0, theme::placeholder())))
                .child(div().flex_1().min_w_0().text_size(px(12.0)).child(input.clone()))
                .when(!hint.is_empty(), |field| field.child(text(12.0, 16.0, theme::placeholder(), hint)))
                .children(trailing))
    }

    /// Each source with its level, then the speech model to use: on this Mac (downloaded only when
    /// chosen) or Deepgram (with a key). Start waits until one is chosen.
    fn listening_page(&self, state: &PageState, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> AnyElement {
        let meters = [Source::Me, Source::Them].map(|source| {
            let (picker, icon, who) = match source { Source::Me => (Picker::Mic, "icons/setup-mic.svg", "You"), Source::Them => (Picker::Desktop, "icons/setup-speaker.svg", "Them") };
            let meter = &state.audio[if source == Source::Me { 0 } else { 1 }];
            let (value, options, chosen, set) = self.device_choice(picker);
            // A source that couldn't open says why in place of its device.
            let device = match &meter.state {
                MeterState::Failed(why) => text(12.0, 16.0, theme::danger(), why.clone()).truncate(),
                _ => self.dropdown_with(picker, ui_v3::device_face(("device", picker as usize), value), 240.0, 22.0, None, options, &chosen, set, cx),
            };
            ui_v3::meter_row(icon, who, device, meter.shown_level(), meter_status(meter)).into_any_element()
        });
        let choice = state.speech;
        let model = parakeet::MODEL;
        let size = model_size_label(&model);
        let on_mac: AnyElement = match self.download_progress() {
            Some(progress) => ui_v3::download_progress(progress as f32, format!("{:.0} of {}", progress * model.bytes as f64 / 1e6, size)).into_any_element(),
            None if self.model_installed => ui_v3::done_note("Downloaded").into_any_element(),
            None => ui_v3::card_button("download-model", "icons/setup-download.svg", "Download").into_any_element(),
        };
        let deepgram_saved = self.store.value.saved_keys.contains_key(deepgram::PROVIDER_ID);
        let deepgram: AnyElement = if choice == Some(SttProvider::Deepgram) && !deepgram_saved {
            self.input_field("deepgram-key", &self.key_input, None, "",
                Some(link("save-deepgram-key", "Save", cx, |this, window, cx| this.save_key(window, cx)).text_color(theme::accent_soft()).into_any_element()), 30.0)
                .into_any_element()
        } else if deepgram_saved {
            ui_v3::done_note("Key in Keychain").into_any_element()
        } else {
            ui_v3::card_button("add-deepgram-key", "icons/setup-key.svg", "Add API key").into_any_element()
        };
        let pick = |provider: SttProvider| on_setup(setup, move |this, _, cx| this.choose_speech(provider, cx));
        let cards = div().flex().gap(px(10.0))
            .child(ui_v3::speech_card("speech-mac", "icons/laptop-minimal.svg", 18.0, "On this Mac", format!("Parakeet · private · {size}"), choice == Some(SttProvider::Parakeet), on_mac)
                .on_mouse_down(MouseButton::Left, pick(SttProvider::Parakeet)))
            .child(ui_v3::speech_card("speech-deepgram", "logos/deepgram.svg", 16.0, "Deepgram", "Cloud · your API key", choice == Some(SttProvider::Deepgram), deepgram)
                .on_mouse_down(MouseButton::Left, pick(SttProvider::Deepgram)));
        // On this Mac can start once its download has; Deepgram once chosen (its key can follow).
        let ready = match choice { Some(SttProvider::Parakeet) => self.model_installed || self.download_progress().is_some(), Some(SttProvider::Deepgram) => true, None => false };
        let start = ui_v3::big_button("start", "Start CluelyRS", if ready { Kind::Primary } else { Kind::Disabled })
            .when(ready, |start| start.on_mouse_down(MouseButton::Left, on_setup(setup, |this, window, cx| this.navigate(Nav::Finish, window, cx))));
        let hints = [(Action::Assist, "Assist"), (Action::Focus, "Type"), (Action::Toggle, "Show / hide")].map(|(action, what)| (self.hotkeys.label(action), what));
        let notice = self.model_notice.clone().filter(|_| choice == Some(SttProvider::Parakeet)).or_else(|| self.key_notice.clone().filter(|_| choice == Some(SttProvider::Deepgram)));
        ui_v3::setup_page(div().gap(px(22.0)).child(ui_v3::heading("Check what it hears", "Say something, play a video, then pick a speech model."))
                .child(div().w(px(ui_v3::SETUP_COLUMN)).flex().flex_col().gap(px(12.0))
                    .child(ui_v3::rows_box(meters.into()))
                    .child(cards)
                    .children(notice.map(|notice| text(12.0, 16.0, theme::accent_soft(), notice).px(px(2.0))))),
            div().child(start).child(ui_v3::shortcut_hints(&hints)), true).into_any_element()
    }
}

/// A quiet text action on a page.
fn link(id: &'static str, label: &'static str, cx: &Context<Overlay>, f: impl Fn(&mut Overlay, &mut Window, &mut Context<Overlay>) + 'static) -> gpui::Stateful<Div> {
    div().id(id).flex_none().cursor_pointer().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::MEDIUM).text_color(theme::muted())
        .hover(|link| link.text_color(theme::body()))
        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); f(this, window, cx) }))
        .child(label)
}

/// Welcome: the app, what it does, and Get started.
fn welcome(setup: &WeakEntity<OnboardingWindow>) -> AnyElement {
    ui_v3::welcome(26.0, ui_v3::big_button("get-started", "Get started", Kind::Primary)
        .on_mouse_down(MouseButton::Left, on_setup(setup, |this, window, cx| this.navigate(Nav::Next, window, cx)))).into_any_element()
}
