//! Markdown answers rendered as GPUI elements for the dark glass panels.
//!
//! Two layers: `parse` turns markdown into a small block model (pure, unit-tested), and
//! `render` maps that model onto GPUI elements. Inline styling is expressed as text runs
//! over one string per paragraph so mixed bold/italic/code/link text wraps as a unit.

use std::cell::Cell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use gpui::{
    AnyElement, ClipboardItem, Div, ElementId, FontStyle, FontWeight, Hsla, InteractiveText, IntoElement, ParentElement,
    Rgba, SharedString, StrikethroughStyle, StyledText, TextRun, UnderlineStyle, div, prelude::*, px,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme;

/// Narrow no-break space placed around inline code so its background doesn't hug the glyphs.
const CODE_PAD: &str = "\u{202F}";

// ---------------------------------------------------------------------------------------------
// Block model
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    /// Index into `Inline::links`.
    pub link: Option<usize>,
}

/// A styled byte range of `Inline::text`. Runs are contiguous and cover the whole text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub range: Range<usize>,
    pub style: Style,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Inline {
    pub text: String,
    pub runs: Vec<Run>,
    pub links: Vec<String>,
}

impl Inline {
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        match self.runs.last_mut() {
            Some(last) if last.style == style && last.range.end == start => last.range.end = end,
            _ => self.runs.push(Run { range: start..end, style }),
        }
    }

    /// Drop trailing whitespace, clipping the runs that covered it.
    fn trim_end(&mut self) {
        let len = self.text.trim_end().len();
        self.text.truncate(len);
        self.runs.retain_mut(|run| {
            run.range.end = run.range.end.min(len);
            run.range.start < run.range.end
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Paragraph(Inline),
    Heading { level: u8, text: Inline },
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Code { language: String, code: String },
    Rule,
    Table { header: Vec<Inline>, rows: Vec<Vec<Inline>> },
}

enum Frame {
    Blocks(Vec<Block>),
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Item(Vec<Block>),
    Table { header: Vec<Inline>, rows: Vec<Vec<Inline>>, row: Vec<Inline> },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Paragraph,
    Heading(u8),
    Cell,
}

#[derive(Default)]
struct Builder {
    frames: Vec<Frame>,
    inline: Option<(Target, Inline)>,
    code: Option<(String, String)>,
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<usize>,
}

impl Builder {
    fn style(&self) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            code: false,
            strike: self.strike > 0,
            link: self.links.last().copied(),
        }
    }

    fn inline(&mut self) -> &mut Inline {
        &mut self.inline.get_or_insert_with(|| (Target::Paragraph, Inline::default())).1
    }

    fn text(&mut self, text: &str) {
        let style = self.style();
        self.inline().push(text, style);
    }

    fn push_block(&mut self, block: Block) {
        match self.frames.last_mut() {
            Some(Frame::Blocks(blocks) | Frame::Quote(blocks) | Frame::Item(blocks)) => blocks.push(block),
            Some(Frame::List { items, .. }) => match items.last_mut() {
                Some(item) => item.push(block),
                None => items.push(vec![block]),
            },
            // Tables only hold cells; stray blocks are dropped.
            Some(Frame::Table { .. }) | None => {}
        }
    }

    /// Finish the open inline run (paragraph, heading or cell) and place it.
    fn flush(&mut self) {
        let Some((target, mut inline)) = self.inline.take() else { return };
        inline.trim_end();
        match target {
            Target::Cell => {
                if let Some(Frame::Table { row, .. }) = self.frames.last_mut() {
                    row.push(inline);
                }
            }
            Target::Heading(level) => self.push_block(Block::Heading { level, text: inline }),
            Target::Paragraph if !inline.text.is_empty() => self.push_block(Block::Paragraph(inline)),
            Target::Paragraph => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.flush(),
            Tag::Heading { level, .. } => {
                self.flush();
                self.inline = Some((Target::Heading(heading_level(level)), Inline::default()));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.frames.push(Frame::Quote(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                self.frames.push(Frame::List { start, items: Vec::new() });
            }
            Tag::Item => {
                self.flush();
                self.frames.push(Frame::Item(Vec::new()));
            }
            Tag::Table(_) => {
                self.flush();
                self.frames.push(Frame::Table { header: Vec::new(), rows: Vec::new(), row: Vec::new() });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(Frame::Table { row, .. }) = self.frames.last_mut() {
                    row.clear();
                }
            }
            Tag::TableCell => self.inline = Some((Target::Cell, Inline::default())),
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                let inline = self.inline();
                inline.links.push(dest_url.to_string());
                let index = inline.links.len() - 1;
                self.links.push(index);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock | TagEnd::TableCell => self.flush(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                if let Some(Frame::Quote(blocks)) = self.frames.pop() {
                    self.push_block(Block::Quote(blocks));
                }
            }
            TagEnd::CodeBlock => {
                if let Some((language, mut code)) = self.code.take() {
                    if code.ends_with('\n') {
                        code.pop();
                    }
                    self.push_block(Block::Code { language, code });
                }
            }
            TagEnd::List(_) => {
                self.flush();
                if let Some(Frame::List { start, items }) = self.frames.pop() {
                    self.push_block(Block::List { start, items });
                }
            }
            TagEnd::Item => {
                self.flush();
                if let Some(Frame::Item(blocks)) = self.frames.pop()
                    && let Some(Frame::List { items, .. }) = self.frames.last_mut()
                {
                    items.push(blocks);
                }
            }
            TagEnd::TableHead => {
                if let Some(Frame::Table { header, row, .. }) = self.frames.last_mut() {
                    *header = std::mem::take(row);
                }
            }
            TagEnd::TableRow => {
                if let Some(Frame::Table { rows, row, .. }) = self.frames.last_mut() {
                    rows.push(std::mem::take(row));
                }
            }
            TagEnd::Table => {
                if let Some(Frame::Table { header, rows, .. }) = self.frames.pop() {
                    self.push_block(Block::Table { header, rows });
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => {
                self.links.pop();
            }
            _ => {}
        }
    }

    fn event(&mut self, event: Event) {
        if let Some((_, code)) = self.code.as_mut()
            && let Event::Text(text) = &event
        {
            code.push_str(text);
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Html(text) => self.text(&text),
            Event::Code(code) => {
                let style = Style { code: true, ..self.style() };
                let inline = self.inline();
                inline.push(CODE_PAD, style);
                inline.push(&code, style);
                inline.push(CODE_PAD, style);
            }
            Event::InlineHtml(html) => {
                let tag = html.trim().to_ascii_lowercase();
                if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") { self.text("\n") } else { self.text(&html) }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\n"),
            Event::Rule => {
                self.flush();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(done) => self.text(if done { "[x] " } else { "[ ] " }),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        // pulldown-cmark closes every container, but unwind defensively.
        while self.frames.len() > 1 {
            match self.frames.pop() {
                Some(Frame::Quote(blocks)) => self.push_block(Block::Quote(blocks)),
                Some(Frame::Item(blocks)) => {
                    if let Some(Frame::List { items, .. }) = self.frames.last_mut() {
                        items.push(blocks);
                    }
                }
                Some(Frame::List { start, items }) => self.push_block(Block::List { start, items }),
                Some(Frame::Table { header, rows, .. }) => self.push_block(Block::Table { header, rows }),
                Some(Frame::Blocks(_)) | None => {}
            }
        }
        match self.frames.pop() {
            Some(Frame::Blocks(blocks)) => blocks,
            _ => Vec::new(),
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Parse markdown into the block model. Tolerates partially streamed input: an unclosed
/// fence is a code block up to the end, and a dangling `**` or backtick on the last line is closed.
pub(crate) fn parse(markdown: &str) -> Vec<Block> {
    let source = close_dangling(markdown);
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder { frames: vec![Frame::Blocks(Vec::new())], ..Builder::default() };
    for event in Parser::new_ext(&source, options) {
        builder.event(event);
    }
    builder.finish()
}

fn is_fence(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("```") || line.starts_with("~~~")
}

/// Close an unfinished inline backtick or `**` on the final line so streaming text doesn't
/// flash raw markers. Skipped inside an open code fence (one linear pass over the text).
fn close_dangling(markdown: &str) -> std::borrow::Cow<'_, str> {
    let in_fence = markdown.lines().filter(|line| is_fence(line)).count() % 2 == 1;
    let last = markdown.trim_end().rsplit('\n').next().unwrap_or("");
    if in_fence || is_fence(last) {
        return markdown.into();
    }
    let mut closer = String::new();
    let ticks = last.matches('`').count();
    if ticks % 2 == 1 && !last.contains("``") {
        closer.push('`');
    }
    // Count `**` outside inline code spans.
    let outside: String = last.split('`').step_by(2).collect::<Vec<_>>().join(" ");
    if closer.is_empty() && outside.matches("**").count() % 2 == 1 {
        let after = &outside[outside.rfind("**").unwrap_or(0) + 2..];
        if !after.trim().is_empty() {
            closer.push_str("**");
        }
    }
    if closer.is_empty() {
        return markdown.into();
    }
    let body = markdown.trim_end();
    format!("{body}{closer}{}", &markdown[body.len()..]).into()
}

// ---------------------------------------------------------------------------------------------
// GPUI rendering
// ---------------------------------------------------------------------------------------------

struct Ctx {
    size: f32,
    seed: u64,
    next: Cell<usize>,
}

impl Ctx {
    /// Unique, stable-while-streaming ID: seeded from the answer's opening bytes, numbered in document order.
    fn id(&self, kind: &str) -> ElementId {
        let n = self.next.get();
        self.next.set(n + 1);
        ElementId::Name(SharedString::from(format!("md-{kind}-{:x}-{n}", self.seed)))
    }
}

#[derive(Clone, Copy)]
struct Base {
    color: Rgba,
    weight: FontWeight,
}

/// Render markdown into GPUI elements. `size` is the base text size in px (13–15).
pub fn render(markdown: &str, size: f32) -> Div {
    let mut hasher = DefaultHasher::new();
    markdown.as_bytes()[..markdown.len().min(96)].hash(&mut hasher);
    let ctx = Ctx { size, seed: hasher.finish(), next: Cell::new(0) };
    let blocks = parse(markdown);
    stack(&ctx, &blocks, size * 0.7, 0)
        .w_full()
        .font_family(theme::FONT)
        .text_size(px(size))
        .line_height(px((size * 1.5).round()))
        .text_color(theme::body())
}

fn stack(ctx: &Ctx, blocks: &[Block], gap: f32, depth: usize) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(px(gap))
        .children(blocks.iter().map(|block| render_block(ctx, block, depth)))
}

fn render_block(ctx: &Ctx, block: &Block, depth: usize) -> AnyElement {
    let size = ctx.size;
    let body = Base { color: theme::body(), weight: FontWeight::NORMAL };
    match block {
        Block::Paragraph(inline) => div().min_w_0().child(inline_element(ctx, inline, body)).into_any_element(),
        Block::Heading { level, text } => {
            let scale = match level {
                1 => 1.3,
                2 => 1.17,
                3 => 1.07,
                _ => 1.0,
            };
            let heading = Base { color: theme::text(), weight: FontWeight::SEMIBOLD };
            div()
                .min_w_0()
                .when(*level <= 3, |div| div.pt(px(size * 0.2)))
                .text_size(px(size * scale))
                .line_height(px((size * scale * 1.35).round()))
                .child(inline_element(ctx, text, heading))
                .into_any_element()
        }
        Block::List { start, items } => {
            let marker_width = if start.is_some() { size * 1.6 } else { size * 1.1 };
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap(px(size * 0.3))
                .children(items.iter().enumerate().map(|(index, item)| {
                    let marker = match start {
                        Some(first) => format!("{}.", first + index as u64),
                        None => ["•", "◦", "▪"][depth % 3].to_string(),
                    };
                    div()
                        .flex()
                        .min_w_0()
                        .gap(px(size * 0.35))
                        .child(div().flex_none().w(px(marker_width)).flex().justify_end().text_color(theme::muted()).child(marker))
                        .child(stack(ctx, item, size * 0.3, depth + 1).flex_1())
                }))
                .into_any_element()
        }
        Block::Quote(blocks) => stack(ctx, blocks, size * 0.5, depth)
            .border_l_2()
            .border_color(theme::accent())
            .pl(px(12.0))
            .text_color(theme::muted())
            .into_any_element(),
        Block::Code { language, code } => code_block(ctx, language, code),
        Block::Rule => div().h(px(1.0)).w_full().my(px(size * 0.2)).bg(theme::divider()).into_any_element(),
        Block::Table { header, rows } => table(ctx, header, rows),
    }
}

fn code_block(ctx: &Ctx, language: &str, code: &str) -> AnyElement {
    let label: SharedString = if language.is_empty() { "code".into() } else { language.to_string().into() };
    let copied = code.to_string();
    let copy = div()
        .id(ctx.id("copy"))
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(5.0))
        .cursor_pointer()
        .text_size(px(11.0))
        .text_color(theme::muted())
        .hover(|button| button.bg(theme::raised()).text_color(theme::text()))
        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone())))
        .child("Copy");
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .pl(px(12.0))
        .pr(px(6.0))
        .py(px(4.0))
        .border_b_1()
        .border_color(theme::divider())
        .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child(label))
        .child(copy);
    let lines = div()
        .flex_none()
        .font_family(theme::MONO)
        .text_size(px(12.0))
        .line_height(px(18.0))
        .whitespace_nowrap()
        .text_color(theme::text())
        .child(SharedString::from(code.to_string()));
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(8.0))
        .bg(theme::field())
        .border_1()
        .border_color(theme::hairline())
        .overflow_hidden()
        .child(header)
        .child(div().id(ctx.id("code")).flex().min_w_0().overflow_x_scroll().px(px(12.0)).py(px(10.0)).child(lines))
        .into_any_element()
}

fn table(ctx: &Ctx, header: &[Inline], rows: &[Vec<Inline>]) -> AnyElement {
    let columns = rows.iter().map(Vec::len).chain([header.len()]).max().unwrap_or(0);
    let row = |cells: &[Inline], base: Base, head: bool, first: bool| {
        div()
            .flex()
            .min_w_0()
            .when(head, |row| row.bg(theme::raised()))
            .when(!first, |row| row.border_t_1().border_color(theme::hairline()))
            .children((0..columns).map(|column| {
                let cell = div().flex_1().min_w_0().px(px(10.0)).py(px(5.0));
                match cells.get(column) {
                    Some(inline) => cell.child(inline_element(ctx, inline, base)),
                    None => cell,
                }
            }))
    };
    let head = Base { color: theme::text(), weight: FontWeight::SEMIBOLD };
    let body = Base { color: theme::body(), weight: FontWeight::NORMAL };
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(8.0))
        .border_1()
        .border_color(theme::hairline())
        .overflow_hidden()
        .when(!header.is_empty(), |table| table.child(row(header, head, true, true)))
        .children(rows.iter().enumerate().map(|(index, cells)| row(cells, body, false, header.is_empty() && index == 0)))
        .into_any_element()
}

fn is_web_url(url: &str) -> bool {
    let lower = url.trim_start().get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn text_run(run: &Run, links: &[String], base: Base) -> TextRun {
    let style = run.style;
    let mut font = gpui::font(if style.code { theme::MONO } else { theme::FONT });
    font.weight = if style.bold { FontWeight::BOLD } else { base.weight };
    if style.italic {
        font.style = FontStyle::Italic;
    }
    let link = style.link.and_then(|index| links.get(index)).is_some();
    let color: Hsla = if link {
        theme::accent_soft().into()
    } else if style.code {
        theme::text().into()
    } else {
        base.color.into()
    };
    TextRun {
        len: run.range.len(),
        font,
        color,
        background_color: style.code.then(|| theme::raised().into()),
        underline: link.then(|| UnderlineStyle { thickness: px(1.0), color: Some(color), wavy: false }),
        strikethrough: style.strike.then(|| StrikethroughStyle { thickness: px(1.0), color: Some(color) }),
    }
}

/// One wrapping text element for a paragraph, heading or cell; links become clickable ranges.
fn inline_element(ctx: &Ctx, inline: &Inline, base: Base) -> AnyElement {
    let runs = inline.runs.iter().map(|run| text_run(run, &inline.links, base)).collect();
    let text = StyledText::new(SharedString::from(inline.text.clone())).with_runs(runs);
    let (ranges, urls): (Vec<_>, Vec<_>) = inline
        .runs
        .iter()
        .filter_map(|run| {
            let url = inline.links.get(run.style.link?)?;
            is_web_url(url).then(|| (run.range.clone(), url.trim().to_string()))
        })
        .unzip();
    if ranges.is_empty() {
        return text.into_any_element();
    }
    InteractiveText::new(ctx.id("link"), text)
        .on_click(ranges, move |index, _, cx| {
            if let Some(url) = urls.get(index) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(block: &Block) -> &Inline {
        match block {
            Block::Paragraph(inline) => inline,
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    fn slice<'a>(inline: &'a Inline, run: &Run) -> &'a str {
        &inline.text[run.range.clone()]
    }

    #[test]
    fn headings_and_paragraphs() {
        let blocks = parse("# Title\n\nBody text\nwraps here.\n\n#### Small");
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], Block::Heading { level: 1, text } if text.text == "Title"));
        assert_eq!(paragraph(&blocks[1]).text, "Body text wraps here.");
        assert!(matches!(&blocks[2], Block::Heading { level: 4, text } if text.text == "Small"));
    }

    #[test]
    fn inline_styles_have_correct_ranges() {
        let blocks = parse("a **b** *c* ***d*** `e` ~~f~~");
        let inline = paragraph(&blocks[0]);
        let styled: Vec<(&str, Style)> = inline.runs.iter().map(|run| (slice(inline, run), run.style)).collect();
        let plain = Style::default();
        let code = format!("{CODE_PAD}e{CODE_PAD}");
        assert_eq!(styled, vec![
            ("a ", plain),
            ("b", Style { bold: true, ..plain }),
            (" ", plain),
            ("c", Style { italic: true, ..plain }),
            (" ", plain),
            ("d", Style { bold: true, italic: true, ..plain }),
            (" ", plain),
            (code.as_str(), Style { code: true, ..plain }),
            (" ", plain),
            ("f", Style { strike: true, ..plain }),
        ]);
        // Runs are contiguous and cover the whole text.
        assert_eq!(inline.runs.first().unwrap().range.start, 0);
        assert_eq!(inline.runs.last().unwrap().range.end, inline.text.len());
        assert!(inline.runs.windows(2).all(|pair| pair[0].range.end == pair[1].range.start));
    }

    #[test]
    fn multibyte_ranges_stay_on_char_boundaries() {
        let blocks = parse("héllo **wörld** ✓");
        let inline = paragraph(&blocks[0]);
        assert_eq!(slice(inline, &inline.runs[1]), "wörld");
        assert!(inline.runs.iter().all(|run| inline.text.is_char_boundary(run.range.start) && inline.text.is_char_boundary(run.range.end)));
    }

    #[test]
    fn links_record_targets() {
        let blocks = parse("See [docs](https://example.com) and [local](file:///x) now");
        let inline = paragraph(&blocks[0]);
        assert_eq!(inline.links, vec!["https://example.com", "file:///x"]);
        let linked: Vec<(&str, usize)> = inline.runs.iter().filter_map(|run| Some((slice(inline, run), run.style.link?))).collect();
        assert_eq!(linked, vec![("docs", 0), ("local", 1)]);
        assert!(is_web_url("https://example.com"));
        assert!(is_web_url("HTTP://EXAMPLE.COM"));
        assert!(!is_web_url("file:///x"));
        assert!(!is_web_url("javascript:alert(1)"));
    }

    #[test]
    fn nested_lists() {
        let blocks = parse("- one\n  - inner\n- two\n\n3. three\n4. four");
        assert_eq!(blocks.len(), 2);
        let Block::List { start: None, items } = &blocks[0] else { panic!("bullet list") };
        assert_eq!(items.len(), 2);
        assert_eq!(paragraph(&items[0][0]).text, "one");
        let Block::List { start: None, items: inner } = &items[0][1] else { panic!("nested list") };
        assert_eq!(paragraph(&inner[0][0]).text, "inner");
        assert_eq!(paragraph(&items[1][0]).text, "two");
        let Block::List { start: Some(3), items } = &blocks[1] else { panic!("ordered list") };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn loose_list_items_and_task_markers() {
        let blocks = parse("- [x] done\n\n- [ ] todo\n");
        let Block::List { items, .. } = &blocks[0] else { panic!("list") };
        assert_eq!(paragraph(&items[0][0]).text, "[x] done");
        assert_eq!(paragraph(&items[1][0]).text, "[ ] todo");
    }

    #[test]
    fn closed_code_fence() {
        let blocks = parse("Intro\n\n```rust title\nfn main() {}\n```\nAfter");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1], Block::Code { language: "rust".into(), code: "fn main() {}".into() });
        assert_eq!(paragraph(&blocks[2]).text, "After");
    }

    #[test]
    fn unclosed_code_fence_is_code_so_far() {
        let blocks = parse("Here:\n```py\nprint(1)\nx = **2");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1], Block::Code { language: "py".into(), code: "print(1)\nx = **2".into() });
    }

    #[test]
    fn indented_code_block() {
        let blocks = parse("text\n\n    let x = 1;\n    let y = 2;\n");
        assert_eq!(blocks[1], Block::Code { language: String::new(), code: "let x = 1;\nlet y = 2;".into() });
    }

    #[test]
    fn dangling_markers_close_while_streaming() {
        let blocks = parse("This is **important");
        let inline = paragraph(&blocks[0]);
        assert_eq!(inline.text, "This is important");
        assert!(inline.runs[1].style.bold);

        let blocks = parse("Run `cargo te");
        let inline = paragraph(&blocks[0]);
        assert!(inline.runs[1].style.code);

        // A lone opener with nothing after it is left alone rather than becoming a rule.
        assert_eq!(close_dangling("**"), "**");
        assert_eq!(close_dangling("a **b** c"), "a **b** c");
        assert_eq!(close_dangling("**bold \n"), "**bold** \n");
    }

    #[test]
    fn quotes_rules_and_breaks() {
        let blocks = parse("> quoted\n> line\n\n---\n\nhard  \nbreak");
        let Block::Quote(inner) = &blocks[0] else { panic!("quote") };
        assert_eq!(paragraph(&inner[0]).text, "quoted line");
        assert_eq!(blocks[1], Block::Rule);
        assert_eq!(paragraph(&blocks[2]).text, "hard\nbreak");
    }

    #[test]
    fn tables() {
        let blocks = parse("| A | **B** |\n|---|---|\n| 1 | 2 |\n| 3 |");
        let Block::Table { header, rows } = &blocks[0] else { panic!("table, got {blocks:?}") };
        assert_eq!(header.iter().map(|cell| cell.text.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert!(header[1].runs[0].style.bold);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1].text, "2");
        assert_eq!(rows[1][0].text, "3");
    }

    #[test]
    fn empty_input() {
        assert!(parse("").is_empty());
        assert!(parse("   \n\n").is_empty());
    }
}
