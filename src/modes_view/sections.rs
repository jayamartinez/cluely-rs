//! The note sections of the mode on the Modes page: a name and a description field per row,
//! edited in place and saved once typing pauses, plus add, remove, reorder and reset. The state
//! and actions are shared; Windows draws them (`fluent::fluent_note_sections`), and the macOS page
//! is still to be built on them.

use gpui::{AppContext, Context, Entity, Task, Window};

use super::SAVE_DELAY;
use crate::input::{InputEvent, TextInput};
use crate::modes::{MAX_SECTIONS, NoteSection};
use crate::overlay::Overlay;

pub struct SectionsUi {
    /// A name and a description field for each possible row; the first `rows` are shown.
    pub(super) fields: Vec<(Entity<TextInput>, Entity<TextInput>)>,
    pub(super) rows: usize,
    pending_save: Option<Task<()>>,
}

impl SectionsUi {
    pub fn new(window: &mut Window, cx: &mut Context<Overlay>) -> Self {
        let fields = (0..MAX_SECTIONS).map(|_| {
            let name = cx.new(|cx| TextInput::new("Section name", cx));
            let description = cx.new(|cx| TextInput::new("What the notes put here", cx));
            for input in [&name, &description] {
                cx.subscribe_in(input, window, |this, _, event, _, cx| if matches!(event, InputEvent::Changed) { this.sections_edited(cx) }).detach();
            }
            (name, description)
        }).collect();
        Self { fields, rows: 0, pending_save: None }
    }
}

impl Overlay {
    /// The rows as typed.
    fn typed_sections(&self, cx: &Context<Self>) -> Vec<NoteSection> {
        self.modes_ui.sections.fields[..self.modes_ui.sections.rows].iter()
            .map(|(name, description)| NoteSection { name: name.read(cx).text().to_string(), description: description.read(cx).text().to_string() })
            .collect()
    }

    /// Fill the rows from the selected mode.
    pub(super) fn load_sections(&mut self, cx: &mut Context<Self>) {
        let sections = self.modes.get(&self.modes_ui.selected).map(|mode| mode.sections.clone()).unwrap_or_default();
        let ui = &mut self.modes_ui.sections;
        ui.pending_save = None;
        ui.rows = sections.len().min(MAX_SECTIONS);
        for ((name, description), section) in ui.fields.iter().zip(&sections) {
            name.update(cx, |input, cx| input.set_text(section.name.clone(), cx));
            description.update(cx, |input, cx| input.set_text(section.description.clone(), cx));
        }
        cx.notify();
    }

    fn sections_edited(&mut self, cx: &mut Context<Self>) {
        self.modes_ui.sections.pending_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            this.update(cx, |this, cx| this.flush_sections(cx)).ok();
        }));
    }

    /// Save a section edit that is waiting for typing to pause. Call before the selection changes.
    pub(super) fn flush_sections(&mut self, cx: &mut Context<Self>) {
        if self.modes_ui.sections.pending_save.take().is_none() { return; }
        let (id, sections) = (self.modes_ui.selected.clone(), self.typed_sections(cx));
        if self.modes.set_sections(&id, &sections) { cx.notify(); }
    }

    /// Change the rows and save straight away.
    fn change_sections(&mut self, change: impl FnOnce(&mut Vec<NoteSection>), cx: &mut Context<Self>) {
        self.modes_ui.sections.pending_save = None;
        let mut sections = self.typed_sections(cx);
        change(&mut sections);
        let id = self.modes_ui.selected.clone();
        self.modes.set_sections(&id, &sections);
        self.load_sections(cx);
    }

    /// Append an empty row; the caller gives its name field the keyboard.
    pub(super) fn add_section(&mut self, cx: &mut Context<Self>) {
        if self.modes_ui.sections.rows >= MAX_SECTIONS { return; }
        self.change_sections(|sections| sections.push(NoteSection::default()), cx);
    }

    pub(super) fn remove_section(&mut self, index: usize, cx: &mut Context<Self>) {
        self.change_sections(|sections| if index < sections.len() { sections.remove(index); }, cx);
    }

    /// Move row `from` to where row `to` is.
    pub(super) fn move_section(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to { return; }
        self.change_sections(|sections| if from < sections.len() && to < sections.len() {
            let section = sections.remove(from);
            sections.insert(to, section);
        }, cx);
    }

    /// Put a built-in's sections back to the ones it ships with.
    pub(super) fn reset_sections(&mut self, cx: &mut Context<Self>) {
        self.modes_ui.sections.pending_save = None;
        let id = self.modes_ui.selected.clone();
        if self.modes.reset_sections(&id) { self.load_sections(cx); }
    }
}
