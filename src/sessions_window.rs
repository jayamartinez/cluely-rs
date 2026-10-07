//! Sessions: a normal, resizable window for reviewing saved Live sessions. Unlike the
//! overlay it appears in the taskbar and in screen capture; it's for reading, not live use.
//! Layout follows the Paper "Sessions" page: list · summary/timeline · questions rail.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Datelike, Local, TimeZone};
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Div, Entity, Focusable, FontWeight, IntoElement, MouseButton, ObjectFit,
    ParentElement, PromptLevel, Render, ScrollHandle, SharedString, Stateful, Styled, StyledImage, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, div, img, prelude::*, px, rgb, size,
};

use crate::archive::{Archive, Line, Session, Speaker, Summary, SummaryLength, Turn, clock};
use crate::codex::CodexClient;
use crate::input::{InputEvent, TextInput};
use crate::settings::Store;
use crate::theme;
use crate::ui;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab { Summary, Timeline, Transcript, Answers, Screenshots }

pub struct SessionsWindow {
    archive: Archive,
    listings: Vec<Summary>,
    session: Option<Session>,
    tab: Tab,
    length: SummaryLength,
    timeline: ScrollHandle,
    notice: Option<SharedString>,
    codex: Arc<CodexClient>,
    search: Entity<TextInput>,
    ask_input: Entity<TextInput>,
    /// Lower-cased searchable text per session id, rebuilt on reload.
    index: Vec<(String, String)>,
    query: String,
    /// Questions asked about the open session and their answers (None while waiting).
    asks: Vec<(String, Option<Result<String, String>>)>,
    /// What the model is currently writing for this session, if anything.
    working: Option<&'static str>,
}

/// Open the window, or bring the existing one forward.
pub fn open(existing: &mut Option<WindowHandle<SessionsWindow>>, root: PathBuf, codex: Arc<CodexClient>, cx: &mut App) {
    if let Some(handle) = existing && handle.update(cx, |this, window, cx| { this.reload(cx); window.activate_window(); }).is_ok() {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.0), px(800.0)), cx))),
        // The frame is drawn by the window itself (title bar with its own controls).
        titlebar: Some(TitlebarOptions { title: Some("Sessions".into()), appears_transparent: true, traffic_light_position: None }),
        kind: WindowKind::Normal,
        focus: true,
        show: true,
        is_resizable: true,
        is_minimizable: true,
        window_min_size: Some(size(px(900.0), px(560.0))),
        ..Default::default()
    };
    *existing = cx.open_window(options, |_, cx| cx.new(|cx| SessionsWindow::new(Archive::at(root), codex, cx))).ok();
}

impl SessionsWindow {
    fn new(archive: Archive, codex: Arc<CodexClient>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search transcripts and answers", cx));
        let ask_input = cx.new(|cx| TextInput::new("What did they ask about caching?", cx));
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
        let mut window = Self { archive, listings, session, tab: Tab::Summary, length: SummaryLength::Standard, timeline: ScrollHandle::new(),
            notice: None, codex, search, ask_input, index: Vec::new(), query: String::new(), asks: Vec::new(), working: None };
        window.rebuild_index();
        window
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
        self.session = selected.and_then(|id| self.archive.load(&id));
        cx.notify();
    }

    fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.session = self.archive.load(id);
        self.tab = Tab::Summary;
        self.notice = None;
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

    fn generate_notes(&mut self, cx: &mut Context<Self>) {
        self.with_model("Writing notes…", cx, |settings, codex, session| crate::notes::generate(&settings, &codex, &session),
            |this, id, result, cx| match result {
                Ok((title, notes)) => match this.archive.update(id, |s| { s.title = Some(title); s.notes = Some(notes); }) {
                    Ok(_) => { this.length = SummaryLength::Standard; this.reload(cx); }
                    Err(_) => this.notice = Some("The notes couldn't be saved.".into()),
                },
                Err(error) => this.notice = Some(error.into()),
            });
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
                let _ = std::process::Command::new("explorer.exe").arg("/select,").arg(&path).spawn();
                "Saved session.md next to the session".into()
            }
            Err(_) => "Export failed. Check the sessions folder permissions.".into(),
        });
        cx.notify();
    }

    fn reveal(&self) {
        if let Some(session) = &self.session {
            let _ = std::process::Command::new("explorer.exe").arg(self.archive.root().join(&session.id)).spawn();
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

fn plural(count: usize, word: &str) -> String { format!("{count} {word}{}", if count == 1 { "" } else { "s" }) }

fn label(text: &'static str) -> Div {
    div().text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(text)
}

fn outline_button(id: &'static str, text: &'static str) -> Stateful<Div> {
    div().id(id).cursor_pointer().px(px(12.0)).py(px(7.0)).rounded(px(9.0)).border_1().border_color(theme::hairline())
        .text_size(px(13.0)).text_color(theme::body()).hover(|b| b.border_color(theme::bubble_border())).child(text)
}

fn speaker(speaker: Speaker) -> (&'static str, gpui::Rgba) {
    match speaker { Speaker::You => ("You", theme::accent_soft()), Speaker::Them => ("Them", theme::ok()) }
}

impl SessionsWindow {
    fn screenshot(&self, session: &Session, file: &str) -> Option<PathBuf> { self.archive.screenshot_path(&session.id, file) }

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
            list = list.child(div().p(px(12.0)).text_size(px(13.0)).text_color(theme::muted()).child("No saved sessions yet. Press Start in the overlay to record one."));
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
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Saved on this PC")))
    }

    fn header(&self, session: &Session, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
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
        if let Some(model) = &session.model { meta.push(model.clone()); }
        let title = session.title.clone().unwrap_or_else(|| "Untitled session".into());
        let shots = session.turns.iter().filter(|t| t.screenshot.is_some()).count();
        let tabs = [(Tab::Summary, "Summary".to_string()), (Tab::Timeline, "Timeline".to_string()), (Tab::Transcript, "Transcript".to_string()),
            (Tab::Answers, format!("Answers  {}", session.turns.len())), (Tab::Screenshots, format!("Screenshots  {shots}"))];
        let mut tab_row = div().flex().gap(px(22.0)).border_b_1().border_color(theme::divider());
        for (index, (tab, text)) in tabs.into_iter().enumerate() {
            let selected = tab == self.tab;
            tab_row = tab_row.child(div().id(("tab", index)).cursor_pointer().pb(px(10.0)).text_size(px(13.0))
                .when(selected, |t| t.font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).border_b_2().border_color(theme::accent()))
                .when(!selected, |t| t.text_color(theme::muted()).hover(|t| t.text_color(theme::body())))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| { this.tab = tab; cx.notify(); }))
                .child(text));
        }
        div().flex().flex_col().gap(px(14.0)).px(px(32.0)).pt(px(26.0))
            .child(div().flex().justify_between().items_start().gap(px(24.0))
                .child(div().flex().flex_col().gap(px(6.0)).min_w_0()
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child(meta.into_iter().filter(|m| !m.is_empty()).collect::<Vec<_>>().join(" · ")))
                    .child(div().text_size(px(30.0)).line_height(px(36.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title)))
                .child(div().flex().gap(px(8.0)).pt(px(4.0)).flex_none()
                    .child(outline_button("export", "Export").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.export(cx))))
                    .child(outline_button("folder", "Open folder").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.reveal())))
                    .child(outline_button("delete", "Delete").text_color(rgb(0xffb4a8))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.confirm_delete(window, cx))))))
            .when_some(self.notice.clone(), |header, notice| header.child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice)))
            .child(tab_row)
    }

    fn summary_tab(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let notes = session.notes.as_ref();
        let mut lengths = ui::segmented();
        for (index, length) in SummaryLength::ALL.into_iter().enumerate() {
            lengths = lengths.child(ui::segment(("length", index), length.label(), length == self.length)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| { this.length = length; cx.notify(); })));
        }
        let busy = self.working.is_some();
        let expand = div().id("expand").flex().items_center().gap(px(6.0)).px(px(11.0)).py(px(6.0)).rounded(px(8.0)).bg(theme::bubble())
            .border_1().border_color(theme::bubble_border()).cursor_pointer().when(busy, |b| b.opacity(0.55))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { let length = this.length; this.write_overview(length, true, cx); }))
            .child(div().text_size(px(12.0)).text_color(theme::accent_soft()).child("✦"))
            .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Expand with AI"));
        let action = |id: &'static str, label: String| div().id(id).cursor_pointer().px(px(12.0)).py(px(7.0)).rounded(px(9.0)).bg(theme::accent())
            .text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink()).child(label);
        let overview: AnyElement = match (self.working, notes.and_then(|n| n.overviews.get(&self.length))) {
            (Some(label), _) => div().text_size(px(14.0)).text_color(theme::muted()).child(label).into_any_element(),
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
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.generate_notes(cx))))
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
        div().id("summary").flex().flex_col().gap(px(24.0)).px(px(32.0)).py(px(22.0)).flex_1().overflow_y_scroll()
            .child(div().flex().items_center().justify_between()
                .child(label("OVERVIEW"))
                .child(div().flex().items_center().gap(px(10.0))
                    .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Length"))
                    .child(lengths).child(expand)))
            .child(overview).child(stats).child(covered)
            .into_any_element()
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
        let mut follow = div().flex().flex_col().gap(px(10.0)).child(label("FOLLOW UP"));
        let items = session.notes.as_ref().map(|n| n.follow_ups.as_slice()).unwrap_or(&[]);
        for item in items {
            follow = follow.child(div().flex().gap(px(10.0)).items_start()
                .child(div().size(px(14.0)).mt(px(2.0)).flex_none().rounded(px(4.0)).border(px(1.5)).border_color(rgb(0x4a4d52)))
                .child(div().flex_1().min_w_0().text_size(px(13.0)).line_height(px(19.0)).text_color(theme::body()).child(item.clone())));
        }
        if items.is_empty() { follow = follow.child(div().text_size(px(13.0)).text_color(theme::muted()).child("Written with the summary.")); }
        div().id("rail").w(px(300.0)).flex_none().h_full().flex().flex_col().gap(px(22.0)).px(px(22.0)).py(px(26.0)).overflow_y_scroll()
            .bg(rgb(0x131417)).border_l_1().border_color(theme::divider())
            .child(key).child(follow)
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
                        Some(Err(error)) => div().text_size(px(13.0)).text_color(rgb(0xffb4a8)).child(error.clone()).into_any_element(),
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
                    Tab::Summary => self.summary_tab(&session, cx),
                    Tab::Timeline => self.timeline_tab(&session, cx),
                    Tab::Transcript => self.transcript_tab(&session),
                    Tab::Answers => self.answers_tab(&session),
                    Tab::Screenshots => self.screenshots_tab(&session, cx),
                };
                content.child(div().flex_1().min_w_0().h_full().flex().flex_col().child(self.header(&session, window, cx)).child(body))
                    .child(self.rail(&session, cx))
            }
        };
        // A normal, opaque window with its own title bar: Sessions is a workspace, not an overlay.
        div().size_full().flex().flex_col().bg(rgb(0x0f1012)).font_family(theme::FONT).text_color(theme::text())
            .child(title_bar())
            .child(content)
    }
}

/// Drag area with the app mark, the window's name and its own minimize / maximize / close.
fn title_bar() -> impl IntoElement {
    let control = |id: &'static str, icon: &'static str, danger: bool| {
        div().id(id).w(px(46.0)).h(px(36.0)).flex().items_center().justify_center().cursor_pointer()
            .hover(move |button| if danger { button.bg(rgb(0xc42b1c)).text_color(rgb(0xffffff)) } else { button.bg(theme::raised()) })
            .child(crate::ui::icon(icon, 10.0, theme::body()))
    };
    div().id("title-bar").flex_none().h(px(36.0)).flex().items_center().justify_between().pl(px(14.0))
        .bg(rgb(0x131417)).border_b_1().border_color(theme::divider())
        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
        .child(div().flex().items_center().gap(px(8.0))
            .child(crate::ui::mark(16.0))
            .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child("Sessions")))
        .child(div().flex().h_full()
            .child(control("minimize", "icons/minimize.svg", false).on_mouse_down(MouseButton::Left, |_, window, cx| { cx.stop_propagation(); window.minimize_window(); }))
            .child(control("maximize", "icons/maximize.svg", false).on_mouse_down(MouseButton::Left, |_, window, cx| { cx.stop_propagation(); window.zoom_window(); }))
            .child(control("close-window", "icons/window-close.svg", true).on_mouse_down(MouseButton::Left, |_, window, cx| { cx.stop_propagation(); window.remove_window(); })))
}
