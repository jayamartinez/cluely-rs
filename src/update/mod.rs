//! Update checks: is a newer CluelyRS published on GitHub Releases? Notify only; nothing is
//! downloaded or installed. `github` asks GitHub; this module holds the version rules, the state
//! Settings › About and the overlay show, and when the automatic checks run.
//!
//! The release found keeps its download per platform (`Release::asset_for`) for a later
//! "Download and restart"; for now the overlay and About only open its release page.

use std::cmp::Ordering;
use std::fmt;
use std::time::Duration;

mod github;

pub use github::{CheckError, check};

/// The first automatic check waits this long after launch, so it never competes with startup.
pub const LAUNCH_DELAY: Duration = Duration::from_secs(10);
/// Automatic checks after the first.
pub const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// A semantic version ("1.2.3", "v0.2.0-rc.1"); build metadata ("+abc") is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Identifier>,
}

/// One dot-separated part of a prerelease ("rc", "1").
#[derive(Clone, Debug, PartialEq, Eq)]
enum Identifier { Numeric(u64), Text(String) }

impl Version {
    /// Parses "1.2.3" with an optional leading "v", prerelease and build metadata. "1.2" and "1"
    /// read as "1.2.0" and "1.0.0", as release tags are sometimes written.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(version, _)| version);
        let (core, pre) = match text.split_once('-') { Some((core, pre)) => (core, Some(pre)), None => (text, None) };
        let number = |part: &str| (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())).then(|| part.parse::<u64>().ok()).flatten();
        let mut parts = core.split('.');
        let major = number(parts.next()?)?;
        let minor = parts.next().map_or(Some(0), number)?;
        let patch = parts.next().map_or(Some(0), number)?;
        if parts.next().is_some() { return None; }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre.split('.').map(|part| {
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') { return None; }
                Some(match number(part) { Some(value) => Identifier::Numeric(value), None => Identifier::Text(part.to_string()) })
            }).collect::<Option<Vec<_>>>()?,
        };
        Some(Self { major, minor, patch, pre })
    }

    /// This build's version.
    pub fn current() -> Self { Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is a semantic version") }

    pub fn is_prerelease(&self) -> bool { !self.pre.is_empty() }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| {
            // A release sorts after its prereleases; prereleases compare part by part, numbers
            // below words, and a shorter list first when one is a prefix of the other.
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Ord for Identifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(a), Self::Numeric(b)) => a.cmp(b),
            (Self::Numeric(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Identifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (index, part) in self.pre.iter().enumerate() {
            f.write_str(if index == 0 { "-" } else { "." })?;
            match part { Identifier::Numeric(value) => write!(f, "{value}")?, Identifier::Text(text) => f.write_str(text)? }
        }
        Ok(())
    }
}

/// The newest published release.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// The release notes as written (Markdown).
    pub notes: String,
    /// Its page on GitHub, where it can be downloaded.
    pub page: String,
    /// When it was published, "2026-10-07".
    pub published: Option<String>,
    pub assets: Vec<Asset>,
}

/// A file attached to a release.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform { MacOs, Windows }

impl Platform {
    pub fn current() -> Self { if cfg!(target_os = "macos") { Self::MacOs } else { Self::Windows } }
}

impl Release {
    /// The download for `platform` (a macOS disk image or app archive, a Windows installer), for
    /// the later "Download and restart". `None` when the release has none.
    pub fn asset_for(&self, platform: Platform) -> Option<&Asset> {
        let suffixes: &[&str] = match platform {
            Platform::MacOs => &[".dmg", ".app.tar.gz"],
            Platform::Windows => &[".exe", ".msi"],
        };
        suffixes.iter().find_map(|suffix| self.assets.iter().find(|asset| asset.name.to_ascii_lowercase().ends_with(suffix)))
    }
}

/// What a check found, compared with this build.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Nothing has been published yet (today's repository). Not an error.
    NoReleases,
    UpToDate,
    Available(Release),
}

impl Outcome {
    /// `latest` against `current`. An older latest release (a newer local build) is up to date.
    pub fn of(latest: Option<Release>, current: &Version) -> Self {
        match latest {
            None => Self::NoReleases,
            Some(release) if release.version > *current => Self::Available(release),
            Some(_) => Self::UpToDate,
        }
    }
}

/// What Settings › About shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// Not checked since launch.
    #[default]
    NotChecked,
    Checking,
    UpToDate,
    NoReleases,
    Available,
    /// Offline, timed out, rate limited or an unexpected reply; shown calmly with Retry.
    Failed,
}

/// Update state for this run: the last result and the newer release, if one was found.
#[derive(Debug, Default)]
pub struct Tracker {
    pub status: Status,
    /// The newer release last found. Kept when a later check fails, so the notice stays.
    pub latest: Option<Release>,
    /// Whether a check has run since launch (the launch check, or one the user started).
    pub checked_this_run: bool,
}

impl Tracker {
    /// Start a check; `false` when one is already running.
    pub fn begin(&mut self) -> bool {
        if self.status == Status::Checking { return false; }
        self.status = Status::Checking;
        self.checked_this_run = true;
        true
    }

    pub fn finish(&mut self, result: Result<Outcome, CheckError>) {
        self.status = match result {
            Ok(Outcome::NoReleases) => { self.latest = None; Status::NoReleases }
            Ok(Outcome::UpToDate) => { self.latest = None; Status::UpToDate }
            Ok(Outcome::Available(release)) => { self.latest = Some(release); Status::Available }
            Err(_) => Status::Failed,
        };
    }

    /// The release the overlay's notice offers: newer than this build, not dismissed.
    pub fn notice(&self, dismissed: &str) -> Option<&Release> {
        self.latest.as_ref().filter(|release| release.version.to_string() != dismissed.trim_start_matches(['v', 'V']))
    }
}

/// Whether an automatic check is due.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
    /// Settings › About › "Check for updates automatically".
    pub automatic: bool,
    /// During Live nothing is checked; the check waits until Live ends.
    pub live: bool,
    pub checking: bool,
    pub checked_this_run: bool,
    pub since_launch: Duration,
    /// Unix seconds of the last check (any run), from settings.
    pub last_checked: Option<u64>,
    pub now: u64,
}

impl Schedule {
    pub fn due(&self) -> bool {
        if !self.automatic || self.live || self.checking { return false; }
        if !self.checked_this_run { return self.since_launch >= LAUNCH_DELAY; }
        // A clock set back counts as due too, once; the check then records the new time.
        self.last_checked.is_none_or(|then| then > self.now || self.now - then >= INTERVAL.as_secs())
    }
}

/// Up to `max` lines of release notes for a glance, and how many more there are. Headings,
/// blank lines and GitHub's "Full Changelog" link are left out; list markers and Markdown
/// emphasis are dropped; long lines are shortened.
pub fn note_lines(notes: &str, max: usize) -> (Vec<String>, usize) {
    const LONGEST: usize = 110;
    let lines: Vec<String> = notes.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("**Full Changelog**") && !line.starts_with("<!--"))
        .map(|line| {
            let line = line.strip_prefix(['-', '*', '+']).map_or(line, str::trim_start);
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            let line = if digits > 0 && line[digits..].starts_with(". ") { line[digits + 2..].trim_start() } else { line };
            let line = line.replace("**", "").replace('`', "");
            if line.chars().count() > LONGEST { format!("{}…", line.chars().take(LONGEST - 1).collect::<String>().trim_end()) } else { line }
        })
        .filter(|line| !line.is_empty())
        .collect();
    let more = lines.len().saturating_sub(max);
    (lines.into_iter().take(max).collect(), more)
}

/// "2026-10-07…" → "Oct 7".
pub fn released_label(published: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut parts = published.get(..10)?.split('-');
    let (_, month, day) = (parts.next()?, parts.next()?.parse::<usize>().ok()?, parts.next()?.parse::<u32>().ok()?);
    Some(format!("{} {day}", MONTHS.get(month.checked_sub(1)?)?))
}

/// How long ago a check ran: "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago_label(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ if seconds < 2 * 86400 => "yesterday".into(),
        _ => format!("{} days ago", seconds / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version { Version::parse(text).unwrap_or_else(|| panic!("{text} parses")) }

    fn release(version: &str) -> Release {
        Release { version: v(version), notes: String::new(), page: String::new(), published: None, assets: Vec::new() }
    }

    #[test]
    fn versions_parse_with_or_without_a_v_and_compare_as_semver() {
        assert_eq!(v("v0.2.0"), v("0.2.0"));
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("1.0.0+build.5"), v("1.0.0"));
        assert_eq!(v("0.10.0-rc.1").to_string(), "0.10.0-rc.1");
        for bad in ["", "v", "1.2.3.4", "1..2", "x.1.0", "1.0.0-", "1.0.0-rc..1", "1.0.0-rc_1", " 1.2.-3"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        // Numbers, not text: 0.10 is after 0.9.
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        // The semver precedence example, in order.
        let ordered = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"];
        for pair in ordered.windows(2) { assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]); }
        assert!(v("1.0.0-rc.1").is_prerelease() && !v("1.0.0").is_prerelease());
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn only_a_newer_release_is_an_update_and_downgrades_are_up_to_date() {
        let current = v("0.2.0");
        assert_eq!(Outcome::of(None, &current), Outcome::NoReleases);
        assert_eq!(Outcome::of(Some(release("0.2.0")), &current), Outcome::UpToDate);
        assert_eq!(Outcome::of(Some(release("0.1.9")), &current), Outcome::UpToDate, "a newer local build");
        assert_eq!(Outcome::of(Some(release("0.2.1")), &current), Outcome::Available(release("0.2.1")));
        // This build being a prerelease of the latest: the release is newer.
        assert!(matches!(Outcome::of(Some(release("0.3.0")), &v("0.3.0-rc.1")), Outcome::Available(_)));
    }

    #[test]
    fn the_tracker_keeps_a_found_release_through_a_failed_check_and_honours_dismissal() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.status, Status::NotChecked);
        assert!(tracker.begin());
        assert!(!tracker.begin(), "one check at a time");
        tracker.finish(Ok(Outcome::Available(release("0.2.0"))));
        assert_eq!((tracker.status, tracker.notice("").map(|r| r.version.to_string())), (Status::Available, Some("0.2.0".into())));
        assert!(tracker.notice("0.2.0").is_none() && tracker.notice("v0.2.0").is_none(), "dismissed for that version");
        assert!(tracker.begin());
        tracker.finish(Err(CheckError::Offline));
        assert_eq!(tracker.status, Status::Failed);
        assert!(tracker.notice("").is_some(), "still newer than this build");
        // A newer release than the one dismissed shows again.
        assert!(tracker.begin());
        tracker.finish(Ok(Outcome::Available(release("0.3.0"))));
        assert!(tracker.notice("0.2.0").is_some());
        assert!(tracker.begin());
        tracker.finish(Ok(Outcome::NoReleases));
        assert_eq!((tracker.status, tracker.latest.is_none()), (Status::NoReleases, true));
    }

    #[test]
    fn automatic_checks_wait_for_launch_skip_live_and_repeat_every_six_hours() {
        let hours = |h: u64| h * 3600;
        let base = Schedule { automatic: true, live: false, checking: false, checked_this_run: false,
            since_launch: Duration::from_secs(3), last_checked: Some(1_000_000), now: 1_000_000 + 60 };
        assert!(!base.due(), "not in the first seconds after launch");
        let launched = Schedule { since_launch: LAUNCH_DELAY, ..base };
        assert!(launched.due(), "the launch check runs even if the last run checked a minute ago");
        assert!(!Schedule { live: true, ..launched }.due(), "never during Live");
        assert!(!Schedule { automatic: false, ..launched }.due(), "the toggle is off");
        assert!(!Schedule { checking: true, ..launched }.due());
        let later = Schedule { checked_this_run: true, since_launch: Duration::from_secs(hours(1)), ..base };
        assert!(!Schedule { now: 1_000_000 + hours(6) - 1, ..later }.due());
        assert!(Schedule { now: 1_000_000 + hours(6), ..later }.due());
        assert!(Schedule { now: 1_000_000 - 5, ..later }.due(), "the clock went back");
        assert!(Schedule { last_checked: None, ..later }.due());
        assert!(!Schedule { live: true, now: 1_000_000 + hours(7), ..later }.due(), "waits for Live to end");
    }

    #[test]
    fn release_notes_read_as_a_few_plain_lines() {
        let notes = "## What's Changed\n\n* **Modes**: switch the meeting context from the overlay\n- Settings › About tells you when a new version is out\r\n\
                     1. Faster first answer with `claude`\n+ Fourth\n\n<!-- hidden -->\n**Full Changelog**: https://github.com/x/y/compare/a...b\n";
        let (lines, more) = note_lines(notes, 3);
        assert_eq!(lines, ["Modes: switch the meeting context from the overlay", "Settings › About tells you when a new version is out", "Faster first answer with claude"]);
        assert_eq!(more, 1);
        assert_eq!(note_lines("", 3), (Vec::new(), 0));
        let (long, _) = note_lines(&"word ".repeat(60), 3);
        assert!(long[0].ends_with('…') && long[0].chars().count() <= 110);
    }

    #[test]
    fn dates_and_ages_read_naturally() {
        assert_eq!(released_label("2026-10-07T12:00:00Z").as_deref(), Some("Oct 7"));
        assert_eq!(released_label("2026-13-07"), None);
        assert_eq!(released_label("soon"), None);
        assert_eq!(ago_label(100, 130), "just now");
        assert_eq!(ago_label(100, 100 + 120), "2 min ago");
        assert_eq!(ago_label(100, 100 + 3 * 3600), "3 h ago");
        assert_eq!(ago_label(100, 100 + 30 * 3600), "yesterday");
        assert_eq!(ago_label(100, 100 + 5 * 86400), "5 days ago");
        assert_eq!(ago_label(200, 100), "just now", "a clock set back");
    }

    #[test]
    fn each_platform_finds_its_own_download() {
        let asset = |name: &str| Asset { name: name.into(), url: format!("https://example.invalid/{name}"), size: 1 };
        let mut found = release("0.2.0");
        found.assets = vec![asset("CluelyRS-0.2.0.app.tar.gz"), asset("CluelyRS-0.2.0-setup.exe"), asset("CluelyRS-0.2.0.dmg"), asset("checksums.txt")];
        assert_eq!(found.asset_for(Platform::MacOs).map(|a| a.name.as_str()), Some("CluelyRS-0.2.0.dmg"));
        assert_eq!(found.asset_for(Platform::Windows).map(|a| a.name.as_str()), Some("CluelyRS-0.2.0-setup.exe"));
        found.assets.clear();
        assert_eq!(found.asset_for(Platform::current()), None);
    }
}
