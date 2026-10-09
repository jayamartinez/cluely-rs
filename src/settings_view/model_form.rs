//! macOS Settings › Model: provider, API key and model as one form, top to bottom, with the model
//! picked from a searchable popover whose list loads by itself once a key is saved. Laid out like
//! the Paper "Model v3 · macOS" artboards. The Windows panel keeps its own layout.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, Bounds, Context, Div, Entity, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels,
    SharedString, Styled, Transformation, Window, deferred, div, percentage, prelude::*, px, rgb,
};

use super::reveal::revealed_text;
use super::{Picker, answer_early, button, key_eye, listen, switch};
use crate::input::{InputEvent, TextInput};
use crate::overlay::Overlay;
use crate::providers::{self, Preset, ProviderError};
use crate::settings::{Provider, Settings};
use crate::{theme, ui};

/// Width of the controls on the right of each row.
const CONTROL_WIDTH: f32 = 300.0;
const POPOVER_WIDTH: f32 = 320.0;
/// One model row in the popover (`ui::menu_item`).
const ROW_HEIGHT: f32 = 30.0;
/// The tallest the popover's list gets; longer lists scroll (and search narrows them).
const LIST_MAX_HEIGHT: f32 = 300.0;
/// Space kept between the popover and the window edge.
const EDGE_MARGIN: f32 = 12.0;
/// The face is 36 px tall; the popover hangs 4 px from it.
const FACE_OFFSET: f32 = 40.0;

/// The model face's bounds and the window's height, from the last frame, to place the popover.
type FaceBounds = Rc<Cell<Option<(Bounds<Pixels>, Pixels)>>>;

/// State of the macOS model form.
pub(crate) struct ModelUi {
    search: Entity<TextInput>,
    /// Replacing a saved key: the key field shows although a key is stored.
    key_editing: bool,
    /// Typing a model id that isn't listed.
    other: bool,
    /// The provider and endpoint the form shows, and the one whose list was loaded or is loading.
    target: Option<String>,
    loaded_for: Option<String>,
    error: Option<SharedString>,
    face: FaceBounds,
}

impl ModelUi {
    pub(crate) fn new(key_input: &Entity<TextInput>, base_url_input: &Entity<TextInput>, window: &mut Window, cx: &mut Context<Overlay>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search models", cx));
        cx.subscribe_in(&search, window, |this, input, event, window, cx| match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Submit => {
                let query = input.read(cx).text().trim().to_string();
                this.submit_model_search(query, window, cx);
            }
        }).detach();
        cx.subscribe_in(key_input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Submit) { this.save_key(window, cx); }
        }).detach();
        // A custom endpoint's models load once its URL is entered, not on every keystroke.
        cx.subscribe_in(base_url_input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Submit) { this.ensure_models(true, true, window, cx); }
        }).detach();
        Self { search, key_editing: false, other: false, target: None, loaded_for: None, error: None, face: Rc::default() }
    }
}

impl Overlay {
    /// The selected API provider and the endpoint its models come from.
    fn endpoint(&self) -> Option<(&'static Preset, String)> {
        let s = &self.store.value;
        let preset = providers::preset(&s.api_provider)?;
        let base = if preset.base_url.is_empty() { s.custom_base_url.trim().to_string() } else { preset.base_url.to_string() };
        Some((preset, base))
    }

    /// Load the selected provider's models when they aren't loaded yet and can be: a key is saved
    /// (or none is needed) and, for a custom endpoint, its URL was entered (`include_custom`).
    /// Loading reads a saved key, which can show a Keychain prompt, so for providers that need one
    /// it happens only after a user action (`user_action`), never because Settings opened.
    pub(crate) fn ensure_models(&mut self, include_custom: bool, user_action: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.value.provider != Provider::ApiKey { return; }
        let Some((preset, base)) = self.endpoint() else { return };
        let target = format!("{}\n{base}", preset.id);
        if self.model_ui.target.as_deref() != Some(target.as_str()) {
            self.model_ui.target = Some(target.clone());
            self.model_ui.loaded_for = None;
            self.model_ui.key_editing = false;
            self.model_ui.other = false;
            self.model_ui.error = None;
        }
        if self.models_loading || base.is_empty() || self.model_ui.loaded_for.as_deref() == Some(target.as_str()) { return; }
        if preset.base_url.is_empty() && !include_custom { return; }
        if preset.needs_key && (!user_action || !self.store.value.saved_keys.contains_key(preset.id)) { return; }
        self.model_ui.loaded_for = Some(target);
        self.load_models(window, cx);
    }

    /// A list finished loading after the provider changed: load the current provider's instead.
    pub(crate) fn models_stale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_ui.loaded_for = None;
        self.ensure_models(false, true, window, cx);
    }

    pub(crate) fn models_loaded(&mut self, preset: &Preset, result: Result<Vec<String>, ProviderError>) {
        match result {
            Ok(models) => {
                self.model_ui.error = None;
                // Nothing to pick from: go straight to typing an id.
                if models.is_empty() { self.model_ui.other = true; }
                self.loaded_models = models;
            }
            Err(error) => self.model_ui.error = Some(load_error_text(preset, &error).into()),
        }
    }

    fn retry_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_ui.loaded_for = None;
        self.model_ui.error = None;
        self.ensure_models(true, true, window, cx);
    }

    /// A key was stored: leave the key field and, for the API provider, load its models.
    pub(crate) fn key_saved(&mut self, provider: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.model_ui.key_editing = false;
        if provider == self.store.value.api_provider {
            self.key_notice = None;
            self.retry_models(window, cx);
        }
    }

    fn remove_key_and_reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_key(window, cx);
        self.model_ui.key_editing = false;
        self.model_ui.loaded_for = None;
        self.model_ui.error = None;
        if self.endpoint().is_some_and(|(preset, _)| preset.needs_key) { self.loaded_models.clear(); }
    }

    /// The models offered for the API provider: its loaded list, or its known models until then.
    fn api_model_options(&self) -> Vec<(String, String)> {
        let suggested = self.endpoint().map(|(preset, _)| preset.suggested_models).unwrap_or_default();
        if self.loaded_models.is_empty() {
            suggested.iter().map(|id| (id.to_string(), id.to_string())).collect()
        } else {
            self.loaded_models.iter().map(|id| (id.clone(), id.clone())).collect()
        }
    }

    fn pick_model(&mut self, id: String, api: bool, set: fn(&mut Settings, String), window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(cx);
        self.model_ui.search.update(cx, |input, cx| input.clear(cx));
        self.model_ui.other = false;
        if api { self.choose_model(id, window, cx); } else { self.update_settings(|s| set(s, id), window, cx); }
    }

    /// Return in the search field: the first match, or the typed text as a model id.
    fn submit_model_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        if query.is_empty() { return; }
        let options = self.api_model_options();
        let id = matching(&options, &query).first().map(|(id, _)| id.clone()).unwrap_or(query);
        self.pick_model(id, true, |s, v| { s.api_models.insert(s.api_provider.clone(), v); }, window, cx);
    }

    pub(crate) fn model_form(&self, answer_with: Div, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let form = div().flex().flex_col().gap(px(16.0)).child(answer_with);
        let form = if s.provider == Provider::ApiKey {
            form.child(self.connection_group(cx))
        } else {
            let choice = self.model_choice();
            form.child(plain_row("Model", self.model_popover_picker(choice.picker, choice.value, choice.options, &choice.selected, None, false, choice.set, cx)))
        };
        form.child(plain_row("Answer style", self.answer_style_picker(cx)))
            .child(ui::setting_row("Smart mode · slower, deeper reasoning", switch("smart-mode", s.smart_mode, |s, v| s.smart_mode = v, cx)).border_b_0())
            .child(answer_early(s, cx))
    }

    /// Provider, then its key (or endpoint), then the model, as one grouped box.
    fn connection_group(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let presets: Vec<(String, String)> = providers::PRESETS.iter().map(|p| (p.id.to_string(), p.label.to_string())).collect();
        let current = providers::preset(&s.api_provider).map(|p| p.label).unwrap_or("Choose a provider");
        let mut rows = vec![group_row(label("Provider"), self.dropdown(Picker::ApiProvider, current, presets, &s.api_provider, |s, v| s.api_provider = v, cx))];
        if let Some(preset) = providers::preset(&s.api_provider) {
            if preset.local {
                rows.push(local_line(preset));
            } else {
                if preset.base_url.is_empty() {
                    rows.push(group_row(label("Base URL"), text_box(&self.base_url_input, "base-url-box")));
                }
                rows.push(group_row(key_label(preset, cx), self.key_control(preset, cx)));
            }
            rows.push(group_row(label("Model"), self.model_control(preset, cx)));
        }
        let last = rows.len() - 1;
        rows.into_iter().enumerate().fold(
            div().flex().flex_col().rounded(px(12.0)).bg(rgb(0x141518)).border_1().border_color(rgb(0x23262a)),
            |group, (index, row)| group.child(if index == last { row } else { row.border_b_1().border_color(rgb(0x202327)) }),
        )
    }

    /// The saved key (masked) with Replace and Remove, or the key field with its eye and Save.
    fn key_control(&self, preset: &'static Preset, cx: &mut Context<Self>) -> Div {
        let column = div().flex().flex_col().gap(px(6.0));
        let saved = self.store.value.saved_keys.get(preset.id).cloned();
        if let Some(hint) = saved.as_ref().filter(|_| !self.model_ui.key_editing) {
            let revealed = self.revealed_key(preset.id);
            let shown: gpui::AnyElement = match revealed {
                Some(key) => revealed_text(key).into_any_element(),
                // A key found before its hint was known shows bullets only.
                None => div().flex_1().min_w_0().font_family(theme::MONO).text_size(px(12.0)).text_color(theme::body())
                    .child(if hint.is_empty() { "••••••••".to_string() } else { hint.clone() }).into_any_element(),
            };
            let field = div().flex_1().min_w_0().flex().items_center().gap(px(8.0)).h(px(36.0)).pl(px(10.0)).pr(px(5.0)).rounded(px(9.0)).border_1()
                .when(revealed.is_some(), |field| field.bg(theme::field()).border_color(theme::accent()))
                .when(revealed.is_none(), |field| field.bg(rgb(0x121316)).border_color(rgb(0x23262a)))
                .child(div().text_size(px(12.0)).font_weight(FontWeight::BOLD).text_color(theme::ok()).child("✓"))
                .child(shown)
                .child(self.saved_key_eye(preset.id, cx));
            let status = match (self.reveal_countdown(false), self.reveal_unavailable()) {
                (Some(countdown), _) => countdown,
                (None, Some(reason)) => div().text_color(theme::muted()).child(reason),
                (None, None) => div().text_color(theme::muted()).child("In your Keychain"),
            };
            let key = self.key_input.clone();
            let edit = listen(cx, |this, _: &MouseDownEvent, _, cx| {
                this.key_reveal.hide();
                this.model_ui.key_editing = true;
                this.key_notice = None;
                cx.notify();
            });
            let replace = button("replace-key", "Replace…", false).on_mouse_down(MouseButton::Left, move |event, window, cx| {
                edit(event, window, cx);
                window.focus(&key.focus_handle(cx));
            });
            return column.child(div().flex().items_center().gap(px(6.0)).child(field).child(replace))
                .child(div().flex().gap(px(10.0)).text_size(px(11.0))
                    .child(status)
                    .child(div().id("remove-key").cursor_pointer().text_color(rgb(0xffb4a8))
                        .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.remove_key_and_reset(window, cx))).child("Remove")));
        }
        let input = text_box(&self.key_input, "key-box").flex().items_center().gap(px(6.0)).pr(px(5.0)).child(key_eye(&self.key_input, cx));
        let save = button("save-key", "Save", true).on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_key(window, cx)));
        let mut note = div().flex().gap(px(10.0)).text_size(px(11.0)).text_color(theme::muted());
        if let Some(notice) = self.key_notice.clone() {
            note = note.child(div().text_color(theme::accent_soft()).child(notice));
        } else if self.key_input.read(cx).is_revealed() {
            note = note.child("Shown while you check it. Saved keys stay hidden.");
        }
        if saved.is_some() {
            note = note.child(div().id("cancel-key").cursor_pointer().text_color(theme::accent_soft())
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| {
                    this.model_ui.key_editing = false;
                    this.key_input.update(cx, |input, cx| input.clear(cx));
                    cx.notify();
                }))
                .child("Cancel"));
        }
        column.child(div().flex().items_center().gap(px(6.0)).child(input).child(save)).child(note)
    }

    /// The Model picker in its current state: waiting for a key or URL, loading, failed, or ready.
    fn model_control(&self, preset: &'static Preset, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let column = div().flex().flex_col().gap(px(6.0));
        if preset.needs_key && !s.saved_keys.contains_key(preset.id) {
            return column.child(idle_face("Save a key to see models"));
        }
        if preset.base_url.is_empty() && s.custom_base_url.trim().is_empty() {
            return column.child(idle_face("Enter the base URL to see models"));
        }
        if self.models_loading {
            return column.child(loading_face(format!("Loading {} models…", short_name(preset))));
        }
        let value = if s.api_model().is_empty() { "Choose a model".to_string() } else { s.api_model().to_string() };
        let header = Some(short_name(preset).to_uppercase());
        let mut column = column.child(self.model_popover_picker(Picker::ApiModel, value, self.api_model_options(), s.api_model(), header, true,
            |s, v| { s.api_models.insert(s.api_provider.clone(), v); }, cx));
        if let Some(error) = self.model_ui.error.clone() {
            column = column.child(div().flex().gap(px(12.0)).text_size(px(12.0))
                .child(div().flex_1().min_w_0().text_color(rgb(0xffb4a8)).child(error))
                .child(div().id("retry-models").flex_none().cursor_pointer().font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft())
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.retry_models(window, cx))).child("Retry")));
        }
        if self.model_ui.other {
            let done = button("use-model", "Use", true).on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { this.model_ui.other = false; cx.notify(); }));
            column = column.child(div().flex().items_center().gap(px(6.0)).child(text_box(&self.model_input, "model-box")).child(done))
                .child(div().text_size(px(11.0)).text_color(theme::muted()).child("Sent exactly as typed."));
        }
        column
    }

    /// A picker face whose list opens as a popover: below the face, or above it when there is more
    /// room there, sized to stay inside the window. `searchable` adds the search field and a way to
    /// use an id that isn't listed.
    #[allow(clippy::too_many_arguments)]
    fn model_popover_picker(&self, picker: Picker, value: String, options: Vec<(String, String)>, selected: &str, header: Option<String>,
        searchable: bool, set: fn(&mut Settings, String), cx: &mut Context<Self>) -> Div {
        let open = self.open_picker == Some(picker);
        let error = self.model_ui.error.is_some() && picker == Picker::ApiModel;
        let face_cell = self.model_ui.face.clone();
        let search = self.model_ui.search.clone();
        let toggle = listen(cx, move |this, _: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            this.model_ui.search.update(cx, |input, cx| input.clear(cx));
            this.toggle_picker(picker, cx);
            // Opening the list is the user action that may read a saved key to load it.
            if picker == Picker::ApiModel && this.open_picker == Some(picker) { this.ensure_models(true, true, window, cx); }
        });
        let face = ui::picker(("model-face", picker as usize), value, open).relative()
            .when(error && !open, |face| face.border_color(rgb(0x6b3a33)))
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                toggle(event, window, cx);
                if searchable { window.focus(&search.focus_handle(cx)); }
            })
            .child(gpui::canvas(move |bounds, window, _| face_cell.set(Some((bounds, window.viewport_size().height))), |_, _, _, _| {})
                .absolute().top_0().left_0().size_full());
        let wrapper = div().relative().w_full().child(face);
        if !open { return wrapper; }

        let query = if searchable { self.model_ui.search.read(cx).text().trim().to_string() } else { String::new() };
        let shown = matching(&options, &query);
        let fixed = 10.0 + (if searchable { 36.0 + 35.0 } else { 0.0 }) + (if header.is_some() { 23.0 } else { 0.0 });
        let wanted = fixed + shown.len().max(1) as f32 * ROW_HEIGHT;
        let (above, height) = match self.model_ui.face.get() {
            Some((bounds, window_height)) => place_popover(bounds.top().into(), bounds.bottom().into(), window_height.into(), wanted),
            None => (false, wanted),
        };
        let list_height = (height - fixed).clamp(ROW_HEIGHT, LIST_MAX_HEIGHT);

        let face_bounds = self.model_ui.face.clone();
        let mut popover = ui::menu().id("model-popover").w(px(POPOVER_WIDTH)).shadow_lg()
            .on_mouse_down_out(listen(cx, move |this, event: &MouseDownEvent, _, cx| {
                if face_bounds.get().is_some_and(|(face, _)| face.contains(&event.position)) { return; }
                this.close_picker(cx);
            }));
        if searchable {
            popover = popover.child(div().flex().items_center().gap(px(8.0)).h(px(30.0)).px(px(9.0)).mb(px(4.0)).rounded(px(7.0))
                .bg(rgb(0x141518)).border_1().border_color(theme::accent())
                .child(ui::icon("icons/search.svg", 13.0, theme::muted()))
                .child(div().flex_1().min_w_0().child(self.model_ui.search.clone()))
                .child(div().flex_none().text_size(px(11.0)).text_color(theme::muted()).child(format!("{} of {}", shown.len(), options.len()))));
        }
        if let Some(header) = header {
            popover = popover.child(div().px(px(10.0)).pt(px(5.0)).pb(px(4.0)).text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(0x7c7973)).child(header));
        }
        let mut list = div().id("model-list").flex().flex_col().max_h(px(list_height)).overflow_y_scroll();
        if shown.is_empty() {
            list = list.child(div().px(px(10.0)).py(px(7.0)).text_size(px(13.0)).text_color(theme::muted()).child("No matching models"));
        }
        for (index, (id, label)) in shown.into_iter().enumerate() {
            let id = id.clone();
            list = list.child(ui::menu_item(("model", index), label.clone(), id == selected)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.pick_model(id.clone(), searchable, set, window, cx) })));
        }
        popover = popover.child(list);
        if searchable {
            let listed = options.iter().any(|(id, _)| *id == query);
            let footer = if !query.is_empty() && !listed {
                let typed = query.clone();
                ui::menu_item("use-typed", format!("Use \"{query}\" as a model id…"), false).text_color(theme::accent_soft())
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.pick_model(typed.clone(), true, set, window, cx) }))
            } else {
                let model = self.model_input.clone();
                let other = listen(cx, |this, _: &MouseDownEvent, _, cx| { cx.stop_propagation(); this.close_picker(cx); this.model_ui.other = true; cx.notify(); });
                ui::menu_item("other-model", "Other model id…", false).text_color(theme::accent_soft())
                    .on_mouse_down(MouseButton::Left, move |event, window, cx| { other(event, window, cx); window.focus(&model.focus_handle(cx)); })
            };
            popover = popover.child(div().h(px(1.0)).my(px(3.0)).bg(theme::hairline())).child(footer);
        }
        let anchor = div().absolute().right_0();
        let anchor = if above { anchor.bottom(px(FACE_OFFSET)) } else { anchor.top(px(FACE_OFFSET)) };
        wrapper.child(deferred(anchor.child(popover)))
    }
}

fn label(text: &'static str) -> Div {
    div().text_size(px(13.0)).text_color(theme::text()).child(text)
}

/// "API key" with a link to the provider's key page, or "Optional" for a custom endpoint.
fn key_label(preset: &'static Preset, cx: &mut Context<Overlay>) -> Div {
    let column = div().flex().flex_col().gap(px(2.0)).child(label("API key"));
    if !preset.needs_key {
        return column.child(div().text_size(px(12.0)).text_color(theme::muted()).child("Optional"));
    }
    if preset.key_page.is_empty() { return column; }
    let page = preset.key_page;
    column.child(div().id("key-page").cursor_pointer().text_size(px(12.0)).text_color(theme::accent_soft())
        .on_mouse_down(MouseButton::Left, listen(cx, move |_, _, _, cx| cx.open_url(page))).child("Get a key ↗"))
}

/// A row of the grouped box: label on the left, control on the right.
fn group_row(label: impl IntoElement, control: impl IntoElement) -> Div {
    div().flex().items_start().gap(px(16.0)).px(px(14.0)).py(px(10.0))
        .child(div().flex_1().min_w_0().pt(px(9.0)).child(label))
        .child(div().w(px(CONTROL_WIDTH)).flex_none().child(control))
}

/// A row outside the box, aligned with the toggle rows below it.
fn plain_row(text: &'static str, control: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(16.0))
        .child(div().flex_1().min_w_0().child(label(text)))
        .child(div().w(px(CONTROL_WIDTH)).flex_none().child(control))
}

/// Local providers need no key: say where they run instead.
fn local_line(preset: &'static Preset) -> Div {
    div().flex().items_center().gap(px(8.0)).px(px(14.0)).py(px(12.0))
        .child(div().size(px(7.0)).flex_none().rounded_full().bg(theme::ok()))
        .child(div().text_size(px(12.0)).text_color(theme::body()).child("Runs on this computer · no key needed"))
        .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(host(preset.base_url).to_string()))
}

fn text_box(input: &Entity<TextInput>, id: &'static str) -> gpui::Stateful<Div> {
    let focus = input.clone();
    div().id(id).flex_1().min_w_0().px(px(10.0)).py(px(7.0)).rounded(px(9.0))
        .bg(theme::field()).border_1().border_color(theme::hairline()).cursor_text()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&focus.focus_handle(cx)))
        .child(input.clone())
}

/// A face that can't open yet, saying what it waits for.
fn idle_face(text: &'static str) -> Div {
    div().flex().items_center().h(px(36.0)).px(px(12.0)).rounded(px(9.0)).bg(rgb(0x121316)).border_1().border_color(rgb(0x23262a))
        .child(div().text_size(px(13.0)).text_color(rgb(0x6e6b65)).child(text))
}

fn loading_face(text: String) -> Div {
    let spinner = gpui::svg().path("icons/spinner.svg").size(px(14.0)).flex_none().text_color(theme::accent_soft())
        .with_animation("models-loading", Animation::new(Duration::from_millis(900)).repeat(),
            |spinner, delta| spinner.with_transformation(Transformation::rotate(percentage(delta))));
    div().flex().items_center().gap(px(10.0)).h(px(36.0)).px(px(12.0)).rounded(px(9.0)).bg(theme::field()).border_1().border_color(theme::hairline())
        .child(spinner)
        .child(div().text_size(px(13.0)).text_color(theme::muted()).child(text))
}

/// "Ollama (local)" → "Ollama".
fn short_name(preset: &Preset) -> &'static str {
    preset.label.trim_end_matches(" (local)")
}

/// "http://localhost:11434/v1" → "localhost:11434".
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

/// What to say when a provider's models couldn't be listed.
fn load_error_text(preset: &Preset, error: &ProviderError) -> String {
    let name = short_name(preset);
    match error {
        ProviderError::Unauthorized => format!("{name} didn't accept this key. Replace it, or try again."),
        ProviderError::Network(_) if preset.local => format!("{name} isn't running on {}. Start it, then try again.", host(preset.base_url)),
        other => format!("Couldn't load models: {other}"),
    }
}

/// The options whose id or label contains `query`, ignoring case; all of them for an empty query.
fn matching<'a>(options: &'a [(String, String)], query: &str) -> Vec<&'a (String, String)> {
    let query = query.trim().to_lowercase();
    options.iter().filter(|(id, label)| query.is_empty() || id.to_lowercase().contains(&query) || label.to_lowercase().contains(&query)).collect()
}

/// Whether the popover opens above its face, and how tall it may be: below unless it doesn't fit
/// there and there is more room above; never past the window's edge.
fn place_popover(face_top: f32, face_bottom: f32, window_height: f32, wanted: f32) -> (bool, f32) {
    let below = (window_height - face_bottom - EDGE_MARGIN).max(0.0);
    let above = (face_top - EDGE_MARGIN).max(0.0);
    let up = below < wanted && above > below;
    (up, wanted.min(if up { above } else { below }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter().map(|id| (id.to_string(), id.to_string())).collect()
    }

    #[test]
    fn search_matches_ids_ignoring_case() {
        let all = options(&["claude-opus-5-5", "claude-sonnet-5-5", "claude-sonnet-4-5", "gpt-6-luna"]);
        let ids = |query| matching(&all, query).into_iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids("SON"), ["claude-sonnet-5-5", "claude-sonnet-4-5"]);
        assert_eq!(ids("  "), ["claude-opus-5-5", "claude-sonnet-5-5", "claude-sonnet-4-5", "gpt-6-luna"]);
        assert!(ids("mistral").is_empty());
    }

    #[test]
    fn popover_opens_below_when_it_fits_and_above_when_there_is_more_room() {
        // Plenty of room below.
        assert_eq!(place_popover(100.0, 136.0, 800.0, 300.0), (false, 300.0));
        // Near the bottom: flips up, sized to the room above.
        assert_eq!(place_popover(600.0, 636.0, 700.0, 300.0), (true, 300.0));
        assert_eq!(place_popover(200.0, 236.0, 300.0, 300.0), (true, 188.0));
        // Short of room both ways: the larger side, never past the edge.
        assert_eq!(place_popover(150.0, 186.0, 400.0, 300.0), (false, 202.0));
    }

    #[test]
    fn load_errors_say_what_to_do() {
        let anthropic = providers::preset("anthropic").unwrap();
        let ollama = providers::preset("ollama").unwrap();
        assert_eq!(load_error_text(anthropic, &ProviderError::Unauthorized), "Anthropic didn't accept this key. Replace it, or try again.");
        assert_eq!(load_error_text(ollama, &ProviderError::Network("refused".into())), "Ollama isn't running on localhost:11434. Start it, then try again.");
        assert!(load_error_text(anthropic, &ProviderError::Server(503)).starts_with("Couldn't load models: "));
    }

    #[test]
    fn hosts_drop_scheme_and_path() {
        assert_eq!(host("http://localhost:11434/v1"), "localhost:11434");
        assert_eq!(host("https://llm.internal.example/v1"), "llm.internal.example");
        assert_eq!(host("localhost:1234"), "localhost:1234");
    }
}
