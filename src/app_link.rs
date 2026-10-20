//! `cluelyrs://` links, which hand focus back from the browser after Google sign-in ("Back to
//! CluelyRS"). Anything can open such a link (a web page, another app), so it is untrusted input:
//! only the exact paths below are recognised, nothing in a link is ever passed on, and anything
//! else is ignored.

/// The URL scheme CluelyRS registers (Windows: HKCU\Software\Classes; macOS: CFBundleURLTypes).
pub const SCHEME: &str = "cluelyrs";
/// What the sign-in pages' "Back to CluelyRS" button opens.
pub const CALENDAR: &str = "cluelyrs://calendar";
/// Longer than any link CluelyRS makes, so a huge argument is rejected before it's looked at.
const MAX_LEN: usize = 64;

/// What a recognised link asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppLink {
    /// Show Settings › Calendar (never the overlay).
    Calendar,
}

/// The link in a command-line argument or an opened URL, when it's one CluelyRS knows. The scheme
/// and path are matched whole (case-insensitively, as browsers may change their case), with at most
/// a trailing slash; a query, fragment, credentials, port or any other path is refused.
pub fn parse(text: &str) -> Option<AppLink> {
    let text = text.trim();
    if text.len() > MAX_LEN || !text.is_ascii() { return None; }
    let link = text.to_ascii_lowercase();
    let rest = link.strip_prefix(SCHEME)?.strip_prefix("://")?;
    match rest.strip_suffix('/').unwrap_or(rest) {
        "calendar" => Some(AppLink::Calendar),
        _ => None,
    }
}

/// The first recognised link among the process arguments (Windows passes it as `"%1"`).
pub fn from_args(args: impl IntoIterator<Item = String>) -> Option<AppLink> { args.into_iter().find_map(|arg| parse(&arg)) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_calendar_link_is_recognised() {
        for link in ["cluelyrs://calendar", "cluelyrs://calendar/", "CLUELYRS://Calendar", "  cluelyrs://calendar \r\n"] {
            assert_eq!(parse(link), Some(AppLink::Calendar), "{link:?}");
        }
        for link in ["", "cluelyrs://", "cluelyrs:calendar", "cluelyrs:///calendar", "cluelyrs://calendar?x=1", "cluelyrs://calendar#a",
            "cluelyrs://calendar//", "cluelyrs://calendar/../settings", "cluelyrs://user@calendar", "cluelyrs://calendar:80",
            "cluelyrs://settings", "http://calendar", "cluelyrsx://calendar", "cluelyrs://calendar\" --flag", "cluelyrs://cal\u{e9}ndar",
            &format!("cluelyrs://calendar{}", "/".repeat(80))] {
            assert_eq!(parse(link), None, "{link:?}");
        }
    }

    #[test]
    fn the_link_is_found_among_the_arguments() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert_eq!(from_args(args(&["C:\\CluelyRS.exe", "cluelyrs://calendar/"])), Some(AppLink::Calendar));
        assert_eq!(from_args(args(&["C:\\CluelyRS.exe"])), None);
        assert_eq!(from_args(args(&["C:\\CluelyRS.exe", "cluelyrs://evil"])), None);
        assert_eq!(parse(CALENDAR), Some(AppLink::Calendar));
    }
}
