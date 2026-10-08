//! Text from the files a mode holds: PDF, DOCX, TXT and Markdown. Each file is read once, when
//! it's added; only the extracted text is kept and sent.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The most file bytes one mode may hold.
pub const MAX_MODE_BYTES: u64 = 50 * 1024 * 1024;
/// The most files one mode may hold.
pub const MAX_FILES: usize = 5;
/// Extracted text kept per file. Far above any prompt budget; it only bounds disk use.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// A DOCX's document part may not inflate beyond this (zip bombs).
const MAX_DOCX_XML: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind { Pdf, Docx, Txt, Md }

impl FileKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "pdf" => Some(Self::Pdf),
            "docx" => Some(Self::Docx),
            "txt" | "text" => Some(Self::Txt),
            "md" | "markdown" => Some(Self::Md),
            _ => None,
        }
    }

    /// The short tag shown on the file's row.
    pub fn tag(self) -> &'static str { match self { Self::Pdf => "PDF", Self::Docx => "DOCX", Self::Txt => "TXT", Self::Md => "MD" } }
}

/// Why a file couldn't be added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileError {
    Unsupported,
    TooLarge,
    LimitReached,
    NoText,
    Unreadable,
    TimedOut,
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "This file type isn't supported. Use PDF, DOCX, TXT or MD.",
            Self::TooLarge => "Too large: files can add up to 50 MB per mode.",
            Self::LimitReached => "Limit reached: a mode holds up to 5 files.",
            Self::NoText => "No text found. Scanned PDFs need a text layer.",
            Self::Unreadable => "This file couldn't be read.",
            Self::TimedOut => "Reading this file took too long. It may be damaged.",
        })
    }
}

/// A file read and turned into text, ready to be added to a mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Extracted {
    /// The file's own name, for display.
    pub name: String,
    pub kind: FileKind,
    pub bytes: u64,
    pub pages: Option<u32>,
    pub text: String,
}

/// Read `path` and extract its text. `room` is how many more bytes the mode may take. Blocks
/// (a large PDF takes a while); call it off the UI thread.
pub fn extract(path: &Path, room: u64) -> Result<Extracted, FileError> {
    let kind = FileKind::from_path(path).ok_or(FileError::Unsupported)?;
    let bytes = std::fs::metadata(path).map_err(|_| FileError::Unreadable)?.len();
    if bytes > room { return Err(FileError::TooLarge); }
    let data = std::fs::read(path).map_err(|_| FileError::Unreadable)?;
    let (text, pages) = match kind {
        FileKind::Txt | FileKind::Md => (decode_text(&data), None),
        FileKind::Docx => (docx_text(&data)?, None),
        FileKind::Pdf => pdf_text(&data)?,
    };
    let text = tidy(&text);
    if text.trim().is_empty() { return Err(FileError::NoText); }
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Extracted { name, kind, bytes, pages, text })
}

/// The argument that makes the app read one file and exit instead of starting (see [`child_main`]).
pub const CHILD_FLAG: &str = "--cluelyrs-read-mode-file";
/// A file not read by then is given up on, and its reader stopped.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Like [`extract`], but in a separate process that is stopped after [`READ_TIMEOUT`]: a damaged
/// or hostile PDF can make the parser loop or overflow its stack, which neither a thread nor
/// `catch_unwind` can contain. The process is this app run with [`CHILD_FLAG`]. Blocks.
pub fn extract_isolated(path: &Path, room: u64) -> Result<Extracted, FileError> {
    FileKind::from_path(path).ok_or(FileError::Unsupported)?;
    if std::fs::metadata(path).map_err(|_| FileError::Unreadable)?.len() > room { return Err(FileError::TooLarge); }
    let exe = std::env::current_exe().map_err(|_| FileError::Unreadable)?;
    let mut command = Command::new(exe);
    command.arg(CHILD_FLAG).arg(path).arg(room.to_string());
    run_reader(command, READ_TIMEOUT)
}

/// When this process was started by [`extract_isolated`], read the file, print the result as
/// JSON and return the exit code; otherwise `None`. Call first thing in `main`.
pub fn child_main() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != CHILD_FLAG { return None; }
    let path = PathBuf::from(args.next()?);
    let room = args.next().and_then(|room| room.to_str()?.parse().ok()).unwrap_or(MAX_MODE_BYTES);
    let json = serde_json::to_vec(&extract(&path, room)).unwrap_or_default();
    let mut out = std::io::stdout().lock();
    Some(if out.write_all(&json).and_then(|_| out.flush()).is_ok() { 0 } else { 1 })
}

/// Run a reader process and take its JSON result, killing it at `timeout`.
fn run_reader(mut command: Command, timeout: Duration) -> Result<Extracted, FileError> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command.spawn().map_err(|_| FileError::Unreadable)?;
    let mut stdout = child.stdout.take().ok_or(FileError::Unreadable)?;
    // Read concurrently so a large result can't block the child on a full pipe.
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_TEXT_BYTES as u64 * 7 + 65_536).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().unwrap_or_default();
                if !status.success() { return Err(FileError::Unreadable); }
                return serde_json::from_slice::<Result<Extracted, FileError>>(&out).map_err(|_| FileError::Unreadable)?;
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FileError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Err(FileError::Unreadable),
        }
    }
}

/// Plain text in UTF-8 or UTF-16 (with a byte-order mark); invalid bytes become U+FFFD.
fn decode_text(data: &[u8]) -> String {
    let utf16 = |bytes: &[u8], little: bool| {
        let units: Vec<u16> = bytes.chunks(2).filter(|pair| pair.len() == 2)
            .map(|pair| if little { u16::from_le_bytes([pair[0], pair[1]]) } else { u16::from_be_bytes([pair[0], pair[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    match data {
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xff, 0xfe, rest @ ..] => utf16(rest, true),
        [0xfe, 0xff, rest @ ..] => utf16(rest, false),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
}

/// Unix line ends, no trailing spaces, at most one blank line in a row, and capped at
/// [`MAX_TEXT_BYTES`] on a character boundary.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TEXT_BYTES));
    let mut blank_run = 0;
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 || out.is_empty() { continue; }
        } else {
            blank_run = 0;
        }
        if out.len() + line.len() + 1 > MAX_TEXT_BYTES {
            let mut cut = MAX_TEXT_BYTES.saturating_sub(out.len());
            while cut > 0 && !line.is_char_boundary(cut) { cut -= 1; }
            out.push_str(&line[..cut]);
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn pdf_text(data: &[u8]) -> Result<(String, Option<u32>), FileError> {
    // The extractor panics on some malformed files; treat that as unreadable.
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(data))
        .map_err(|_| FileError::Unreadable)?
        .map_err(|_| FileError::Unreadable)?;
    let count = u32::try_from(pages.len()).ok();
    Ok((pages.join("\n\n"), count))
}

/// The paragraphs of a DOCX's main document.
fn docx_text(data: &[u8]) -> Result<String, FileError> {
    let xml = zip_entry(data, "word/document.xml", MAX_DOCX_XML).ok_or(FileError::Unreadable)?;
    Ok(document_xml_text(&String::from_utf8_lossy(&xml)))
}

/// Text runs (`w:t`), tabs and breaks of WordprocessingML, one line per paragraph.
fn document_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_text = false;
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        if in_text { out.push_str(&unescape(&rest[..open])); }
        let Some(close) = rest[open..].find('>') else { break };
        let tag = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let name = tag.trim_end_matches('/').split_whitespace().next().unwrap_or("");
        match name {
            "w:t" => in_text = !tag.ends_with('/'),
            "/w:t" => in_text = false,
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" | "/w:p" => out.push('\n'),
            _ => {}
        }
    }
    out
}

/// XML's five named entities and numeric character references.
fn unescape(text: &str) -> String {
    if !text.contains('&') { return text.to_string(); }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(end) = rest.find(';').filter(|end| *end <= 10) else { out.push('&'); rest = &rest[1..]; continue };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''),
            _ => entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")).map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse::<u32>))
                .and_then(Result::ok).and_then(char::from_u32),
        };
        match decoded {
            Some(c) => { out.push(c); rest = &rest[end + 1..]; }
            None => { out.push('&'); rest = &rest[1..]; }
        }
    }
    out.push_str(rest);
    out
}

/// One entry of a zip archive (stored or deflated), up to `limit` bytes uncompressed. Only what
/// DOCX needs: no ZIP64, encryption or multi-disk archives.
fn zip_entry(data: &[u8], wanted: &str, limit: u64) -> Option<Vec<u8>> {
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    // The end-of-central-directory record is in the last 64 KiB + 22 bytes.
    let floor = data.len().saturating_sub(65_557);
    let end = (floor..data.len().saturating_sub(21)).rev().find(|&at| u32_at(at) == Some(0x0605_4b50))?;
    let (count, mut at) = (u16_at(end + 10)?, u32_at(end + 16)?);
    for _ in 0..count {
        if u32_at(at)? != 0x0201_4b50 { return None; }
        let (method, compressed) = (u16_at(at + 10)?, u32_at(at + 20)?);
        let (name_len, extra_len, comment_len) = (u16_at(at + 28)?, u16_at(at + 30)?, u16_at(at + 32)?);
        let local = u32_at(at + 42)?;
        let name = data.get(at + 46..at + 46 + name_len)?;
        at += 46 + name_len + extra_len + comment_len;
        if name != wanted.as_bytes() { continue; }
        if u32_at(local)? != 0x0403_4b50 { return None; }
        let start = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
        let body = data.get(start..start.checked_add(compressed)?)?;
        return match method {
            0 => (body.len() as u64 <= limit).then(|| body.to_vec()),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(body).take(limit + 1).read_to_end(&mut out).ok()?;
                (out.len() as u64 <= limit).then_some(out)
            }
            _ => None,
        };
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A zip archive with the given entries, deflated when `deflate` is set.
    pub(crate) fn zip(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, content) in entries {
            let body = if deflate {
                let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(content).unwrap();
                encoder.finish().unwrap()
            } else { content.to_vec() };
            let method: u16 = if deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]); // time, date, crc
            out.extend((body.len() as u32).to_le_bytes());
            out.extend((content.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(&body);
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend([20, 0, 20, 0, 0, 0]);
            central.extend(method.to_le_bytes());
            central.extend([0; 8]);
            central.extend((body.len() as u32).to_le_bytes());
            central.extend((content.len() as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0; 12]); // extra, comment, disk, attributes
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let central_offset = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0; 4]);
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(central_offset.to_le_bytes());
        out.extend([0; 2]);
        out
    }

    pub(crate) const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="x"><w:body>
        <w:p><w:r><w:t>Jane Doe</w:t></w:r></w:p>
        <w:p><w:r><w:t xml:space="preserve">Rust &amp; Go </w:t></w:r><w:r><w:tab/><w:t>&#8212; 2026</w:t></w:r></w:p>
        <w:p><w:r><w:t/><w:t>Line</w:t><w:br/><w:t>break &lt;ok&gt;</w:t></w:r></w:p></w:body></w:document>"#;

    #[test]
    fn docx_paragraphs_runs_tabs_breaks_and_entities_are_kept() {
        let expected = "Jane Doe\nRust & Go \t\u{2014} 2026\nLine\nbreak <ok>";
        for deflate in [false, true] {
            let docx = zip(&[("[Content_Types].xml", b"<Types/>"), ("word/document.xml", DOCUMENT_XML.as_bytes())], deflate);
            assert_eq!(tidy(&docx_text(&docx).unwrap()), expected, "deflate: {deflate}");
        }
    }

    #[test]
    fn broken_or_oversized_archives_are_rejected() {
        assert_eq!(docx_text(b"not a zip"), Err(FileError::Unreadable));
        let without = zip(&[("word/other.xml", b"<x/>")], true);
        assert_eq!(docx_text(&without), Err(FileError::Unreadable));
        let mut truncated = zip(&[("word/document.xml", DOCUMENT_XML.as_bytes())], false);
        truncated.drain(40..60);
        assert!(zip_entry(&truncated, "word/document.xml", MAX_DOCX_XML).is_none());
        // Inflating past the limit is refused, so a zip bomb can't exhaust memory.
        let bomb = zip(&[("word/document.xml", &vec![b'a'; 100_000])], true);
        assert!(zip_entry(&bomb, "word/document.xml", 10_000).is_none());
        assert_eq!(zip_entry(&bomb, "word/document.xml", 100_000).map(|x| x.len()), Some(100_000));
    }

    #[test]
    fn text_files_decode_utf8_and_utf16_and_are_tidied() {
        assert_eq!(decode_text("\u{feff}héllo".as_bytes()), "héllo");
        let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("hi é".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode_text(&utf16), "hi é");
        assert_eq!(decode_text(&[b'o', b'k', 0xff]), "ok\u{fffd}");
        assert_eq!(tidy("\n\n a  \r\n\r\n\r\n\r\nb\t \rc\n\n"), " a\n\nb\nc");
        let long = "é".repeat(MAX_TEXT_BYTES);
        let capped = tidy(&long);
        assert!(capped.len() <= MAX_TEXT_BYTES && capped.chars().all(|c| c == 'é'));
    }

    #[test]
    fn entities_that_are_not_entities_stay_as_written() {
        assert_eq!(unescape("a & b &amp; &#65;&#x42; &bogus; &#xZZ;"), "a & b & AB &bogus; &#xZZ;");
    }

    #[test]
    fn files_are_checked_by_type_size_and_content() {
        let dir = tempdir("extract");
        let write = |name: &str, data: &[u8]| { let path = dir.join(name); std::fs::write(&path, data).unwrap(); path };
        assert_eq!(extract(&write("notes.pages", b"x"), MAX_MODE_BYTES), Err(FileError::Unsupported));
        assert_eq!(extract(&write("empty.txt", b"  \n\n "), MAX_MODE_BYTES), Err(FileError::NoText));
        assert_eq!(extract(&write("big.md", &[b'a'; 100]), 99), Err(FileError::TooLarge));
        assert_eq!(extract(&write("broken.pdf", b"%PDF-1.4 nonsense"), MAX_MODE_BYTES), Err(FileError::Unreadable));
        let notes = extract(&write("Notes.MD", b"# Plan\r\n\r\n\r\nShip it"), MAX_MODE_BYTES).unwrap();
        assert_eq!((notes.name.as_str(), notes.kind, notes.bytes, notes.pages, notes.text.as_str()), ("Notes.MD", FileKind::Md, 19, None, "# Plan\n\nShip it"));
        let docx = extract(&write("cv.docx", &zip(&[("word/document.xml", DOCUMENT_XML.as_bytes())], true)), MAX_MODE_BYTES).unwrap();
        assert!(docx.text.starts_with("Jane Doe\nRust & Go"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pdf_text_layer_is_extracted_with_its_page_count() {
        let dir = tempdir("pdf");
        let path = dir.join("resume.pdf");
        std::fs::write(&path, minimal_pdf("Senior engineer")).unwrap();
        let pdf = extract(&path, MAX_MODE_BYTES).unwrap();
        assert_eq!((pdf.kind, pdf.pages), (FileKind::Pdf, Some(1)));
        assert!(pdf.text.contains("Senior engineer"), "{:?}", pdf.text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A one-page PDF showing `text` in Helvetica, with a correct cross-reference table.
    fn minimal_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets { out.extend(format!("{offset:010} 00000 n \n").as_bytes()); }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[cfg(unix)]
    #[test]
    fn the_reader_process_result_is_taken_and_a_stuck_reader_is_stopped() {
        let shell = |script: &str| { let mut command = Command::new("/bin/sh"); command.arg("-c").arg(script); command };
        let ok = serde_json::to_string(&Ok::<_, FileError>(Extracted { name: "a.md".into(), kind: FileKind::Md, bytes: 3, pages: None, text: "abc".into() })).unwrap();
        let read = run_reader(shell(&format!("printf '%s' '{ok}'")), Duration::from_secs(5)).unwrap();
        assert_eq!((read.name.as_str(), read.text.as_str()), ("a.md", "abc"));
        assert_eq!(run_reader(shell("printf '%s' '{\"Err\":\"NoText\"}'"), Duration::from_secs(5)), Err(FileError::NoText));
        assert_eq!(run_reader(shell("kill -SEGV $$"), Duration::from_secs(5)), Err(FileError::Unreadable), "a crashed reader");
        assert_eq!(run_reader(shell("printf garbage"), Duration::from_secs(5)), Err(FileError::Unreadable));
        let started = Instant::now();
        assert_eq!(run_reader(shell("sleep 30"), Duration::from_millis(300)), Err(FileError::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5), "stopped at the time limit");
    }

    pub(crate) fn tempdir(name: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("cluelyrs-modes-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
