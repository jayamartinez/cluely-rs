//! Setup › Answer model: a ChatGPT or Claude plan through its CLI, or an API key. Signing in runs
//! the CLI's own login in a console window ("Sign in" never opens a browser itself), so what the
//! user sees and approves is the CLI's. The key, the model and a test answer are set here too.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Animation, AnimationExt, Context, Transformation, percentage, Div, IntoElement, MouseButton, ParentElement, Styled, Window, div, prelude::*, px, rgb};

use super::steps::{self, ERROR, boxed, external, heading, labelled, link, option_card, page, small_button, status_line, trailing};
use super::{Menu, Setup};
use crate::chat::SubscriptionStatus;
use crate::claude_cli::ClaudeCli;
use crate::overlay::Overlay;
use crate::providers::{self, Message, Part, Role};
use crate::settings::Provider;
use crate::settings_view::{listen, plan_label};
use crate::{platform, theme, ui};

/// How often a terminal sign-in is checked for having ended.
const SIGN_IN_POLL: Duration = Duration::from_millis(300);

/// The Answer model step's own state.
#[derive(Default)]
pub(super) struct AnswerState {
    /// Where each CLI (Codex, Claude) was found, or why it wasn't; `None` while looking.
    cli: [Option<Result<String, String>>; 2],
    sign_in: Option<SignIn>,
    /// The last terminal sign-in ended without signing in.
    failed: Option<Provider>,
    pub(super) test: Option<Test>,
    /// The saved answer key, checked by listing the provider's models: `Err` says why it was refused.
    key_check: Option<Result<(), String>>,
    /// The step has opened once, on the path that was ready.
    chose_path: bool,
}

/// A CLI login running in its own console window.
struct SignIn {
    provider: Provider,
    pid: u32,
    cancel: Arc<AtomicBool>,
}

/// A one-word answer from the chosen model, to show it works.
pub(super) enum Test { Running, Answered(Duration), Failed(String) }

impl AnswerState {
    pub(super) fn signing_in(&self) -> Option<Provider> { self.sign_in.as_ref().map(|sign_in| sign_in.provider) }

    /// Stop waiting for a sign-in: its console is closed, as the user would with ×.
    pub(super) fn cancel_sign_in(&self) {
        if let Some(sign_in) = &self.sign_in { sign_in.cancel.store(true, Ordering::Relaxed); }
    }

    /// What changes the answer (the path, provider, key or model) makes the last test stale.
    fn changed(&mut self) { self.test = None; self.key_check = None; self.failed = None; }
}

fn cli_slot(provider: Provider) -> usize { if provider == Provider::Codex { 0 } else { 1 } }

impl Overlay {
    /// Look for both CLIs in the background, to show where they are (or that they're missing).
    pub(super) fn find_clis(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let found = cx.background_executor().spawn(async move {
                let path = |found: Result<std::path::PathBuf, String>| found.map(|path| path.display().to_string());
                [path(crate::codex::executable()), path(ClaudeCli::executable())]
            }).await;
            let _ = this.update(cx, |this, cx| if let Some(setup) = &mut this.setup { setup.answer.cli = found.map(Some); cx.notify(); });
        }).detach();
    }

    /// On its first visit the step opens on a path that is ready to answer, if the chosen one isn't.
    pub(super) fn choose_ready_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        if std::mem::replace(&mut setup.answer.chose_path, true) { return; }
        let ready = |provider| self.path_ready(provider);
        if ready(self.store.value.provider) { return; }
        if let Some(provider) = [Provider::Codex, Provider::Claude, Provider::ApiKey].into_iter().find(|provider| ready(*provider)) {
            self.update_settings(|s| s.provider = provider, window, cx);
        }
    }

    fn path_ready(&self, provider: Provider) -> bool {
        let s = &self.store.value;
        match provider {
            Provider::Codex => self.codex_status.as_ref().is_some_and(|status| status.signed_in),
            Provider::Claude => self.claude_status.as_ref().is_some_and(|status| status.signed_in),
            Provider::ApiKey => providers::preset(&s.api_provider).is_some_and(|preset| !preset.needs_key || s.saved_keys.contains_key(preset.id)),
        }
    }

    fn choose_path(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup { setup.answer.changed(); }
        self.update_settings(|s| s.provider = provider, window, cx);
    }

    /// Run the CLI's own login (`codex login`, `claude auth login --claudeai`) in a console window,
    /// then read the sign-in again once it ends. A Codex app-server already running still has the
    /// old account, so it is stopped first and the check starts a fresh one.
    fn start_sign_in(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        if setup.answer.sign_in.is_some() { return; }
        let Some(Ok(path)) = setup.answer.cli[cli_slot(provider)].clone() else { return };
        let args: &[&str] = match provider { Provider::Codex => &["login"], Provider::Claude => &["auth", "login", "--claudeai"], Provider::ApiKey => return };
        setup.answer.changed();
        let mut child = match platform::run_in_console(Path::new(&path), args) {
            Ok(child) => child,
            Err(error) => {
                eprintln!("sign-in console could not start: {error}");
                setup.answer.failed = Some(provider);
                cx.notify();
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        setup.answer.sign_in = Some(SignIn { provider, pid: child.id(), cancel: cancel.clone() });
        cx.notify();
        let codex = self.codex.clone();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                if !matches!(child.try_wait(), Ok(None)) { break; }
                cx.background_executor().timer(SIGN_IN_POLL).await;
            }
            let cancelled = cancel.load(Ordering::Relaxed);
            let status = cx.background_executor().spawn(async move {
                match provider { Provider::Codex => { codex.shutdown(); codex.status() } _ => ClaudeCli::status() }
            }).await;
            let _ = this.update(cx, |this, cx| {
                let signed_in = status.signed_in;
                match provider { Provider::Codex => this.codex_status = Some(status), _ => this.claude_status = Some(status) }
                if let Some(setup) = &mut this.setup {
                    setup.answer.sign_in = None;
                    if !signed_in && !cancelled { setup.answer.failed = Some(provider); }
                }
                cx.notify();
            });
        }).detach();
    }

    /// Ask the chosen model for one word, timing the answer.
    fn test_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        if matches!(setup.answer.test, Some(Test::Running)) { return; }
        setup.answer.test = Some(Test::Running);
        cx.notify();
        let (settings, codex) = (self.store.value.clone(), self.codex.clone());
        cx.spawn_in(window, async move |this, cx| {
            let started = Instant::now();
            let result = cx.background_executor().spawn(async move {
                let messages = vec![Message { role: Role::User, parts: vec![Part::Text("Reply with the single word OK.".into())] }];
                crate::answer::target(&settings, &codex, "This is a connection test.".into(), messages, 16).and_then(crate::answer::complete)
            }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(setup) = &mut this.setup else { return };
                setup.answer.test = Some(match result { Ok(_) => Test::Answered(started.elapsed()), Err(error) => Test::Failed(error) });
                cx.notify();
            });
        }).detach();
    }

    /// A key was saved during setup. An answer key is checked at once by listing its provider's
    /// models, which fills the model list too.
    pub(crate) fn setup_key_saved(&mut self, provider: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        setup.replacing_key = false;
        if provider == crate::stt::deepgram::PROVIDER_ID { return; }
        let Some(preset) = providers::preset(provider) else { return };
        setup.answer.changed();
        let base = if preset.base_url.is_empty() { self.store.value.custom_base_url.trim().to_string() } else { preset.base_url.to_string() };
        cx.spawn_in(window, async move |this, cx| {
            let listed = cx.background_executor().spawn(async move {
                let key = crate::secrets::get(preset.id);
                providers::list_models(preset.wire, &base, key.as_deref())
            }).await;
            let _ = this.update(cx, |this, cx| {
                // Another provider was chosen meanwhile: the answer is about the old one.
                if this.store.value.api_provider != preset.id { return; }
                let check = match listed {
                    Ok(models) => { this.loaded_models = models; Ok(()) }
                    Err(providers::ProviderError::Unauthorized) => Err(format!("401 · {} rejected the key", preset.label)),
                    Err(error) => Err(error.to_string()),
                };
                if let Some(setup) = &mut this.setup { setup.answer.key_check = Some(check); }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn answer_model(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let chosen = s.provider;
        let found = |status: Option<&SubscriptionStatus>| match status {
            Some(status) if status.installed => steps::status_text("Found", theme::ok()),
            Some(_) => steps::status_text("Not found", theme::muted()),
            None => div().into_any_element(),
        };
        let tint = |provider| if chosen == provider { theme::accent_soft() } else { theme::body() };
        let key_saved = providers::preset(&s.api_provider).is_some_and(|preset| s.saved_keys.contains_key(preset.id));
        let strip = div().h(px(16.0)).flex().items_center().gap(px(7.0))
            .children(["icons/logo-anthropic.svg", "icons/logo-openai.svg", "icons/logo-gemini.svg", "icons/logo-openrouter.svg"]
                .map(|logo| ui::icon(logo, 13.0, theme::muted())))
            .child(div().text_size(px(12.0)).text_color(theme::muted()).child(format!("+{}", providers::PRESETS.len() - 4)));
        let cards = [
            (Provider::Codex, ui::icon("icons/logo-openai.svg", 20.0, tint(Provider::Codex)).into_any_element(), found(self.codex_status.as_ref()),
                "ChatGPT plan", steps::detail_text("Codex CLI")),
            (Provider::Claude, ui::icon("icons/logo-claude.svg", 20.0, rgb(0xd97757)).into_any_element(), found(self.claude_status.as_ref()),
                "Claude plan", steps::detail_text("Claude Code")),
            (Provider::ApiKey, ui::icon("icons/setup-key.svg", 20.0, tint(Provider::ApiKey)).into_any_element(),
                if key_saved { steps::status_text("Key saved", theme::ok()) } else { steps::status_text("Pay per use", theme::muted()) },
                "Your API key", strip.into_any_element()),
        ];
        let mut row = div().flex().gap(px(10.0));
        for (index, (provider, icon, status, name, detail)) in cards.into_iter().enumerate() {
            row = row.child(option_card(("setup-provider", index), chosen == provider, icon, status, name, detail)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.choose_path(provider, window, cx))));
        }
        let (rows, status) = match chosen {
            Provider::ApiKey => self.api_key_rows(setup, cx),
            plan => self.plan_rows(plan, setup, cx),
        };
        page().child(heading("Choose what answers", "A subscription you already pay for, or your own key."))
            .child(row)
            .child(boxed(rows))
            .children(status)
    }

    /// A plan's CLI, the account it's signed in to, and the model.
    fn plan_rows(&self, provider: Provider, setup: &Setup, cx: &mut Context<Self>) -> (Vec<Div>, Option<Div>) {
        let status = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let name = if provider == Provider::Codex { "Codex CLI" } else { "Claude Code" };
        let locate = link("setup-locate", "Locate…")
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.locate_cli(provider, window, cx)));
        let cli = match &setup.answer.cli[cli_slot(provider)] {
            None => labelled("icons/setup-terminal.svg", name, steps::muted_text("Looking…")),
            Some(Ok(path)) => labelled("icons/setup-terminal.svg", name, div().font_family(theme::MONO).text_size(px(11.0)).line_height(px(14.0))
                    .text_color(theme::body()).truncate().child(path.clone()))
                .child(ui::icon("icons/setup-check.svg", 14.0, theme::ok()))
                .child(trailing(64.0, locate)),
            Some(Err(_)) => {
                let page = crate::settings_view::install_page(provider).unwrap_or_default();
                labelled("icons/setup-terminal.svg", name, div().flex().items_center().gap(px(10.0))
                        .child(steps::muted_text("Not found"))
                        .child(external("setup-install", "How to install").on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page)))))
                    .child(trailing(64.0, locate))
            }
        };
        let signed_in = status.is_some_and(|status| status.signed_in);
        let waiting = setup.answer.signing_in() == Some(provider);
        let can_sign_in = matches!(setup.answer.cli[cli_slot(provider)], Some(Ok(_)));
        let sign_in = move |label: &'static str, primary: bool, cx: &mut Context<Self>| small_button("setup-sign-in", label, primary)
            .when(primary, |button| button.child(ui::icon("icons/setup-terminal.svg", 13.0, theme::accent_ink())))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.start_sign_in(provider, window, cx)));
        let account = if waiting {
            let pid = setup.answer.sign_in.as_ref().map_or(0, |sign_in| sign_in.pid);
            let spinner = gpui::svg().path("icons/spinner.svg").size(px(16.0)).flex_none().text_color(theme::accent_soft())
                .with_animation("setup-signing-in", Animation::new(Duration::from_millis(900)).repeat(),
                    |spinner, delta| spinner.with_transformation(Transformation::rotate(percentage(delta))));
            steps::labelled_with(spinner, "Account", div().text_size(px(13.0)).text_color(theme::text()).child("Waiting for sign-in in Terminal…"))
                .bg(rgb(0x121a2b))
                .child(small_button("setup-show-terminal", "Show Terminal", false).h(px(28.0)).px(px(10.0)).rounded(px(7.0))
                    .child(ui::icon("icons/setup-terminal.svg", 13.0, rgb(steps::LIGHT)))
                    .on_mouse_down(MouseButton::Left, move |_, _, _| platform::raise_console(pid)))
                .child(trailing(48.0, link("setup-cancel-sign-in", "Cancel")
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| if let Some(setup) = &this.setup { setup.answer.cancel_sign_in(); cx.notify(); }))))
        } else if signed_in {
            let status = status.expect("signed in");
            let who = status.account.clone().unwrap_or_else(|| "Signed in".into());
            let who = match status.plan.as_deref() { Some(plan) => format!("{who} · {}", plan_label(provider, plan)), None => who };
            let mut row = labelled("icons/setup-person.svg", "Account", div().text_size(px(13.0)).text_color(theme::text()).truncate().child(who))
                .child(ui::icon("icons/setup-check.svg", 14.0, theme::ok()));
            if provider == Provider::Codex {
                row = row.child(trailing(64.0, link("setup-sign-out", "Sign out")
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.sign_out_codex(window, cx)))));
            }
            row
        } else if status.is_none() {
            labelled("icons/setup-person.svg", "Account", steps::muted_text("Checking…"))
        } else if setup.answer.failed == Some(provider) {
            labelled("icons/setup-circle-alert.svg", "Account", div().text_size(px(13.0)).text_color(rgb(ERROR)).child("Sign-in didn't finish"))
                .child(sign_in("Try again", false, cx))
        } else {
            labelled("icons/setup-person.svg", "Account", div().text_size(px(13.0)).text_color(theme::body()).child("Not signed in"))
                .when(can_sign_in, |row| row.child(sign_in("Sign in", true, cx)))
        };
        let model = self.setup_model_row(signed_in && !waiting, cx);
        (vec![cli, account, model], self.test_status(setup))
    }

    /// The provider, its key (or base URL for a custom one) and the model.
    fn api_key_rows(&self, setup: &Setup, cx: &mut Context<Self>) -> (Vec<Div>, Option<Div>) {
        let s = &self.store.value;
        let presets = providers::PRESETS.iter().map(|preset| (preset.id.to_string(), preset.label.to_string())).collect();
        let current = providers::preset(&s.api_provider);
        let dropdown = self.setup_dropdown(setup, Menu::ApiProvider, current.map_or("Choose a provider", |preset| preset.label), presets, &s.api_provider, 32.0,
            |this, id, window, cx| {
                if let Some(setup) = &mut this.setup { setup.answer.changed(); setup.replacing_key = false; }
                this.loaded_models.clear();
                this.update_settings(|s| s.api_provider = id, window, cx);
                let model = this.store.value.api_model().to_string();
                this.model_input.update(cx, |input, cx| input.set_text(model, cx));
            }, cx);
        let mut provider = labelled("icons/setup-globe.svg", "Provider", dropdown);
        let Some(preset) = current else { return (vec![provider], None) };
        if !preset.key_page.is_empty() {
            let page = preset.key_page;
            provider = provider.child(trailing(76.0, external("setup-key-page", "Get a key").on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page)))));
        }
        let mut rows = vec![provider];
        let refused = matches!(setup.answer.key_check, Some(Err(_)));
        if preset.needs_key {
            let (field, action) = self.key_field(setup, s.saved_keys.get(preset.id).map(String::as_str), refused, cx);
            rows.push(labelled("icons/setup-key.svg", "API key", field).child(trailing(76.0, action)));
        }
        if preset.base_url.is_empty() {
            rows.push(labelled("icons/setup-link.svg", "Base URL", self.text_field("setup-base-url", self.base_url_input.clone()))
                .child(trailing(76.0, steps::muted_text("Key optional"))));
        }
        rows.push(self.setup_model_row(true, cx));
        let status = match &setup.answer.key_check {
            Some(Err(why)) => Some(status_line("icons/setup-circle-alert.svg", why.clone(), rgb(ERROR))),
            _ => self.test_status(setup).or_else(|| self.key_notice.clone().map(|notice| status_line("icons/info.svg", notice, theme::muted()))),
        };
        (rows, status)
    }

    /// The model, from the provider's list (a typed id for providers that don't list), and Test.
    fn setup_model_row(&self, enabled: bool, cx: &mut Context<Self>) -> Div {
        let Some(setup) = &self.setup else { return div() };
        let choice = self.model_choice();
        let provider = self.store.value.provider;
        let picker = if choice.options.is_empty() && provider == Provider::ApiKey {
            self.text_field("setup-model-id", self.model_input.clone())
        } else {
            let set = choice.set;
            self.setup_dropdown(setup, Menu::Model, choice.value, choice.options, &choice.selected, 32.0, move |this, id, window, cx| {
                if let Some(setup) = &mut this.setup { setup.answer.test = None; }
                if this.store.value.provider == Provider::ApiKey { this.choose_model(id, window, cx) } else { this.update_settings(|s| set(s, id), window, cx) }
            }, cx)
        };
        let testing = matches!(setup.answer.test, Some(Test::Running));
        let test = div().id("setup-test").w(px(76.0)).h(px(32.0)).flex_none().flex().items_center().justify_center().gap(px(6.0)).rounded(px(8.0))
            .bg(theme::field()).border_1().border_color(theme::hairline())
            .child(ui::icon("icons/setup-play.svg", 12.0, rgb(steps::LIGHT)))
            .child(div().text_size(px(13.0)).text_color(rgb(steps::LIGHT)).child(if testing { "…" } else { "Test" }))
            .when(enabled && !testing, |test| test.cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.test_answer(window, cx))));
        labelled("icons/setup-chip.svg", "Model", picker).child(test).when(!enabled, |row| row.opacity(0.4))
    }

    fn test_status(&self, setup: &Setup) -> Option<Div> {
        Some(match setup.answer.test.as_ref()? {
            Test::Running => status_line("icons/setup-play.svg", "Asking…", theme::muted()),
            Test::Answered(took) => status_line("icons/setup-circle-check.svg", format!("Answered in {:.1} s", took.as_secs_f32()), theme::ok()),
            Test::Failed(why) => status_line("icons/setup-circle-alert.svg", why.clone(), rgb(ERROR)),
        })
    }

    /// Whether setup's model was tested and answered, for the Done step.
    pub(super) fn answer_tested(setup: &Setup) -> bool { matches!(setup.answer.test, Some(Test::Answered(_))) }
}
