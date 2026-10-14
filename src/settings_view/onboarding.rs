//! The pages of macOS setup (`onboarding_window`), laid out like the Paper "Menu bar + macOS
//! onboarding" v2 artboards and drawn from the overlay's own settings and actions, so every choice
//! saves and applies as it does in Settings. Every page's words are in `copy`; the building blocks
//! (`option`, `line`, `toggle_row`, `boxed`) each have an icon slot.

use gpui::{
    AnyElement, App, Context, Div, FontWeight, ImageSource, IntoElement, MouseButton, MouseDownEvent, ParentElement, Resource, Rgba,
    SharedString, Styled, WeakEntity, Window, div, img, prelude::*, px, relative, rgb,
};

use super::about::spinner;
use super::cli_location::details;
use super::model_form::text_box;
use super::{button, choice, listen, masked, menu_bar_switch, plan_label, recommended, saved_key_label, switch};
use crate::archive::Retention;
use crate::chat::SubscriptionStatus;
use crate::hotkeys::Action;
use crate::onboarding_window::{Nav, OnboardingWindow, TestRun, TestState};
use crate::overlay::Overlay;
use crate::settings::{OnboardingStep as Step, Provider, Settings, SttProvider};
use crate::stt::{deepgram, parakeet};
use crate::transcript_view::model_size_label;
use crate::{providers, theme, ui};

/// Every page's words. Rows are (icon, title, detail).
mod copy {
    pub type Row = (&'static str, &'static str, &'static str);
    pub const WELCOME: (&str, &str) = ("Welcome to CluelyRS", "Live answers for your calls, in a small overlay.");
    pub const STAYS: (&str, [Row; 3]) = ("STAYS ON THIS MAC", [("icons/tab-listening.svg", "Transcription", "Parakeet, on-device"),
        ("icons/tab-sessions.svg", "Sessions", "Transcripts, answers, screenshots"), ("icons/key-round.svg", "API keys", "In your Keychain")]);
    pub const SENT: (&str, [Row; 3]) = ("SENT OUT", [("icons/message-square.svg", "Questions + conversation", "To your chosen model"),
        ("icons/image.svg", "Screenshots", "With each question · optional"), ("icons/cloud.svg", "Audio", "Only with Deepgram")]);
    pub const FACTS: [(&str, &str); 3] = [("icons/eye-off.svg", "Hidden from screen share"), ("icons/tab-sessions.svg", "About 2 minutes"),
        ("icons/sliders.svg", "Change anything later")];
    pub const MODEL: (&str, &str) = ("Choose what answers", "A subscription you already have, or your own key.");
    pub const CHATGPT: (&str, &str) = ("ChatGPT subscription", "logos/openai.svg");
    pub const CLAUDE: (&str, &str) = ("Claude subscription", "logos/claude.svg");
    pub const API_KEY: (&str, &str) = ("Your API key", "+ Ollama, Custom");
    pub const API_LOGOS: [&str; 4] = ["logos/anthropic.svg", "logos/openai.svg", "logos/googlegemini.svg", "logos/openrouter.svg"];
    /// The providers offered as chips (preset id, label, logo); the others are in Settings › Model.
    pub const PRESETS: [(&str, &str, &str); 6] = [("anthropic", "Anthropic", "logos/anthropic.svg"), ("openai", "OpenAI", "logos/openai.svg"),
        ("gemini", "Gemini", "logos/googlegemini.svg"), ("openrouter", "OpenRouter", "logos/openrouter.svg"), ("ollama", "Ollama", "logos/ollama.svg"),
        ("custom", "Custom", "icons/server.svg")];
    pub const LOCAL: &str = "Runs on this computer · no key needed";
    pub const SIGNING_IN: (&str, &str) = ("Finish in Terminal", "Waiting for Terminal…");
    pub const TEST_IDLE: &str = "One short question, no screenshot";
    pub const TRANSCRIPTION: (&str, &str) = ("Transcribe the conversation", "So answers know what was asked.");
    pub const OFF: Row = ("icons/mic-off.svg", "Off", "Screen and typed questions only");
    pub const ON_MAC: Row = ("icons/laptop-minimal.svg", "On this Mac", "Parakeet · English · audio stays here");
    pub const SPEECH_MODEL: (&str, &str) = ("icons/download.svg", "Speech model");
    pub const GPU: Row = ("icons/zap.svg", "Use GPU (Metal)", "Lighter on the CPU");
    pub const GPU_FELL_BACK: &str = "The GPU couldn't be used, so it runs on the CPU";
    pub const DEEPGRAM: Row = ("logos/deepgram.svg", "Deepgram", "Cloud · needs a key");
    pub const DEEPGRAM_NOTE: &str = "Mic + desktop audio stream to Deepgram while Live is on.";
    pub const PRIVACY: (&str, &str) = ("Privacy defaults", "On by default. Change anytime.");
    pub const HIDE: Row = ("icons/eye-off.svg", "Hide from screen sharing", "Shares and recordings leave the overlay out");
    pub const SCREENSHOT: Row = ("icons/image.svg", "Screenshot with each question", "Lets your model read the screen");
    pub const SAVE_SESSIONS: Row = ("icons/tab-sessions.svg", "Save sessions", "Transcripts and answers");
    pub const SAVE_SCREENSHOTS: Row = ("icons/archive.svg", "Save screenshots", "The ones sent with answers");
    pub const KEEP_FOR: Row = ("icons/hourglass.svg", "Keep for", "");
    pub const DONE: (&str, &str) = ("You're all set", "Show the overlay anytime with");
    pub const MENU_BAR: Row = ("icons/tab-window.svg", "Show menu bar icon", "Live, modes, Settings");
    pub const DOCK: Row = ("icons/panel-bottom.svg", "Show in Dock", "Also in ⌘Tab");
    pub const LIVE_ON_LAUNCH: Row = ("icons/radio.svg", "Start Live on launch", "");
    pub const UPDATES: Row = ("icons/update.svg", "Check for updates", "Asks GitHub only, every 6 h");
    pub const RERUN: &str = "Rerun setup from the menu bar or Settings › About";
}

/// Colours of the artboards that the theme doesn't name.
const SELECTED: u32 = 0x0d1830;
const TILE_ICON: u32 = 0xe8e4dc;
const CLAUDE_ORANGE: u32 = 0xd97757;
const ERROR: u32 = 0xffb4a8;

/// A click that changes settings, as picking an option does.
fn pick(cx: &Context<Overlay>, set: impl Fn(&mut Settings) + Copy + 'static) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    listen(cx, move |this, _, window, cx| this.update_settings(set, window, cx))
}

/// A click that acts on the setup window rather than the overlay.
fn on_setup(setup: &WeakEntity<OnboardingWindow>, f: impl Fn(&mut OnboardingWindow, &mut Window, &mut Context<OnboardingWindow>) + 'static)
    -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let setup = setup.clone();
    move |_, window, cx| { let _ = setup.update(cx, |this, cx| f(this, window, cx)); }
}

fn heading((title, detail): (&'static str, &'static str)) -> Div {
    div().flex().flex_col().gap(px(4.0))
        .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title))
        .child(div().text_size(px(13.0)).line_height(px(18.0)).text_color(theme::muted()).child(detail))
}

fn text(size: f32, color: Rgba, content: impl Into<SharedString>) -> Div { div().text_size(px(size)).text_color(color).child(content.into()) }

fn radio(selected: bool) -> Div {
    let ring = div().size(px(16.0)).flex_none().rounded_full().flex().items_center().justify_center();
    if selected { ring.bg(theme::accent()).child(div().size(px(6.0)).rounded_full().bg(theme::accent_ink())) } else { ring.border(px(1.5)).border_color(rgb(0x5a5e64)) }
}

/// An icon on a small dark tile.
fn tile(icon: impl IntoElement, border: Option<Rgba>) -> Div {
    div().size(px(30.0)).flex_none().flex().items_center().justify_center().rounded(px(8.0)).bg(theme::raised())
        .when_some(border, |tile, color| tile.border_1().border_color(color)).child(icon)
}

/// A status beside an option or in a row: an optional icon and a few words.
fn status(icon: Option<&'static str>, label: impl Into<SharedString>, color: Rgba) -> Div {
    div().flex().items_center().gap(px(6.0)).flex_none().text_size(px(12.0)).text_color(color)
        .when_some(icon, |status, icon| status.child(ui::icon(icon, 14.0, color))).child(label.into())
}

fn link(id: &'static str, label: &'static str, url: &'static str, cx: &Context<Overlay>) -> impl IntoElement {
    div().id(id).cursor_pointer().text_size(px(12.0)).text_color(theme::accent_soft())
        .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(url))).child(label)
}

fn note(words: &'static str) -> Div {
    div().flex().items_start().gap(px(8.0)).px(px(2.0)).text_size(px(12.0)).line_height(px(17.0)).text_color(theme::muted())
        .child(div().pt(px(1.5)).child(ui::icon("icons/info.svg", 14.0, theme::muted()))).child(words)
}

/// A radio option: its icon tile, title and detail, and a status on the right. Selected, it is
/// highlighted and its setup `rows` follow inside the same box.
#[allow(clippy::too_many_arguments)]
fn option(id: &'static str, icon: impl IntoElement, title: impl IntoElement, detail: impl IntoElement, status: Option<Div>, selected: bool,
    rows: Vec<Div>, pick: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static) -> Div {
    let edge = if selected { theme::bubble_border() } else { theme::hairline() };
    let header = div().id(id).flex().items_center().gap(px(12.0)).px(px(14.0)).py(px(10.0)).cursor_pointer()
        .when(selected, |header| header.bg(rgb(SELECTED)))
        .on_mouse_down(MouseButton::Left, pick)
        .child(radio(selected)).child(tile(icon, Some(edge)))
        .child(div().flex().flex_col().gap(px(1.0)).flex_1().min_w_0()
            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
            .child(div().text_size(px(12.0)).text_color(theme::muted()).child(detail)))
        .children(status);
    rows.into_iter().filter(|_| selected).fold(div().flex().flex_col().rounded(px(12.0)).overflow_hidden().border_1().border_color(edge).child(header),
        |card, row| card.child(row.border_t_1().border_color(theme::divider())))
}

/// A row inside an option: its icon and label in a fixed column; callers add content and an action.
/// `top` lines the label up with the first line of taller content.
fn line(icon: &'static str, label: &'static str, top: bool) -> Div {
    div().flex().gap(px(12.0)).px(px(14.0)).py(px(11.0)).when(top, |line| line.items_start()).when(!top, |line| line.items_center())
        .child(div().w(px(116.0)).flex_none().flex().items_center().gap(px(8.0)).when(top, |label| label.pt(px(9.0)))
            .child(ui::icon(icon, 15.0, theme::muted())).child(text(13.0, theme::body(), label)))
}

/// A switch (or other control) in a box: icon tile, title and detail.
fn toggle_row((icon, title, detail): copy::Row, control: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(16.0)).px(px(14.0)).py(px(11.0))
        .child(tile(ui::icon(icon, 16.0, theme::body()), None))
        .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
            .when(!detail.is_empty(), |column| column.child(text(12.0, theme::muted(), detail))))
        .child(control)
}

/// Rows in one box, divided by rules.
fn boxed(rows: Vec<Div>) -> Div {
    let last = rows.len().saturating_sub(1);
    rows.into_iter().enumerate().fold(div().flex().flex_col().rounded(px(12.0)).overflow_hidden().border_1().border_color(theme::hairline()),
        |group, (index, row)| group.child(if index == last { row } else { row.border_b_1().border_color(theme::divider()) }))
}

/// A box under its small caps label, with an aside on the right.
fn section(label: &'static str, aside: &'static str, rows: Vec<Div>) -> Div {
    div().flex().flex_col().gap(px(8.0))
        .child(div().flex().justify_between().items_baseline().px(px(2.0)).child(ui::section_label(label)).child(text(12.0, theme::muted(), aside)))
        .child(boxed(rows))
}

/// The keys of a shortcut, each on its own cap.
fn keycaps(label: &str) -> Div {
    label.split(' ').fold(div().flex().items_center().gap(px(4.0)), |caps, key| caps.child(
        div().min_w(px(22.0)).h(px(22.0)).px(px(5.0)).flex().items_center().justify_center().rounded(px(6.0)).bg(theme::raised())
            .border_1().border_b_2().border_color(theme::keycap_border()).text_size(px(12.0)).text_color(rgb(TILE_ICON)).child(key.to_string())))
}

fn status_badge(label: &'static str) -> Div { status(None, label, theme::muted()) }

/// Setup › All set's privacy facts: (icon, words).
fn privacy_facts(s: &Settings) -> [(&'static str, &'static str); 3] {
    [
        if s.hide_from_capture { ("icons/eye-off.svg", "Hidden") } else { ("icons/eye.svg", "Visible when sharing") },
        if s.screen_on_send { ("icons/image.svg", "Screenshots") } else { ("icons/image-off.svg", "No screenshots") },
        ("icons/tab-sessions.svg", match (s.save_sessions, s.keep_sessions) {
            (false, _) => "Not saved", (true, Retention::Days7) => "7 days", (true, Retention::Days30) => "30 days", (true, Retention::Forever) => "Forever",
        }),
    ]
}

/// An API provider's logo, or a key for one without a chip.
fn preset_logo(id: &str) -> &'static str {
    copy::PRESETS.iter().find(|(preset, ..)| *preset == id).map_or("icons/key-round.svg", |(.., logo)| logo)
}

impl Overlay {
    pub(crate) fn onboarding_page(&self, step: Step, test: Option<&TestRun>, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> AnyElement {
        match step {
            Step::Welcome => welcome().into_any_element(),
            Step::Model => self.model_page(test, setup, cx).into_any_element(),
            Step::Transcription => self.transcription_page(cx).into_any_element(),
            Step::Privacy => self.privacy_page(cx).into_any_element(),
            Step::Done => self.done_page(setup, cx).into_any_element(),
            // Not in this build's `onboarding_window::STEPS`.
            Step::Permissions | Step::Shortcuts => div().into_any_element(),
        }
    }

    /// The three ways to answer; the chosen one opens on its setup and a test request.
    fn model_page(&self, test: Option<&TestRun>, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> Div {
        let mut options = div().flex().flex_col().gap(px(8.0));
        for provider in [Provider::Codex, Provider::Claude] { options = options.child(self.subscription_option(provider, test, setup, cx)); }
        div().flex().flex_col().gap(px(20.0)).child(heading(copy::MODEL)).child(options.child(self.api_option(test, setup, cx)))
    }

    /// ChatGPT or Claude: where its CLI is, the account, the model and a test.
    fn subscription_option(&self, provider: Provider, test: Option<&TestRun>, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> Div {
        let selected = self.store.value.provider == provider;
        let status = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let cli = details(provider).expect("subscriptions have a CLI");
        let ((title, logo), tint) = match provider {
            Provider::Codex => (copy::CHATGPT, rgb(TILE_ICON)),
            _ => (copy::CLAUDE, if selected { rgb(CLAUDE_ORANGE) } else { rgb(TILE_ICON) }),
        };
        let plan = status.filter(|st| st.signed_in).and_then(|st| st.plan.as_deref()).map(|plan| plan_label(provider, plan));
        let badge = match status {
            None => status_badge("Checking…"),
            Some(st) if !st.installed => status_badge("CLI not found"),
            Some(st) if st.signed_in => self::status(Some("icons/check-circle.svg"), "Ready", theme::ok()),
            Some(_) => status_badge("Not signed in"),
        };
        let mut rows = Vec::new();
        if selected {
            rows.push(self.cli_line(provider, status, cx));
            // Without the CLI there's no account or model to show yet.
            if status.is_some_and(|st| st.installed) {
                rows.push(self.account_line(provider, status, cx));
                rows.push(line("icons/sparkles.svg", "Model", false).child(div().w(px(240.0)).child(self.subscription_model_picker(cx))));
            }
            rows.push(self.test_line(test, setup));
        }
        option(if provider == Provider::Codex { "provider-codex" } else { "provider-claude" }, ui::icon(logo, 16.0, tint), title,
            plan.map_or(cli.name.to_string(), |plan| format!("{} · {plan}", cli.name)), Some(badge), selected, rows, pick(cx, move |s| s.provider = provider))
    }

    fn cli_line(&self, provider: Provider, status: Option<&SubscriptionStatus>, cx: &mut Context<Self>) -> Div {
        let cli = details(provider).expect("subscriptions have a CLI");
        let locate = |primary| button("cli-locate", "Locate…", primary)
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.locate_cli(provider, window, cx)));
        let chosen = if provider == Provider::Codex { &self.store.value.codex_path } else { &self.store.value.claude_path };
        let (found, detail, action) = match status {
            None => ("Checking…", None, None),
            Some(st) if !st.installed => ("Not found", Some(link("cli-install", "How to install ↗", cli.install_page, cx).into_any_element()), Some(locate(true))),
            Some(st) if st.custom_location => ("Custom location", Some(text(11.0, theme::muted(), chosen.clone()).font_family(theme::MONO).into_any_element()),
                Some(button("cli-automatic", "Use automatic", false)
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.set_cli_location(provider, String::new(), window, cx))))),
            Some(_) => ("Found automatically", None, Some(locate(false))),
        };
        let error = self.cli_error.as_ref().filter(|(shown_for, _)| *shown_for == provider).map(|(_, message)| text(11.0, rgb(ERROR), message.clone()));
        line("icons/terminal.svg", cli.name, false)
            .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0().child(text(13.0, theme::text(), found)).children(detail).children(error))
            .children(action)
    }

    /// Signed in (the account masked until clicked, and its plan), finishing in Terminal, or Sign in.
    fn account_line(&self, provider: Provider, status: Option<&SubscriptionStatus>, cx: &mut Context<Self>) -> Div {
        let line = line("icons/circle-user.svg", "Account", false);
        let notice = self.key_notice.clone().map(|notice| text(11.0, rgb(ERROR), notice));
        let sign_in = |label| button("sign-in", label, true).on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.sign_in(provider, window, cx)));
        match status {
            Some(st) if st.signed_in => {
                let account = st.account.clone().unwrap_or("Connected".into());
                let shown = if self.reveal_accounts { account } else { masked(&account) };
                let plan = st.plan.as_deref().map(|plan| text(13.0, theme::text(), plan_label(provider, plan)));
                let chip = div().id("account").px(px(8.0)).py(px(2.0)).rounded(px(6.0)).bg(theme::raised()).cursor_pointer()
                    .font_family(theme::MONO).text_size(px(12.0)).text_color(theme::ok()).child(shown)
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { this.reveal_accounts = !this.reveal_accounts; cx.notify(); }));
                // Only the Codex CLI signs out from here, as in Settings.
                let sign_out = (provider == Provider::Codex).then(|| div().id("sign-out").px(px(4.0)).cursor_pointer().text_size(px(12.0)).text_color(theme::muted())
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.sign_out_codex(window, cx))).child("Sign out"));
                line.child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
                        .child(div().flex().items_center().gap(px(8.0)).child(chip).children(plan)).children(notice))
                    .children(sign_out)
            }
            // Clicking the waiting button again stops waiting.
            Some(_) if self.signing_in == Some(provider) => line.child(text(13.0, theme::text(), copy::SIGNING_IN.0).flex_1()).child(sign_in(copy::SIGNING_IN.1).opacity(0.7)),
            Some(_) => line.child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0().child(text(13.0, theme::text(), "Not signed in")).children(notice))
                .child(sign_in("Sign in")),
            None => line.child(text(13.0, theme::muted(), "Checking…")),
        }
    }

    /// Your API key: the provider (as chips), its endpoint and key, the model and a test.
    fn api_option(&self, test: Option<&TestRun>, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let selected = s.provider == Provider::ApiKey;
        let preset = providers::preset(&s.api_provider);
        let ready = preset.is_some_and(|p| !p.needs_key || s.saved_keys.contains_key(p.id));
        let logos = copy::API_LOGOS.iter().fold(div().flex().items_center().gap(px(9.0)).h(px(16.0)), |strip, logo| strip.child(ui::icon(logo, 13.0, theme::muted())))
            .child(copy::API_KEY.1);
        let mut rows = Vec::new();
        if selected {
            let chips = copy::PRESETS.iter().fold(div().flex().flex_wrap().gap(px(8.0)).w(px(442.0)), |chips, &(id, label, logo)| {
                let (chosen, color) = (s.api_provider == id, if s.api_provider == id { theme::text() } else { theme::body() });
                chips.child(div().id(id).w(px(142.0)).h(px(34.0)).flex().items_center().gap(px(8.0)).px(px(10.0)).rounded(px(8.0)).cursor_pointer().border_1()
                    .bg(if chosen { rgb(SELECTED) } else { theme::field() }).border_color(if chosen { theme::accent() } else { theme::hairline() })
                    .on_mouse_down(MouseButton::Left, pick(cx, move |s| s.api_provider = id.into()))
                    .child(ui::icon(logo, 14.0, color)).child(text(12.0, color, label).when(chosen, |label| label.font_weight(FontWeight::SEMIBOLD))))
            });
            rows.push(line("icons/plug.svg", "Provider", false).child(chips));
            if let Some(preset) = preset {
                if preset.base_url.is_empty() { rows.push(line("icons/link.svg", "Base URL", false).child(text_box(&self.base_url_input, "base-url-box"))); }
                rows.push(if preset.local { line("icons/key-round.svg", "API key", false).child(text(13.0, theme::muted(), copy::LOCAL)) }
                    else { line("icons/key-round.svg", "API key", true).child(div().flex_1().min_w_0().child(self.key_control(preset, cx))) });
                rows.push(line("icons/sparkles.svg", "Model", true).child(div().flex_1().min_w_0().child(self.model_control(preset, cx))));
            }
            rows.push(self.test_line(test, setup));
        }
        let badge = if ready { status(Some("icons/check-circle.svg"), "Ready", theme::ok()) } else { status_badge("Not set") };
        option("provider-api", ui::icon("icons/key-round.svg", 16.0, rgb(TILE_ICON)), copy::API_KEY.0, logos, Some(badge), selected, rows,
            pick(cx, |s| s.provider = Provider::ApiKey))
    }

    /// Send a test request, then how it went: never required, Continue works whatever it says.
    fn test_line(&self, test: Option<&TestRun>, setup: &WeakEntity<OnboardingWindow>) -> Div {
        let s = &self.store.value;
        let model = self.model_choice().value;
        let test = test.filter(|test| test.provider == s.provider && test.model == model).map(|test| &test.state);
        let unready = (s.provider == Provider::ApiKey && s.api_model().is_empty()).then_some("Needs a model");
        let result = div().flex().items_center().gap(px(8.0)).flex_1().min_w_0().text_size(px(13.0));
        let result = match test {
            Some(TestState::Sending) => result.text_color(theme::muted()).child(spinner()).child("Sending…"),
            Some(TestState::Answered(took)) => result.text_color(theme::ok()).child(ui::icon("icons/check-circle.svg", 14.0, theme::ok()))
                .child(format!("Answered in {:.1} s", took.as_secs_f32())).child(text(12.0, theme::muted(), format!("· {model}")).truncate()),
            Some(TestState::Failed(reason)) => result.text_color(rgb(ERROR)).child(ui::icon("icons/alert-circle.svg", 14.0, rgb(ERROR)))
                .child(div().min_w_0().truncate().child(format!("No answer: {}", reason.lines().next().unwrap_or_default()))),
            None => result.text_color(theme::placeholder()).child(unready.unwrap_or(copy::TEST_IDLE)),
        };
        let disabled = unready.is_some() || matches!(test, Some(TestState::Sending));
        let send = button("send-test", if matches!(test, Some(TestState::Failed(_))) { "Try again" } else { "Send test request" }, false)
            .when(disabled, |send| send.opacity(0.45).cursor_default())
            .when(!disabled, |send| send.on_mouse_down(MouseButton::Left, on_setup(setup, |this, _, cx| this.send_test(cx))));
        line("icons/send.svg", "Test", false).bg(rgb(0x121417)).child(result).child(send)
    }

    /// Off, on this Mac (the speech model and GPU) or Deepgram (its key), as Settings › Listening.
    fn transcription_page(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let (off, mac) = (!s.transcribe, s.transcribe && s.stt_provider == SttProvider::Parakeet);
        let icon = |path, selected| ui::icon(path, 16.0, if selected { rgb(TILE_ICON) } else { theme::body() });
        let on_mac_rows = if mac { self.speech_model_rows(cx) } else { Vec::new() };
        let deepgram_rows = if !off && !mac { self.deepgram_rows(cx) } else { Vec::new() };
        div().flex().flex_col().gap(px(20.0))
            .child(heading(copy::TRANSCRIPTION))
            .child(div().flex().flex_col().gap(px(8.0))
                .child(option("transcribe-off", icon(copy::OFF.0, off), copy::OFF.1, copy::OFF.2, None, off, Vec::new(), pick(cx, |s| s.transcribe = false)))
                .child(option("transcribe-mac", icon(copy::ON_MAC.0, mac), div().flex().items_center().gap(px(8.0)).child(copy::ON_MAC.1).child(recommended()),
                    copy::ON_MAC.2, None, mac, on_mac_rows, pick(cx, |s| { s.transcribe = true; s.stt_provider = SttProvider::Parakeet; })))
                .child(option("transcribe-deepgram", icon(copy::DEEPGRAM.0, !off && !mac), copy::DEEPGRAM.1, copy::DEEPGRAM.2, None, !off && !mac, deepgram_rows,
                    pick(cx, |s| { s.transcribe = true; s.stt_provider = SttProvider::Deepgram; }))))
    }

    /// The speech model (Download, progress with Cancel, or Installed) and the GPU switch. The
    /// download starts only from its button and carries on after Continue.
    fn speech_model_rows(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let model = parakeet::MODEL;
        let size = model_size_label(&model);
        let progress = self.download_progress();
        let action = match progress {
            Some(_) => button("cancel-download", "Cancel", false).on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { this.cancel_download(); cx.notify(); })).into_any_element(),
            None if self.model_installed => status(Some("icons/check-circle.svg"), "Installed", theme::ok()).into_any_element(),
            None => button("download-model", format!("Download {size}"), true)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.download_model(window, cx))).into_any_element(),
        };
        let (icon, title) = copy::SPEECH_MODEL;
        let speech = div().flex().flex_col().gap(px(10.0)).px(px(14.0)).py(px(12.0))
            .child(div().flex().items_center().gap(px(12.0))
                .child(ui::icon(icon, 16.0, theme::muted()))
                .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
                    .child(text(13.0, theme::text(), title).font_weight(FontWeight::SEMIBOLD))
                    .child(div().flex().gap(px(4.0)).child(text(12.0, theme::muted(), format!("{size} ·"))).child(link("model-license", "NVIDIA Open Model License ↗", model.license_url, cx))))
                .child(action))
            .when_some(progress, |speech, progress| speech.child(div().flex().items_center().gap(px(12.0)).pl(px(28.0))
                .child(div().flex_1().h(px(6.0)).rounded_full().overflow_hidden().bg(theme::hairline())
                    .child(div().h_full().rounded_full().bg(theme::accent()).w(relative(progress.clamp(0.0, 1.0) as f32))))
                .child(text(11.0, theme::muted(), format!("{:.0} of {size}", progress * model.bytes as f64 / 1e6)).w(px(110.0)).flex().justify_end().font_family(theme::MONO))))
            .when_some(self.model_notice.clone(), |speech, notice| speech.child(text(12.0, theme::accent_soft(), notice).pl(px(28.0))));
        let use_gpu = self.store.value.use_gpu;
        let detail = if use_gpu && parakeet::gpu_unavailable() { copy::GPU_FELL_BACK } else { copy::GPU.2 };
        let gpu = div().flex().items_center().gap(px(12.0)).px(px(14.0)).py(px(12.0))
            .child(ui::icon(copy::GPU.0, 16.0, theme::muted()))
            .child(div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
                .child(text(13.0, theme::text(), copy::GPU.1).font_weight(FontWeight::SEMIBOLD)).child(text(12.0, theme::muted(), detail)))
            .child(switch("use-gpu", use_gpu, |s, v| s.use_gpu = v, cx));
        vec![speech, gpu]
    }

    /// The Deepgram key: paste and Save, or the saved key's hint with Remove.
    fn deepgram_rows(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let saved = self.store.value.saved_keys.get(deepgram::PROVIDER_ID).cloned();
        let save = button("save-key", "Save", true)
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_key(window, cx)));
        let remove = saved.is_some().then(|| div().id("remove-key").cursor_pointer().text_color(rgb(ERROR))
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.remove_key(window, cx))).child("Remove"));
        let store = div().flex().items_center().gap(px(6.0)).child(ui::icon("icons/lock.svg", 13.0, theme::muted()))
            .child(if saved.is_some() { saved_key_label(saved.as_deref()) } else { "Keychain".into() });
        let key = div().flex().flex_col().gap(px(8.0)).px(px(14.0)).py(px(12.0))
            .child(div().flex().items_center().gap(px(8.0)).child(text_box(&self.key_input, "key-box")).child(save))
            .child(div().flex().items_center().gap(px(12.0)).text_size(px(12.0)).text_color(theme::muted()).child(store).children(remove)
                .child(link("key-page", "Get a key ↗", "https://console.deepgram.com/", cx)))
            .children(self.key_notice.clone().map(|notice| text(12.0, theme::accent_soft(), notice)))
            .child(note(copy::DEEPGRAM_NOTE).px(px(12.0)).py(px(10.0)).rounded(px(8.0)).border_1().border_color(theme::hairline()));
        vec![key]
    }

    /// Settings › Window's capture and screenshot switches and Settings › Sessions' saving.
    fn privacy_page(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut kept = vec![toggle_row(copy::SAVE_SESSIONS, switch("save-sessions", s.save_sessions, |s, v| s.save_sessions = v, cx))];
        if s.save_sessions {
            kept.push(toggle_row(copy::SAVE_SCREENSHOTS, switch("save-screenshots", s.save_screenshots, |s, v| s.save_screenshots = v, cx)));
            kept.push(toggle_row(copy::KEEP_FOR, choice("retention",
                &[(Retention::Days7, "7 days"), (Retention::Days30, "30 days"), (Retention::Forever, "Forever")], s.keep_sessions, |s, v| s.keep_sessions = v, cx)));
        }
        div().flex().flex_col().gap(px(20.0))
            .child(heading(copy::PRIVACY))
            .child(section("WHAT OTHERS SEE", "", vec![toggle_row(copy::HIDE, switch("hide-capture", s.hide_from_capture, |s, v| s.hide_from_capture = v, cx))]))
            .child(section("WHAT'S SENT", "", vec![toggle_row(copy::SCREENSHOT, switch("screen-on-send", s.screen_on_send, |s, v| s.screen_on_send = v, cx))]))
            .child(section("KEPT ON THIS MAC", "Manage in Sessions", kept))
    }

    /// What was chosen, each with a Change link to its step, and a few more switches.
    fn done_page(&self, setup: &WeakEntity<OnboardingWindow>, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let summary = |icon: &'static str, label: &'static str, value: Div, step: Step| div().flex().items_center().gap(px(12.0)).px(px(14.0)).py(px(10.0))
            .child(div().w(px(120.0)).flex_none().flex().items_center().gap(px(8.0)).child(ui::icon(icon, 15.0, theme::muted())).child(text(13.0, theme::muted(), label)))
            .child(value.flex_1().min_w_0().flex().items_center().gap(px(8.0)))
            .child(div().id(("change", step as usize)).flex_none().cursor_pointer().text_size(px(12.0)).text_color(theme::accent_soft())
                .on_mouse_down(MouseButton::Left, on_setup(setup, move |this, window, cx| this.navigate(Nav::Go(step), window, cx))).child("Change"));
        let (name, logo, tint) = match s.provider {
            Provider::Codex => (copy::CHATGPT.0, copy::CHATGPT.1, theme::body()),
            Provider::Claude => (copy::CLAUDE.0, copy::CLAUDE.1, rgb(CLAUDE_ORANGE)),
            Provider::ApiKey => (providers::preset(&s.api_provider).map_or(copy::API_KEY.0, |preset| preset.label), preset_logo(&s.api_provider), theme::body()),
        };
        let answers = div().child(ui::icon(logo, 14.0, tint)).child(text(13.0, theme::text(), name)).child(text(12.0, theme::muted(), self.model_choice().value).truncate());
        let (icon, name, state) = match (s.transcribe, s.stt_provider) {
            (false, _) => (copy::OFF.0, copy::OFF.1, None),
            (true, SttProvider::Parakeet) => (copy::ON_MAC.0, copy::ON_MAC.1, match self.download_progress() {
                Some(progress) => Some(format!("· downloading {:.0}%", progress * 100.0)),
                None => (!self.model_installed).then(|| "· model not downloaded".to_string()),
            }),
            (true, SttProvider::Deepgram) => (copy::DEEPGRAM.0, copy::DEEPGRAM.1, (!s.saved_keys.contains_key(deepgram::PROVIDER_ID)).then(|| "· no key saved".to_string())),
        };
        let transcription = div().child(ui::icon(icon, 14.0, theme::body())).child(text(13.0, theme::text(), name)).children(state.map(|state| text(12.0, theme::muted(), state)));
        let privacy = privacy_facts(s).into_iter().fold(div(), |facts, (icon, words)| facts.child(
            div().flex().items_center().gap(px(6.0)).mr(px(6.0)).child(ui::icon(icon, 14.0, theme::body())).child(text(13.0, theme::text(), words))));
        let more = vec![
            toggle_row(copy::MENU_BAR, menu_bar_switch(s, cx)),
            toggle_row(copy::DOCK, switch("show-in-dock", s.show_in_dock, |s, v| s.show_in_dock = v, cx)),
            toggle_row(copy::LIVE_ON_LAUNCH, switch("live-on-launch", s.start_live_on_launch, |s, v| s.start_live_on_launch = v, cx)),
            toggle_row(copy::UPDATES, switch("check-for-updates", s.check_for_updates, |s, v| s.check_for_updates = v, cx)),
        ];
        div().flex().flex_col().gap(px(20.0))
            .child(div().flex().items_center().gap(px(14.0))
                .child(div().size(px(44.0)).flex_none().flex().items_center().justify_center().rounded(px(12.0)).bg(rgb(0x12261c)).border_1().border_color(rgb(0x23503a))
                    .child(ui::icon("icons/check.svg", 22.0, theme::ok())))
                .child(div().flex().flex_col().gap(px(4.0))
                    .child(div().text_size(px(20.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(copy::DONE.0))
                    .child(div().flex().items_center().gap(px(6.0)).child(text(13.0, theme::muted(), copy::DONE.1)).child(keycaps(&self.hotkeys.label(Action::Toggle))))))
            .child(boxed(vec![
                summary("icons/sparkles.svg", "Answers", answers, Step::Model),
                summary("icons/tab-listening.svg", "Transcription", transcription, Step::Transcription),
                summary("icons/lock.svg", "Privacy", privacy, Step::Privacy),
            ]))
            .child(section("OPTIONAL", "Settings › Window, About", more))
            .child(note(copy::RERUN))
    }
}

/// What stays on this Mac and what is sent out, under the app's tile.
fn welcome() -> Div {
    let card = |icon: &'static str, color: Rgba, (title, items): (&'static str, [copy::Row; 3])| {
        items.into_iter().fold(div().flex_1().min_w_0().flex().flex_col().gap(px(14.0)).p(px(16.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
            .child(div().flex().items_center().gap(px(8.0)).child(ui::icon(icon, 16.0, color)).child(text(11.0, color, title).font_weight(FontWeight::SEMIBOLD))),
            |card, (icon, item, detail)| card.child(div().flex().items_center().gap(px(12.0))
                .child(div().size(px(32.0)).flex_none().flex().items_center().justify_center().rounded(px(8.0)).bg(theme::field()).border_1().border_color(theme::hairline())
                    .child(ui::icon(icon, 16.0, theme::body())))
                .child(div().flex().flex_col().gap(px(1.0)).min_w_0()
                    .child(text(13.0, theme::text(), item).font_weight(FontWeight::MEDIUM)).child(text(12.0, theme::muted(), detail)))))
    };
    // A colour image (the tile has a gradient), so `img` rather than a tinted `svg`.
    let app_tile = img(ImageSource::Resource(Resource::Embedded("app-icon/app-icon.svg".into()))).size(px(64.0)).flex_none();
    div().flex().flex_col().gap(px(24.0))
        .child(div().flex().flex_col().items_center().gap(px(14.0)).pt(px(8.0)).child(app_tile)
            .child(div().flex().flex_col().items_center().gap(px(4.0))
                .child(div().text_size(px(24.0)).line_height(px(30.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(copy::WELCOME.0))
                .child(text(13.0, theme::muted(), copy::WELCOME.1))))
        .child(div().flex().gap(px(12.0)).child(card("icons/laptop-minimal.svg", theme::ok(), copy::STAYS)).child(card("icons/arrow-right.svg", theme::accent_soft(), copy::SENT)))
        .child(copy::FACTS.iter().fold(div().flex().justify_center().gap(px(20.0)), |facts, (icon, words)| facts.child(
            div().flex().items_center().gap(px(6.0)).child(ui::icon(icon, 14.0, theme::muted())).child(text(12.0, theme::muted(), *words)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_reads_the_privacy_switches_and_provider_logos() {
        let words = |s: &Settings| privacy_facts(s).map(|(_, words)| words);
        assert_eq!(words(&Settings::default()), ["Hidden", "Screenshots", "30 days"]);
        let changed = Settings { hide_from_capture: false, screen_on_send: false, save_sessions: false, ..Settings::default() };
        assert_eq!(words(&changed), ["Visible when sharing", "No screenshots", "Not saved"]);
        assert_eq!(preset_logo("gemini"), "logos/googlegemini.svg");
        assert_eq!(preset_logo("xai"), "icons/key-round.svg", "providers without a chip");
    }
}
