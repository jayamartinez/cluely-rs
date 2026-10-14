//! Setup's model picker for the ChatGPT and Claude plans, from the Paper "Model picker v4" artboard:
//! one searchable list, the latest models first and the legacy ones on request (searching finds
//! both). The list scrolls inside its popover, which opens upward when there is more room above.

use gpui::{Context, Div, Focusable, MouseButton, ParentElement, Pixels, Styled, Window, canvas, deferred, div, prelude::*, px, rgb};

use super::steps::LIGHT;
use super::{Menu, Setup};
use crate::claude_cli::{LATEST_MODELS, LEGACY_MODELS, alias_label, model_label};
use crate::overlay::Overlay;
use crate::settings::{ClaudeModel, Provider};
use crate::codex::gpt_version;
use crate::settings_view::listen;
use crate::{theme, ui};

const POPOVER: u32 = 0x1d1f23;
const POPOVER_BORDER: u32 = 0x393c42;
const SEARCH_FIELD: u32 = 0x141518;
const QUIET: u32 = 0x7c7973;
const CHOSEN: u32 = 0x26292d;
const RULE: u32 = 0x2f3237;
/// A row's height, for working out how much of the list fits.
const ROW: f32 = 32.0;

/// A model as the picker lists it.
struct Entry { id: String, name: String, detail: String }

/// The plan's models, the latest first, and the id chosen now ("" is Codex's default).
struct Models { latest: Vec<Entry>, legacy: Vec<Entry>, selected: String }

impl Models {
    fn total(&self) -> usize { self.latest.len() + self.legacy.len() }

    /// What a search shows: the latest and the legacy models whose name or id contains `query`.
    fn matching(&self, query: &str) -> (Vec<&Entry>, Vec<&Entry>) {
        let query = query.to_lowercase();
        let hit = |entry: &&Entry| entry.name.to_lowercase().contains(&query) || entry.id.to_lowercase().contains(&query);
        (self.latest.iter().filter(hit).collect(), self.legacy.iter().filter(hit).collect())
    }
}

/// What a model is for, as short as the picker shows it: Codex's "Workhorse model for coding and
/// everyday work." becomes "Workhorse model".
fn short_detail(detail: &str) -> String {
    let detail = detail.trim().trim_end_matches('.');
    detail.split(" for ").next().unwrap_or(detail).trim().to_string()
}

impl Overlay {
    fn plan_models(&self) -> Models {
        let s = &self.store.value;
        if s.provider == Provider::Claude {
            // The aliases show their version: the one Claude Code last reported, or today's.
            let name = |id: &str| if id.starts_with("claude-") { model_label(id) } else { alias_label(id, &s.claude_models) };
            return Models {
                latest: LATEST_MODELS.iter().map(|(id, detail)| Entry { id: id.to_string(), name: name(id), detail: detail.to_string() }).collect(),
                legacy: LEGACY_MODELS.iter().map(|id| Entry { id: id.to_string(), name: model_label(id), detail: String::new() }).collect(),
                selected: s.claude_model_arg().to_string(),
            };
        }
        let status = self.codex_status.as_ref();
        let models = status.map(|status| status.models.clone()).unwrap_or_default();
        let detail = |id: &str| status.and_then(|status| status.details.get(id)).map(|detail| short_detail(detail)).unwrap_or_default();
        let name_of = |id: &str| models.iter().find(|(known, _)| known == id).map_or_else(|| id.to_string(), |(_, name)| name.clone());
        // The newest GPT generation is the latest; everything older is legacy.
        let newest = models.iter().filter_map(|(id, _)| gpt_version(id)).map(|(major, _)| major).max();
        let (latest, legacy): (Vec<_>, Vec<_>) = models.iter().partition(|(id, _)| newest.is_none() || gpt_version(id).map(|(major, _)| major) == newest);
        let entry = |(id, name): &(String, String)| Entry { id: id.clone(), name: name.clone(), detail: detail(id) };
        let default = status.and_then(|status| status.default_model.as_deref()).map(|id| format!("Your account's: {}", name_of(id))).unwrap_or_default();
        let mut first = vec![Entry { id: String::new(), name: "Default".into(), detail: default }];
        first.extend(latest.into_iter().map(entry));
        Models { latest: first, legacy: legacy.into_iter().map(entry).collect(), selected: s.codex_model.clone() }
    }

    pub(super) fn pick_plan_model(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup {
            setup.answer.test = None;
            setup.menu = None;
            setup.model_search.update(cx, |search, cx| search.clear(cx));
        }
        match self.store.value.provider {
            Provider::Claude => self.update_settings(|s| match id.as_str() {
                "opus" | "sonnet" | "haiku" => {
                    s.claude_model = ClaudeModel::new(&id).unwrap_or_default();
                    s.claude_model_id.clear();
                }
                _ => s.claude_model_id = id,
            }, window, cx),
            _ => self.update_settings(|s| s.codex_model = id, window, cx),
        }
    }

    /// Enter in the search field chooses the first model shown.
    pub(super) fn pick_first_plan_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &self.setup else { return };
        let query = setup.model_search.read(cx).text().trim().to_string();
        let models = self.plan_models();
        let (latest, legacy) = models.matching(&query);
        if let Some(first) = latest.into_iter().chain(legacy).next() { self.pick_plan_model(first.id.clone(), window, cx); }
    }

    fn close_plan_picker(&mut self, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        setup.model_search.update(cx, |search, cx| search.clear(cx));
        if setup.menu == Some(Menu::Model) { setup.menu = None; cx.notify(); }
    }

    pub(super) fn plan_model_picker(&self, setup: &Setup, enabled: bool, cx: &mut Context<Self>) -> Div {
        let models = self.plan_models();
        let open = enabled && setup.menu == Some(Menu::Model);
        let value = if models.selected.is_empty() { self.model_choice().value }
            else { models.latest.iter().chain(&models.legacy).find(|entry| entry.id == models.selected).map_or_else(|| model_label(&models.selected), |entry| entry.name.clone()) };
        let (search, face_bounds) = (setup.model_search.clone(), setup.model_face.clone());
        let face = div().id("setup-plan-model").relative().h(px(36.0)).px(px(12.0)).flex().items_center().justify_between().gap(px(8.0)).rounded(px(9.0))
            .bg(rgb(0x1f2125)).border_1().border_color(if open { theme::accent() } else { theme::hairline() })
            .child(div().min_w_0().truncate().text_size(px(13.0)).text_color(theme::text()).child(value))
            .child(ui::icon(if open { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" }, 12.0, theme::muted()))
            .child(canvas(move |bounds, _, _| face_bounds.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .when(enabled, |face| face.cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| {
                    if open { this.close_plan_picker(cx) } else { this.set_menu(Some(Menu::Model), cx) }
                }))
                // Typing goes straight to the search field. A plain handler: `listen` would hand it the
                // overlay's window, which can't take this window's keyboard focus.
                .on_mouse_down(MouseButton::Left, move |_, window, cx| if !open { window.focus(&search.focus_handle(cx)) }));
        let mut picker = div().relative().w_full().child(face);
        if open {
            let query = setup.model_search.read(cx).text().trim().to_string();
            let mut rows = div().id("setup-model-rows").flex().flex_col().gap(px(2.0)).overflow_y_scroll();
            let mut shown = 0;
            let section = |title: &'static str| div().px(px(8.0)).pt(px(6.0)).pb(px(3.0))
                .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(rgb(QUIET)).child(title));
            if query.is_empty() {
                rows = rows.child(section("LATEST")).children(models.latest.iter().enumerate().map(|(index, entry)| self.model_entry(index, entry, &models.selected, cx)));
                shown += models.latest.len() + 2;
                if !models.legacy.is_empty() {
                    let unfolded = setup.legacy_open;
                    let toggle = div().id("setup-legacy").flex().items_center().gap(px(8.0)).py(px(7.0)).px(px(8.0)).rounded(px(6.0)).cursor_pointer()
                        .hover(|row| row.bg(rgb(CHOSEN)))
                        .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| {
                            cx.stop_propagation();
                            if let Some(setup) = &mut this.setup { setup.legacy_open = !setup.legacy_open; cx.notify(); }
                        }))
                        .child(ui::icon(if unfolded { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" }, 12.0, theme::muted()))
                        .child(div().text_size(px(13.0)).text_color(theme::body()).child("Legacy models"))
                        .child(div().flex_1().flex().justify_end().text_size(px(12.0)).text_color(rgb(QUIET)).child(models.legacy.len().to_string()));
                    rows = rows.child(div().my(px(2.0)).h(px(1.0)).bg(rgb(RULE))).child(toggle);
                    shown += 1;
                    if unfolded {
                        let offset = models.latest.len();
                        rows = rows.children(models.legacy.iter().enumerate().map(|(index, entry)| self.model_entry(offset + index, entry, &models.selected, cx)));
                        shown += models.legacy.len();
                    }
                }
            } else {
                let (latest, legacy) = models.matching(&query);
                if latest.is_empty() && legacy.is_empty() {
                    rows = rows.child(div().px(px(8.0)).py(px(10.0)).text_size(px(13.0)).text_color(rgb(QUIET)).child("No models match"));
                }
                if !latest.is_empty() {
                    rows = rows.child(section("LATEST")).children(latest.iter().enumerate().map(|(index, entry)| self.model_entry(index, entry, &models.selected, cx)));
                    shown += latest.len() + 1;
                }
                if !legacy.is_empty() {
                    let offset = models.total();
                    rows = rows.child(section("LEGACY")).children(legacy.iter().enumerate().map(|(index, entry)| self.model_entry(offset + index, entry, &models.selected, cx)));
                    shown += legacy.len() + 1;
                }
            }
            let count = (!query.is_empty()).then(|| {
                let (latest, legacy) = models.matching(&query);
                div().flex_none().text_size(px(11.0)).text_color(rgb(QUIET)).child(format!("{} of {}", latest.len() + legacy.len(), models.total()))
            });
            let search = div().h(px(30.0)).px(px(9.0)).flex().items_center().gap(px(8.0)).rounded(px(7.0)).bg(rgb(SEARCH_FIELD))
                .border_1().border_color(theme::accent())
                .child(ui::icon("icons/search.svg", 13.0, theme::muted()))
                .child(div().flex_1().min_w_0().text_size(px(13.0)).child(setup.model_search.clone()))
                .children(count);
            // Below unless the list would run past the window's bottom and there is more room above.
            let needed = px(shown as f32 * ROW + 56.0);
            let (up, room) = match setup.model_face.get() {
                Some(face) => {
                    let below = setup.viewport.get() - face.bottom() - px(12.0);
                    let above = face.top() - px(48.0);
                    if below < needed && above > below { (true, above) } else { (false, below) }
                }
                None => (false, px(300.0)),
            };
            let rows = rows.max_h((room - px(56.0)).max(px(120.0)));
            let popover = div().id("setup-model-popover").w_full().flex().flex_col().gap(px(2.0)).p(px(6.0)).rounded(px(12.0))
                .bg(rgb(POPOVER)).border_1().border_color(rgb(POPOVER_BORDER)).shadow_lg().occlude()
                .on_mouse_down_out(listen(cx, |this, _, _, cx| this.close_plan_picker(cx)))
                .child(search)
                .child(rows);
            let place = div().absolute().left_0().w_full();
            let place: Div = if up { place.bottom(px(40.0)) } else { place.top(px(40.0)) };
            picker = picker.child(deferred(place.child(popover)).with_priority(1));
        }
        picker
    }

    /// One model: a check when it's the chosen one, its name, and what it's for on the right.
    fn model_entry(&self, index: usize, entry: &Entry, selected: &str, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let chosen = entry.id == selected;
        let id = entry.id.clone();
        div().id(("setup-model", index)).flex().items_center().gap(px(8.0)).py(px(7.0)).px(px(8.0)).rounded(px(6.0)).cursor_pointer()
            .hover(|row| row.bg(rgb(CHOSEN)))
            .when(chosen, |row| row.bg(rgb(CHOSEN)))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.pick_plan_model(id.clone(), window, cx) }))
            .child(div().w(px(12.0)).flex_none().when(chosen, |slot| slot.child(ui::icon("icons/setup-check.svg", 12.0, theme::accent()))))
            .child(div().flex_none().text_size(px(13.0)).text_color(if chosen { theme::text() } else { rgb(LIGHT) }).child(entry.name.clone()))
            .child(div().flex_1().min_w_0().flex().justify_end()
                .child(div().min_w_0().truncate().text_size(px(12.0)).text_color(rgb(QUIET)).child(entry.detail.clone())))
    }
}

/// Where the picker's face was last drawn, kept on `Setup` to decide which way the list opens.
pub(super) type FaceBounds = std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<Pixels>>>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str) -> Entry { Entry { id: id.into(), name: name.into(), detail: String::new() } }

    #[test]
    fn searching_finds_latest_and_legacy_models_by_name_or_id() {
        let models = Models {
            latest: vec![entry("opus", "Opus 5.5"), entry("sonnet", "Sonnet 5.5")],
            legacy: vec![entry("claude-opus-4-6", "Opus 4.6"), entry("claude-sonnet-4-6", "Sonnet 4.6"), entry("claude-opus-4-5", "Opus 4.5")],
            selected: "sonnet".into(),
        };
        let names = |found: Vec<&Entry>| found.iter().map(|entry| entry.name.clone()).collect::<Vec<_>>();
        let (latest, legacy) = models.matching("4.6");
        assert!(latest.is_empty());
        assert_eq!(names(legacy), ["Opus 4.6", "Sonnet 4.6"]);
        let (latest, legacy) = models.matching("OPUS");
        assert_eq!((names(latest), names(legacy)), (vec!["Opus 5.5".to_string()], vec!["Opus 4.6".to_string(), "Opus 4.5".to_string()]));
        assert_eq!(models.matching("claude-sonnet").1.len(), 1, "ids match too");
        assert_eq!(models.total(), 5);
    }

    #[test]
    fn claude_models_read_as_versions() {
        assert_eq!(model_label("claude-fable-5-1"), "Fable 5.1");
        assert_eq!(model_label("claude-opus-4"), "Opus 4");
        let none = std::collections::BTreeMap::new();
        assert_eq!(alias_label("haiku", &none), "Haiku 5.5", "the version the alias points at today");
        let reported = std::collections::BTreeMap::from([("opus".to_string(), "claude-opus-5-6".to_string())]);
        assert_eq!(alias_label("opus", &reported), "Opus 5.6", "what Claude Code reported wins");
        assert_eq!(LEGACY_MODELS.len(), 8);
    }

    #[test]
    fn codex_descriptions_are_shortened_to_what_the_model_is() {
        assert_eq!(short_detail("Workhorse model for coding and everyday work."), "Workhorse model");
        assert_eq!(short_detail("Fast and affordable model for easier tasks."), "Fast and affordable model");
        assert_eq!(short_detail("Previous generation workhorse model."), "Previous generation workhorse model");
    }
}
