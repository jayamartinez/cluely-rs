//! The overlay's side of listening: starts and stops the pipeline with Live, keeps the
//! transcript it shows (committed lines plus one provisional line per source), writes committed
//! lines to the session archive, renders the transcript block, and installs the local model.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use gpui::{AnyElement, Context, FontStyle, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, Styled, StyledText, Window, div, px};

use crate::archive::{self, Line, Speaker};
use crate::audio::Source;
use crate::listening::{self, Listening, Message, Status};
use crate::models::ModelFile;
use crate::overlay::Overlay;
use crate::stt::parakeet::MODEL;
use crate::theme;
use crate::transcript::live::Update;
use crate::transcript::state::UtteranceId;

/// Committed lines shown in the overlay, so it stays the same size however long the
/// conversation runs. The whole transcript is kept in memory and in the session archive.
const SHOWN_LINES: usize = 2;

/// A committed utterance as the overlay keeps it: who, when (both clocks) and what.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptLine {
    pub id: UtteranceId,
    pub source: Source,
    pub text: String,
    /// Audio time on the pipeline's clock.
    pub start_ms: f64,
    pub end_ms: f64,
    /// Milliseconds into the Live session (the archive's clock).
    pub at_ms: u64,
}

/// The utterance a source is still speaking.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProvisionalLine {
    pub stable: String,
    pub unstable: String,
    pub question: bool,
}

/// A model download in progress.
pub struct Download {
    pub received: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
}

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

fn speaker(source: Source) -> Speaker { match source { Source::Me => Speaker::You, Source::Them => Speaker::Them } }

impl Overlay {
    /// Start capture and transcription for the Live session, if transcription is on.
    pub(crate) fn start_listening(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_listening();
        if !self.store.value.transcribe { return; }
        let provider = listening::provider();
        if let Some(reason) = listening::not_ready(&provider.availability()) {
            self.listening_status = Some(Status::Failed(reason));
            cx.notify();
            return;
        }
        self.listening_epoch += 1;
        let epoch = self.listening_epoch;
        let (listening, mut messages) = Listening::start(provider, self.store.value.audio_source.sources().to_vec());
        self.listening = Some(listening);
        self.listening_status = Some(Status::Starting);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            while let Some(message) = messages.next().await {
                if this.update(cx, |this, cx| this.apply_listening(epoch, message, cx)).is_err() { break; }
            }
        }).detach();
    }

    /// Stop the pipeline. The transcript stays until Live ends.
    pub(crate) fn stop_listening(&mut self) {
        self.listening = None;
        self.listening_status = None;
        self.provisional = Default::default();
    }

    /// Live is ending: keep whatever each source was still saying, as the archive's last lines.
    pub(crate) fn archive_provisional(&mut self) {
        let Some(recorder) = &mut self.recorder else { return };
        let at_ms = self.live_since.map(|start| start.elapsed().as_millis() as u64).unwrap_or(0);
        for source in [Source::Them, Source::Me] {
            let Some(line) = self.provisional[slot(source)].take() else { continue };
            let text = format!("{} {}", line.stable, line.unstable).trim().to_string();
            if text.is_empty() { continue; }
            if let Err(error) = recorder.add_line(Line { at_ms, speaker: speaker(source), text }) { eprintln!("transcript line could not be saved: {error}"); }
        }
    }

    pub(crate) fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.provisional = Default::default();
    }

    /// Settings that change what's captured take effect in the running session.
    pub(crate) fn restart_listening_if_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.live_since.is_some() { self.start_listening(window, cx); }
    }

    fn apply_listening(&mut self, epoch: u64, message: Message, cx: &mut Context<Self>) {
        // Messages from a pipeline that was replaced or stopped are stale.
        if epoch != self.listening_epoch || self.listening.is_none() { return; }
        match message {
            Message::Status(status) => self.listening_status = Some(status),
            Message::Transcript(update) => self.apply_update(update),
        }
        cx.notify();
    }

    fn apply_update(&mut self, update: Update) {
        match update {
            Update::Provisional { source, stable, unstable, .. } => {
                let question = self.provisional[slot(source)].as_ref().is_some_and(|line| line.question);
                self.provisional[slot(source)] = Some(ProvisionalLine { stable, unstable, question });
            }
            Update::QuestionLikely { source, .. } => {
                if let Some(line) = &mut self.provisional[slot(source)] { line.question = true; }
            }
            Update::Committed { utterance, .. } => {
                self.provisional[slot(utterance.source)] = None;
                let at_ms = self.session_ms(utterance.start_ms);
                let line = TranscriptLine { id: utterance.id, source: utterance.source, text: utterance.text, start_ms: utterance.start_ms, end_ms: utterance.end_ms, at_ms };
                if let Some(recorder) = &mut self.recorder
                    && let Err(error) = recorder.add_line(Line { at_ms, speaker: speaker(line.source), text: line.text.clone() }) {
                    eprintln!("transcript line could not be saved: {error}");
                }
                self.transcript.push(line);
            }
            Update::Error { source, message, fatal } => {
                if fatal {
                    self.listening_status = Some(Status::Failed(format!("{} transcription stopped: {message}", source.label())));
                    self.provisional[slot(source)] = None;
                } else {
                    eprintln!("{} transcription: {message}", source.label());
                }
            }
        }
    }

    /// Pipeline audio time → milliseconds since the Live session started.
    fn session_ms(&self, audio_ms: f64) -> u64 {
        let offset = match (self.live_since, self.listening.as_ref()) {
            (Some(live), Some(listening)) => listening.started.saturating_duration_since(live).as_millis() as f64,
            _ => 0.0,
        };
        (offset + audio_ms).max(0.0) as u64
    }

    /// Status line plus the recent transcript, at the top of the Live panel.
    pub(crate) fn transcript_block(&self) -> gpui::Div {
        let (dot, label, color): (gpui::Rgba, SharedString, gpui::Rgba) = match &self.listening_status {
            None if !self.store.value.transcribe => (theme::muted(), "Transcription off · turn it on in Settings → Listening".into(), theme::muted()),
            None => (theme::muted(), "Not transcribing".into(), theme::muted()),
            Some(Status::Starting) => (theme::accent_soft(), "Starting transcription…".into(), theme::muted()),
            Some(Status::Listening { sources, failures }) => {
                let heard = sources.iter().map(|source| source.label()).collect::<Vec<_>>().join(" + ");
                let mut text = format!("Transcribing {heard}");
                for (source, error) in failures { text.push_str(&format!(" · {} unavailable: {error}", source.label())); }
                (theme::ok(), text.into(), theme::muted())
            }
            Some(Status::Failed(reason)) => (gpui::rgb(0xffb4a8), reason.clone().into(), gpui::rgb(0xffb4a8)),
        };
        let status = div().flex().items_center().gap(px(8.0)).px(px(16.0)).pt(px(10.0)).pb(px(6.0))
            .child(div().text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()).child("HEARD"))
            .child(div().size(px(6.0)).flex_none().rounded_full().bg(dot))
            .child(div().text_size(px(12.0)).text_color(color).truncate().child(label));
        // Two committed lines plus up to one in-progress line per source: a fixed strip.
        let mut lines = div().flex().flex_col().gap(px(4.0)).px(px(16.0)).pb(px(10.0));
        let shown = self.transcript.len().saturating_sub(SHOWN_LINES);
        for line in &self.transcript[shown..] {
            lines = lines.child(transcript_row(line.source, archive::clock(line.at_ms), committed_text(&line.text)));
        }
        for source in [Source::Them, Source::Me] {
            if let Some(line) = &self.provisional[slot(source)] { lines = lines.child(transcript_row(source, "now".into(), provisional_text(line))); }
        }
        if self.transcript.is_empty() && self.provisional.iter().all(Option::is_none) {
            let hint = if matches!(self.listening_status, Some(Status::Listening { .. })) { "Listening for the conversation…" } else { "" };
            lines = lines.child(div().text_size(px(12.0)).text_color(theme::muted()).child(hint));
        }
        div().flex().flex_col().border_b_1().border_color(theme::divider()).child(status).child(lines)
    }

    /// Re-check whether the local model file is present (cheap: file metadata).
    pub(crate) fn refresh_model_status(&mut self) {
        self.model_installed = MODEL.path().is_some_and(|path| MODEL.is_installed_at(&path));
    }

    pub(crate) fn download_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model_download.is_some() { return; }
        let Some(dest) = MODEL.path() else { self.model_notice = Some("No local data folder is available for models.".into()); cx.notify(); return; };
        let received = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (done, mut result) = futures::channel::oneshot::channel::<Result<(), String>>();
        {
            let (received, cancel) = (received.clone(), cancel.clone());
            std::thread::Builder::new().name("cluelyrs-model-download".into()).spawn(move || {
                let outcome = MODEL.download_to(&dest, &cancel, |got, _| received.store(got, Ordering::Relaxed));
                let _ = done.send(outcome.map_err(|error| error.to_string()));
            }).ok();
        }
        self.model_download = Some(Download { received, cancel });
        self.model_notice = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            match result.try_recv() {
                Ok(None) => { if this.update(cx, |_, cx| cx.notify()).is_err() { break; } }
                Ok(Some(outcome)) => { let _ = this.update_in(cx, |this, window, cx| this.finish_download(outcome, window, cx)); break; }
                Err(_) => { let _ = this.update_in(cx, |this, window, cx| this.finish_download(Err("The download stopped unexpectedly.".into()), window, cx)); break; }
            }
        }).detach();
    }

    pub(crate) fn cancel_download(&mut self) {
        if let Some(download) = &self.model_download { download.cancel.store(true, Ordering::Relaxed); }
    }

    fn finish_download(&mut self, outcome: Result<(), String>, window: &mut Window, cx: &mut Context<Self>) {
        self.model_download = None;
        self.refresh_model_status();
        self.model_notice = Some(match outcome {
            Ok(()) => "Installed and verified.".into(),
            Err(reason) if reason.contains("cancelled") => "Download cancelled.".into(),
            Err(reason) => reason.into(),
        });
        // A Live session that was waiting on the model can start transcribing now.
        if self.model_installed && matches!(self.listening_status, Some(Status::Failed(_))) { self.restart_listening_if_live(window, cx); }
        cx.notify();
    }

    /// Download progress as a fraction, while one is running.
    pub(crate) fn download_progress(&self) -> Option<f64> {
        self.model_download.as_ref().map(|download| download.received.load(Ordering::Relaxed) as f64 / MODEL.bytes as f64)
    }
}

pub fn model_size_label(model: &ModelFile) -> String { format!("{:.0} MB", model.bytes as f64 / 1e6) }

fn transcript_row(source: Source, when: String, text: AnyElement) -> impl IntoElement {
    let (label_color, label_bg) = match source {
        Source::Them => (theme::accent_soft(), theme::bubble()),
        Source::Me => (theme::body(), theme::raised()),
    };
    div().flex().items_start().gap(px(8.0))
        .child(div().w(px(40.0)).flex_none().text_size(px(10.0)).font_weight(FontWeight::SEMIBOLD).text_color(label_color)
            .px(px(6.0)).py(px(1.0)).rounded(px(5.0)).bg(label_bg).flex().justify_center().child(source.label()))
        .child(div().w(px(34.0)).flex_none().font_family(theme::MONO).text_size(px(10.0)).text_color(theme::muted()).pt(px(2.0)).child(when))
        .child(div().flex_1().min_w_0().text_size(px(13.0)).line_height(px(18.0)).child(text))
}

/// One row each, so the strip keeps its height; the full text is in the session archive.
fn committed_text(text: &str) -> AnyElement {
    div().text_color(theme::text()).truncate().child(SharedString::from(text.to_string())).into_any_element()
}

/// Stable words in the body color, the unstable tail muted and italic, plus a question mark
/// once the utterance reads as a question.
fn provisional_text(line: &ProvisionalLine) -> AnyElement {
    let mut text = line.stable.clone();
    if !line.unstable.is_empty() {
        if !text.is_empty() { text.push(' '); }
        text.push_str(&line.unstable);
    }
    let unstable_from = text.len() - line.unstable.len();
    let question_from = text.len();
    if line.question { text.push_str(" ?"); }
    let styled = StyledText::new(SharedString::from(text)).with_highlights([
        (unstable_from..question_from, HighlightStyle { color: Some(theme::muted().into()), font_style: Some(FontStyle::Italic), ..Default::default() }),
        (question_from..question_from + if line.question { 2 } else { 0 }, HighlightStyle { color: Some(theme::accent_soft().into()), font_weight: Some(FontWeight::BOLD), ..Default::default() }),
    ].into_iter().filter(|(range, _)| !range.is_empty()));
    div().text_color(theme::body()).child(styled).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_map_to_archive_speakers_and_sizes_read_naturally() {
        assert_eq!(speaker(Source::Me), Speaker::You);
        assert_eq!(speaker(Source::Them), Speaker::Them);
        assert_eq!(model_size_label(&MODEL), "176 MB");
    }
}
