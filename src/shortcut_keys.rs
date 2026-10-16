//! What the app in front sees of CluelyRS's shortcuts and of typing into the overlay.
//!
//! The system's global hotkeys (`RegisterHotKey`, Carbon hot keys) only eat the press of a shortcut's
//! last key: the app still saw Ctrl, Alt or ⌘ go down, Alt repeat, and the arrow or \ come back up.
//! Typing into the overlay took the keyboard from the app, which saw its window lose focus. So a
//! keyboard hook (Windows, `platform::windows_keys`) or event tap (macOS, `platform::macos_keys`)
//! feeds every key to [`Chords`], which decides here, apart from either system:
//!
//! - A modifier is held back while it could still become one of the shortcuts claimed right now.
//!   When the shortcut completes, none of its keys reach the app. When it doesn't (Ctrl+C, ⌘-click,
//!   a lone Shift tap), the held-back keys are sent first, so the app sees them in the order pressed.
//! - While the overlay is typed into, every key goes to the overlay instead, and the app keeps focus.
//!
//! Key codes and the events kept for replay are each platform's own.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

pub const CONTROL: u8 = 1;
pub const ALT: u8 = 2;
pub const SHIFT: u8 = 4;
/// ⌘ on macOS, the Windows key on Windows.
pub const SUPER: u8 = 8;

/// With no input at all for this long, held-back keys are forgotten: their releases happened where
/// the hook can't see them (the Ctrl+Alt+Del screen, the macOS login window). A key that is really
/// still held keeps repeating.
const STALE: Duration = Duration::from_secs(3);

/// A shortcut as the hook matches it: these modifiers held, then `key` pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut { pub mods: u8, pub key: u16 }

impl Shortcut {
    /// `None` for a key `key_code` can't name; that shortcut keeps only the system hotkey's behaviour.
    pub fn of(hotkey: &HotKey, key_code: fn(Code) -> Option<u16>) -> Option<Self> {
        let mut mods = 0;
        for (modifier, bit) in [(Modifiers::CONTROL, CONTROL), (Modifiers::ALT, ALT), (Modifiers::SHIFT, SHIFT), (Modifiers::SUPER | Modifiers::META, SUPER)] {
            if hotkey.mods.intersects(modifier) { mods |= bit; }
        }
        Some(Self { mods, key: key_code(hotkey.key)? })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict<E> {
    /// The app gets the event.
    Pass,
    /// Nobody gets it.
    Swallow,
    /// Swallow it, send these held-back events, then send it again.
    Replay(Vec<E>),
    /// The overlay gets it as typing; the app doesn't.
    Type,
}

/// A shortcut whose key went down or came back up (macOS tracks held arrows from both).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fired { Pressed(usize), Released(usize) }

/// Why a held key's events are being eaten.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Eaten { Not, Typed, Shortcut(usize) }

/// The decisions for one keyboard. `E` is the event kept to replay a held-back modifier.
pub struct Chords<E> {
    /// Which modifier a key code is, if any.
    modifier: fn(u16) -> Option<u8>,
    shortcuts: Vec<(Shortcut, bool)>,
    held: [bool; 256],
    /// Modifiers held back, in the order they went down.
    hidden: Vec<(u16, E)>,
    eaten: [Eaten; 256],
    /// A shortcut completed while `hidden` was held, so releasing them shows nothing.
    fired: bool,
    typing: bool,
    last_input: Option<Instant>,
}

impl<E> Chords<E> {
    pub fn new(shortcuts: Vec<Shortcut>, modifier: fn(u16) -> Option<u8>) -> Self {
        Self { modifier, shortcuts: shortcuts.into_iter().map(|shortcut| (shortcut, false)).collect(), held: [false; 256], hidden: Vec::new(),
            eaten: [Eaten::Not; 256], fired: false, typing: false, last_input: None }
    }

    pub fn set_active(&mut self, index: usize, active: bool) {
        if let Some((_, slot)) = self.shortcuts.get_mut(index) { *slot = active; }
    }

    /// While on, keys that aren't a shortcut go to the overlay (`Verdict::Type`).
    pub fn set_typing(&mut self, typing: bool) {
        self.typing = typing;
        // Modifiers held back as typing starts belong to the shortcut that started it.
        if typing && !self.hidden.is_empty() { self.fired = true; }
    }

    pub fn typing(&self) -> bool { self.typing }

    pub fn hiding(&self) -> bool { !self.hidden.is_empty() }

    fn active(&self) -> impl Iterator<Item = (usize, Shortcut)> + '_ {
        self.shortcuts.iter().enumerate().filter(|(_, (_, active))| *active).map(|(index, (shortcut, _))| (index, *shortcut))
    }

    /// The modifiers held, whether or not the system has seen them.
    pub fn held_mods(&self) -> u8 {
        (0..256u16).filter(|code| self.held[*code as usize]).filter_map(self.modifier).fold(0, |mods, bit| mods | bit)
    }

    fn hidden_mods(&self) -> u8 { self.hidden.iter().filter_map(|(code, _)| (self.modifier)(*code)).fold(0, |mods, bit| mods | bit) }

    /// Note input at `now`, first forgetting held-back keys if there was none for `STALE`.
    fn input(&mut self, now: Instant) {
        if self.last_input.is_some_and(|last| now.saturating_duration_since(last) > STALE) {
            for (code, _) in self.hidden.drain(..) { self.held[code as usize] = false; }
            for code in 0..256 {
                if self.eaten[code] != Eaten::Not { self.eaten[code] = Eaten::Not; self.held[code] = false; }
            }
            self.fired = false;
        }
        self.last_input = Some(now);
    }

    /// Hand the held-back modifiers to the app: their presses, to send ahead of the current event.
    fn reveal(&mut self) -> Vec<E> {
        self.fired = false;
        self.hidden.drain(..).map(|(_, event)| event).collect()
    }

    /// The app gets the held-back modifiers, then the current event.
    fn replay(&mut self) -> Verdict<E> {
        if self.hidden.is_empty() { Verdict::Pass } else { Verdict::Replay(self.reveal()) }
    }

    /// A key going down (`up` false, including repeats) or up. `event` is kept if the key is held back.
    pub fn key(&mut self, code: u16, up: bool, event: E, now: Instant) -> (Verdict<E>, Option<Fired>) {
        self.input(now);
        if code >= 256 { return (Verdict::Pass, None); }
        let slot = code as usize;
        if up { return self.release(code); }
        let repeat = std::mem::replace(&mut self.held[slot], true);
        if self.eaten[slot] != Eaten::Not { return (Verdict::Swallow, None); }
        if let Some(bit) = (self.modifier)(code) {
            if self.typing {
                self.eaten[slot] = Eaten::Typed;
                return (Verdict::Swallow, None);
            }
            let hidden = self.hidden.iter().any(|(each, _)| *each == code);
            if repeat { return (if hidden { Verdict::Swallow } else { Verdict::Pass }, None); }
            // Hold it back only while every modifier down is held back and together they could
            // still become a claimed shortcut; otherwise the app gets them all, in order.
            let others = self.held_mods() & !bit;
            let wanted = self.hidden_mods() | bit;
            if others == self.hidden_mods() & !bit && self.active().any(|(_, shortcut)| shortcut.mods & wanted == wanted) {
                if !hidden { self.hidden.push((code, event)); }
                return (Verdict::Swallow, None);
            }
            return (self.replay(), None);
        }
        let mods = self.held_mods();
        let completed = self.active().find(|(_, shortcut)| shortcut.key == code && shortcut.mods == mods).map(|(index, _)| index);
        if let Some(index) = completed {
            self.eaten[slot] = Eaten::Shortcut(index);
            self.fired = true;
            return (Verdict::Swallow, (!repeat).then_some(Fired::Pressed(index)));
        }
        if self.typing {
            self.eaten[slot] = Eaten::Typed;
            return (Verdict::Type, None);
        }
        (self.replay(), None)
    }

    fn release(&mut self, code: u16) -> (Verdict<E>, Option<Fired>) {
        let slot = code as usize;
        self.held[slot] = false;
        match std::mem::replace(&mut self.eaten[slot], Eaten::Not) {
            Eaten::Shortcut(index) => return (Verdict::Swallow, Some(Fired::Released(index))),
            Eaten::Typed => return (Verdict::Swallow, None),
            Eaten::Not => {}
        }
        let Some(position) = self.hidden.iter().position(|(each, _)| *each == code) else { return (Verdict::Pass, None) };
        if self.fired {
            self.hidden.remove(position);
            if self.hidden.is_empty() { self.fired = false; }
            return (Verdict::Swallow, None);
        }
        // Released without completing a shortcut: a lone Ctrl or Shift tap, shown as it happened.
        (self.replay(), None)
    }

    /// A click or wheel turn: Ctrl+click and ⌘-click need the held-back modifiers down first.
    pub fn mouse_press(&mut self, now: Instant) -> Verdict<E> {
        self.input(now);
        self.replay()
    }

    /// Mouse movement: input that keeps held-back keys from going stale.
    pub fn mouse_move(&mut self, now: Instant) { self.input(now); }

    /// Whether `code` is down as far as the hook has seen; held-back keys never reach the system's
    /// own key state.
    pub fn held(&self, code: u16, now: Instant) -> bool {
        self.last_input.is_some_and(|last| now.saturating_duration_since(last) <= STALE) && self.held.get(code as usize).copied().unwrap_or(false)
    }
}

/// What the hook hands the overlay while it is typed into.
#[derive(Debug)]
pub enum Typed {
    Key(gpui::Keystroke),
    /// Typing ended without the overlay asking (a click in another app, Alt+Tab, ⌘Tab).
    Lost,
}

static TYPED: Mutex<Option<UnboundedSender<Typed>>> = Mutex::new(None);
/// The window being typed into, as the platform names it; 0 when none is.
static TYPING_INTO: AtomicUsize = AtomicUsize::new(0);

/// Keys typed into the overlay without it taking the keyboard. One receiver at a time.
pub fn typed_keys() -> UnboundedReceiver<Typed> {
    let (sender, receiver) = unbounded();
    *TYPED.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(sender);
    receiver
}

pub(crate) fn send_typed(typed: Typed) {
    if let Some(sender) = TYPED.lock().unwrap_or_else(|poison| poison.into_inner()).as_ref() { let _ = sender.unbounded_send(typed); }
}

pub(crate) fn set_typing_into(window: usize) { TYPING_INTO.store(window, Ordering::Relaxed); }

pub(crate) fn typing_into() -> usize { TYPING_INTO.load(Ordering::Relaxed) }

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_KEY: u16 = 1;
    const ALT_KEY: u16 = 2;
    const SHIFT_KEY: u16 = 3;
    const ENTER: u16 = 10;
    const UP: u16 = 11;
    const BACKSLASH: u16 = 12;
    const C: u16 = 13;

    fn modifier(code: u16) -> Option<u8> {
        match code { CTRL_KEY => Some(CONTROL), ALT_KEY => Some(ALT), SHIFT_KEY => Some(SHIFT), _ => None }
    }

    /// Assist (claimed always), Show / hide (always), Move up (only while the overlay is on screen).
    fn chords(overlay_visible: bool) -> Chords<u16> {
        let mut chords = Chords::new(vec![
            Shortcut { mods: CONTROL, key: ENTER },
            Shortcut { mods: CONTROL, key: BACKSLASH },
            Shortcut { mods: CONTROL | ALT, key: UP },
        ], modifier);
        chords.set_active(0, true);
        chords.set_active(1, true);
        chords.set_active(2, overlay_visible);
        chords
    }

    fn down(code: u16) -> (u16, bool) { (code, false) }
    fn up(code: u16) -> (u16, bool) { (code, true) }

    /// Runs the events at one instant, returning each verdict and what fired. A key's event is its code.
    fn run(chords: &mut Chords<u16>, events: &[(u16, bool)]) -> (Vec<Verdict<u16>>, Vec<Fired>) {
        let now = Instant::now();
        let mut fired = Vec::new();
        let verdicts = events.iter().map(|(code, up)| {
            let (verdict, shortcut) = chords.key(*code, *up, *code, now);
            fired.extend(shortcut);
            verdict
        }).collect();
        (verdicts, fired)
    }

    #[test]
    fn a_shortcut_shows_the_app_none_of_its_keys() {
        let mut chords = chords(true);
        // Ctrl Alt ↑ with Alt and ↑ repeating, released in another order (the move screenshot).
        let (verdicts, fired) = run(&mut chords, &[down(CTRL_KEY), down(ALT_KEY), down(ALT_KEY), down(UP), down(UP), up(CTRL_KEY), up(UP), up(ALT_KEY)]);
        assert!(verdicts.iter().all(|verdict| *verdict == Verdict::Swallow), "{verdicts:?}");
        assert_eq!(fired, vec![Fired::Pressed(2), Fired::Released(2)], "pressed once, not per repeat");

        let (verdicts, fired) = run(&mut chords, &[down(CTRL_KEY), down(BACKSLASH), up(BACKSLASH), up(CTRL_KEY)]);
        assert!(verdicts.iter().all(|verdict| *verdict == Verdict::Swallow), "{verdicts:?}");
        assert_eq!(fired, vec![Fired::Pressed(1), Fired::Released(1)]);
    }

    #[test]
    fn other_ctrl_chords_reach_the_app_in_order() {
        let mut chords = chords(true);
        let (verdicts, fired) = run(&mut chords, &[down(CTRL_KEY), down(C), up(C), up(CTRL_KEY)]);
        assert_eq!(verdicts, vec![Verdict::Swallow, Verdict::Replay(vec![CTRL_KEY]), Verdict::Pass, Verdict::Pass]);
        assert!(fired.is_empty());
    }

    #[test]
    fn a_lone_modifier_tap_reaches_the_app() {
        let mut chords = chords(true);
        let (verdicts, _) = run(&mut chords, &[down(CTRL_KEY), down(CTRL_KEY), up(CTRL_KEY)]);
        assert_eq!(verdicts, vec![Verdict::Swallow, Verdict::Swallow, Verdict::Replay(vec![CTRL_KEY])]);
    }

    /// Shift alone starts no claimed shortcut here, so typing capitals is untouched.
    #[test]
    fn modifiers_outside_every_claimed_shortcut_pass_straight_through() {
        let mut chords = chords(true);
        assert_eq!(run(&mut chords, &[down(SHIFT_KEY), down(C), up(C), up(SHIFT_KEY)]).0, vec![Verdict::Pass, Verdict::Pass, Verdict::Pass, Verdict::Pass]);
    }

    /// Moving is claimed only while the overlay is on screen; otherwise Ctrl Alt ↑ is the app's.
    #[test]
    fn unclaimed_shortcuts_reach_the_app() {
        let mut chords = chords(false);
        let (verdicts, fired) = run(&mut chords, &[down(CTRL_KEY), down(ALT_KEY), down(UP), up(UP)]);
        assert_eq!(verdicts, vec![Verdict::Swallow, Verdict::Replay(vec![CTRL_KEY]), Verdict::Pass, Verdict::Pass]);
        assert!(fired.is_empty());
    }

    #[test]
    fn ctrl_held_after_a_shortcut_still_works_with_other_keys() {
        let mut chords = chords(true);
        let (verdicts, fired) = run(&mut chords, &[down(CTRL_KEY), down(ENTER), up(ENTER), down(C)]);
        assert_eq!(verdicts, vec![Verdict::Swallow, Verdict::Swallow, Verdict::Swallow, Verdict::Replay(vec![CTRL_KEY])]);
        assert_eq!(fired, vec![Fired::Pressed(0), Fired::Released(0)]);
    }

    #[test]
    fn ctrl_click_presses_ctrl_before_the_click() {
        let mut chords = chords(true);
        run(&mut chords, &[down(CTRL_KEY)]);
        assert_eq!(chords.mouse_press(Instant::now()), Verdict::Replay(vec![CTRL_KEY]));
        assert_eq!(run(&mut chords, &[up(CTRL_KEY)]).0, vec![Verdict::Pass]);
        assert_eq!(chords.mouse_press(Instant::now()), Verdict::Pass);
    }

    /// Releases on the Ctrl+Alt+Del screen never reach the hook; the keys mustn't stay held back.
    #[test]
    fn held_back_keys_are_forgotten_after_a_silence() {
        let mut chords = chords(true);
        let start = Instant::now();
        chords.key(CTRL_KEY, false, CTRL_KEY, start);
        chords.key(ALT_KEY, false, ALT_KEY, start);
        assert!(chords.held(CTRL_KEY, start));
        let later = start + STALE + Duration::from_millis(1);
        assert!(!chords.held(CTRL_KEY, later));
        assert_eq!(chords.key(C, false, C, later).0, Verdict::Pass);
    }

    /// Typing into the overlay: the app sees none of it, shortcuts still work, and the modifiers of
    /// the shortcut that started it stay hidden as they're let go.
    #[test]
    fn typing_goes_to_the_overlay_only() {
        let mut chords = chords(true);
        run(&mut chords, &[down(CTRL_KEY)]);
        chords.set_typing(true);
        let (verdicts, fired) = run(&mut chords, &[up(CTRL_KEY), down(SHIFT_KEY), down(C), up(C), up(SHIFT_KEY), down(CTRL_KEY), down(ENTER), up(ENTER), up(CTRL_KEY)]);
        assert_eq!(verdicts, vec![Verdict::Swallow, Verdict::Swallow, Verdict::Type, Verdict::Swallow, Verdict::Swallow, Verdict::Swallow, Verdict::Swallow,
            Verdict::Swallow, Verdict::Swallow]);
        assert_eq!(fired, vec![Fired::Pressed(0), Fired::Released(0)]);
        assert_eq!(chords.held_mods(), 0);
    }

    #[test]
    fn every_default_shortcut_has_a_key_code() {
        for (_, accelerator) in crate::hotkeys::DEFAULTS {
            let hotkey: HotKey = accelerator.parse().unwrap();
            assert!(Shortcut::of(&hotkey, crate::platform::shortcut_key).is_some(), "{accelerator}");
        }
    }
}
