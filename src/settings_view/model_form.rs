//! macOS Settings › Model: provider, API key and model as one form, top to bottom, with the model
//! picked from a searchable popover whose list loads by itself once a key is saved. Laid out like
//! the Paper "Model v3 · macOS" artboards. The Windows panel keeps its own layout.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, Context, Div, Entity, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels,
    SharedString, Styled, Transformation, Window, deferred, div, percentage, prelude::*, px, rgb,
};

use super::reveal::revealed_text;
use super::{BOX_PADDING, Picker, ROW_INSET, answer_early, button, key_eye, listen, switch};
use crate::chat::ModelEntry;
use crate::input::{InputEvent, TextInput};
use crate::overlay::Overlay;
use crate::providers::{self, Preset, ProviderError};
use crate::settings::{Provider, Settings};
use crate::{theme, ui};

/// Width of the controls on the right of each row.
const CONTROL_WIDTH: f32 = 300.0;
const POPOVER_WIDTH: f32 = 320.0;
/// The popover's text lines are this tall, so its rows have the heights below; at the default
/// line height (about 1.6 × the text) rows were taller than the list was sized for, and the last one
/// was cut off.
const LINE_HEIGHT: f32 = 16.0;
/// One model row in the popover: 7 px above and below a line.
const ROW_HEIGHT: f32 = 30.0;
/// A section header ("LATEST"): 5 px above and 4 px below a 14 px line.
const HEADER_HEIGHT: f32 = 23.0;
/// The search field and the gap under it.
const SEARCH_HEIGHT: f32 = 34.0;
/// The hairline between the list and what follows, with 3 px either side.
const DIVIDER_HEIGHT: f32 = 7.0;
/// The popover's border and padding, top and bottom together.
const CHROME_HEIGHT: f32 = 10.0;
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
    /// The subscription picker shows its Legacy models (opened with the current model, if it's one).
    legacy_open: bool,
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
        Self { search, key_editing: false, other: false, target: None, loaded_for: None, error: None, face: Rc::default(), legacy_open: false }
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
        if matches!(self.open_picker, Some(Picker::CodexModel | Picker::ClaudeModel)) {
            // A subscription takes only listed models: the first match, if any.
            let choice = self.model_choice();
            let entries = self.subscription_entries();
            if let Some(entry) = entries.iter().find(|entry| entry_matches(entry, &query)) { self.pick_model(entry.id.clone(), false, choice.set, window, cx); }
            return;
        }
        let options = self.api_model_options();
        let id = matching(&options, &query).first().map(|(id, _)| id.clone()).unwrap_or(query);
        self.pick_model(id, true, |s, v| { s.api_models.insert(s.api_provider.clone(), v); }, window, cx);
    }

    pub(crate) fn model_form(&self, answer_with: Div, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let form = div().flex().flex_col().gap(px(16.0)).child(answer_with);
        let form = if s.provider == Provider::ApiKey { form.child(self.connection_group(cx)) }
            else { form.child(plain_row("Model", self.subscription_model_picker(cx))) };
        form.child(plain_row("Answer style", self.answer_style_picker(cx)))
            .child(ui::setting_row("Smart mode · slower, deeper reasoning", switch("smart-mode", s.smart_mode, |s, v| s.smart_mode = v, cx)).border_b_0().px(px(ROW_INSET)))
            .child(answer_early(s, cx))
    }

    /// The selected subscription's model picker (Settings › Model and setup's Model step): its
    /// latest models, then "Legacy models" that open on request, all searchable. Laid out like the
    /// Paper artboard "Model picker v4 · macOS".
    pub(super) fn subscription_model_picker(&self, cx: &mut Context<Self>) -> Div {
        let choice = self.model_choice();
        let picker = choice.picker;
        let wrapper = self.popover_face(picker, choice.value, false, |this, _, _| {
            // Opening with a legacy model chosen shows where it is.
            let selected = this.model_choice().selected;
            this.model_ui.legacy_open = this.subscription_entries().iter().any(|entry| entry.legacy && entry.id == selected);
        }, cx);
        if self.open_picker != Some(picker) { return wrapper; }

        let entries = self.subscription_entries();
        let query = self.model_ui.search.read(cx).text().trim().to_string();
        let shown: Vec<&ModelEntry> = entries.iter().filter(|entry| entry_matches(entry, &query)).collect();
        let (latest, legacy): (Vec<&ModelEntry>, Vec<&ModelEntry>) = shown.iter().copied().partition(|entry| !entry.legacy);
        let legacy_count = entries.iter().filter(|entry| entry.legacy).count();
        // Searching looks through the legacy models too; otherwise they wait behind their row.
        let searching = !query.is_empty();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut content = 0.0;
        let mut add = |row: AnyElement, height: f32| { rows.push(row); content += height; };
        if shown.is_empty() { add(no_match().into_any_element(), ROW_HEIGHT); }
        if !latest.is_empty() && legacy_count > 0 { add(section_header("LATEST").into_any_element(), HEADER_HEIGHT); }
        let mut index = 0;
        for entry in latest {
            add(entry_row(index, entry, entry.id == choice.selected, choice.set, cx).into_any_element(), ROW_HEIGHT);
            index += 1;
        }
        let legacy_rows: Vec<&ModelEntry> = if searching {
            if !legacy.is_empty() { add(section_header("LEGACY").into_any_element(), HEADER_HEIGHT); }
            legacy
        } else if legacy_count > 0 {
            add(divider().into_any_element(), DIVIDER_HEIGHT);
            add(legacy_toggle(self.model_ui.legacy_open, legacy_count, cx).into_any_element(), ROW_HEIGHT);
            if self.model_ui.legacy_open { legacy } else { Vec::new() }
        } else { Vec::new() };
        for entry in legacy_rows {
            add(entry_row(index, entry, entry.id == choice.selected, choice.set, cx).into_any_element(), ROW_HEIGHT);
            index += 1;
        }

        let fixed = CHROME_HEIGHT + SEARCH_HEIGHT;
        let (above, height) = self.place(fixed + content);
        let list_height = (height - fixed).clamp(ROW_HEIGHT, LIST_MAX_HEIGHT);
        let popover = self.popover_shell(cx)
            .child(self.search_field(if searching { format!("{} of {}", shown.len(), entries.len()) } else { String::new() }))
            .child(div().id("subscription-models").flex().flex_col().max_h(px(list_height)).overflow_y_scroll().children(rows));
        wrapper.child(anchored(above, popover))
    }

    /// The selected subscription's models for its picker: the list Codex or Claude Code reported,
    /// with Codex's "Default" first, or the plain options when there is none.
    fn subscription_entries(&self) -> Vec<ModelEntry> {
        let codex = self.store.value.provider == Provider::Codex;
        let status = if codex { self.codex_status.as_ref() } else { self.claude_status.as_ref() };
        let mut entries = status.map(|status| status.catalog.clone()).unwrap_or_default();
        if entries.is_empty() {
            return self.model_choice().options.into_iter().map(|(id, name)| ModelEntry { id, name, ..ModelEntry::default() }).collect();
        }
        if codex {
            let default = status.and_then(|status| status.default_model.as_deref()).and_then(|id| entries.iter().find(|entry| entry.id == id));
            let note = default.map(|entry| format!("Your account's: {}", entry.name)).unwrap_or_default();
            entries.insert(0, ModelEntry { id: String::new(), name: "Default".into(), note, legacy: false });
        }
        entries
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
    pub(super) fn key_control(&self, preset: &'static Preset, cx: &mut Context<Self>) -> Div {
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
                .child(note_line().child(status.min_w_0())
                    .child(div().id("remove-key").flex_none().cursor_pointer().text_color(rgb(0xffb4a8))
                        .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.remove_key_and_reset(window, cx))).child("Remove")));
        }
        let input = text_box(&self.key_input, "key-box").flex().items_center().gap(px(6.0)).pr(px(5.0)).child(key_eye(&self.key_input, cx));
        let save = button("save-key", "Save", true).on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_key(window, cx)));
        let mut note = note_line().text_color(theme::muted());
        if let Some(notice) = self.key_notice.clone() {
            note = note.child(div().min_w_0().text_color(theme::accent_soft()).child(notice));
        } else if self.key_input.read(cx).is_revealed() {
            note = note.child(div().min_w_0().child("Shown while you check it. Saved keys stay hidden."));
        }
        if saved.is_some() {
            note = note.child(div().id("cancel-key").flex_none().cursor_pointer().text_color(theme::accent_soft())
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
    pub(super) fn model_control(&self, preset: &'static Preset, cx: &mut Context<Self>) -> Div {
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
        let header = short_name(preset).to_uppercase();
        let mut column = column.child(self.model_popover_picker(value, self.api_model_options(), s.api_model(), header,
            |s, v| { s.api_models.insert(s.api_provider.clone(), v); }, cx));
        if let Some(error) = self.model_ui.error.clone() {
            // Retry sits at the end of the line when the message leaves room, else on its own line.
            column = column.child(div().flex().flex_wrap().justify_between().gap_x(px(12.0)).gap_y(px(4.0)).text_size(px(12.0))
                .child(div().min_w_0().text_color(rgb(0xffb4a8)).child(error))
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

    /// The API model picker: the provider's models under its name in a searchable popover, and a
    /// way to use an id that isn't listed. Opening it may load the list (`ensure_models`).
    fn model_popover_picker(&self, value: String, options: Vec<(String, String)>, selected: &str, header: String,
        set: fn(&mut Settings, String), cx: &mut Context<Self>) -> Div {
        let picker = Picker::ApiModel;
        let wrapper = self.popover_face(picker, value, self.model_ui.error.is_some(), |this, window, cx| this.ensure_models(true, true, window, cx), cx);
        if self.open_picker != Some(picker) { return wrapper; }

        let query = self.model_ui.search.read(cx).text().trim().to_string();
        let shown = matching(&options, &query);
        let fixed = CHROME_HEIGHT + SEARCH_HEIGHT + HEADER_HEIGHT + DIVIDER_HEIGHT + ROW_HEIGHT;
        let (above, height) = self.place(fixed + shown.len().max(1) as f32 * ROW_HEIGHT);
        let list_height = (height - fixed).clamp(ROW_HEIGHT, LIST_MAX_HEIGHT);

        let mut list = div().id("model-list").flex().flex_col().max_h(px(list_height)).overflow_y_scroll();
        if shown.is_empty() { list = list.child(no_match()); }
        for (index, (id, label)) in shown.iter().enumerate() {
            let id = id.clone();
            list = list.child(ui::menu_item(("model", index), label.clone(), id == selected)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.pick_model(id.clone(), true, set, window, cx) })));
        }
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
        let popover = self.popover_shell(cx)
            .child(self.search_field(format!("{} of {}", shown.len(), options.len())))
            .child(section_header(header))
            .child(list)
            .child(divider())
            .child(footer);
        wrapper.child(anchored(above, popover))
    }

    /// A picker face whose list opens as a popover: clicking toggles it with an empty, focused
    /// search field, and runs `opened` when it opened. Returns the face in the wrapper the popover
    /// hangs from (`anchored`).
    fn popover_face(&self, picker: Picker, value: String, error: bool, opened: fn(&mut Self, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>) -> Div {
        let open = self.open_picker == Some(picker);
        let face_cell = self.model_ui.face.clone();
        let search = self.model_ui.search.clone();
        let toggle = listen(cx, move |this, _: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            this.model_ui.search.update(cx, |input, cx| input.clear(cx));
            this.toggle_picker(picker, cx);
            if this.open_picker == Some(picker) { opened(this, window, cx); }
        });
        let face = ui::picker(("model-face", picker as usize), value, open).relative()
            .when(error && !open, |face| face.border_color(rgb(0x6b3a33)))
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                toggle(event, window, cx);
                window.focus(&search.focus_handle(cx));
            })
            .child(gpui::canvas(move |bounds, window, _| face_cell.set(Some((bounds, window.viewport_size().height))), |_, _, _, _| {})
                .absolute().top_0().left_0().size_full());
        div().relative().w_full().child(face)
    }

    /// Whether a popover that wants `wanted` px opens above its face, and how tall it may be.
    fn place(&self, wanted: f32) -> (bool, f32) {
        match self.model_ui.face.get() {
            Some((bounds, window_height)) => place_popover(bounds.top().into(), bounds.bottom().into(), window_height.into(), wanted),
            None => (false, wanted),
        }
    }

    /// The popover's box, closed by a click anywhere else but its face (which toggles it itself).
    fn popover_shell(&self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let face_bounds = self.model_ui.face.clone();
        ui::menu().id("model-popover").w(px(POPOVER_WIDTH)).shadow_lg().line_height(px(LINE_HEIGHT))
            .on_mouse_down_out(listen(cx, move |this, event: &MouseDownEvent, _, cx| {
                if face_bounds.get().is_some_and(|(face, _)| face.contains(&event.position)) { return; }
                this.close_picker(cx);
            }))
    }

    /// The search field at the top of a popover, with how many models match (`count`, may be empty).
    fn search_field(&self, count: String) -> Div {
        div().flex().flex_none().items_center().gap(px(8.0)).h(px(SEARCH_HEIGHT - 4.0)).px(px(9.0)).mb(px(4.0)).rounded(px(7.0))
            .bg(rgb(0x141518)).border_1().border_color(theme::accent())
            .child(ui::icon("icons/search.svg", 13.0, theme::muted()))
            .child(div().flex_1().min_w_0().child(self.model_ui.search.clone()))
            .child(div().flex_none().text_size(px(11.0)).text_color(theme::muted()).child(count))
    }
}

/// Where a popover hangs from its face: under it, or above it.
fn anchored(above: bool, popover: impl IntoElement) -> impl IntoElement {
    let anchor = div().absolute().right_0();
    deferred(if above { anchor.bottom(px(FACE_OFFSET)) } else { anchor.top(px(FACE_OFFSET)) }.child(popover))
}

/// A popover section's name ("LATEST", "ANTHROPIC").
fn section_header(text: impl Into<SharedString>) -> Div {
    div().flex_none().px(px(10.0)).pt(px(5.0)).pb(px(4.0)).text_size(px(11.0)).line_height(px(14.0))
        .font_weight(FontWeight::SEMIBOLD).text_color(rgb(0x7c7973)).child(text.into())
}

fn divider() -> Div {
    div().flex_none().h(px(1.0)).my(px(3.0)).bg(theme::hairline())
}

fn no_match() -> Div {
    div().px(px(10.0)).py(px(7.0)).text_size(px(13.0)).text_color(theme::muted()).child("No matching models")
}

/// A subscription model: a lane for the check, its name, and what it's for on the right.
fn entry_row(index: usize, entry: &ModelEntry, selected: bool, set: fn(&mut Settings, String), cx: &mut Context<Overlay>) -> gpui::Stateful<Div> {
    let id = entry.id.clone();
    div().id(("subscription-model", index)).flex().flex_none().items_center().gap(px(8.0)).px(px(8.0)).py(px(7.0)).rounded(px(7.0)).cursor_pointer()
        .text_size(px(13.0)).hover(|row| row.bg(rgb(0x26292d))).when(selected, |row| row.bg(rgb(0x26292d)))
        .child(div().w(px(12.0)).flex_none().text_color(theme::accent()).font_weight(FontWeight::BOLD).when(selected, |lane| lane.child("✓")))
        .child(div().flex_none().text_color(if selected { theme::text() } else { theme::body() }).child(entry.name.clone()))
        .child(div().flex_1().min_w_0().flex().justify_end()
            .child(div().min_w_0().truncate().text_size(px(12.0)).text_color(theme::muted()).child(entry.note.clone())))
        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.pick_model(id.clone(), false, set, window, cx) }))
}

/// The Legacy row's chevron as a fraction of a turn: down when open, pointing right (a quarter
/// turn back, written as three quarters forward) when closed. `percentage` panics outside 0..=1.
fn chevron_turn(open: bool) -> f32 { if open { 0.0 } else { 0.75 } }

/// "Legacy models" with how many there are; clicking shows or hides them.
fn legacy_toggle(open: bool, count: usize, cx: &mut Context<Overlay>) -> gpui::Stateful<Div> {
    let chevron = gpui::svg().path("icons/chevron-down.svg").size(px(12.0)).flex_none().text_color(theme::muted())
        .with_transformation(Transformation::rotate(percentage(chevron_turn(open))));
    div().id("legacy-models").flex().flex_none().items_center().gap(px(8.0)).px(px(8.0)).py(px(7.0)).rounded(px(7.0)).cursor_pointer()
        .text_size(px(13.0)).hover(|row| row.bg(rgb(0x26292d)))
        .child(chevron)
        .child(div().flex_1().text_color(theme::body()).child("Legacy models"))
        .child(div().text_size(px(12.0)).text_color(theme::muted()).child(count.to_string()))
        .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| {
            cx.stop_propagation();
            this.model_ui.legacy_open = !this.model_ui.legacy_open;
            cx.notify();
        }))
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
    div().flex().items_start().gap(px(16.0)).px(px(BOX_PADDING)).py(px(10.0))
        .child(div().flex_1().min_w_0().pt(px(9.0)).child(label))
        .child(div().w(px(CONTROL_WIDTH)).flex_none().child(control))
}

/// A row outside the box, lined up with the box's labels and controls.
fn plain_row(text: &'static str, control: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(16.0)).px(px(ROW_INSET))
        .child(div().flex_1().min_w_0().child(label(text)))
        .child(div().w(px(CONTROL_WIDTH)).flex_none().child(control))
}

/// The small line under the key field: a status or notice, then its action (Remove, Cancel), which
/// moves to its own line when both don't fit the column.
fn note_line() -> Div {
    div().flex().flex_wrap().gap_x(px(10.0)).gap_y(px(2.0)).text_size(px(11.0))
}

/// Local providers need no key: say where they run instead.
fn local_line(preset: &'static Preset) -> Div {
    div().flex().items_center().gap(px(8.0)).px(px(BOX_PADDING)).py(px(12.0))
        .child(div().size(px(7.0)).flex_none().rounded_full().bg(theme::ok()))
        .child(div().text_size(px(12.0)).text_color(theme::body()).child("Runs on this computer · no key needed"))
        .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(host(preset.base_url).to_string()))
}

pub(super) fn text_box(input: &Entity<TextInput>, id: &'static str) -> gpui::Stateful<Div> {
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

/// Whether `query` is in a model's id or name, ignoring case; every model for an empty query.
fn entry_matches(entry: &ModelEntry, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || entry.id.to_lowercase().contains(&query) || entry.name.to_lowercase().contains(&query)
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

    /// The closed chevron once rotated by -0.25, which `percentage` rejects: the picker panicked as it drew.
    #[test]
    fn the_legacy_chevron_turns_within_what_percentage_accepts() {
        for open in [true, false] {
            let turn = chevron_turn(open);
            assert!((0.0..=1.0).contains(&turn), "{open}: {turn}");
            let _ = percentage(turn);
        }
        assert_eq!(chevron_turn(false), 0.75, "closed points right: a quarter turn back");
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
