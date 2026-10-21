//! Local full-text search over any set of documents. Each document's text is split into
//! overlapping chunks that follow its headings and paragraphs, and chunks are ranked against a
//! query with BM25. Everything runs on this PC: no network, no model to download, and a query
//! over a few megabytes of text takes a few milliseconds.
//!
//! A document's [`Index`] owns its text and is saved beside it ([`Index::open`]); the saved
//! index names the SHA-256 of the text it was built from, so a changed text is indexed again.
//! [`search`] ranks the chunks of several indexes together (term statistics are summed over all
//! of them) and [`fit`] turns the best hits into passages that fit a byte budget.
//! [`Registry`] builds indexes on a background thread and reports progress.
//!
//! Nothing here knows what the documents are: the files a mode holds use it (`modes`), and any
//! other set of texts can be indexed the same way.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use sha2::{Digest, Sha256};

/// Chunks aim for this many bytes (about 300 tokens): big enough to hold an answer with its
/// surroundings, small enough that a few of them fit a small model's budget.
const TARGET: usize = 1_200;
/// A paragraph longer than this is split into pieces.
const MAX: usize = 2_000;
/// The last paragraph of a chunk, when at most this long, also starts the next one, so text
/// near a boundary is found with what comes before it.
const OVERLAP: usize = 300;
/// Words longer than this are cut (they're hashes, URLs or base64, not words anyone says).
const MAX_TERM_BYTES: usize = 40;
/// At most this many distinct query terms are scored, the most heavily weighted first.
const MAX_QUERY_TERMS: usize = 128;
/// A query term's weight, summed over the parts it appears in, is capped here, so one word
/// repeated in a long transcript can't drown out the rest.
const MAX_TERM_WEIGHT: f32 = 6.0;
const K1: f32 = 1.2;
const B: f32 = 0.75;
/// Texts up to this size are indexed on the calling thread rather than in the background.
const INLINE_BYTES: u64 = 64 * 1024;
/// The saved index format. A different one is built again rather than read.
const MAGIC: &[u8; 8] = b"CRSIDX01";

/// One chunk: a byte range of the text, how many terms it holds, and the heading it is under.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Chunk {
    start: u32,
    end: u32,
    terms: u32,
    /// Index into `headings`, or `NO_HEADING`.
    heading: u32,
}

const NO_HEADING: u32 = u32::MAX;

/// One document's text and its search index.
pub struct Index {
    text: String,
    digest: [u8; 32],
    chunks: Vec<Chunk>,
    /// Byte ranges of the headings found in the text.
    headings: Vec<(u32, u32)>,
    /// For each term, the chunks it appears in and how often.
    postings: HashMap<Box<str>, Vec<(u32, u32)>>,
    /// Terms in all chunks together.
    total: u64,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index").field("bytes", &self.text.len()).field("chunks", &self.chunks.len()).field("terms", &self.postings.len()).finish()
    }
}

impl Index {
    /// Index `text` (texts over 4 GB aren't supported). `progress` gets 0..=1 as chunks are
    /// indexed. Blocks: a few hundred milliseconds for 10 MB in a release build.
    pub fn build(text: String, progress: &mut dyn FnMut(f32)) -> Self {
        let digest = digest(&text);
        Self::build_with(text, digest, progress)
    }

    fn build_with(text: String, digest: [u8; 32], progress: &mut dyn FnMut(f32)) -> Self {
        let (ranges, headings) = chunk(&text);
        let mut postings: HashMap<Box<str>, Vec<(u32, u32)>> = HashMap::new();
        let mut chunks = Vec::with_capacity(ranges.len());
        let mut counts: HashMap<String, u32> = HashMap::new();
        let mut total = 0u64;
        let step = (ranges.len() / 100).max(1);
        for (number, &(start, end, heading)) in ranges.iter().enumerate() {
            counts.clear();
            let mut count = |term: &str| match counts.get_mut(term) {
                Some(tf) => *tf += 1,
                None => { counts.insert(term.to_string(), 1); }
            };
            for_each_term(&text[start..end], &mut count);
            // A section's later chunks are found by its heading too.
            if let Some(&(heading_start, heading_end)) = heading.and_then(|index| headings.get(index))
                && heading_start != start { for_each_term(&text[heading_start..heading_end], &mut count); }
            let terms: u32 = counts.values().sum();
            total += u64::from(terms);
            for (term, &tf) in &counts {
                match postings.get_mut(term.as_str()) {
                    Some(list) => list.push((number as u32, tf)),
                    None => { postings.insert(term.as_str().into(), vec![(number as u32, tf)]); }
                }
            }
            chunks.push(Chunk { start: start as u32, end: end as u32, terms, heading: heading.map_or(NO_HEADING, |index| index as u32) });
            if number % step == 0 { progress(number as f32 / ranges.len() as f32); }
        }
        progress(1.0);
        let headings = headings.into_iter().map(|(start, end)| (start as u32, end as u32)).collect();
        Self { text, digest, chunks, headings, postings, total }
    }

    /// The index of the text at `text_path`: read from `index_path` when it was built from this
    /// exact text, otherwise built (`progress` as for [`Index::build`]) and saved there. A save
    /// that fails only costs a rebuild next time. Blocks; call it off the UI thread.
    pub fn open(text_path: &Path, index_path: &Path, progress: &mut dyn FnMut(f32)) -> io::Result<Self> {
        let text = fs::read_to_string(text_path)?;
        let digest = digest(&text);
        let text = match fs::read(index_path) {
            Ok(bytes) => match Self::decode(&bytes, text, digest) {
                Ok(index) => { progress(1.0); return Ok(index); }
                Err(text) => text,
            },
            Err(_) => text,
        };
        let index = Self::build_with(text, digest, progress);
        // Not when the text went away meanwhile (its file or folder was deleted).
        if text_path.exists() && let Err(error) = index.save(index_path) { eprintln!("search index not saved: {error}"); }
        Ok(index)
    }

    /// Write the index (not the text) to `path`, atomically.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut out = Vec::with_capacity(64 + self.chunks.len() * 16 + self.postings.len() * 24);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.digest);
        let put = |out: &mut Vec<u8>, value: u32| out.extend_from_slice(&value.to_le_bytes());
        put(&mut out, self.text.len() as u32);
        put(&mut out, self.chunks.len() as u32);
        for chunk in &self.chunks { for value in [chunk.start, chunk.end, chunk.terms, chunk.heading] { put(&mut out, value); } }
        put(&mut out, self.headings.len() as u32);
        for &(start, end) in &self.headings { put(&mut out, start); put(&mut out, end); }
        out.extend_from_slice(&self.total.to_le_bytes());
        put(&mut out, self.postings.len() as u32);
        for (term, list) in &self.postings {
            put(&mut out, term.len() as u32);
            out.extend_from_slice(term.as_bytes());
            put(&mut out, list.len() as u32);
            for &(chunk, tf) in list { put(&mut out, chunk); put(&mut out, tf); }
        }
        write_atomic(path, &out)
    }

    /// A saved index, if it is in this format, was built from text with `digest`, and holds
    /// together (ranges inside the text, on character boundaries; chunks that exist). Otherwise
    /// the text is handed back.
    fn decode(bytes: &[u8], text: String, digest: [u8; 32]) -> Result<Self, String> {
        match Self::parse(bytes, &text, digest) {
            Some((chunks, headings, postings, total)) => Ok(Self { text, digest, chunks, headings, postings, total }),
            None => Err(text),
        }
    }

    #[allow(clippy::type_complexity)]
    fn parse(bytes: &[u8], text: &str, digest: [u8; 32]) -> Option<(Vec<Chunk>, Vec<(u32, u32)>, HashMap<Box<str>, Vec<(u32, u32)>>, u64)> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(8)? != MAGIC || reader.take(32)? != digest || reader.u32()? as usize != text.len() { return None; }
        let in_text = |start: u32, end: u32| start <= end && text.is_char_boundary(start as usize) && text.is_char_boundary(end as usize) && end as usize <= text.len();
        let count = reader.u32()? as usize;
        let mut chunks = Vec::with_capacity(count.min(bytes.len() / 16));
        for _ in 0..count {
            let chunk = Chunk { start: reader.u32()?, end: reader.u32()?, terms: reader.u32()?, heading: reader.u32()? };
            if !in_text(chunk.start, chunk.end) { return None; }
            chunks.push(chunk);
        }
        let count = reader.u32()? as usize;
        let mut headings = Vec::with_capacity(count.min(bytes.len() / 8));
        for _ in 0..count {
            let (start, end) = (reader.u32()?, reader.u32()?);
            if !in_text(start, end) { return None; }
            headings.push((start, end));
        }
        if chunks.iter().any(|chunk| chunk.heading != NO_HEADING && chunk.heading as usize >= headings.len()) { return None; }
        let total = u64::from_le_bytes(reader.take(8)?.try_into().ok()?);
        let count = reader.u32()? as usize;
        let mut postings = HashMap::with_capacity(count.min(bytes.len() / 12));
        for _ in 0..count {
            let length = reader.u32()? as usize;
            let term: Box<str> = std::str::from_utf8(reader.take(length)?).ok()?.into();
            let entries = reader.u32()? as usize;
            let mut list = Vec::with_capacity(entries.min(bytes.len() / 8));
            for _ in 0..entries {
                let (chunk, tf) = (reader.u32()?, reader.u32()?);
                if chunk as usize >= chunks.len() { return None; }
                list.push((chunk, tf));
            }
            postings.insert(term, list);
        }
        (reader.at == bytes.len()).then_some((chunks, headings, postings, total))
    }

    pub fn text(&self) -> &str { &self.text }

    /// How many chunks the text was split into.
    pub fn chunks(&self) -> usize { self.chunks.len() }

    /// The text of chunk `number`.
    pub fn chunk(&self, number: usize) -> &str {
        let chunk = self.chunks[number];
        &self.text[chunk.start as usize..chunk.end as usize]
    }

    /// The heading chunk `number` is under, if the text has one before it.
    pub fn heading(&self, number: usize) -> Option<&str> {
        let &(start, end) = self.headings.get(self.chunks.get(number)?.heading as usize)?;
        Some(self.text[start as usize..end as usize].trim_start_matches('#').trim())
    }
}

struct Reader<'a> { bytes: &'a [u8], at: usize }

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(count)?)?;
        self.at += count;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> { Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?)) }
}

fn digest(text: &str) -> [u8; 32] { Sha256::digest(text.as_bytes()).into() }

/// Write to a temporary file, then rename over the old one so a crash never leaves half a file.
/// The folder must exist: an index is saved beside its text.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("idx-tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

/// A paragraph, or a heading on its own line.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Block { start: usize, end: usize, heading: bool }

/// The text's paragraphs (runs of non-blank lines) and headings, as byte ranges without their
/// trailing line breaks. A heading is a Markdown `#` line, or a short line standing on its own
/// that reads like a title (no closing punctuation, starts with a capital or a digit).
fn blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Option<(usize, usize, usize)> = None;
    let mut at = 0;
    let finish = |paragraph: &mut Option<(usize, usize, usize)>, blocks: &mut Vec<Block>| {
        if let Some((start, end, lines)) = paragraph.take() {
            blocks.push(Block { start, end, heading: lines == 1 && reads_like_title(&text[start..end]) });
        }
    };
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let content = line.trim_end();
        if content.trim().is_empty() { finish(&mut paragraph, &mut blocks); continue; }
        let end = start + content.len();
        if is_markdown_heading(content) {
            finish(&mut paragraph, &mut blocks);
            blocks.push(Block { start, end, heading: true });
            continue;
        }
        paragraph = Some(match paragraph { Some((first, _, lines)) => (first, end, lines + 1), None => (start, end, 1) });
    }
    finish(&mut paragraph, &mut blocks);
    blocks
}

fn is_markdown_heading(line: &str) -> bool {
    let line = line.trim_start();
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') && !line[hashes..].trim().is_empty()
}

fn reads_like_title(line: &str) -> bool {
    let line = line.trim();
    line.len() <= 80 && line.split_whitespace().count() <= 12 && !line.ends_with(['.', ',', ';', ':', '!', '?'])
        && line.chars().next().is_some_and(|c| c.is_uppercase() || c.is_ascii_digit()) && line.chars().any(char::is_alphabetic)
}

/// A chunk's byte range and the heading it is under, by index.
type Range = (usize, usize, Option<usize>);

/// Split `text` into chunks: (start, end, heading index) byte ranges, and the headings' ranges.
/// A heading starts a new chunk; paragraphs are packed up to [`TARGET`] bytes, a short last
/// paragraph is repeated at the start of the next chunk, and paragraphs over [`MAX`] are cut
/// into overlapping pieces at sentence ends (or spaces).
fn chunk(text: &str) -> (Vec<Range>, Vec<(usize, usize)>) {
    let mut chunks = Vec::new();
    let mut headings = Vec::new();
    let mut heading: Option<usize> = None;
    // The chunk being filled, and the last paragraph in it (when it isn't its only one).
    let mut current: Option<(usize, usize)> = None;
    let mut last: Option<(usize, usize)> = None;
    for block in blocks(text) {
        let (start, end) = (block.start, block.end);
        if block.heading {
            if let Some((from, to)) = current.take() { chunks.push((from, to, heading)); }
            headings.push((start, end));
            heading = Some(headings.len() - 1);
            current = Some((start, end));
            last = None;
            continue;
        }
        if end - start > MAX {
            if let Some((from, to)) = current.take() { chunks.push((from, to, heading)); }
            chunks.extend(pieces(text, start, end).into_iter().map(|(from, to)| (from, to, heading)));
            last = None;
            continue;
        }
        current = Some(match current {
            None => { last = None; (start, end) }
            // Room for it, or the chunk so far is too small to stand alone.
            Some((from, to)) if end - from <= TARGET || (to - from < TARGET / 4 && end - from <= MAX) => { last = Some((start, end)); (from, end) }
            Some((from, to)) => {
                chunks.push((from, to, heading));
                let carried = last.filter(|&(first, previous_end)| previous_end - first <= OVERLAP && end - first <= MAX);
                last = Some((start, end));
                match carried { Some((first, _)) => (first, end), None => { last = None; (start, end) } }
            }
        });
    }
    if let Some((from, to)) = current { chunks.push((from, to, heading)); }
    (chunks, headings)
}

/// `text[start..end]` in pieces of about [`TARGET`] bytes that overlap by up to half of
/// [`OVERLAP`], ending at a sentence end or a space where there is one.
fn pieces(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut pieces = Vec::new();
    let mut at = start;
    while end - at > TARGET + TARGET / 2 {
        let (from, limit) = (floor(text, at + TARGET / 2), floor(text, at + TARGET));
        let window = &text[from..limit];
        let cut = window.rfind(". ").or_else(|| window.rfind("? ")).or_else(|| window.rfind("! ")).map(|i| from + i + 1)
            .or_else(|| window.rfind(char::is_whitespace).map(|i| from + i))
            .unwrap_or(limit);
        pieces.push((at, cut));
        let back = floor(text, cut.saturating_sub(OVERLAP / 2).max(at + 1));
        let next = text[back..cut].find(char::is_whitespace).map_or(cut, |i| back + i);
        at = next + text[next..end].len() - text[next..end].trim_start().len();
    }
    pieces.push((at, end));
    pieces
}

/// The nearest character boundary at or before `at`.
fn floor(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) { at -= 1; }
    at
}

/// Words that say little about what a passage is about, sorted for binary search. Includes
/// the fillers transcripts are full of.
const STOPWORDS: [&str; 125] = [
    "about", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be", "because", "been", "before", "being",
    "between", "both", "but", "by", "can", "could", "did", "do", "does", "doing", "don", "down", "each", "few", "for", "from", "further",
    "gonna", "had", "has", "have", "having", "he", "her", "here", "hers", "him", "his", "how", "if", "in", "into", "is", "it", "its",
    "just", "kind", "know", "let", "like", "me", "mean", "more", "most", "my", "no", "nor", "not", "now", "of", "off", "ok", "okay",
    "on", "once", "only", "or", "other", "our", "out", "over", "own", "really", "same", "she", "should", "so", "some", "such", "than",
    "that", "the", "their", "them", "then", "there", "these", "they", "thing", "think", "this", "those", "through", "to", "too", "uh",
    "um", "under", "until", "up", "very", "was", "we", "well", "were", "what", "when", "where", "which", "while", "who", "why", "will",
    "with", "would", "yeah", "you", "your",
];

/// Call `f` with each searchable term of `text`: lowercased letter-and-digit runs, without
/// stopwords or single letters, with plural and common verb endings taken off. Queries and
/// documents go through the same steps, so "policies" finds "policy".
fn for_each_term(text: &str, f: &mut dyn FnMut(&str)) {
    let mut word = String::with_capacity(MAX_TERM_BYTES + 4);
    let mut emit = |word: &mut String| {
        if !word.is_empty() {
            let single = word.chars().nth(1).is_none();
            if !(single && !word.as_bytes()[0].is_ascii_digit()) && STOPWORDS.binary_search(&word.as_str()).is_err() { f(stem(word)); }
            word.clear();
        }
    };
    for c in text.chars() {
        if c.is_alphanumeric() {
            if word.len() < MAX_TERM_BYTES { word.extend(c.to_lowercase()); }
        } else {
            emit(&mut word);
        }
    }
    emit(&mut word);
}

/// A light English stemmer: enough to match plurals and -ing/-ed forms. Only ASCII endings
/// are touched.
fn stem(word: &str) -> &str {
    let n = word.len();
    if n > 4 && word.ends_with("ies") { return &word[..n - 3]; }
    if n > 3 && word.ends_with('y') { return &word[..n - 1]; }
    if n > 5 && word.ends_with("ing") { return &word[..n - 3]; }
    if n > 4 && word.ends_with("ed") { return &word[..n - 2]; }
    if n > 3 && word.ends_with('s') && !word.ends_with("ss") && !word.ends_with("us") && !word.ends_with("is") { return &word[..n - 1]; }
    word
}

/// A ranked chunk: `doc` is its position in the indexes searched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit { pub doc: usize, pub chunk: usize, pub score: f32 }

/// The `limit` chunks of `docs` that best match `query`, best first. The query is weighted
/// parts (a question counts more than the talk around it); a chunk scores by BM25 over the
/// query's terms, with term statistics taken over all of `docs` together.
pub fn search(docs: &[&Index], query: &[(&str, f32)], limit: usize) -> Vec<Hit> {
    let mut weights: HashMap<String, f32> = HashMap::new();
    for &(text, weight) in query {
        for_each_term(text, &mut |term: &str| match weights.get_mut(term) {
            Some(total) => *total += weight,
            None => { weights.insert(term.to_string(), weight); }
        });
    }
    let mut terms: Vec<(String, f32)> = weights.into_iter().map(|(term, weight)| (term, weight.min(MAX_TERM_WEIGHT))).collect();
    if terms.len() > MAX_QUERY_TERMS {
        terms.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        terms.truncate(MAX_QUERY_TERMS);
    }
    let count: usize = docs.iter().map(|doc| doc.chunks.len()).sum();
    if count == 0 || terms.is_empty() || limit == 0 { return Vec::new(); }
    let average = (docs.iter().map(|doc| doc.total).sum::<u64>() as f32 / count as f32).max(1.0);
    let mut scores: Vec<Vec<f32>> = docs.iter().map(|doc| vec![0.0; doc.chunks.len()]).collect();
    for (term, weight) in &terms {
        let found: usize = docs.iter().map(|doc| doc.postings.get(term.as_str()).map_or(0, Vec::len)).sum();
        if found == 0 { continue; }
        let idf = ((count as f32 - found as f32 + 0.5) / (found as f32 + 0.5)).ln_1p();
        for (number, doc) in docs.iter().enumerate() {
            let Some(list) = doc.postings.get(term.as_str()) else { continue };
            let scores = &mut scores[number];
            for &(chunk, tf) in list {
                let (tf, length) = (tf as f32, doc.chunks[chunk as usize].terms as f32);
                scores[chunk as usize] += weight * idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * length / average));
            }
        }
    }
    let mut hits: Vec<Hit> = scores.iter().enumerate()
        .flat_map(|(doc, scores)| scores.iter().enumerate().filter(|(_, score)| **score > 0.0).map(move |(chunk, &score)| Hit { doc, chunk, score }))
        .collect();
    let order = |a: &Hit, b: &Hit| b.score.total_cmp(&a.score).then(a.doc.cmp(&b.doc)).then(a.chunk.cmp(&b.chunk));
    if hits.len() > limit {
        hits.select_nth_unstable_by(limit - 1, order);
        hits.truncate(limit);
    }
    hits.sort_by(order);
    hits
}

/// A stretch of one document made of one or more neighbouring chunks.
#[derive(Clone, Debug, PartialEq)]
pub struct Passage {
    pub doc: usize,
    /// The first and last chunk it covers.
    pub first: usize,
    pub last: usize,
    /// Its byte range in the document's text.
    pub start: usize,
    pub end: usize,
}

impl Passage {
    pub fn text<'a>(&self, docs: &[&'a Index]) -> &'a str { docs[self.doc].text[self.start..self.end].trim() }
}

/// The best of `hits` (best first, as [`search`] returns them) as passages whose text, plus
/// `overhead` bytes each for whatever wraps them, adds up to at most `max_bytes`. Overlapping
/// or neighbouring chunks become one passage; a hit that doesn't fit is skipped for a smaller
/// one further down. Passages come in document order, then in the order they appear.
pub fn fit(docs: &[&Index], hits: &[Hit], max_bytes: usize, overhead: usize) -> Vec<Passage> {
    let mut passages: Vec<Passage> = Vec::new();
    let mut used = 0;
    for hit in hits {
        let chunk = docs[hit.doc].chunks[hit.chunk];
        let mut grown = Passage { doc: hit.doc, first: hit.chunk, last: hit.chunk, start: chunk.start as usize, end: chunk.end as usize };
        let mut absorbed = Vec::new();
        // Joining one passage can make the result reach another, so repeat until nothing changes.
        loop {
            let before = absorbed.len();
            for (index, passage) in passages.iter().enumerate() {
                if absorbed.contains(&index) || passage.doc != grown.doc || passage.start > grown.end + 2 || grown.start > passage.end + 2 { continue; }
                grown = Passage { doc: grown.doc, first: grown.first.min(passage.first), last: grown.last.max(passage.last),
                    start: grown.start.min(passage.start), end: grown.end.max(passage.end) };
                absorbed.push(index);
            }
            if absorbed.len() == before { break; }
        }
        let freed: usize = absorbed.iter().map(|&index| passages[index].end - passages[index].start + overhead).sum();
        let cost = grown.end - grown.start + overhead;
        if used + cost > max_bytes + freed { continue; }
        used = used + cost - freed;
        absorbed.sort_unstable();
        for index in absorbed.into_iter().rev() { passages.remove(index); }
        passages.push(grown);
    }
    passages.sort_by_key(|passage| (passage.doc, passage.start));
    passages
}

/// Where a background index is.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Being read or built: 0..=1.
    Indexing(f32),
    Ready,
    /// Why it couldn't be built.
    Failed(String),
}

enum State { Indexing(f32), Ready(Arc<Index>), Failed(String) }

/// An index being loaded or built on a background thread, and then the index.
pub struct Slot { state: Mutex<State> }

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_tuple("Slot").field(&self.status()).finish() }
}

impl Slot {
    /// A slot holding an index that is already built.
    pub fn ready(index: Index) -> Arc<Self> { Arc::new(Self { state: Mutex::new(State::Ready(Arc::new(index))) }) }

    pub fn status(&self) -> Status {
        match &*lock(&self.state) {
            State::Indexing(progress) => Status::Indexing(*progress),
            State::Ready(_) => Status::Ready,
            State::Failed(reason) => Status::Failed(reason.clone()),
        }
    }

    /// The index, once it's ready.
    pub fn index(&self) -> Option<Arc<Index>> {
        match &*lock(&self.state) { State::Ready(index) => Some(index.clone()), _ => None }
    }

    fn set(&self, state: State) { *lock(&self.state) = state; }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }

/// Indexes being built or in use, by where they're saved: asking twice for one that's still
/// held gives the same [`Slot`], and one nobody holds any more is freed.
#[derive(Default)]
pub struct Registry { slots: Mutex<HashMap<PathBuf, Weak<Slot>>> }

impl Registry {
    /// The index of the text at `text_path`, saved at `index_path` ([`Index::open`]). The first
    /// time it's asked for (while nobody holds it) it is read or built on a background thread,
    /// meanwhile the slot reports progress; a text of at most 64 KB is indexed right away.
    pub fn open(&self, text_path: &Path, index_path: &Path) -> Arc<Slot> {
        let mut slots = lock(&self.slots);
        slots.retain(|_, slot| slot.strong_count() > 0);
        if let Some(slot) = slots.get(index_path).and_then(Weak::upgrade) { return slot; }
        // A small text is indexed at once: it takes well under a millisecond.
        if fs::metadata(text_path).is_ok_and(|meta| meta.len() <= INLINE_BYTES) {
            let slot = Arc::new(Slot { state: Mutex::new(match Index::open(text_path, index_path, &mut |_| {}) {
                Ok(index) => State::Ready(Arc::new(index)),
                Err(error) => State::Failed(error.to_string()),
            }) });
            slots.insert(index_path.to_path_buf(), Arc::downgrade(&slot));
            return slot;
        }
        let slot = Arc::new(Slot { state: Mutex::new(State::Indexing(0.0)) });
        slots.insert(index_path.to_path_buf(), Arc::downgrade(&slot));
        let (building, text_path, index_path) = (slot.clone(), text_path.to_path_buf(), index_path.to_path_buf());
        let started = std::thread::Builder::new().name("cluelyrs-index".into()).spawn(move || {
            let mut last = 0.0;
            let mut progress = |done: f32| if done - last >= 0.01 || done >= 1.0 { last = done; building.set(State::Indexing(done.min(0.99))); };
            building.set(match Index::open(&text_path, &index_path, &mut progress) {
                Ok(index) => State::Ready(Arc::new(index)),
                Err(error) => State::Failed(error.to_string()),
            });
        });
        if let Err(error) = started { slot.set(State::Failed(error.to_string())); }
        slot
    }

    /// Start over for `index_path` (Retry): the saved index is deleted and the text indexed again.
    /// Whoever holds the old slot keeps it as it was.
    pub fn rebuild(&self, text_path: &Path, index_path: &Path) -> Arc<Slot> {
        lock(&self.slots).remove(index_path);
        let _ = fs::remove_file(index_path);
        self.open(text_path, index_path)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn index(text: &str) -> Index { Index::build(text.to_string(), &mut |_| {}) }

    fn chunk_texts(text: &str) -> Vec<String> { chunk(text).0.iter().map(|&(start, end, _)| text[start..end].to_string()).collect() }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cluelyrs-search-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Paragraphs of filler, with `needle` as paragraph `at`.
    fn corpus(paragraphs: usize, at: usize, needle: &str) -> String {
        let mut text = String::new();
        for n in 0..paragraphs {
            if n % 40 == 0 { text.push_str(&format!("## Chapter {}\n\n", n / 40 + 1)); }
            if n == at { text.push_str(needle); } else {
                text.push_str(&format!("Paragraph {n} describes routine operations, quarterly planning, staffing levels and the general \
                    process for reviews {}. Teams meet weekly and track progress in the shared tracker.", n % 17));
            }
            text.push_str("\n\n");
        }
        text
    }

    #[test]
    fn stopwords_are_sorted_for_binary_search() {
        assert!(STOPWORDS.windows(2).all(|pair| pair[0] < pair[1]), "{:?}", STOPWORDS.windows(2).find(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn terms_are_lowercased_stemmed_and_stopwords_dropped() {
        let mut terms = Vec::new();
        for_each_term("The Policies, policy & indexing: we Indexed 3 Mutexes in Zürich — a b 42", &mut |term| terms.push(term.to_string()));
        assert_eq!(terms, ["polic", "polic", "index", "index", "3", "mutexe", "zürich", "42"]);
    }

    #[test]
    fn headings_start_chunks_and_paragraphs_are_packed_up_to_the_target() {
        let paragraph = "Word ".repeat(60).trim().to_string() + ".";
        let text = format!("# Intro\n\n{paragraph}\n\n{paragraph}\n\n{paragraph}\n\n## Pricing\n\nPro is $20 a month.\n\nTeams pay $15 per seat.");
        let (chunks, headings) = chunk(&text);
        let texts = chunk_texts(&text);
        assert_eq!(headings.len(), 2);
        assert!(texts[0].starts_with("# Intro") && texts.last().unwrap().starts_with("## Pricing"), "{texts:#?}");
        assert!(texts.last().unwrap().ends_with("$15 per seat."), "a short section is one chunk");
        assert!(chunks.iter().all(|&(start, end, _)| end - start <= MAX));
        // Three 300-byte paragraphs after the heading: packed while they fit in 1,200 bytes.
        let intro: Vec<&String> = texts.iter().filter(|text| text.contains("Word")).collect();
        assert_eq!(intro.len(), 1, "{intro:#?}");
        assert_eq!(chunks.last().unwrap().2, Some(1), "under the second heading");
        let built = index(&text);
        assert_eq!(built.heading(built.chunks() - 1), Some("Pricing"));
        assert_eq!(built.heading(0), Some("Intro"));
    }

    #[test]
    fn a_short_last_paragraph_carries_into_the_next_chunk() {
        let long = "Long sentence here. ".repeat(40); // 800 bytes
        let text = format!("{long}\n\nShort bridge.\n\n{long}\n\nEnd.");
        let texts = chunk_texts(&text);
        assert_eq!(texts.len(), 2, "{texts:#?}");
        assert!(texts[0].ends_with("Short bridge.") && texts[1].starts_with("Short bridge."), "{texts:#?}");
    }

    #[test]
    fn long_paragraphs_are_split_at_sentences_with_overlap_and_every_word_is_covered() {
        // One 9 KB paragraph of numbered, multi-byte sentences.
        let text: String = (0..300).map(|n| format!("Sätze {n} ends here. ")).collect::<String>().trim_end().to_string();
        let (chunks, _) = chunk(&text);
        assert!(chunks.len() > 5);
        for &(start, end, _) in &chunks {
            assert!(text.is_char_boundary(start) && text.is_char_boundary(end));
            assert!(end - start <= TARGET + TARGET / 2, "{}", end - start);
        }
        for pair in chunks.windows(2) {
            assert!(pair[1].0 < pair[0].1, "pieces overlap");
            assert!(text[..pair[0].1].ends_with('.'), "cut at a sentence end: {:?}", &text[pair[0].1 - 10..pair[0].1]);
        }
        for n in 0..300 {
            let sentence = format!("Sätze {n} ends here.");
            assert!(chunks.iter().any(|&(start, end, _)| text[start..end].contains(&sentence)), "{sentence} is whole in some chunk");
        }
        // No spaces at all: still cut, on character boundaries.
        let solid = "é".repeat(5_000);
        assert!(chunk(&solid).0.iter().all(|&(start, end, _)| solid.is_char_boundary(start) && solid.is_char_boundary(end) && end > start));
    }

    #[test]
    fn pdf_style_titles_count_as_headings_but_sentences_do_not() {
        let text = "Refund Policy\n\nCustomers may ask for a refund within 30 days.\n\nThis line ends with a period.\n\n3.2 Escalation Paths\n\nCall the duty manager.";
        let headings: Vec<&str> = blocks(text).iter().filter(|block| block.heading).map(|block| &text[block.start..block.end]).collect();
        assert_eq!(headings, ["Refund Policy", "3.2 Escalation Paths"]);
    }

    #[test]
    fn the_chunk_holding_the_rare_terms_ranks_first() {
        let needle = "The failover runbook: when the primary Postgres replica lags, page the on-call DBA and promote the standby in us-east-2.";
        let built = index(&corpus(400, 287, needle));
        let hits = search(&[&built], &[("how do we handle postgres replica lag", 1.0)], 5);
        assert!(built.chunk(hits[0].chunk).contains("failover runbook"), "{:?}", built.chunk(hits[0].chunk));
        assert!(hits.windows(2).all(|pair| pair[0].score >= pair[1].score));
        assert!(search(&[&built], &[("the and of", 1.0)], 5).is_empty(), "stopwords alone match nothing");
        assert!(search(&[&built], &[("zebra", 1.0)], 5).is_empty());
    }

    #[test]
    fn a_heading_finds_the_chunks_of_its_section_and_weights_shift_the_ranking() {
        let filler = "Ordinary text about many unrelated topics that goes on for a while. ".repeat(20);
        let text = format!("## Parental leave\n\n{filler}\n\n{filler}\n\n## Travel\n\nBook flights through the travel desk.\n\n## Expenses\n\nSubmit receipts within a week.");
        let built = index(&text);
        let hits = search(&[&built], &[("parental leave", 1.0)], 10);
        assert!(hits.len() >= 2, "the section's later chunks match through its heading: {hits:?}");
        assert!(hits.iter().all(|hit| built.heading(hit.chunk) == Some("Parental leave")));
        let travel = search(&[&built], &[("receipts", 1.0), ("flights", 3.0)], 1);
        assert!(built.chunk(travel[0].chunk).contains("flights"), "the heavier part wins");
    }

    #[test]
    fn term_statistics_are_shared_across_documents() {
        let common = index(&"Kubernetes clusters. ".repeat(10));
        let rare = index("Our Kubernetes budget for next year is in the finance sheet.\n\nNothing else here.");
        let hits = search(&[&common, &rare], &[("kubernetes budget", 1.0)], 3);
        assert_eq!((hits[0].doc, hits[0].chunk), (1, 0), "{hits:?}");
        assert!(search(&[], &[("anything", 1.0)], 3).is_empty());
    }

    #[test]
    fn passages_fit_the_budget_merge_neighbours_and_come_in_document_order() {
        let paragraphs: Vec<String> = (0..20).map(|n| format!("Item {n} {}.", "filler words ".repeat(90))).collect();
        let text = paragraphs.join("\n\n");
        let built = index(&text);
        let docs = [&built];
        let hit = |chunk: usize, score: f32| Hit { doc: 0, chunk, score };
        let sizes: Vec<usize> = (0..built.chunks()).map(|n| built.chunk(n).len()).collect();
        // Chunks 5 and 6 are neighbours: one passage. Chunk 2 doesn't fit after them; 9 is skipped too.
        let budget = sizes[5] + sizes[6] + 2 + 2 * 10 + 10;
        let passages = fit(&docs, &[hit(6, 3.0), hit(5, 2.0), hit(2, 1.0), hit(9, 0.5)], budget, 10);
        assert_eq!(passages.len(), 1, "{passages:?}");
        assert_eq!((passages[0].first, passages[0].last), (5, 6));
        assert!(passages[0].text(&docs).starts_with("Item 5") && passages[0].text(&docs).contains("Item 6") && !passages[0].text(&docs).contains("Item 7"));
        // With room for all, they come in document order.
        let all = fit(&docs, &[hit(9, 3.0), hit(2, 2.0), hit(5, 1.0)], usize::MAX, 10);
        assert_eq!(all.iter().map(|passage| passage.first).collect::<Vec<_>>(), [2, 5, 9]);
        let total: usize = all.iter().map(|passage| passage.end - passage.start + 10).sum();
        assert!(fit(&docs, &[hit(9, 3.0), hit(2, 2.0), hit(5, 1.0)], total - 1, 10).len() == 2);
        assert!(fit(&docs, &[hit(1, 1.0)], 5, 10).is_empty(), "nothing fits");
    }

    #[test]
    fn a_saved_index_is_reused_until_its_text_changes_and_damage_means_a_rebuild() {
        let dir = tempdir("persist");
        let (text_path, index_path) = (dir.join("f.txt"), dir.join("f.idx"));
        let text = corpus(200, 150, "The quarterly offsite is in Lisbon this year.");
        fs::write(&text_path, &text).unwrap();
        let mut builds = 0;
        let first = Index::open(&text_path, &index_path, &mut |progress| if progress < 1.0 { builds += 1 }).unwrap();
        assert!(builds > 0 && index_path.exists());
        let query = [("where is the offsite", 1.0)];
        let expected = search(&[&first], &query, 3);

        let mut rebuilt = false;
        let again = Index::open(&text_path, &index_path, &mut |progress| rebuilt |= progress < 1.0).unwrap();
        assert!(!rebuilt, "read from disk, not built");
        assert_eq!(search(&[&again], &query, 3), expected);
        assert_eq!((again.chunks(), again.heading(expected[0].chunk)), (first.chunks(), first.heading(expected[0].chunk)));

        // The text changed: the saved index names another digest and is replaced.
        fs::write(&text_path, text.replace("Lisbon", "Porto")).unwrap();
        let changed = Index::open(&text_path, &index_path, &mut |progress| rebuilt |= progress < 1.0).unwrap();
        assert!(rebuilt);
        assert!(changed.chunk(search(&[&changed], &query, 1)[0].chunk).contains("Porto"));

        // A damaged or truncated file is rebuilt, never trusted.
        let mut bytes = fs::read(&index_path).unwrap();
        bytes.truncate(bytes.len() - 3);
        fs::write(&index_path, &bytes).unwrap();
        rebuilt = false;
        Index::open(&text_path, &index_path, &mut |progress| rebuilt |= progress < 1.0).unwrap();
        assert!(rebuilt);
        let text_now = fs::read_to_string(&text_path).unwrap();
        let mut bytes = fs::read(&index_path).unwrap();
        bytes[8 + 32 + 8..8 + 32 + 12].copy_from_slice(&(text_now.len() as u32 + 7).to_le_bytes()); // first chunk's start, out of range
        assert!(Index::decode(&bytes, text_now.clone(), digest(&text_now)).is_err());
        assert!(Index::decode(b"CRSIDX00", text_now.clone(), digest(&text_now)).is_err(), "another format");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_registry_builds_in_the_background_shares_slots_and_rebuilds_on_request() {
        let dir = tempdir("registry");
        let (text_path, index_path) = (dir.join("f.txt"), dir.join("f.idx"));
        fs::write(&text_path, corpus(400, 50, "Nothing special.")).unwrap();
        let registry = Registry::default();
        let slot = registry.open(&text_path, &index_path);
        assert!(Arc::ptr_eq(&slot, &registry.open(&text_path, &index_path)), "held: the same slot");
        let wait = |slot: &Slot| {
            let started = Instant::now();
            while matches!(slot.status(), Status::Indexing(_)) && started.elapsed() < Duration::from_secs(20) { std::thread::sleep(Duration::from_millis(5)); }
            slot.status()
        };
        assert_eq!(wait(&slot), Status::Ready);
        assert!(slot.index().unwrap().chunks() > 0);
        let rebuilt = registry.rebuild(&text_path, &index_path);
        assert!(!Arc::ptr_eq(&slot, &rebuilt));
        assert_eq!(wait(&rebuilt), Status::Ready);
        let missing = registry.open(&dir.join("gone.txt"), &dir.join("gone.idx"));
        assert!(matches!(wait(&missing), Status::Failed(_)));
        fs::remove_dir_all(dir).unwrap();
    }

    /// About `bytes` of text that reads like prose to the ranker: words drawn from a 40,000-word
    /// vocabulary by Zipf's law (the query's words among the common ones, so their postings are
    /// long, as in real documents), in sentences and paragraphs with a heading every 30, and
    /// `needle` three quarters of the way in.
    fn prose(bytes: usize, needle: &str) -> String {
        let common = ["system", "team", "data", "page", "people", "handle", "quarter", "behind", "incident", "walk", "right", "minute",
            "drill", "lag", "primary", "standby", "replica", "postgres", "failover", "replication", "fall"];
        let vocabulary: Vec<String> = (0..40_000).map(|rank| match rank {
            rank if (20..20 + common.len() * 15).contains(&rank) && (rank - 20) % 15 == 0 => common[(rank - 20) / 15].to_string(),
            rank => format!("w{rank:x}q"),
        }).collect();
        let weights: Vec<f64> = (1..=vocabulary.len()).map(|rank| 1.0 / rank as f64).collect();
        let total: f64 = weights.iter().sum();
        let cumulative: Vec<f64> = weights.iter().scan(0.0, |sum, weight| { *sum += weight / total; Some(*sum) }).collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; (seed >> 11) as f64 / (1u64 << 53) as f64 };
        let mut text = String::with_capacity(bytes + 4096);
        let (mut paragraphs, mut placed) = (0, false);
        while text.len() < bytes {
            if paragraphs % 30 == 0 { text.push_str(&format!("## Part {}

", paragraphs / 30 + 1)); }
            if !placed && text.len() > bytes * 3 / 4 { text.push_str(needle); text.push_str("

"); placed = true; }
            for _ in 0..4 {
                for word in 0..14 {
                    let at = cumulative.partition_point(|&edge| edge < next()).min(vocabulary.len() - 1);
                    if word > 0 { text.push(' '); }
                    text.push_str(&vocabulary[at]);
                }
                text.push_str(". ");
            }
            text.push_str("

");
            paragraphs += 1;
        }
        text
    }

    /// Retrieval cost on the answer path, for a ~1 MB and a ~10 MB corpus. Run with
    /// `cargo test --release --lib search::tests::latency -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn latency() {
        let question = "So how would you handle a postgres replica that falls behind during the failover drill?";
        let transcript = "Them: we had an incident last quarter with replication lag. Me: right, the standby was minutes behind. \
            Them: walk me through what you would do. Them: also how do you page the right people";
        for megabytes in [1, 10] {
            let text = prose(megabytes * 1024 * 1024, "The failover runbook: when the primary Postgres replica lags, page the on-call DBA and promote the standby.");
            let started = Instant::now();
            let built = index(&text);
            let build = started.elapsed();
            let dir = tempdir("latency");
            built.save(&dir.join("f.idx")).unwrap();
            fs::write(dir.join("f.txt"), &text).unwrap();
            let started = Instant::now();
            let loaded = Index::open(&dir.join("f.txt"), &dir.join("f.idx"), &mut |_| {}).unwrap();
            let load = started.elapsed();
            let query = [(question, 3.0), (transcript, 1.0)];
            let runs = 50;
            let started = Instant::now();
            let mut passages = Vec::new();
            for _ in 0..runs {
                let hits = search(&[&loaded], &query, 64);
                passages = fit(&[&loaded], &hits, 32_000, 80);
            }
            let per_query = started.elapsed() / runs;
            assert!(passages.iter().any(|passage| passage.text(&[&loaded]).contains("failover runbook")));
            println!("{} MB ({} bytes, {} chunks, {} terms): build {build:?}, load {load:?}, search+fit {per_query:?} per query",
                megabytes, text.len(), loaded.chunks(), loaded.postings.len());
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
