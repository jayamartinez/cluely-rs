//! Setup › Answer model: three cards (a ChatGPT or Claude plan through its CLI, or an API key) and,
//! under them, the chosen one's state in place: the plan's account (signed in, Sign in, waiting), or
//! the API key as three compact fields (provider, key, model id). Signing in runs the CLI's own login
//! in a console window ("Sign in" never opens a browser itself), so what the user sees and approves
//! is the CLI's. The plan's model and a test answer live in Settings › Model.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{AnyElement, Context, Div, Entity, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Stateful, Styled, Window, div, prelude::*, px};

use super::steps::{check_badge, heading, link, panel, small_button, spinner, tile};
use super::{Menu, Setup, Step};
use crate::claude_cli::ClaudeCli;
use crate::input::TextInput;
use crate::overlay::Overlay;
use crate::providers;
use crate::settings::Provider;
use crate::settings_view::fluent::{error_text, status};
use crate::settings_view::{listen, plan_label};
use crate::{platform, theme, ui};

/// How often a terminal sign-in is checked for having ended.
const SIGN_IN_POLL: Duration = Duration::from_millis(300);
/// The cards and what's under them.
const COLUMN: f32 = 504.0;

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
    fn signing_in(&self) -> Option<Provider> { self.sign_in.as_ref().map(|sign_in| sign_in.provider) }

    /// Stop waiting for a sign-in: its console is closed, as the user would with ×.
    pub(super) fn cancel_sign_in(&self) {
        if let Some(sign_in) = &self.sign_in { sign_in.cancel.store(true, Ordering::Relaxed); }
    }

    /// What changes the answer (the path, provider or key) makes the last check stale.
    fn changed(&mut self) { self.key_check = None; self.failed = None; }
}

fn cli_slot(provider: Provider) -> usize { if provider == Provider::Codex { 0 } else { 1 } }

/// A provider preset's logo for the Provider dropdown. Brands without a usable mark get the globe;
/// a custom endpoint gets the link.
fn provider_logo(id: &str) -> &'static str {
    match id {
        "anthropic" => "icons/logo-anthropic.svg",
        "openai" => "icons/logo-openai.svg",
        "openrouter" => "icons/logo-openrouter.svg",
        "gemini" => "icons/logo-gemini.svg",
        "deepseek" => "icons/logo-deepseek.svg",
        "mistral" => "icons/logo-mistral.svg",
        "ollama" => "icons/logo-ollama.svg",
        "lmstudio" => "icons/logo-lmstudio.svg",
        "custom" => "icons/setup-link.svg",
        _ => "icons/setup-globe.svg",
    }
}

/// The strip under the cards: an icon, what it says, and any actions on the right.
fn strip() -> Div { panel().w(px(COLUMN)).min_h(px(44.0)).flex().items_center().gap(px(10.0)).px(px(14.0)).py(px(6.0)) }

fn strip_text(text: impl Into<gpui::SharedString>, strong: bool) -> Div {
    div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(18.0))
        .when(strong, |text| text.font_weight(FontWeight::SEMIBOLD).text_color(theme::text()))
        .when(!strong, |text| text.text_color(theme::body()))
        .child(text.into())
}

/// A compact 40 px field (provider, key, model id): blue edged while it has the keyboard.
fn field(id: &'static str, focused: bool) -> Stateful<Div> {
    div().id(id).relative().h(px(40.0)).px(px(12.0)).flex().items_center().gap(px(9.0)).rounded(px(4.0)).bg(theme::field()).border_1()
        .border_color(if focused { theme::accent() } else { theme::hairline() })
}

/// Clicking gives `input` the keyboard in the setup window. A plain handler: `listen` would hand it
/// the overlay's window, which can't take this window's keyboard focus.
fn focus_on_click(field: Stateful<Div>, input: &Entity<TextInput>) -> Stateful<Div> {
    let input = input.clone();
    field.cursor_text().on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&input.focus_handle(cx)))
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
        if let Some(setup) = &mut self.setup { setup.answer.changed(); setup.menu = None; }
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

    pub(super) fn answer_model(&self, setup: &Setup, window: &Window, cx: &mut Context<Self>) -> Div {
        let chosen = self.store.value.provider;
        let cards = [
            (Provider::Codex, "icons/logo-openai.svg", 24.0, "ChatGPT", "Your subscription".to_string()),
            (Provider::Claude, "icons/logo-claude.svg", 24.0, "Claude", "Your subscription".to_string()),
            (Provider::ApiKey, "icons/setup-key.svg", 22.0, "API key", format!("{} providers", providers::PRESETS.len())),
        ];
        let mut row = div().flex().gap(px(12.0));
        for (index, (provider, icon, size, name, detail)) in cards.into_iter().enumerate() {
            let selected = chosen == provider;
            let color = match provider {
                Provider::ApiKey => if selected { theme::accent_soft() } else { theme::body() },
                Provider::Codex if selected => theme::text(),
                _ => theme::setup_logo(),
            };
            let card = div().id(("setup-provider", index)).relative().w(px(160.0)).flex_none().flex().flex_col().items_center().gap(px(10.0))
                .pt(px(20.0)).pb(px(16.0)).px(px(12.0)).rounded(px(6.0)).border_1().cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.choose_path(provider, window, cx)))
                .child(tile(44.0, selected).child(ui::icon(icon, size, color)))
                .child(div().flex().flex_col().items_center().gap(px(2.0))
                    .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(name))
                    .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(detail)));
            row = row.child(if selected { card.bg(theme::selected_row()).border_color(theme::accent()).child(check_badge()) }
                else { card.bg(theme::setup_panel()).border_color(theme::divider()).hover(|card| card.border_color(theme::keycap_border())) });
        }
        let inline = match chosen {
            Provider::ApiKey => self.api_key_fields(setup, window, cx),
            plan => vec![self.plan_strip(plan, setup, cx)],
        };
        let waiting = setup.answer.signing_in().is_some();
        Self::step_frame(28.0, 36.0, 32.0, vec![
            heading("Choose what answers", "Use a plan you already have, or your own key."),
            div().w(px(COLUMN)).flex().flex_col().items_center().gap(px(12.0)).child(row).children(inline).into_any_element(),
        ], self.primary("setup-next", "Continue", !waiting, Some(Step::Listening), cx))
    }

    /// A plan's account in one strip: signed in (with Sign out for ChatGPT), signing in, or the way
    /// to sign in; or that its CLI isn't installed.
    fn plan_strip(&self, provider: Provider, setup: &Setup, cx: &mut Context<Self>) -> AnyElement {
        let status = if provider == Provider::Codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let cli = if provider == Provider::Codex { "Codex CLI" } else { "Claude Code" };
        let sign_in = move |label: &'static str, cx: &mut Context<Self>| small_button("setup-sign-in", label, true)
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.start_sign_in(provider, window, cx)));
        let strip = if setup.answer.signing_in() == Some(provider) {
            let pid = setup.answer.sign_in.as_ref().map_or(0, |sign_in| sign_in.pid);
            strip().child(spinner("setup-signing-in"))
                .child(strip_text("Waiting for sign-in in the terminal…", false))
                .child(small_button("setup-show-terminal", "Show terminal", false).on_mouse_down(MouseButton::Left, move |_, _, _| platform::raise_console(pid)))
                .child(link("setup-cancel-sign-in", "Cancel")
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| if let Some(setup) = &this.setup { setup.answer.cancel_sign_in(); cx.notify(); })))
        } else if let Some(status) = status.filter(|status| status.signed_in) {
            let who = status.account.clone().unwrap_or_else(|| "Signed in".into());
            let who = match status.plan.as_deref() { Some(plan) => format!("{who} · {}", plan_label(provider, plan)), None => who };
            strip().child(ui::icon("icons/setup-circle-check.svg", 16.0, theme::ok())).child(strip_text(who, true))
                .when(provider == Provider::Codex, |strip| strip.child(link("setup-sign-out", "Sign out")
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.sign_out_codex(window, cx)))))
        } else if let Some(Err(_)) = &setup.answer.cli[cli_slot(provider)] {
            let page = crate::settings_view::install_page(provider).unwrap_or_default();
            strip().child(ui::icon("icons/setup-circle-alert.svg", 16.0, theme::muted())).child(strip_text(format!("{cli} isn't installed"), false))
                .child(small_button("setup-install", "How to install", false).child(ui::icon("icons/setup-external.svg", 10.0, theme::body()))
                    .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page))))
                .child(link("setup-locate", "Locate…").on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.locate_cli(provider, window, cx))))
        } else if status.is_none() || setup.answer.cli[cli_slot(provider)].is_none() {
            strip().child(spinner("setup-checking")).child(strip_text("Checking…", false))
        } else if setup.answer.failed == Some(provider) {
            strip().child(ui::icon("icons/setup-circle-alert.svg", 16.0, theme::muted())).child(strip_text("Sign-in didn't finish", false)).child(sign_in("Try again", cx))
        } else {
            strip().child(ui::icon("icons/setup-person.svg", 16.0, theme::muted())).child(strip_text("Not signed in", false)).child(sign_in("Sign in", cx))
        };
        strip.into_any_element()
    }

    /// The API key as compact fields: the provider and its key side by side, the model id under them
    /// (and a base URL for a custom endpoint), then why a key was refused, if it was.
    fn api_key_fields(&self, setup: &Setup, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = &self.store.value;
        let current = providers::preset(&s.api_provider);
        let open = setup.menu == Some(Menu::ApiProvider);
        let presets = providers::PRESETS.iter().map(|preset| (preset.id.to_string(), preset.label.to_string())).collect();
        let list = self.setup_menu(setup, Menu::ApiProvider, presets, Some(provider_logo), &s.api_provider, 44.0, true, 240.0, |this, id, window, cx| {
            if let Some(setup) = &mut this.setup { setup.answer.changed(); setup.replacing_key = false; }
            this.loaded_models.clear();
            this.update_settings(|s| s.api_provider = id, window, cx);
            this.show_model_example(cx);
        }, cx);
        let provider = field("setup-api-provider", open).w(px(168.0)).flex_none().cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.set_menu(Some(Menu::ApiProvider), cx)))
            .child(ui::icon(provider_logo(&s.api_provider), 15.0, theme::text()))
            .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::text())
                .child(current.map_or("Choose a provider", |preset| preset.label)))
            .child(ui::icon("icons/chevron-down.svg", 12.0, theme::muted()))
            .children(list);
        let focused = |input: &Entity<TextInput>| input.read(cx).focus_handle(cx).is_focused(window);
        let mut rows = Vec::new();
        let key = match current {
            None => None,
            Some(preset) if !preset.needs_key => Some(field("setup-api-key", false).flex_1()
                .child(div().flex_1().text_size(px(12.0)).text_color(theme::muted()).child("No key needed"))),
            Some(preset) => Some(match s.saved_keys.get(preset.id).filter(|_| !setup.replacing_key) {
                Some(hint) => {
                    let replace = listen(cx, |this, _, _, cx| if let Some(setup) = &mut this.setup { setup.replacing_key = true; cx.notify(); });
                    focus_on_click(field("setup-api-key", false).flex_1(), &self.key_input).on_mouse_down(MouseButton::Left, replace)
                        .child(div().flex_1().min_w_0().truncate().text_size(px(12.0)).text_color(theme::text()).child(if hint.is_empty() { "Saved".to_string() } else { hint.clone() }))
                        .child(ui::icon("icons/setup-circle-check.svg", 14.0, theme::ok()))
                        .child(div().flex_none().text_size(px(12.0)).text_color(theme::ok()).child("In Credential Manager"))
                }
                None => {
                    let typed = !self.key_input.read(cx).text().trim().is_empty();
                    focus_on_click(field("setup-api-key", focused(&self.key_input)).flex_1(), &self.key_input)
                        .child(div().flex_1().min_w_0().text_size(px(12.0)).child(self.key_input.clone()))
                        .when(typed, |field| field.child(link("setup-save-key", "Save").text_color(theme::accent_soft())
                            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| { cx.stop_propagation(); this.save_key(window, cx) }))))
                }
            }),
        };
        rows.push(div().w_full().flex().gap(px(8.0)).child(provider).children(key).into_any_element());
        if current.is_some_and(|preset| preset.base_url.is_empty()) {
            rows.push(focus_on_click(field("setup-base-url", focused(&self.base_url_input)).w_full(), &self.base_url_input)
                .child(ui::icon("icons/setup-link.svg", 15.0, theme::placeholder()))
                .child(div().flex_1().min_w_0().text_size(px(12.0)).child(self.base_url_input.clone()))
                .child(div().flex_none().text_size(px(12.0)).text_color(theme::placeholder()).child("Base URL"))
                .into_any_element());
        }
        rows.push(focus_on_click(field("setup-model-id", focused(&self.model_input)).w_full(), &self.model_input)
            .child(ui::icon("icons/setup-sparkle.svg", 15.0, theme::placeholder()))
            .child(div().flex_1().min_w_0().text_size(px(12.0)).child(self.model_input.clone()))
            .child(div().flex_none().text_size(px(12.0)).text_color(theme::placeholder()).child("Model id"))
            .into_any_element());
        let note = match &setup.answer.key_check {
            Some(Err(why)) => Some(error_text(why.clone())),
            _ => self.key_notice.clone().map(status),
        };
        vec![div().w_full().flex().flex_col().gap(px(8.0)).children(rows).children(note.map(|note| note.pl(px(2.0)))).into_any_element()]
    }
}
