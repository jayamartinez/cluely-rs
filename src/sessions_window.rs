//! Sessions: a normal, resizable window for reviewing saved Live sessions. Unlike the
//! overlay it appears in the taskbar and in screen capture; it's for reading, not live use.
//! Layout follows the Paper "Sessions" page and F5 ("Features Oct 2026"): list · notes, the
//! follow-up email and the timeline · questions rail. Shared by Windows and macOS.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Local, TimeZone};
use gpui::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, Context, Div, Entity, Focusable, FontWeight, IntoElement, MouseButton, ObjectFit,
    ParentElement, PromptLevel, Render, ScrollHandle, SharedString, Stateful, Styled, StyledImage, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, deferred, div, img, prelude::*, px, rgb, size,
};

use crate::archive::{self, Archive, EmailDraft, EmailField, Line, NoteBlock, Notes, Session, Speaker, Summary, SummaryLength, Turn, clock};
use crate::codex::CodexClient;
use crate::input::{InputEvent, TextInput};
use crate::modes::ModeStore;
use crate::notes;
use crate::platform;
use crate::settings::Store;
use crate::text_area::TextArea;
use crate::theme;
use crate::ui;

/// Continue a saved session Live, by id (`Overlay::resume_session`).
pub type Resume = Rc<dyn Fn(String, &mut App)>;

const DANGER: u32 = 0xffb4a8;
const WRITING_NOTES: &str = "Writing notes…";
const WRITING_EMAIL: &str = "Writing the email…";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab { Notes, Email, Timeline, Transcript, Answers, Screenshots }

/// What is open for editing in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Editing { Section(usize), Email(EmailField) }

/// Which Copy button just put something on the clipboard; it reads "Copied" for a moment.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Copied { Notes, Transcript, Email }

pub struct SessionsWindow {
    archive: Archive,
    listings: Vec<Summary>,
    session: Option<Session>,
    /// The open session's mode name ("Coding Interview"), for the header and the notes.
    mode_name: Option<String>,
    tab: Tab,
    length: SummaryLength,
    timeline: ScrollHandle,
    notice: Option<SharedString>,
    codex: Arc<CodexClient>,
    resume: Resume,
    search: Entity<TextInput>,
    ask_input: Entity<TextInput>,
    /// The text being edited in place (a note section or part of the email).
    editor: Entity<TextArea>,
    editing: Option<Editing>,
    menu_open: bool,
    copied: Option<Copied>,
    /// Lower-cased searchable text per session id, rebuilt on reload.
    index: Vec<(String, String)>,
    query: String,
    /// Questions asked about the open session and their answers (None while waiting).
    asks: Vec<(String, Option<Result<String, String>>)>,
    /// What the model is currently writing for this session, if anything.
    working: Option<&'static str>,
}

/// Open the window, or reload and raise it. `focus`: whether it may become key (macOS: only while
/// CluelyRS is the active app, see `platform::activate_then`). `resume` is what its Resume calls.
pub fn open(existing: &mut Option<WindowHandle<SessionsWindow>>, root: PathBuf, codex: Arc<CodexClient>, resume: Resume, focus: bool, cx: &mut App) {
    if let Some(handle) = existing && handle.update(cx, |this, window, cx| { this.reload(cx); if focus { window.activate_window(); } }).is_ok() {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.0), px(800.0)), cx))),
        // The frame is drawn by the window itself: a title bar with its own controls on Windows, and
        // around the system's traffic lights (centred in it) on macOS.
        titlebar: Some(TitlebarOptions { title: Some("Sessions".into()), appears_transparent: true,
            traffic_light_position: cfg!(target_os = "macos").then(|| gpui::point(px(14.0), px(11.0))) }),
        kind: WindowKind::Normal,
        focus,
        show: true,
        is_resizable: true,
        is_minimizable: true,
        window_min_size: Some(size(px(900.0), px(560.0))),
        ..Default::default()
    };
    *existing = cx.open_window(options, |window, cx| cx.new(|cx| SessionsWindow::new(Archive::at(root), codex, resume, window, cx))).ok();
}

impl SessionsWindow {
    fn new(archive: Archive, codex: Arc<CodexClient>, resume: Resume, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search transcripts and answers", cx));
        let ask_input = cx.new(|cx| TextInput::new("What did they ask about caching?", cx));
        let editor = cx.new(|cx| TextArea::new("", cx));
        // Notes written in the background and a Live session ending in the overlay change the open
        // session while this window is in the background: coming back to it shows them. Only that
        // session is read again, not the whole list.
        cx.observe_window_activation(window, |this, window, cx| if window.is_window_active() { this.refresh_open(cx) }).detach();
        cx.subscribe(&search, |this, input, event, cx| {
            if matches!(event, InputEvent::Changed) { this.query = input.read(cx).text().trim().to_lowercase(); cx.notify(); }
        }).detach();
        cx.subscribe(&ask_input, |this, input, event, cx| {
            if matches!(event, InputEvent::Submit) {
                let question = input.read(cx).text().trim().to_string();
                if !question.is_empty() { input.update(cx, |input, cx| input.clear(cx)); this.ask(question, cx); }
            }
        }).detach();
        let listings = archive.list();
        let session = listings.first().and_then(|first| archive.load(&first.id));
        let mut this = Self { archive, listings, session: None, mode_name: None, tab: Tab::Notes, length: SummaryLength::Standard, timeline: ScrollHandle::new(),
            notice: None, codex, resume, search, ask_input, editor, editing: None, menu_open: false, copied: None,
            index: Vec::new(), query: String::new(), asks: Vec::new(), working: None };
        this.set_session(session);
        this.rebuild_index();
        this
    }

    /// Show `session`, looking up its mode's name.
    fn set_session(&mut self, session: Option<Session>) {
        self.mode_name = session.as_ref().and_then(|s| s.mode.as_deref()).and_then(|id| ModeStore::load().get(id).map(|mode| mode.name.clone()));
        self.session = session;
    }

    fn rebuild_index(&mut self) {
        self.index = self.listings.iter().filter_map(|l| self.archive.load(&l.id)).map(|s| {
            let text = [s.title.clone().unwrap_or_default(), crate::notes::transcript(&s)].join("\n").to_lowercase();
            (s.id, text)
        }).collect();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.listings = self.archive.list();
        self.rebuild_index();
        let selected = self.session.as_ref().map(|s| s.id.clone()).filter(|id| self.listings.iter().any(|l| &l.id == id))
            .or_else(|| self.listings.first().map(|l| l.id.clone()));
        if self.session.as_ref().map(|s| &s.id) != selected.as_ref() { self.editing = None; }
        self.set_session(selected.and_then(|id| self.archive.load(&id)));
        cx.notify();
    }

    /// Read the open session again, unless it is being edited.
    fn refresh_open(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.session.as_ref().map(|s| s.id.clone()).filter(|_| self.editing.is_none()) else { return };
        let Some(session) = self.archive.load(&id) else { return };
        // Its row in the list too: Live becomes its length, and notes bring a title.
        if let Some(listing) = self.listings.iter_mut().find(|listing| listing.id == id) {
            (listing.ended_at, listing.title) = (session.ended_at, session.title.clone());
        }
        self.set_session(Some(session));
        cx.notify();
    }

    /// Show session `id` (also from the overlay's Sessions panel, `Overlay::open_sessions_at`).
    pub(crate) fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.set_session(self.archive.load(id));
        self.tab = Tab::Notes;
        self.notice = None;
        self.editing = None;
        self.menu_open = false;
        self.asks.clear();
        cx.notify();
    }

    /// Run blocking model work off the UI thread, then apply the result to the session it was for.
    fn with_model<T: Send + 'static>(&mut self, label: &'static str, cx: &mut Context<Self>,
        work: impl FnOnce(crate::settings::Settings, Arc<CodexClient>, Session) -> Result<T, String> + Send + 'static,
        apply: impl FnOnce(&mut Self, &str, Result<T, String>, &mut Context<Self>) + 'static) {
        let Some(session) = self.session.clone() else { return };
        if self.working.is_some() { return; }
        self.working = Some(label);
        self.notice = None;
        let codex = self.codex.clone();
        let settings = Store::load().value;
        let id = session.id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { work(settings, codex, session) }).await;
            let _ = this.update(cx, |this, cx| { this.working = None; apply(this, &id, result, cx); cx.notify(); });
        }).detach();
        cx.notify();
    }

    /// Write the notes and the email draft under the mode's current sections. `keep_edits` keeps
    /// what the user changed.
    fn generate_notes(&mut self, keep_edits: bool, cx: &mut Context<Self>) {
        self.editing = None;
        self.with_model(WRITING_NOTES, cx, |settings, codex, session| {
            let sections = ModeStore::load().note_template(session.mode.as_deref());
            notes::generate(&settings, &codex, &session, &sections)
        }, move |this, id, result, cx| match result {
            Ok(generated) => match this.archive.update(id, |s| notes::apply(s, generated, keep_edits)) {
                Ok(_) => { this.length = SummaryLength::Standard; this.reload(cx); }
                Err(_) => this.notice = Some("The notes couldn't be saved.".into()),
            },
            Err(error) => this.notice = Some(error.into()),
        });
    }

    /// Regenerate: asks first when that would replace what the user wrote.
    fn regenerate_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        if !notes::has_edits(session) { self.generate_notes(false, cx); return; }
        let answer = window.prompt(PromptLevel::Info, "Regenerate the notes?",
            Some("You've edited these notes or the email. Keep your edits and rewrite the rest, or replace everything."),
            &["Keep my edits", "Replace my edits", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            let keep = match answer.await { Ok(0) => true, Ok(1) => false, _ => return };
            let _ = this.update(cx, |this, cx| this.generate_notes(keep, cx));
        }).detach();
    }

    /// Draft the email on its own, from the notes.
    fn write_email(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.with_model(WRITING_EMAIL, cx, |settings, codex, session| notes::email(&settings, &codex, &session),
            |this, id, result, cx| match result {
                Ok(draft) => match this.archive.update(id, |s| s.email = Some(draft)) {
                    Ok(_) => this.reload(cx),
                    Err(_) => this.notice = Some("The email couldn't be saved.".into()),
                },
                Err(error) => this.notice = Some(error.into()),
            });
    }

    fn regenerate_email(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let edited = self.session.as_ref().and_then(|s| s.email.as_ref()).is_some_and(|email| email.edited_at.is_some());
        if !edited { self.write_email(cx); return; }
        let answer = window.prompt(PromptLevel::Info, "Rewrite the email?", Some("Your changes to the draft are replaced."), &["Rewrite", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) { let _ = this.update(cx, |this, cx| this.write_email(cx)); }
        }).detach();
    }

    fn write_overview(&mut self, length: SummaryLength, expand: bool, cx: &mut Context<Self>) {
        let target = if expand { SummaryLength::Detailed } else { length };
        self.with_model(if expand { "Expanding…" } else { "Writing overview…" }, cx,
            move |settings, codex, session| crate::notes::overview(&settings, &codex, &session, length, expand),
            move |this, id, result, cx| match result {
                Ok(text) => match this.archive.update(id, |s| { s.notes.get_or_insert_with(Default::default).overviews.insert(target, text); }) {
                    Ok(_) => { this.length = target; this.reload(cx); }
                    Err(_) => this.notice = Some("The overview couldn't be saved.".into()),
                },
                Err(error) => this.notice = Some(error.into()),
            });
    }

    fn ask(&mut self, question: String, cx: &mut Context<Self>) {
        if self.working.is_some() || self.session.is_none() { return; }
        self.asks.push((question.clone(), None));
        let index = self.asks.len() - 1;
        self.with_model("Answering…", cx, move |settings, codex, session| crate::notes::ask(&settings, &codex, &session, &question),
            move |this, id, result, _| {
                if this.session.as_ref().is_some_and(|s| s.id == id) && let Some(entry) = this.asks.get_mut(index) { entry.1 = Some(result); }
            });
    }

    /// Open `editing` with `text` in the editor and give it the keyboard.
    fn start_edit(&mut self, editing: Editing, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.working.is_some() { return; }
        self.editing = Some(editing);
        // To and Subject are one line; the body and the sections grow from two.
        let lines = if matches!(editing, Editing::Email(EmailField::To | EmailField::Subject)) { 1 } else { 2 };
        self.editor.update(cx, |editor, cx| { editor.set_min_lines(lines, cx); editor.set_text(&text, cx) });
        window.focus(&self.editor.focus_handle(cx));
        cx.notify();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(editing), Some(id)) = (self.editing.take(), self.session.as_ref().map(|s| s.id.clone())) else { return };
        let text = self.editor.read(cx).text().to_string();
        let now = archive::unix_now();
        let saved = match editing {
            Editing::Section(index) => self.archive.edit_section(&id, index, &text, now),
            Editing::Email(field) => self.archive.edit_email(&id, field, &text, now),
        };
        match saved {
            Ok(session) => self.set_session(Some(session)),
            Err(_) => self.notice = Some("The change couldn't be saved.".into()),
        }
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        cx.notify();
    }

    fn copy(&mut self, what: Copied, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(what);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| if this.copied == Some(what) { this.copied = None; cx.notify(); });
        }).detach();
        cx.notify();
    }

    /// Continue the open session Live in the overlay, appending to it.
    fn resume_live(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.session.as_ref().filter(|s| s.ended_at.is_some()).map(|s| s.id.clone()) else { return };
        self.editing = None;
        let resume = self.resume.clone();
        cx.spawn(async move |this, cx| {
            let _ = cx.update(|cx| resume(id, cx));
            let _ = this.update(cx, |this, cx| this.reload(cx));
        }).detach();
    }

    /// Jump to the first timeline entry at or after `at_ms`.
    fn jump_to(&mut self, at_ms: u64, cx: &mut Context<Self>) {
        self.tab = Tab::Timeline;
        if let Some(session) = &self.session {
            let index = timeline(session).iter().position(|(at, _)| *at >= at_ms).unwrap_or(0);
            self.timeline.scroll_to_top_of_item(index);
        }
        cx.notify();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.session.as_ref().map(|s| s.id.clone()) else { return };
        self.notice = Some(match self.archive.export_markdown(&id) {
            Ok(path) => {
                let _ = platform::reveal_file(&path);
                "Saved session.md next to the session".into()
            }
            Err(_) => "Export failed. Check the sessions folder permissions.".into(),
        });
        cx.notify();
    }

    fn reveal(&self) {
        if let Some(session) = &self.session {
            let _ = platform::open_folder(&self.archive.root().join(&session.id));
        }
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.session.as_ref().map(|s| s.id.clone()) else { return };
        let answer = window.prompt(PromptLevel::Warning, "Delete this session?",
            Some("Its transcript, answers and screenshots are removed from this PC."), &["Delete", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    if this.archive.delete(&id).is_err() { this.notice = Some("The session could not be deleted.".into()); }
                    this.session = None;
                    this.reload(cx);
                });
            }
        }).detach();
    }
}

enum Entry<'a> { Line(&'a Line), Turn(&'a Turn) }

fn timeline(session: &Session) -> Vec<(u64, Entry<'_>)> {
    let mut entries: Vec<(u64, Entry)> = session.transcript.iter().map(|line| (line.at_ms, Entry::Line(line)))
        .chain(session.turns.iter().map(|turn| (turn.at_ms, Entry::Turn(turn)))).collect();
    entries.sort_by_key(|(at, _)| *at);
    entries
}

fn local(seconds: u64) -> Option<chrono::DateTime<Local>> { Local.timestamp_opt(seconds as i64, 0).single() }

fn day_group(started_at: u64) -> &'static str {
    let (Some(then), now) = (local(started_at), Local::now()) else { return "Older" };
    let days = (now.date_naive() - then.date_naive()).num_days();
    match days {
        0 => "Today",
        1 => "Yesterday",
        2..=6 if then.iso_week() == now.iso_week() => "Earlier this week",
        _ => "Older",
    }
}

fn minutes(start: u64, end: Option<u64>) -> String {
    match end {
        None => "Live".into(),
        Some(end) => format!("{} min", (end.saturating_sub(start) / 60).max(1)),
    }
}

/// "just now", "2 min ago", "3 h ago", "Oct 5".
fn ago(then: u64, now: u64) -> String {
    match now.saturating_sub(then) {
        0..60 => "just now".into(),
        seconds @ 60..3_600 => format!("{} min ago", seconds / 60),
        seconds @ 3_600..86_400 => format!("{} h ago", seconds / 3_600),
        _ => local(then).map(|t| t.format("%b %-d").to_string()).unwrap_or_default(),
    }
}

fn plural(count: usize, word: &str) -> String { format!("{count} {word}{}", if count == 1 { "" } else { "s" }) }

fn label(text: &'static str) -> Div {
    div().text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(text)
}

fn muted(text: impl Into<SharedString>) -> Div { div().text_size(px(13.0)).text_color(theme::muted()).child(text.into()) }

fn outline(id: &'static str) -> Stateful<Div> {
    div().id(id).flex().items_center().gap(px(6.0)).cursor_pointer().px(px(12.0)).py(px(7.0)).rounded(px(9.0)).border_1().border_color(theme::hairline())
        .text_size(px(13.0)).text_color(theme::body()).hover(|b| b.border_color(theme::bubble_border()))
}

fn outline_button(id: &'static str, text: &'static str) -> Stateful<Div> { outline(id).child(text) }

/// The accent-tinted button of a row (Resume, Regenerate, Copy email).
fn tinted_button(id: &'static str, icon: Option<&'static str>, text: &'static str) -> Stateful<Div> {
    div().id(id).flex().items_center().gap(px(6.0)).cursor_pointer().px(px(12.0)).py(px(7.0)).rounded(px(9.0)).bg(theme::bubble())
        .border_1().border_color(theme::bubble_border()).text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text())
        .children(icon.map(|path| ui::icon(path, 13.0, theme::accent_soft()))).child(text)
}

/// A button with a leading icon and an outline (Copy notes, Regenerate).
fn icon_button(id: &'static str, icon: &'static str, text: &'static str) -> Stateful<Div> {
    outline(id).child(ui::icon(icon, 13.0, theme::body())).child(text)
}

fn primary_button(id: &'static str, text: &'static str) -> Stateful<Div> {
    div().id(id).cursor_pointer().px(px(12.0)).py(px(7.0)).rounded(px(9.0)).bg(theme::accent())
        .text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink()).child(text)
}

fn speaker(speaker: Speaker) -> (&'static str, gpui::Rgba) {
    match speaker { Speaker::You => ("You", theme::accent_soft()), Speaker::Them => ("Them", theme::ok()) }
}

/// What Copy notes puts on the clipboard: the title, then the notes as Markdown.
fn notes_clipboard(session: &Session, notes: &Notes) -> String {
    format!("# {}\n\n{}", session.title.as_deref().unwrap_or("Session"), archive::notes_markdown(notes)).trim_end().to_string()
}

/// What the rail's email card shows of the body: its first lines, cut at a word.
fn preview(body: &str, max: usize) -> String {
    let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max { return text; }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head))
}

impl SessionsWindow {
    fn screenshot(&self, session: &Session, file: &str) -> Option<PathBuf> { self.archive.screenshot_path(&session.id, file) }

    fn copy_label(&self, what: Copied, text: &'static str) -> &'static str { if self.copied == Some(what) { "Copied" } else { text } }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.session.as_ref().map(|s| s.id.as_str());
        let mut list = div().id("session-list").flex().flex_col().gap(px(2.0)).px(px(8.0)).py(px(4.0)).flex_1().overflow_y_scroll();
        let mut group = "";
        let matches = |id: &str| self.query.is_empty() || self.index.iter().any(|(i, text)| i == id && text.contains(&self.query));
        for (index, listing) in self.listings.iter().enumerate().filter(|(_, l)| matches(&l.id)) {
            let heading = day_group(listing.started_at);
            if heading != group {
                group = heading;
                list = list.child(div().px(px(10.0)).pt(px(if index == 0 { 8.0 } else { 14.0 })).pb(px(6.0))
                    .child(label(match heading { "Today" => "TODAY", "Yesterday" => "YESTERDAY", "Earlier this week" => "EARLIER THIS WEEK", _ => "OLDER" })));
            }
            let is_selected = selected == Some(listing.id.as_str());
            let id = listing.id.clone();
            let when = local(listing.started_at).map(|t| if heading == "Today" || heading == "Yesterday" { t.format("%-I:%M %p") } else { t.format("%a %-I:%M %p") }.to_string()).unwrap_or_default();
            let mut meta = vec![when, plural(listing.turns, "answer")];
            if listing.screenshots > 0 { meta.push(plural(listing.screenshots, "screenshot")); }
            let title = listing.title.clone().unwrap_or_else(|| local(listing.started_at).map(|t| t.format("Session · %b %-d").to_string()).unwrap_or("Session".into()));
            list = list.child(div().id(("row", index)).cursor_pointer().flex().flex_col().gap(px(3.0)).p(px(10.0)).rounded(px(10.0)).border_1()
                .border_color(if is_selected { theme::bubble_border() } else { gpui::transparent_black().into() })
                .when(is_selected, |row| row.bg(theme::bubble()))
                .when(!is_selected, |row| row.hover(|row| row.bg(theme::raised())))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.select(&id, cx)))
                .child(div().flex().justify_between().items_baseline()
                    .child(div().flex_1().min_w_0().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).truncate().child(title))
                    .child(div().w(px(52.0)).flex_none().flex().justify_end().text_size(px(12.0)).text_color(theme::muted()).child(minutes(listing.started_at, listing.ended_at))))
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child(meta.join(" · "))));
        }
        if self.listings.is_empty() {
            list = list.child(div().p(px(12.0)).text_size(px(13.0)).text_color(theme::muted()).child("No saved sessions yet. Turn on Live in the overlay to record one."));
        }
        div().w(px(300.0)).flex_none().h_full().flex().flex_col().bg(rgb(0x131417)).border_r_1().border_color(theme::divider())
            .child(div().flex().items_center().gap(px(10.0)).px(px(18.0)).pt(px(18.0)).pb(px(14.0))
                .child(ui::mark(26.0))
                .child(div().text_size(px(15.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("Sessions"))
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child(format!("{} saved", self.listings.len()))))
            .child(div().id("search").mx(px(14.0)).mb(px(10.0)).px(px(10.0)).py(px(8.0)).rounded(px(9.0)).bg(theme::raised())
                .border_1().border_color(theme::hairline()).cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.search.focus_handle(cx))))
                .child(self.search.clone()))
            .child(list)
            .child(div().flex().items_center().justify_between().px(px(18.0)).py(px(14.0)).border_t_1().border_color(theme::divider())
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child(if cfg!(target_os = "macos") { "Saved on this Mac" } else { "Saved on this PC" })))
    }

    fn header(&self, session: &Session, cx: &mut Context<Self>) -> impl IntoElement {
        let start = local(session.started_at);
        let mut meta = vec![
            match day_group(session.started_at) { "Today" => "Today".to_string(), "Yesterday" => "Yesterday".to_string(), _ => start.map(|t| t.format("%a %b %-d").to_string()).unwrap_or_default() },
            match (start, session.ended_at.and_then(local)) {
                (Some(a), Some(b)) => format!("{} – {}", a.format("%-I:%M"), b.format("%-I:%M %p")),
                (Some(a), None) => a.format("%-I:%M %p").to_string(),
                _ => String::new(),
            },
            minutes(session.started_at, session.ended_at),
        ];
        if let Some(name) = self.mode_name.clone().or_else(|| session.model.clone()) { meta.push(name); }
        let title = session.title.clone().unwrap_or_else(|| "Untitled session".into());
        let shots = session.turns.iter().filter(|t| t.screenshot.is_some()).count();
        let tabs = [(Tab::Notes, "Notes".to_string()), (Tab::Email, "Follow-up email".to_string()), (Tab::Timeline, "Timeline".to_string()),
            (Tab::Transcript, "Transcript".to_string()), (Tab::Answers, format!("Answers  {}", session.turns.len())), (Tab::Screenshots, format!("Screenshots  {shots}"))];
        let mut tab_row = div().flex().gap(px(22.0)).border_b_1().border_color(theme::divider());
        for (index, (tab, text)) in tabs.into_iter().enumerate() {
            let selected = tab == self.tab;
            tab_row = tab_row.child(div().id(("tab", index)).cursor_pointer().pb(px(10.0)).text_size(px(13.0))
                .when(selected, |t| t.font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).border_b_2().border_color(theme::accent()))
                .when(!selected, |t| t.text_color(theme::muted()).hover(|t| t.text_color(theme::body())))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| { this.tab = tab; this.editing = None; cx.notify(); }))
                .child(text));
        }
        let live = session.ended_at.is_none();
        let resume = if live {
            tinted_button("resume", Some("icons/setup-wave.svg"), "Live now").opacity(0.6).cursor_default()
        } else {
            tinted_button("resume", Some("icons/setup-wave.svg"), "Resume").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.resume_live(cx)))
        };
        let spoken = notes::spoken_transcript(session);
        let copy_transcript = outline_button("copy-transcript", self.copy_label(Copied::Transcript, "Copy transcript"))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.copy(Copied::Transcript, spoken.clone(), cx)));
        let mut more = div().relative().child(div().id("more").size(px(34.0)).flex().items_center().justify_center().cursor_pointer().rounded(px(9.0))
            .border_1().border_color(if self.menu_open { theme::accent() } else { theme::hairline() }).hover(|b| b.border_color(theme::bubble_border()))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.menu_open = !this.menu_open; cx.notify(); }))
            .child(ui::icon("icons/more.svg", 14.0, theme::body())));
        if self.menu_open { more = more.child(deferred(self.more_menu(cx)).with_priority(1)); }
        div().flex().flex_col().gap(px(14.0)).px(px(32.0)).pt(px(26.0))
            .child(div().flex().justify_between().items_start().gap(px(24.0))
                .child(div().flex().flex_col().gap(px(6.0)).min_w_0()
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child(meta.into_iter().filter(|m| !m.is_empty()).collect::<Vec<_>>().join(" · ")))
                    .child(div().text_size(px(30.0)).line_height(px(36.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title)))
                .child(div().flex().items_center().gap(px(8.0)).pt(px(4.0)).flex_none().child(resume).child(copy_transcript).child(more)))
            .when_some(self.notice.clone(), |header, notice| header.child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice)))
            .child(tab_row)
    }

    /// ⋯: Export, Open folder and Delete.
    fn more_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let close = |this: &mut Self| this.menu_open = false;
        div().absolute().top(px(40.0)).right_0().w(px(200.0)).child(ui::menu().id("more-menu")
            .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.menu_open = false; cx.notify(); }))
            .child(ui::menu_item("menu-export", "Export Markdown", false)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| { close(this); this.export(cx); })))
            .child(ui::menu_item("menu-folder", "Open folder", false)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| { close(this); this.reveal(); cx.notify(); })))
            .child(div().h(px(1.0)).my(px(4.0)).bg(theme::hairline()))
            .child(ui::menu_item("menu-delete", "Delete", false).text_color(rgb(DANGER))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { close(this); this.confirm_delete(window, cx); }))))
    }

    /// A tab's top row: its label, a note beside it and its buttons.
    fn toolbar(caption: &'static str, note: String, buttons: Vec<AnyElement>) -> Div {
        div().flex().items_center().justify_between().gap(px(12.0))
            .child(div().flex().items_baseline().gap(px(10.0)).min_w_0().child(label(caption)).child(div().min_w_0().truncate().text_size(px(13.0)).text_color(theme::muted()).child(note)))
            .child(div().flex().flex_none().gap(px(8.0)).children(buttons))
    }

    /// The editor in its box, as in a section being edited: the field's own text size, a 2 px
    /// accent edge below.
    fn editor_box(&self) -> Div {
        div().relative().w_full().min_w_0().overflow_hidden().px(px(14.0)).py(px(10.0)).rounded(px(10.0)).bg(theme::field()).border_1().border_color(theme::hairline())
            .cursor_text().text_color(theme::text())
            .child(self.editor.clone())
            .child(div().absolute().left_0().right_0().bottom_0().h(px(2.0)).bg(theme::accent()))
    }

    fn cancel_save(&self, cx: &mut Context<Self>) -> Div {
        div().flex().items_center().gap(px(8.0))
            .child(outline_button("edit-cancel", "Cancel").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.cancel_edit(cx))))
            .child(primary_button("edit-save", "Save").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.save_edit(cx))))
    }

    fn notes_tab(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        match session.notes.as_ref().filter(|notes| !notes.sections.is_empty()) {
            Some(notes) if self.working != Some(WRITING_NOTES) => self.sections_view(session, notes, cx),
            _ => self.summary_tab(session, cx),
        }
    }

    /// Notes under the mode's sections, each editable in place.
    fn sections_view(&self, session: &Session, notes: &Notes, cx: &mut Context<Self>) -> AnyElement {
        let mut note = format!("{} sections", self.mode_name.as_deref().unwrap_or("Mode"));
        if let Some(at) = notes.edited_at() { note.push_str(&format!(" · edited by you {}", ago(at, archive::unix_now()))); }
        let copy_text = notes_clipboard(session, notes);
        let live = session.ended_at.is_none();
        let mut buttons = vec![icon_button("copy-notes", "icons/copy.svg", self.copy_label(Copied::Notes, "Copy notes"))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.copy(Copied::Notes, copy_text.clone(), cx))).into_any_element()];
        if !live {
            buttons.push(tinted_button("regenerate", Some("icons/setup-refresh.svg"), "Regenerate").when(self.working.is_some(), |b| b.opacity(0.55))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.regenerate_notes(window, cx))).into_any_element());
        }
        let mut body = div().id("notes").flex().flex_col().gap(px(26.0)).px(px(32.0)).py(px(22.0)).flex_1().overflow_y_scroll()
            .child(Self::toolbar("NOTES", note, buttons));
        if live { body = body.child(muted("Live now. The notes are rewritten when this session ends; your edits are kept.")); }
        for (index, block) in notes.sections.iter().enumerate() {
            body = body.child(self.section_block(index, block, live, cx));
        }
        body.into_any_element()
    }

    fn section_block(&self, index: usize, block: &NoteBlock, live: bool, cx: &mut Context<Self>) -> Div {
        let editing = self.editing == Some(Editing::Section(index));
        let heading = div().flex().items_center().justify_between().gap(px(12.0)).min_h(px(34.0))
            .child(div().text_size(px(17.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(block.name.clone()))
            .when(editing, |row| row.child(self.cancel_save(cx)));
        let content: AnyElement = if editing { self.editor_box().text_size(px(15.0)).line_height(px(23.0)).into_any_element() } else {
            let text = block.text.clone();
            let shown: AnyElement = if text.trim().is_empty() {
                muted(if live { "Nothing yet." } else { "Nothing noted. Click to add." }).into_any_element()
            } else {
                div().text_size(px(15.0)).line_height(px(23.0)).text_color(theme::body()).child(crate::markdown::render(&text, 15.0)).into_any_element()
            };
            div().id(("section", index)).rounded(px(6.0)).when(!live, |area| area.cursor_text().hover(|area| area.bg(theme::raised()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.start_edit(Editing::Section(index), text.clone(), window, cx))))
                .child(shown).into_any_element()
        };
        div().flex().flex_col().gap(px(8.0)).child(heading).child(content)
    }

    /// Notes in the general format (and sessions saved before note sections).
    fn summary_tab(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let notes = session.notes.as_ref();
        let mut lengths = ui::segmented();
        for (index, length) in SummaryLength::ALL.into_iter().enumerate() {
            lengths = lengths.child(ui::segment(("length", index), length.label(), length == self.length)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| { this.length = length; cx.notify(); })));
        }
        let busy = self.working.is_some();
        let live = session.ended_at.is_none();
        let expand = div().id("expand").flex().items_center().gap(px(6.0)).px(px(11.0)).py(px(6.0)).rounded(px(8.0)).bg(theme::bubble())
            .border_1().border_color(theme::bubble_border()).cursor_pointer().when(busy, |b| b.opacity(0.55))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { let length = this.length; this.write_overview(length, true, cx); }))
            .child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child("✦"))
            .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Expand with AI"));
        let action = |id: &'static str, label: String| div().id(id).cursor_pointer().px(px(12.0)).py(px(7.0)).rounded(px(9.0)).bg(theme::accent())
            .text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink()).child(label);
        let overview: AnyElement = match (self.working, notes.and_then(|n| n.overviews.get(&self.length))) {
            (Some(label), _) => div().text_size(px(14.0)).text_color(theme::muted()).child(label).into_any_element(),
            (None, _) if live => muted("Live now. Notes are written when this session ends.").into_any_element(),
            (None, Some(text)) => div().text_size(px(17.0)).line_height(px(27.0)).text_color(theme::text())
                .child(crate::markdown::render(text, 17.0)).into_any_element(),
            (None, None) if notes.is_some() => {
                let length = self.length;
                div().flex().items_center().gap(px(12.0)).p(px(14.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
                    .child(div().flex_1().text_size(px(13.0)).text_color(theme::muted()).child(format!("No {} overview yet.", length.label().to_lowercase())))
                    .child(action("write-overview", format!("Write {}", length.label().to_lowercase()))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.write_overview(length, false, cx))))
                    .into_any_element()
            }
            (None, None) => div().flex().items_center().gap(px(12.0)).p(px(14.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
                .child(div().flex_1().text_size(px(13.0)).text_color(theme::muted())
                    .child("Notes are written by the model you chose in Settings when a session ends."))
                .child(action("generate-notes", "Generate notes".into())
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.generate_notes(false, cx))))
                .into_any_element(),
        };
        // Talk share by spoken text length, from the labelled transcript.
        let (you, them) = session.transcript.iter().fold((0usize, 0usize), |(y, t), line| match line.speaker {
            Speaker::You => (y + line.text.len(), t), Speaker::Them => (y, t + line.text.len()) });
        let share = if you + them == 0 { None } else { Some(you as f32 / (you + them) as f32) };
        let stat = |value: String, caption: &'static str, extra: Option<AnyElement>| div().flex_1().flex().flex_col().gap(px(5.0)).px(px(14.0)).py(px(12.0))
            .rounded(px(12.0)).border_1().border_color(theme::hairline())
            .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(value))
            .children(extra)
            .child(div().text_size(px(12.0)).text_color(theme::muted()).child(caption));
        let bar = share.map(|share| div().flex().h(px(4.0)).rounded(px(2.0)).overflow_hidden().bg(theme::hairline())
            .child(div().w(gpui::relative(share)).bg(theme::accent_soft())).child(div().flex_1().bg(theme::ok()).opacity(0.6)).into_any_element());
        let stats = div().flex().gap(px(10.0))
            .child(stat(minutes(session.started_at, session.ended_at), "Duration", None))
            .child(stat(share.map(|s| format!("{}%", (s * 100.0).round())).unwrap_or("—".into()), "You talked", bar))
            .child(stat(session.turns.len().to_string(), "Answers used", None))
            .child(stat(session.turns.iter().filter(|t| t.screenshot.is_some()).count().to_string(), "Screenshots", None));
        let mut covered = div().flex().flex_col().child(label("WHAT WAS COVERED").pb(px(8.0)));
        let topics = notes.map(|n| n.topics.as_slice()).unwrap_or(&[]);
        for (index, topic) in topics.iter().enumerate() {
            let start = topic.start_ms;
            covered = covered.child(div().id(("topic", index)).cursor_pointer().flex().gap(px(14.0)).py(px(10.0))
                .when(index + 1 < topics.len(), |row| row.border_b_1().border_color(theme::divider()))
                .hover(|row| row.bg(theme::raised()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.jump_to(start, cx)))
                .child(div().w(px(100.0)).flex_none().pt(px(3.0)).font_family(theme::MONO).text_size(px(11.0)).text_color(theme::accent_soft())
                    .child(format!("{} – {}", clock(topic.start_ms), clock(topic.end_ms))))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0))
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(topic.title.clone()))
                    .child(div().text_size(px(13.0)).text_color(theme::muted()).child(topic.detail.clone()))));
        }
        if topics.is_empty() {
            covered = covered.child(div().text_size(px(13.0)).text_color(theme::muted()).child("Topics appear here with the summary."));
        }
        let mut follow = div().flex().flex_col().gap(px(10.0)).child(label("FOLLOW UP"));
        let items = notes.map(|n| n.follow_ups.as_slice()).unwrap_or(&[]);
        for item in items {
            follow = follow.child(div().flex().gap(px(10.0)).items_start()
                .child(div().size(px(14.0)).mt(px(2.0)).flex_none().rounded(px(4.0)).border(px(1.5)).border_color(rgb(0x4a4d52)))
                .child(div().flex_1().min_w_0().text_size(px(13.0)).line_height(px(19.0)).text_color(theme::body()).child(item.clone())));
        }
        // Copy notes and Regenerate once there are notes; regenerating writes them under the mode's sections.
        let toolbar = notes.filter(|_| !live).map(|notes| {
            let copy_text = notes_clipboard(session, notes);
            Self::toolbar("NOTES", "General format".into(), vec![
                icon_button("copy-notes", "icons/copy.svg", self.copy_label(Copied::Notes, "Copy notes"))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.copy(Copied::Notes, copy_text.clone(), cx))).into_any_element(),
                tinted_button("regenerate", Some("icons/setup-refresh.svg"), "Regenerate").when(busy, |b| b.opacity(0.55))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.regenerate_notes(window, cx))).into_any_element(),
            ])
        });
        div().id("summary").flex().flex_col().gap(px(24.0)).px(px(32.0)).py(px(22.0)).flex_1().overflow_y_scroll()
            .children(toolbar)
            .child(div().flex().items_center().justify_between()
                .child(label("OVERVIEW"))
                .child(div().flex().items_center().gap(px(10.0))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Length"))
                    .child(lengths).child(expand)))
            .child(overview).child(stats).child(covered)
            .when(!items.is_empty(), |summary| summary.child(follow))
            .into_any_element()
    }

    /// The follow-up email draft: To, Subject and body, each editable in place.
    fn email_tab(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let live = session.ended_at.is_none();
        let email = session.email.as_ref();
        let editing = self.editing.is_some_and(|editing| matches!(editing, Editing::Email(_)));
        let mut buttons = Vec::new();
        if editing {
            // While a part is edited, Cancel and Save take the buttons' place, in view above the draft.
            buttons.push(self.cancel_save(cx).into_any_element());
        } else if !live && email.is_some() {
            buttons.push(icon_button("email-regenerate", "icons/setup-refresh.svg", "Regenerate").when(self.working.is_some(), |b| b.opacity(0.55))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.regenerate_email(window, cx))).into_any_element());
        }
        if let Some(email) = email.filter(|_| !editing) {
            let text = email.clipboard_text();
            buttons.push(tinted_button("copy-email", Some("icons/copy.svg"), self.copy_label(Copied::Email, "Copy email"))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.copy(Copied::Email, text.clone(), cx))).into_any_element());
        }
        let note = match email.and_then(|email| email.edited_at) {
            Some(at) => format!("Edited by you {} · never sent from CluelyRS", ago(at, archive::unix_now())),
            None => "Written from the notes · never sent from CluelyRS".into(),
        };
        let mut body = div().id("email").flex().flex_col().gap(px(18.0)).px(px(32.0)).py(px(22.0)).flex_1().overflow_y_scroll()
            .child(Self::toolbar("DRAFT", note, buttons));
        body = match (self.working, email) {
            (Some(WRITING_EMAIL), _) | (Some(WRITING_NOTES), None) => body.child(muted(self.working.unwrap_or_default())),
            (_, Some(email)) => body.child(self.email_card(email, live, cx))
                .child(muted("Click the text to edit it. Copy email puts the subject and body on the clipboard; paste into any mail app.").text_size(px(12.0))),
            (_, None) if live => body.child(muted("Live now. A draft is written with the notes when this session ends.")),
            (_, None) => body.child(div().flex().items_center().gap(px(12.0)).p(px(14.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
                .child(div().flex_1().text_size(px(13.0)).text_color(theme::muted()).child("No draft yet. It's written from the notes by the model you chose in Settings."))
                .child(primary_button("write-email", "Write email").when(self.working.is_some(), |b| b.opacity(0.55))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.write_email(cx))))),
        };
        body.into_any_element()
    }

    fn email_card(&self, email: &EmailDraft, live: bool, cx: &mut Context<Self>) -> Div {
        let field = |this: &Self, field: EmailField, text: String, cx: &mut Context<Self>| -> AnyElement {
            if this.editing == Some(Editing::Email(field)) { return this.editor_box().into_any_element(); }
            let id = match field { EmailField::To => "email-to", EmailField::Subject => "email-subject", EmailField::Body => "email-body" };
            let shown = if text.trim().is_empty() { div().text_color(theme::placeholder()).child(if field == EmailField::To { "Add a name" } else { "Empty" }) }
                else { div().child(text.clone()) };
            div().id(id).flex_1().min_w_0().rounded(px(6.0)).when(!live, |area| area.cursor_text().hover(|area| area.bg(theme::raised()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.start_edit(Editing::Email(field), text.clone(), window, cx))))
                .child(shown).into_any_element()
        };
        let row = |caption: &'static str, value: AnyElement| div().flex().items_center().gap(px(16.0)).px(px(20.0)).py(px(14.0)).border_b_1().border_color(theme::divider())
            .child(div().w(px(80.0)).flex_none().text_size(px(13.0)).text_color(theme::muted()).child(caption))
            .child(value);
        div().flex().flex_col().rounded(px(12.0)).bg(theme::field()).border_1().border_color(theme::hairline())
            .child(row("To", field(self, EmailField::To, email.to.clone(), cx)).text_size(px(14.0)).text_color(theme::text()))
            .child(row("Subject", field(self, EmailField::Subject, email.subject.clone(), cx)).text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()))
            .child(div().px(px(20.0)).py(px(18.0)).text_size(px(15.0)).line_height(px(24.0)).text_color(theme::body())
                .child(field(self, EmailField::Body, email.body.clone(), cx)))
    }

    fn line_row(line: &Line) -> Div {
        let (who, color) = speaker(line.speaker);
        div().flex().gap(px(14.0))
            .child(div().w(px(44.0)).flex_none().pt(px(2.0)).font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(clock(line.at_ms)))
            .child(div().w(px(42.0)).flex_none().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(who))
            .child(div().flex_1().min_w_0().text_size(px(14.0)).line_height(px(21.0)).text_color(theme::body()).child(line.text.clone()))
    }

    fn answer_card(&self, session: &Session, turn: &Turn, indent: bool) -> Div {
        let mut card = div().flex().gap(px(16.0)).p(px(14.0)).rounded(px(14.0)).bg(theme::field()).border_1().border_color(theme::hairline());
        if indent { card = card.ml(px(58.0)); }
        if let Some(path) = turn.screenshot.as_deref().and_then(|file| self.screenshot(session, file)) {
            card = card.child(img(path).w(px(200.0)).h(px(124.0)).flex_none().rounded(px(8.0)).object_fit(ObjectFit::Cover)
                .border_1().border_color(theme::keycap_border()));
        }
        card.child(div().flex_1().min_w_0().flex().flex_col().gap(px(8.0))
            .child(div().flex().items_center().gap(px(8.0))
                .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(clock(turn.at_ms)))
                .child(div().text_size(px(12.0)).px(px(8.0)).py(px(2.0)).rounded(px(8.0)).border_1().border_color(theme::bubble_border())
                    .bg(theme::bubble()).text_color(theme::text()).child(turn.action.clone())))
            .when(!turn.question.is_empty(), |c| c.child(div().text_size(px(13.0)).text_color(theme::muted()).child(turn.question.clone())))
            // The files attached to the question, by name.
            .when(!turn.attachments.is_empty(), |c| c.child(div().flex().flex_wrap().gap(px(6.0)).children(turn.attachments.iter().map(|file| {
                div().h(px(24.0)).flex().items_center().gap(px(6.0)).px(px(8.0)).rounded(px(6.0)).border_1().border_color(theme::hairline()).bg(theme::raised())
                    .child(crate::ui::icon("icons/file.svg", 11.0, theme::muted()))
                    .child(div().text_size(px(11.0)).text_color(theme::body()).child(file.name.clone()))
                    .child(div().text_size(px(11.0)).text_color(theme::muted()).child(file.kind.clone()))
            }))))
            .child(div().min_w_0().text_color(theme::text()).child(crate::markdown::render(&turn.answer, 14.0))))
    }

    fn timeline_tab(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let mut strip = div().flex().gap(px(10.0)).px(px(32.0)).pt(px(16.0)).pb(px(4.0)).flex_none();
        for (index, turn) in session.turns.iter().enumerate() {
            let Some(path) = turn.screenshot.as_deref().and_then(|file| self.screenshot(session, file)) else { continue };
            let at = turn.at_ms;
            strip = strip.child(div().id(("shot", index)).cursor_pointer().flex().flex_col().gap(px(6.0))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.jump_to(at, cx)))
                .child(img(path).w(px(112.0)).h(px(70.0)).rounded(px(8.0)).object_fit(ObjectFit::Cover).border_1().border_color(theme::hairline()))
                .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(clock(at))));
        }
        let mut body = div().id("timeline").flex().flex_col().gap(px(12.0)).px(px(32.0)).py(px(18.0)).flex_1().overflow_y_scroll().track_scroll(&self.timeline);
        for (_, entry) in timeline(session) {
            body = body.child(match entry { Entry::Line(line) => Self::line_row(line), Entry::Turn(turn) => self.answer_card(session, turn, true) });
        }
        div().flex().flex_col().flex_1().min_h_0().child(strip).child(body).into_any_element()
    }

    fn transcript_tab(&self, session: &Session) -> AnyElement {
        let mut body = div().id("transcript").flex().flex_col().gap(px(12.0)).px(px(32.0)).py(px(18.0)).flex_1().overflow_y_scroll();
        for line in &session.transcript { body = body.child(Self::line_row(line)); }
        if session.transcript.is_empty() { body = body.child(div().text_size(px(13.0)).text_color(theme::muted()).child("Nothing was transcribed.")); }
        body.into_any_element()
    }

    fn answers_tab(&self, session: &Session) -> AnyElement {
        let mut body = div().id("answers").flex().flex_col().gap(px(12.0)).px(px(32.0)).py(px(18.0)).flex_1().overflow_y_scroll();
        for turn in &session.turns { body = body.child(self.answer_card(session, turn, false)); }
        if session.turns.is_empty() { body = body.child(div().text_size(px(13.0)).text_color(theme::muted()).child("No answers in this session.")); }
        body.into_any_element()
    }

    fn screenshots_tab(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let mut grid = div().id("screenshots").flex().flex_wrap().gap(px(16.0)).px(px(32.0)).py(px(18.0)).flex_1().overflow_y_scroll().content_start();
        for (index, turn) in session.turns.iter().enumerate() {
            let Some(path) = turn.screenshot.as_deref().and_then(|file| self.screenshot(session, file)) else { continue };
            let at = turn.at_ms;
            grid = grid.child(div().id(("grid", index)).cursor_pointer().w(px(296.0)).flex().flex_col().gap(px(8.0))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.jump_to(at, cx)))
                .child(img(path).w(px(296.0)).h(px(185.0)).rounded(px(10.0)).object_fit(ObjectFit::Cover).border_1().border_color(theme::hairline()))
                .child(div().flex().items_center().gap(px(8.0))
                    .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(clock(at)))
                    .child(div().text_size(px(12.0)).text_color(theme::body()).child(turn.action.clone()))));
        }
        grid.into_any_element()
    }

    /// The rail's follow-up email card: subject, the start of the body, View draft and Copy.
    fn email_summary(&self, session: &Session, cx: &mut Context<Self>) -> Div {
        let mut card = div().flex().flex_col().gap(px(10.0)).child(label("FOLLOW-UP EMAIL"));
        let Some(email) = &session.email else {
            return card.child(muted(if session.ended_at.is_none() { "Drafted with the notes when the session ends." } else { "No draft yet." }));
        };
        let text = email.clipboard_text();
        card = card.child(div().flex().flex_col().gap(px(8.0)).p(px(14.0)).rounded(px(12.0)).bg(theme::field()).border_1().border_color(theme::hairline())
            .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(email.subject.clone()))
            .child(div().text_size(px(13.0)).line_height(px(19.0)).text_color(theme::muted()).child(preview(&email.body, 120)))
            .child(div().flex().gap(px(8.0)).pt(px(4.0))
                .child(outline_button("view-draft", "View draft").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { this.tab = Tab::Email; cx.notify(); })))
                .child(tinted_button("rail-copy-email", None, self.copy_label(Copied::Email, "Copy"))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.copy(Copied::Email, text.clone(), cx))))));
        card
    }

    fn rail(&self, session: &Session, cx: &mut Context<Self>) -> impl IntoElement {
        // Questions heard from the other side, straight from the transcript.
        let questions: Vec<&Line> = session.transcript.iter().filter(|l| l.speaker == Speaker::Them && l.text.contains('?')).take(6).collect();
        let mut key = div().flex().flex_col().gap(px(10.0)).child(label("KEY QUESTIONS"));
        for (index, line) in questions.iter().enumerate() {
            let at = line.at_ms;
            key = key.child(div().id(("question", index)).cursor_pointer().flex().gap(px(10.0)).hover(|row| row.opacity(0.8))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.jump_to(at, cx)))
                .child(div().w(px(40.0)).flex_none().pt(px(2.0)).font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(clock(at)))
                .child(div().flex_1().min_w_0().text_size(px(13.0)).line_height(px(19.0)).text_color(theme::body()).child(line.text.clone())));
        }
        if questions.is_empty() { key = key.child(div().text_size(px(13.0)).text_color(theme::muted()).child("No questions were heard.")); }
        div().id("rail").w(px(300.0)).flex_none().h_full().flex().flex_col().gap(px(22.0)).px(px(22.0)).py(px(26.0)).overflow_y_scroll()
            .bg(rgb(0x131417)).border_l_1().border_color(theme::divider())
            .child(key).child(self.email_summary(session, cx))
            .child(div().flex().flex_col().gap(px(8.0)).pt(px(14.0)).border_t_1().border_color(theme::divider())
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Ask about this session"))
                .child(div().id("ask").flex().items_center().gap(px(8.0)).px(px(10.0)).py(px(9.0)).rounded(px(10.0)).bg(theme::field())
                    .border_1().border_color(theme::hairline()).cursor_text()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.ask_input.focus_handle(cx))))
                    .child(div().flex_1().min_w_0().child(self.ask_input.clone()))
                    .child(div().id("ask-send").size(px(24.0)).flex_none().rounded_full().bg(theme::accent()).flex().items_center().justify_center()
                        .cursor_pointer().text_size(px(12.0)).font_weight(FontWeight::BOLD).text_color(theme::accent_ink())
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            let question = this.ask_input.read(cx).text().trim().to_string();
                            if !question.is_empty() { this.ask_input.update(cx, |input, cx| input.clear(cx)); this.ask(question, cx); }
                        }))
                        .child("↑")))
                .children(self.asks.iter().enumerate().map(|(index, (question, answer))| {
                    let reply: AnyElement = match answer {
                        None => div().text_size(px(13.0)).text_color(theme::muted()).child("Answering…").into_any_element(),
                        Some(Ok(text)) => div().id(("ask-answer", index)).min_w_0().text_color(theme::body()).child(crate::markdown::render(text, 13.0)).into_any_element(),
                        Some(Err(error)) => div().text_size(px(13.0)).text_color(rgb(DANGER)).child(error.clone()).into_any_element(),
                    };
                    div().flex().flex_col().gap(px(6.0)).pt(px(6.0))
                        .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(question.clone()))
                        .child(reply)
                })))
    }
}

impl Render for SessionsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div().flex_1().min_h_0().flex().child(self.sidebar(cx));
        content = match self.session.clone() {
            None => content.child(div().flex_1().flex().items_center().justify_center().text_size(px(14.0)).text_color(theme::muted())
                .child("Select a session to review it.")),
            Some(session) => {
                let body = match self.tab {
                    Tab::Notes => self.notes_tab(&session, cx),
                    Tab::Email => self.email_tab(&session, cx),
                    Tab::Timeline => self.timeline_tab(&session, cx),
                    Tab::Transcript => self.transcript_tab(&session),
                    Tab::Answers => self.answers_tab(&session),
                    Tab::Screenshots => self.screenshots_tab(&session, cx),
                };
                content.child(div().flex_1().min_w_0().h_full().flex().flex_col().child(self.header(&session, cx)).child(body))
                    .child(self.rail(&session, cx))
            }
        };
        // A normal, opaque window with its own title bar: Sessions is a workspace, not an overlay.
        div().size_full().flex().flex_col().bg(rgb(0x0f1012)).font_family(theme::FONT).text_color(theme::text())
            .child(title_bar("Sessions", window))
            .child(content)
    }
}

/// Drag area with the app mark, the window's name and its own minimize / maximize / close. macOS
/// keeps its own traffic lights at the left and centres the name, as its windows do.
#[cfg(target_os = "macos")]
pub(crate) fn title_bar(title: &'static str, _window: &Window) -> impl IntoElement {
    div().id("title-bar").flex_none().h(px(36.0)).flex().items_center().justify_center().gap(px(8.0))
        .bg(rgb(0x131417)).border_b_1().border_color(theme::divider())
        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
        .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child(title))
}

/// Drag area with the app mark, the window's name and its own minimize / maximize / close.
#[cfg(not(target_os = "macos"))]
pub(crate) fn title_bar(title: &'static str, window: &Window) -> impl IntoElement {
    let control = |id: &'static str, icon: &'static str, danger: bool| {
        div().id(id).w(px(46.0)).h(px(36.0)).flex().items_center().justify_center().cursor_pointer()
            .hover(move |button| if danger { button.bg(rgb(0xc42b1c)).text_color(rgb(0xffffff)) } else { button.bg(theme::raised()) })
            .child(crate::ui::icon(icon, 10.0, theme::body()))
    };
    let zoom_icon = if window.is_maximized() { "icons/restore.svg" } else { "icons/maximize.svg" };
    let (drag, minimize, maximize, close) = (
        div().id("title-drag").flex_1().h_full().flex().items_center().gap(px(8.0)).pl(px(14.0)),
        control("minimize", "icons/minimize.svg", false),
        control("maximize", zoom_icon, false),
        control("close-window", "icons/window-close.svg", true),
    );
    // Windows has no window-move call: the drag area and buttons report themselves to the
    // system's hit test instead, which also gives double-click to maximize, Aero Snap, and Snap
    // Layouts on the maximize button. Click handlers would swallow those, so there are none.
    #[cfg(target_os = "windows")]
    let (drag, minimize, maximize, close) = (
        drag.window_control_area(gpui::WindowControlArea::Drag),
        minimize.window_control_area(gpui::WindowControlArea::Min),
        maximize.window_control_area(gpui::WindowControlArea::Max),
        close.window_control_area(gpui::WindowControlArea::Close),
    );
    div().id("title-bar").flex_none().h(px(36.0)).flex().items_center()
        .bg(rgb(0x131417)).border_b_1().border_color(theme::divider())
        .child(drag
            .child(crate::ui::mark(16.0))
            .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child(title)))
        .child(div().flex().h_full().child(minimize).child(maximize).child(close))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_times_read_as_recent_and_previews_cut_at_a_word() {
        assert_eq!((ago(100, 130), ago(100, 100 + 120), ago(100, 100 + 7_200)), ("just now".into(), "2 min ago".into(), "2 h ago".into()));
        assert_eq!(preview("Hi Priya,\n\nthanks for the time today.", 200), "Hi Priya, thanks for the time today.");
        assert_eq!(preview("one two three four", 9), "one two…");
    }
}
