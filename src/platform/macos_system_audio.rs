//! macOS desktop audio: everything the system plays, captured with ScreenCaptureKit. macOS has no
//! loopback device like WASAPI's, so `audio::capture` uses this for `Source::Them`. An `SCStream`
//! on the main display captures system audio without CluelyRS's own sounds; its video is the
//! smallest the stream allows (2×2 pixels, at most one frame a second) and is dropped unread.
//!
//! ScreenCaptureKit needs the Screen Recording permission; without it `start` returns a message
//! for the Live panel and never panics.

use std::ptr::{NonNull, null_mut};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_audio_types::{AudioBufferList, kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatLinearPCM};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGMainDisplayID, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMClock, CMSampleBuffer, CMTime, CMTimeFlags,
    kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSOperatingSystemVersion, NSProcessInfo};
use objc2_screen_capture_kit::{SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamErrorCode,
    SCStreamOutput, SCStreamOutputType};

/// Shown in the Live panel when ScreenCaptureKit is not allowed.
pub const SCREEN_RECORDING_NEEDED: &str = "Screen Recording is off. Allow it in System Settings → Privacy & Security, then reopen CluelyRS.";
/// How long starting may take before it counts as failed. `audio::capture` waits 5 s for a source.
const START_TIMEOUT: Duration = Duration::from_secs(4);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// Audio the system played, as delivered by ScreenCaptureKit.
pub struct SystemAudioBuffer {
    /// When the first frame was captured.
    pub captured: Instant,
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved f32 samples.
    pub samples: Vec<f32>,
}

pub enum SystemAudioEvent {
    Audio(SystemAudioBuffer),
    /// The stream ended on its own (permission revoked, display gone); the message says why.
    Stopped(String),
}

type Deliver = Box<dyn Fn(SystemAudioEvent) + Send + Sync>;

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Output` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "CluelyRSSystemAudioOutput"]
    #[ivars = Deliver]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(&self, _stream: &SCStream, sample_buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Audio { return; }
            // SAFETY: ScreenCaptureKit hands over a valid sample buffer for the duration of the call.
            if let Some(buffer) = unsafe { read_audio(sample_buffer) } { (self.ivars())(SystemAudioEvent::Audio(buffer)); }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            (self.ivars())(SystemAudioEvent::Stopped(describe(error)));
        }
    }
);

impl Output {
    fn new(deliver: Deliver) -> Retained<Self> {
        let this = Self::alloc().set_ivars(deliver);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// A running system audio capture. Dropping it stops the stream.
pub struct SystemAudio {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
    started: Instant,
}

impl SystemAudio {
    /// Start capturing system audio as `sample_rate` Hz, `channels`-channel f32. Buffers and the
    /// end of the stream reach `deliver` on a ScreenCaptureKit queue, so it must return quickly.
    /// Blocks until the stream runs (usually well under a second); errors are user-facing text.
    pub fn start(sample_rate: u32, channels: u16, deliver: impl Fn(SystemAudioEvent) + Send + Sync + 'static) -> Result<Self, String> {
        let version = NSOperatingSystemVersion { majorVersion: 13, minorVersion: 0, patchVersion: 0 };
        if !NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(version) { return Err("System audio needs macOS 13 or later.".into()); }
        if !CGPreflightScreenCaptureAccess() {
            // Shows the system's permission prompt the first time; later calls return at once.
            CGRequestScreenCaptureAccess();
            return Err(SCREEN_RECORDING_NEEDED.into());
        }
        let deadline = Instant::now() + START_TIMEOUT;
        let content = shareable_content(deadline)?;
        // SAFETY: plain ScreenCaptureKit calls with valid arguments, made before the stream starts.
        unsafe {
            let displays = content.displays();
            let main = CGMainDisplayID();
            let display = displays.iter().find(|display| display.displayID() == main).or_else(|| displays.firstObject())
                .ok_or("No display is available to capture system audio from.")?;
            let filter = SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new());
            let config = SCStreamConfiguration::new();
            config.setCapturesAudio(true);
            // macOS attributes audio to the responsible app, so a development build run from a
            // terminal also leaves out sounds started from that terminal (e.g. `afplay`).
            config.setExcludesCurrentProcessAudio(true);
            config.setSampleRate(sample_rate as isize);
            config.setChannelCount(channels as isize);
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime { value: 1, timescale: 1, flags: CMTimeFlags::Valid, epoch: 0 });
            config.setShowsCursor(false);

            let output = Output::new(Box::new(deliver));
            let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(ProtocolObject::from_ref(&*output)));
            let queue = DispatchQueue::new("cluelyrs.system-audio", DispatchQueueAttr::SERIAL);
            // The screen output is added too, so its frames are delivered (and dropped) rather than
            // logged as having nowhere to go.
            for kind in [SCStreamOutputType::Audio, SCStreamOutputType::Screen] {
                stream.addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), kind, Some(&queue))
                    .map_err(|error| describe(&error))?;
            }
            let (done, result) = channel();
            stream.startCaptureWithCompletionHandler(Some(&RcBlock::new(move |error: *mut NSError| {
                let _ = done.send(match error.as_ref() { None => Ok(Instant::now()), Some(error) => Err(describe(error)) });
            })));
            // From here on, dropping `capture` stops the stream, even one that starts after the deadline.
            let mut capture = Self { stream, _output: output, _queue: queue, started: Instant::now() };
            capture.started = wait(&result, deadline)??;
            Ok(capture)
        }
    }

    /// When capture started (the stream reported it running).
    pub fn started(&self) -> Instant { self.started }
}

impl Drop for SystemAudio {
    fn drop(&mut self) {
        let (done, result) = channel();
        // SAFETY: stopping a stream that was started; the handler only signals completion.
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&RcBlock::new(move |_: *mut NSError| { let _ = done.send(()); }))) };
        let _ = result.recv_timeout(STOP_TIMEOUT);
    }
}

/// The displays and windows ScreenCaptureKit may capture. Fails without Screen Recording.
fn shareable_content(deadline: Instant) -> Result<Retained<SCShareableContent>, String> {
    /// The content is an immutable snapshot, safe to hand from ScreenCaptureKit's queue to this thread.
    struct Content(Retained<SCShareableContent>);
    unsafe impl Send for Content {}
    let (done, result) = channel();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: ScreenCaptureKit passes either valid objects or null.
        let found = match unsafe { (Retained::retain(content), error.as_ref()) } {
            (Some(content), _) => Ok(Content(content)),
            (None, Some(error)) => Err(describe(error)),
            (None, None) => Err("ScreenCaptureKit returned nothing to capture.".to_string()),
        };
        let _ = done.send(found);
    });
    // SAFETY: the handler matches the declared block type.
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, true, &handler) };
    wait(&result, deadline)?.map(|content| content.0)
}

fn wait<T>(result: &Receiver<T>, deadline: Instant) -> Result<T, String> {
    result.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|_| "System audio capture didn't start in time.".to_string())
}

/// A ScreenCaptureKit error as Live panel text.
fn describe(error: &NSError) -> String {
    if error.code() == SCStreamErrorCode::UserDeclined.0 { return SCREEN_RECORDING_NEEDED.into(); }
    format!("System audio capture failed: {}", error.localizedDescription())
}

/// Copy the samples out of an audio sample buffer, with the moment its first frame was captured.
/// Only linear PCM f32 (what ScreenCaptureKit delivers) is accepted.
///
/// # Safety
/// `buffer` must be a valid sample buffer.
unsafe fn read_audio(buffer: &CMSampleBuffer) -> Option<SystemAudioBuffer> {
    unsafe {
        if !buffer.is_valid() || !buffer.data_is_ready() { return None; }
        let format = buffer.format_description()?;
        let description = CMAudioFormatDescriptionGetStreamBasicDescription(&format).as_ref()?;
        let float = description.mFormatFlags & kAudioFormatFlagIsFloat != 0 && description.mBitsPerChannel == 32;
        if description.mFormatID != kAudioFormatLinearPCM || !float || description.mChannelsPerFrame == 0 { return None; }
        let (channels, frames) = (description.mChannelsPerFrame as usize, buffer.num_samples().max(0) as usize);

        let mut needed = 0usize;
        buffer.audio_buffer_list_with_retained_block_buffer(&mut needed, null_mut(), 0, None, None, 0, null_mut());
        // u64 storage keeps the list's pointers aligned.
        let mut storage = vec![0u64; needed.div_ceil(8).max(1)];
        let list = storage.as_mut_ptr().cast::<AudioBufferList>();
        let mut block: *mut CMBlockBuffer = null_mut();
        let status = buffer.audio_buffer_list_with_retained_block_buffer(null_mut(), list, needed, None, None,
            kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, &mut block);
        // The block buffer owns the sample memory until this function returns.
        let _block = CFRetained::from_raw(NonNull::new(block)?);
        if status != 0 { return None; }
        let buffers = std::slice::from_raw_parts((*list).mBuffers.as_ptr(), (*list).mNumberBuffers as usize);
        let plane = |index: usize| buffers.get(index).filter(|b| !b.mData.is_null())
            .map(|b| std::slice::from_raw_parts(b.mData.cast::<f32>(), b.mDataByteSize as usize / 4)).unwrap_or(&[]);
        let samples = if description.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 {
            interleave(&(0..channels).map(plane).collect::<Vec<_>>(), frames)
        } else {
            let data = plane(0);
            data[..data.len().min(frames * channels)].to_vec()
        };
        Some(SystemAudioBuffer { captured: captured_at(buffer), sample_rate: description.mSampleRate as u32, channels: channels as u16, samples })
    }
}

/// Planar channels → interleaved frames. A short plane is padded with silence.
fn interleave(planes: &[&[f32]], frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * planes.len()];
    for (channel, plane) in planes.iter().enumerate() {
        for (frame, sample) in plane.iter().take(frames).enumerate() { out[frame * planes.len() + channel] = *sample; }
    }
    out
}

/// The buffer's presentation time is on the host clock; its age on that clock, taken from now,
/// gives the capture moment as an `Instant`. An unusable timestamp counts as captured on arrival.
unsafe fn captured_at(buffer: &CMSampleBuffer) -> Instant {
    let now = Instant::now();
    // SAFETY: reading the timestamp of a valid sample buffer and the host clock.
    let (stamp, host_now) = unsafe { (buffer.presentation_time_stamp(), CMClock::host_time_clock().time()) };
    if !stamp.flags.contains(CMTimeFlags::Valid) { return now; }
    let age = unsafe { host_now.seconds() - stamp.seconds() };
    if !(0.0..5.0).contains(&age) { return now; }
    now.checked_sub(Duration::from_secs_f64(age)).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_channels_interleave_and_short_planes_pad_with_silence() {
        assert_eq!(interleave(&[&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0]], 3), vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        assert_eq!(interleave(&[&[1.0, 2.0], &[-1.0]], 2), vec![1.0, -1.0, 2.0, 0.0]);
        assert_eq!(interleave(&[&[0.5, 0.25, 0.125]], 2), vec![0.5, 0.25]);
    }
}
