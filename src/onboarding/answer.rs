//! Setup › Answer model: a ChatGPT or Claude plan through its CLI, or an API key. Signing in runs
//! the CLI's own login in a console window ("Sign in" never opens a browser itself), so what the
//! user sees and approves is the CLI's. An API key's provider, key and model are set here too.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{Animation, AnimationExt, AnyElement, Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Styled, Transformation, Window, div,
    percentage, prelude::*, px};

use super::steps::{external, link, text};
use super::{Menu, Setup, Step};
use crate::claude_cli::ClaudeCli;
use crate::overlay::Overlay;
use crate::providers;
use crate::settings::Provider;
use crate::settings_view::{listen, plan_label};
use crate::ui_v3::{self, Kind, RADII};
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

impl AnswerState {
    pub(super) fn signing_in(&self) -> Option<Provider> { self.sign_in.as_ref().map(|sign_in| sign_in.provider) }

    /// Stop waiting for a sign-in: its console is closed, as the user would with ×.
    pub(super) fn cancel_sign_in(&self) {
        if let Some(sign_in) = &self.sign_in { sign_in.cancel.store(true, Ordering::Relaxed); }
    }

    /// What changes the answer (the path, provider or key) makes the last check stale.
    fn changed(&mut self) { self.key_check = None; self.failed = None; }
}

fn cli_slot(provider: Provider) -> usize { if provider == Provider::Codex { 0 } else { 1 } }

/// A provider preset's logo for the Provider dropdown; a custom endpoint gets the link.
fn provider_logo(id: &str) -> &'static str {
    match id {
        "anthropic" => "logos/anthropic.svg", "openai" => "logos/openai.svg", "openrouter" => "logos/openrouter.svg",
        "gemini" => "logos/googlegemini.svg", "xai" => "logos/xai.svg", "groq" => "logos/groq.svg", "deepseek" => "logos/deepseek.svg",
        "mistral" => "logos/mistral.svg", "together" => "logos/together.svg", "ollama" => "logos/ollama.svg", "lmstudio" => "logos/lmstudio.svg",
        _ => "icons/setup-link.svg",
    }
}

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

    /// A key was saved during setup. An answer key is checked at once by listing its provider's
    /// models; the list itself isn't shown, as setup takes a typed model id.
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

    /// The model box takes a typed id on the API key path; its placeholder shows one the chosen
    /// provider takes. The box is Settings › Model's too, so `close_setup` puts its own back.
    pub(super) fn show_model_example(&self, cx: &mut Context<Self>) {
        let example = providers::preset(&self.store.value.api_provider).and_then(|preset| preset.suggested_models.first());
        let placeholder = example.map_or_else(|| "Type the model id".to_string(), |model| format!("e.g. {model}"));
        self.model_input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
    }

    /// ChatGPT, Claude or an API key; the chosen one's account (or key and model) under them.
    pub(super) fn answer_model(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let chosen = self.store.value.provider;
        let card = |provider: Provider, logo: AnyElement, title: &'static str, detail: String, cx: &mut Context<Self>| {
            ui_v3::answer_card(("setup-provider", provider as usize), logo, title, detail, chosen == provider)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.choose_path(provider, window, cx)))
        };
        let cards = div().flex().gap(px(12.0))
            .child(card(Provider::Codex, ui::icon("logos/openai.svg", 24.0, theme::text()).into_any_element(), "ChatGPT", "Your subscription".into(), cx))
            .child(card(Provider::Claude, ui::icon("logos/claude.svg", 24.0, theme::logo_ink()).into_any_element(), "Claude", "Your subscription".into(), cx))
            .child(card(Provider::ApiKey, ui::icon("icons/setup-key.svg", 22.0, ui_v3::tile_ink(chosen == Provider::ApiKey)).into_any_element(), "API key",
                format!("{} providers", providers::PRESETS.len()), cx));
        let below = match chosen {
            Provider::ApiKey => self.api_form(setup, cx),
            plan => self.account(plan, setup, cx),
        };
        ui_v3::setup_page(div().gap(px(32.0)).child(ui_v3::heading("Choose what answers", "Use a plan you already have, or your own key."))
                .child(div().w(px(504.0)).flex().flex_col().items_center().gap(px(12.0)).child(cards).child(below)),
            div().child(self.next_button(Step::AnswerModel, "Continue", Kind::Primary, cx)), false)
    }

    /// A plan's account: signed in (and Sign out), Sign in (the CLI's login in a console), waiting for
    /// it, or where to get the CLI.
    fn account(&self, provider: Provider, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let status = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let name = if provider == Provider::Codex { "Codex CLI" } else { "Claude Code" };
        let alert = || ui::icon("icons/setup-circle-alert.svg", 16.0, theme::muted()).into_any_element();
        let sign_in = move |label: &'static str, cx: &mut Context<Self>| ui_v3::button("setup-sign-in", label, Kind::Primary)
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.start_sign_in(provider, window, cx)));
        if setup.answer.signing_in() == Some(provider) {
            let pid = setup.answer.sign_in.as_ref().map_or(0, |sign_in| sign_in.pid);
            let spinner = gpui::svg().path("icons/spinner.svg").size(px(16.0)).flex_none().text_color(theme::accent_soft())
                .with_animation("setup-signing-in", Animation::new(Duration::from_millis(900)).repeat(),
                    |spinner, delta| spinner.with_transformation(Transformation::rotate(percentage(delta))));
            return ui_v3::account_line(spinner, "Waiting for sign-in in the console…", div().flex().items_center().gap(px(12.0))
                .child(ui_v3::button("setup-show-terminal", "Show console", Kind::Secondary).on_mouse_down(MouseButton::Left, move |_, _, _| platform::raise_console(pid)))
                .child(link("setup-cancel-sign-in", "Cancel")
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| if let Some(setup) = &this.setup { setup.answer.cancel_sign_in(); cx.notify(); }))));
        }
        match (status, &setup.answer.cli[cli_slot(provider)]) {
            (_, Some(Err(_))) => {
                let page = crate::settings_view::install_page(provider).unwrap_or_default();
                ui_v3::account_line(alert(), format!("{name} isn't installed"), div().flex().items_center().gap(px(12.0))
                    .child(external("setup-install", "How to install", page, cx))
                    .child(ui_v3::button("setup-locate", "Locate…", Kind::Secondary)
                        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.locate_cli(provider, window, cx)))))
            }
            (Some(status), _) if status.signed_in => {
                let who = status.account.clone().unwrap_or_else(|| "Signed in".into());
                let who = match status.plan.as_deref() { Some(plan) => format!("{who} · {}", plan_label(provider, plan).trim_start_matches("ChatGPT ")), None => who };
                let sign_out = (provider == Provider::Codex).then(|| link("setup-sign-out", "Sign out")
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.sign_out_codex(window, cx))));
                ui_v3::account_line(ui::icon("icons/setup-circle-check.svg", 16.0, theme::ok()), who, div().children(sign_out))
            }
            (None, _) | (_, None) => ui_v3::account_line(alert(), "Checking your account…", div()),
            (Some(_), Some(Ok(_))) if setup.answer.failed == Some(provider) =>
                ui_v3::account_line(alert(), "Sign-in didn't finish", sign_in("Try again", cx)),
            (Some(_), Some(Ok(_))) => ui_v3::account_line(alert(), "Not signed in", sign_in("Sign in", cx)),
        }
    }

    /// The provider (a dropdown), its key (or base URL for a custom one) and the model id, typed.
    fn api_form(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let current = providers::preset(&s.api_provider);
        let open = setup.menu == Some(Menu::ApiProvider);
        let face = div().id("setup-provider-face").w(px(168.0)).h(px(40.0)).flex().items_center().gap(px(9.0)).px(px(12.0)).rounded(px(RADII.control)).cursor_pointer()
            .bg(theme::field()).border_1().border_color(if open { theme::accent() } else { theme::hairline() })
            .child(ui::icon(provider_logo(&s.api_provider), 15.0, theme::text()))
            .child(text(13.0, 16.0, theme::text(), current.map_or("Provider", |preset| preset.label.trim_end_matches(" (local)"))).flex_1().min_w_0().truncate()
                .font_weight(FontWeight::MEDIUM))
            .child(ui::icon("icons/chevron-down.svg", 12.0, theme::muted()));
        let presets = providers::PRESETS.iter().map(|preset| (preset.id.to_string(), preset.label.to_string())).collect();
        let provider = self.setup_dropdown(setup, Menu::ApiProvider, face, 168.0, 44.0, presets, Some(provider_logo), &s.api_provider, |this, id, window, cx| {
            if let Some(setup) = &mut this.setup { setup.answer.changed(); setup.replacing_key = false; }
            this.loaded_models.clear();
            this.update_settings(|s| s.api_provider = id, window, cx);
            let model = this.store.value.api_model().to_string();
            this.model_input.update(cx, |input, cx| input.set_text(model, cx));
            this.show_model_example(cx);
        }, cx);
        let mut form = div().w(px(504.0)).flex().flex_col().gap(px(8.0));
        let Some(preset) = current else { return form.child(provider) };
        let refused = matches!(setup.answer.key_check, Some(Err(_)));
        let saved = s.saved_keys.get(preset.id).map(String::as_str);
        let second: AnyElement = if preset.local {
            div().flex_1().h(px(40.0)).flex().items_center().px(px(12.0)).rounded(px(RADII.control)).bg(theme::field()).border_1().border_color(theme::hairline())
                .child(text(12.0, 16.0, theme::muted(), "Runs on this computer · no key needed")).into_any_element()
        } else if preset.base_url.is_empty() {
            self.text_field("setup-base-url", self.base_url_input.clone(), None, Some(text(12.0, 16.0, theme::placeholder(), "Base URL").into_any_element()), 40.0)
                .into_any_element()
        } else {
            self.key_field(setup, saved, refused, 40.0, cx).into_any_element()
        };
        form = form.child(div().flex().gap(px(8.0)).child(provider).child(second));
        // A custom endpoint's key is optional, on its own line.
        if preset.base_url.is_empty() && !preset.local && preset.needs_key { form = form.child(self.key_field(setup, saved, refused, 40.0, cx)); }
        let status = match &setup.answer.key_check {
            Some(Err(why)) => Some(text(12.0, 16.0, theme::danger(), why.clone())),
            _ => self.key_notice.clone().map(|notice| text(12.0, 16.0, theme::muted(), notice)),
        };
        form.child(self.text_field("setup-model-id", self.model_input.clone(), Some("icons/setup-sparkle.svg"),
                Some(text(12.0, 16.0, theme::placeholder(), "Model id").into_any_element()), 40.0))
            .children(status.map(|status| status.px(px(2.0))))
    }
}
