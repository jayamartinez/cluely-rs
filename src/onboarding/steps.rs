//! The pages of first-run setup. Every control changes the setting it shows at once and saves it,
//! as in Settings. Answer model, Transcription and Audio check are placeholders pointing to Settings
//! until their own setup (sign-in, download, levels) arrives.

use gpui::{Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Rgba, SharedString, Styled, deferred, div, prelude::*, px, rgb};

use super::{Setup, Step};
use crate::appearance::{CardWidth, TextSize};
use crate::archive::Retention;
use crate::chat::SubscriptionStatus;
use crate::hotkeys::Action;
use crate::overlay::Overlay;
use crate::settings::{AnswerStyle, Provider, SttProvider};
use crate::settings_view::{choice, listen, switch};
use crate::{platform, theme, ui};

/// Notes about cost and shortcuts that are taken.
const CAUTION: u32 = 0xf0c674;
const LIGHT: u32 = 0xe6e3dc;

/// The global shortcuts, as setup lists them: movement and scrolling as one row each.
const SHORTCUTS: [(&str, &str, &[Action]); 6] = [
    ("icons/setup-eye.svg", "Show / hide", &[Action::Toggle]),
    ("icons/setup-bolt.svg", "Assist", &[Action::Assist]),
    ("icons/setup-field.svg", "Type a question", &[Action::Focus]),
    ("icons/setup-grid.svg", "Switch mode", &[Action::SwitchMode]),
    ("icons/setup-move.svg", "Move the overlay", &[Action::MoveLeft, Action::MoveUp, Action::MoveDown, Action::MoveRight]),
    ("icons/setup-scroll.svg", "Scroll the answer", &[Action::ScrollUp, Action::ScrollDown]),
];

fn page() -> Div { div().flex().flex_col().gap(px(20.0)) }

/// A step's title and the line under it.
fn heading(title: &'static str, detail: &'static str) -> Div {
    div().flex().flex_col().gap(px(6.0))
        .child(div().text_size(px(22.0)).line_height(px(28.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title))
        .child(div().text_size(px(13.0)).line_height(px(18.0)).text_color(theme::muted()).child(detail))
}

/// Rows in a rounded box, a hairline between each.
fn boxed(rows: Vec<Div>) -> Div {
    let last = rows.len().saturating_sub(1);
    div().flex().flex_col().rounded(px(12.0)).border_1().border_color(theme::hairline()).overflow_hidden()
        .children(rows.into_iter().enumerate().map(|(index, row)| row.when(index < last, |row| row.border_b_1().border_color(theme::divider()))))
}

/// A section label over a box of rows.
fn group(label: &'static str, rows: Vec<Div>) -> Div {
    div().flex().flex_col().gap(px(8.0)).child(div().pl(px(2.0)).child(ui::section_label(label))).child(boxed(rows))
}

/// Icon, label and whatever follows on the right.
fn line(icon: &'static str, size: f32, label: impl Into<SharedString>, trailing: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(12.0))
        .child(ui::icon(icon, size, theme::body()))
        .child(div().flex_1().min_w_0().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::text()).child(label.into()))
        .child(trailing)
}

/// A muted line with an icon: where something is set, or where to find setup again.
fn hint(icon: &'static str, text: &'static str) -> Div {
    div().flex().items_center().gap(px(8.0)).pl(px(2.0))
        .child(ui::icon(icon, 14.0, theme::muted()))
        .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(text))
}

/// A key drawn larger than `ui::keycap`, for the keys the pages ask to press.
fn cap(label: impl Into<SharedString>, large: bool, border: Rgba, color: Rgba) -> Div {
    let (y, x, radius, size) = if large { (5.0, 9.0, 7.0, 13.0) } else { (2.0, 7.0, 6.0, 12.0) };
    div().py(px(y)).px(px(x)).rounded(px(radius)).border_1().border_color(border)
        .font_family(theme::MONO).text_size(px(size)).line_height(px(16.0)).text_color(color).child(label.into())
}

/// "Ctrl ↵" → ["Ctrl", "Enter"]: one cap per key, with Enter spelled out as the setup pages write it.
fn key_names(label: &str) -> Vec<String> {
    label.split(' ').map(|key| if key == "↵" { "Enter".to_string() } else { key.to_string() }).collect()
}

impl Overlay {
    pub(super) fn setup_step(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        match setup.step {
            Step::Welcome => welcome(),
            Step::AnswerModel => self.answer_model(cx),
            Step::Transcription => page().child(heading("Transcribe the conversation", "On this PC with Parakeet, or with Deepgram."))
                .child(hint("icons/info.svg", "Choose and download the recognizer in Settings › Listening.")),
            Step::AudioCheck => page().child(heading("Check what it hears", "Your microphone, and the other side through your speakers."))
                .child(hint("icons/info.svg", "Pick the devices in Settings › Listening.")),
            Step::Privacy => self.privacy(cx),
            Step::Shortcuts => self.shortcuts(setup),
            Step::Customize => self.customize(setup, cx),
            Step::Done => self.summary(cx),
        }
    }

    /// The keys for a shortcuts row: the shared modifiers, then each arrow ("Ctrl Alt ← ↑ ↓ →").
    fn shortcut_keys(&self, actions: &[Action]) -> Vec<String> {
        let mut keys = key_names(&self.hotkeys.label(actions[0]));
        if actions.len() > 1 {
            keys.pop();
            keys.push(actions.iter().filter_map(|action| self.hotkeys.label(*action).rsplit(' ').next().map(str::to_string)).collect::<Vec<_>>().join(" "));
        }
        keys
    }

    fn shortcut_taken(&self, actions: &[Action]) -> bool { actions.iter().any(|action| self.hotkeys.unavailable.contains(action)) }

    /// For now the three ways to answer, which choose the provider; signing in, keys and the model
    /// stay in Settings › Model.
    fn answer_model(&self, cx: &mut Context<Self>) -> Div {
        let found = |status: Option<&SubscriptionStatus>| match status {
            Some(status) if status.installed => ("Found", theme::ok()),
            Some(_) => ("Not found", theme::muted()),
            None => ("", theme::muted()),
        };
        let paths = [
            (Provider::Codex, "ChatGPT plan", "Codex CLI", found(self.codex_status.as_ref())),
            (Provider::Claude, "Claude plan", "Claude Code", found(self.claude_status.as_ref())),
            (Provider::ApiKey, "Your API key", "Anthropic, OpenAI…", ("Pay per use", theme::muted())),
        ];
        let mut cards = div().flex().gap(px(10.0));
        for (index, (provider, title, detail, (status, color))) in paths.into_iter().enumerate() {
            let selected = self.store.value.provider == provider;
            cards = cards.child(div().id(("setup-provider", index)).flex_1().flex().flex_col().gap(px(14.0)).p(px(14.0)).rounded(px(12.0)).border_1().cursor_pointer()
                .border_color(if selected { theme::bubble_border() } else { theme::hairline() })
                .when(selected, |card| card.bg(rgb(0x0d1830)))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.update_settings(|s| s.provider = provider, window, cx)))
                .child(div().h(px(16.0)).flex().justify_end().text_size(px(12.0)).text_color(color).child(status))
                .child(div().flex().flex_col().gap(px(2.0))
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child(detail))));
        }
        page().child(heading("Choose what answers", "A subscription you already pay for, or your own key."))
            .child(cards)
            .child(hint("icons/info.svg", "Sign in, add your key and pick the model in Settings › Model."))
    }

    fn privacy(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let row = |icon, label, control| line(icon, 18.0, label, control).h(px(48.0)).px(px(14.0));
        let shown = vec![
            row("icons/eye-off.svg", "Hide from screen sharing", switch("setup-hide", s.hide_from_capture, |s, v| s.hide_from_capture = v, cx).into_any_element()),
            row("icons/image.svg", "Attach my screen to each question", switch("setup-screen", s.screen_on_send, |s, v| s.screen_on_send = v, cx).into_any_element()),
        ];
        let mut kept = vec![row("icons/setup-history.svg", "Save sessions", switch("setup-save", s.save_sessions, |s, v| s.save_sessions = v, cx).into_any_element())];
        // As in Settings › History, the rest only applies while sessions are saved.
        if s.save_sessions {
            kept.push(row("icons/setup-pictures.svg", "Include screenshots",
                switch("setup-screenshots", s.save_screenshots, |s, v| s.save_screenshots = v, cx).into_any_element()));
            kept.push(row("icons/setup-calendar.svg", "Keep for", choice("setup-keep", &[(Retention::Days7, "7 days"), (Retention::Days30, "30 days"),
                (Retention::Forever, "Forever")], s.keep_sessions, |s, v| s.keep_sessions = v, cx).into_any_element()));
        }
        page().child(heading("Privacy defaults", "All on. Change any of them later."))
            .child(group("WHAT OTHERS SEE AND WHAT'S SENT", shown))
            .child(group("KEPT ON THIS PC", kept))
    }

    /// Every global shortcut, with any another app holds marked Taken, and a place to try Assist:
    /// while setup is open, Assist only lights this up.
    fn shortcuts(&self, setup: &Setup) -> Div {
        let mut rows = Vec::new();
        for (icon, label, actions) in SHORTCUTS {
            let assist = actions[0] == Action::Assist;
            let taken = self.shortcut_taken(actions);
            let badge = div().flex().items_center().gap(px(5.0)).px(px(8.0)).py(px(2.0)).rounded_full().bg(rgb(0x2a2416))
                .child(ui::icon("icons/alert-circle.svg", 12.0, rgb(CAUTION)))
                .child(div().text_size(px(12.0)).text_color(rgb(CAUTION)).child("Taken"));
            let keys = div().flex().gap(px(4.0)).when(taken, |keys| keys.opacity(0.5).line_through())
                .children(self.shortcut_keys(actions).into_iter().map(ui::keycap));
            rows.push(div().flex().items_center().gap(px(12.0)).h(px(40.0))
                .child(ui::icon(icon, 16.0, if assist { theme::accent_soft() } else { theme::body() }))
                .child(div().flex_1().text_size(px(13.0)).text_color(theme::text()).when(assist, |label| label.font_weight(FontWeight::SEMIBOLD)).child(label))
                .when(taken, |row| row.child(badge))
                .child(keys));
        }
        let assist = self.shortcut_keys(&[Action::Assist]);
        let tried = setup.tried_assist;
        let (border, key_color) = if tried { (rgb(0x24503a), theme::ok()) } else { (theme::bubble_border(), rgb(0xc9daff)) };
        let status = if tried { ui::icon("icons/check-circle.svg", 18.0, theme::ok()).into_any_element() } else {
            div().flex().items_center().gap(px(6.0))
                .child(div().size(px(6.0)).rounded_full().bg(theme::accent_soft()))
                .child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child("Waiting…"))
                .into_any_element()
        };
        let try_it = div().flex().items_center().gap(px(14.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0)).border_1().border_color(border)
            .bg(if tried { rgb(0x0f1a14) } else { rgb(0x0d1830) })
            .when(!tried, |row| row.border_dashed())
            .child(div().flex().gap(px(6.0)).children(assist.into_iter().map(|key| cap(key, true, border, key_color).when(!tried, |cap| cap.bg(rgb(0x101d38))))))
            .child(div().flex_1().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(if tried { "That's Assist" } else { "Try it now" }))
            .child(status);
        page().child(heading("Shortcuts that work anywhere", "Even with another app in front."))
            .child(boxed(rows).px(px(14.0)))
            .child(try_it)
    }

    fn customize(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let row = |icon, label, control: gpui::AnyElement| line(icon, 16.0, label, control).gap(px(10.0)).h(px(36.0));
        let title = |label, top: f32| div().pt(px(top)).pb(px(6.0)).child(ui::section_label(label));
        let sizes = |name, current: usize, set: fn(&mut crate::settings::Settings, usize), cx: &mut Context<Self>|
            choice(name, &[(0, "S"), (1, "M"), (2, "L")], current, set, cx).into_any_element();

        let mut answers = div().flex_1().flex().flex_col().gap(px(4.0))
            .child(title("ANSWERS", 0.0))
            .child(row("icons/setup-chat.svg", "Style", choice("setup-style", &[(AnswerStyle::Spoken, "Spoken"), (AnswerStyle::Standard, "Standard")],
                s.answer_style, |s, v| s.answer_style = v, cx).into_any_element()))
            .child(row("icons/setup-grid.svg", "Start in mode", self.mode_picker(setup, cx).into_any_element()))
            .child(row("icons/setup-bulb.svg", "Smart mode", switch("setup-smart", s.smart_mode, |s, v| s.smart_mode = v, cx).into_any_element()))
            .child(row("icons/setup-bolt.svg", "Answer early", switch("setup-early", s.speculative_answers, |s, v| s.speculative_answers = v, cx).into_any_element()))
            .child(row("icons/setup-eye.svg", "Show automatically", switch("setup-auto", s.auto_answer, |s, v| s.auto_answer = v, cx).into_any_element()));
        if s.speculative_answers || s.auto_answer {
            answers = answers.child(div().mt(px(6.0)).flex().items_center().gap(px(8.0)).px(px(10.0)).py(px(8.0)).rounded(px(8.0))
                .bg(rgb(0x1c1912)).border_1().border_color(rgb(0x3d3420))
                .child(ui::icon("icons/alert-circle.svg", 14.0, rgb(CAUTION)))
                .child(div().text_size(px(12.0)).text_color(rgb(LIGHT)).child("Early answers use your plan or credits")));
        }

        let login = div().id("setup-login").flex_none().cursor_pointer().child(ui::switch(setup.launch_at_login))
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.toggle_launch_at_login(cx)));
        let percent = s.background_opacity.min(100);
        let opacity = div().flex().items_center().gap(px(10.0)).h(px(36.0))
            .child(ui::icon("icons/setup-contrast.svg", 16.0, theme::body()))
            .child(div().w(px(62.0)).flex_none().text_size(px(13.0)).text_color(theme::text()).child("Opacity"))
            .child(self.opacity_slider(percent, cx))
            .child(div().w(px(30.0)).flex_none().flex().justify_end().text_size(px(12.0)).text_color(theme::muted()).child(format!("{percent}%")));
        let text_size = [TextSize::Small, TextSize::Standard, TextSize::Large].iter().position(|size| *size == s.text_size).unwrap_or(1);
        let card_width = [CardWidth::Narrow, CardWidth::Standard, CardWidth::Wide].iter().position(|width| *width == s.card_width).unwrap_or(1);
        let startup = div().flex_1().flex().flex_col().gap(px(4.0))
            .child(title("STARTUP", 0.0))
            .child(row("icons/setup-power.svg", "Open at sign-in", login.into_any_element()))
            .child(row("icons/setup-wave.svg", "Start Live on open", switch("setup-live", s.start_live_on_launch, |s, v| s.start_live_on_launch = v, cx).into_any_element()))
            .child(row("icons/setup-refresh.svg", "Check for updates", switch("setup-updates", s.check_for_updates, |s, v| s.check_for_updates = v, cx).into_any_element()))
            .child(title("LOOK", 14.0))
            .child(opacity)
            .child(row("icons/setup-text-size.svg", "Text", sizes("setup-text", text_size,
                |s, v| s.text_size = [TextSize::Small, TextSize::Standard, TextSize::Large][v], cx)))
            .child(row("icons/setup-width.svg", "Card width", sizes("setup-width", card_width,
                |s, v| s.card_width = [CardWidth::Narrow, CardWidth::Standard, CardWidth::Wide][v], cx)));
        page().child(heading("Make it yours", "Optional. Defaults are fine."))
            .child(div().flex().gap(px(28.0)).child(answers).child(startup))
    }

    /// The active mode, which the next launch starts in, with the list of modes under it.
    fn mode_picker(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let active = self.modes.active();
        let face = div().id("setup-mode").flex().items_center().gap(px(6.0)).h(px(28.0)).px(px(10.0)).rounded(px(8.0)).cursor_pointer()
            .bg(theme::field()).border_1().border_color(if setup.mode_menu { theme::accent() } else { theme::hairline() })
            .child(div().text_size(px(12.0)).text_color(theme::text()).child(active.name.clone()))
            .child(ui::icon("icons/chevron-down.svg", 10.0, theme::muted()))
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.set_mode_menu(true, cx)));
        let mut picker = div().relative().child(face);
        if setup.mode_menu {
            let mut list = ui::menu().id("setup-modes").w(px(200.0)).max_h(px(260.0)).overflow_y_scroll()
                .on_mouse_down_out(listen(cx, |this, _, _, cx| this.set_mode_menu(false, cx)));
            for (index, mode) in self.modes.modes().iter().enumerate() {
                let id = mode.id.clone();
                list = list.child(ui::menu_item(("setup-mode", index), mode.name.clone(), mode.id == active.id)
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.activate_mode(&id, cx);
                        this.set_mode_menu(false, cx);
                    })));
            }
            picker = picker.child(deferred(div().absolute().top(px(32.0)).right_0().child(list)));
        }
        picker
    }

    fn set_mode_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup && setup.mode_menu != open { setup.mode_menu = open; cx.notify(); }
    }

    /// Add or remove CluelyRS from what starts at sign-in, then show what the registry now says.
    fn toggle_launch_at_login(&mut self, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        if let Err(error) = platform::set_launch_at_login(!setup.launch_at_login) { eprintln!("could not change sign-in startup: {error}"); }
        setup.launch_at_login = platform::launch_at_login();
        cx.notify();
    }

    /// What was chosen, a row per step (each goes back to it), and where to find setup again.
    fn summary(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let keys = |action| div().flex().gap(px(8.0)).children(self.shortcut_keys(&[action]).into_iter().map(|key| cap(key, false, theme::keycap_border(), rgb(LIGHT))));
        let said = |text| div().text_size(px(13.0)).line_height(px(18.0)).text_color(theme::muted()).child(text);
        let reminder = div().flex().items_center().gap(px(8.0))
            .child(keys(Action::Toggle)).child(said("shows the overlay"))
            .child(div().w(px(12.0)).flex_none())
            .child(keys(Action::Assist)).child(said("answers"));

        let model = match s.provider {
            Provider::Codex => "ChatGPT plan · Codex CLI".to_string(),
            Provider::Claude => "Claude plan · Claude Code".to_string(),
            Provider::ApiKey => crate::providers::preset(&s.api_provider).map_or("Your API key".to_string(), |preset| format!("Your API key · {}", preset.label)),
        };
        let transcription = match (s.transcribe, s.stt_provider) {
            (false, _) => "Transcription off", (true, SttProvider::Parakeet) => "On this PC · Parakeet", (true, SttProvider::Deepgram) => "Deepgram",
        };
        let device = |name: &str, default: &str| if name.trim().is_empty() { default.to_string() } else { name.trim().to_string() };
        let kept = if !s.save_sessions { "not saved" } else { match s.keep_sessions { Retention::Days7 => "7 days", Retention::Days30 => "30 days", Retention::Forever => "kept" } };
        let privacy = format!("{} · {} · {kept}", if s.hide_from_capture { "Hidden" } else { "Shown when sharing" },
            if s.screen_on_send { "screen on send" } else { "no screenshots" });
        let taken: Vec<&str> = SHORTCUTS.iter().filter(|(_, _, actions)| self.shortcut_taken(actions)).map(|(_, label, _)| *label).collect();
        let note = |text: String, color: Rgba| div().text_size(px(12.0)).text_color(color).child(text);
        let rows = [
            (Step::AnswerModel, "icons/setup-chat.svg", model, None),
            (Step::Transcription, "icons/setup-wave.svg", transcription.to_string(),
                (s.transcribe && s.stt_provider == SttProvider::Parakeet && !self.model_installed).then(|| note("Not downloaded".into(), theme::muted()))),
            (Step::AudioCheck, "icons/setup-mic.svg", format!("{} · {}", device(&s.mic_device, "Default mic"), device(&s.desktop_device, "Default speakers")), None),
            (Step::Privacy, "icons/setup-shield.svg", privacy, None),
            (Step::Shortcuts, "icons/setup-keyboard.svg", format!("{} of {} shortcuts", SHORTCUTS.len() - taken.len(), SHORTCUTS.len()),
                taken.first().map(|label| note(format!("{label} taken"), rgb(CAUTION)))),
        ];
        let rows = rows.into_iter().map(|(step, icon, label, trailing)| {
            line(icon, 16.0, label, div().children(trailing)).h(px(42.0)).cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| this.go_to(step, cx)))
        }).collect();
        page()
            .child(div().flex().flex_col().gap(px(10.0))
                .child(div().text_size(px(22.0)).line_height(px(28.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("You're all set"))
                .child(reminder))
            .child(boxed(rows).px(px(14.0)))
            .child(hint("icons/setup-refresh.svg", "Settings › About › Run setup again"))
    }
}

fn welcome() -> Div {
    let feature = |icon, label| div().flex_1().flex().flex_col().gap(px(12.0)).p(px(16.0)).rounded(px(12.0)).bg(rgb(0x15171a)).border_1().border_color(theme::hairline())
        .child(ui::icon(icon, 24.0, theme::accent_soft()))
        .child(div().text_size(px(14.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(label));
    let column = |icon, color, title, items: [(&'static str, &'static str); 3]| div().flex_1().flex().flex_col().gap(px(10.0))
        .child(div().flex().items_center().gap(px(8.0)).pb(px(2.0)).child(ui::icon(icon, 14.0, color)).child(ui::section_label(title)))
        .children(items.map(|(icon, label)| div().flex().items_center().gap(px(10.0))
            .child(ui::icon(icon, 16.0, theme::body()))
            .child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(rgb(LIGHT)).child(label))));
    page()
        .child(div().flex().flex_col().gap(px(8.0)).pt(px(8.0))
            .child(div().text_size(px(28.0)).line_height(px(34.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("Welcome to CluelyRS"))
            .child(div().text_size(px(13.0)).line_height(px(20.0)).text_color(theme::body()).child("Answers about your call, in a card only you can see.")))
        .child(div().flex().gap(px(12.0))
            .child(feature("icons/setup-wave.svg", "Hears the call"))
            .child(feature("icons/image.svg", "Sees your screen"))
            .child(feature("icons/eye-off.svg", "Hidden from sharing")))
        .child(div().flex().gap(px(28.0)).pt(px(4.0))
            .child(column("icons/setup-monitor.svg", theme::ok(), "STAYS ON THIS PC",
                [("icons/setup-wave.svg", "Transcription"), ("icons/setup-history.svg", "Saved sessions"), ("icons/setup-key.svg", "API keys · Credential Manager")]))
            .child(column("icons/setup-arrow.svg", theme::accent_soft(), "SENT TO YOUR MODEL",
                [("icons/setup-chat.svg", "Questions and transcript"), ("icons/image.svg", "A screenshot, if on"), ("icons/setup-cloud.svg", "Audio, only with Deepgram")])))
}
