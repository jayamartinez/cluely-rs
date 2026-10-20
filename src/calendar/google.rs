//! The Google Calendar API, read-only: the account's calendars (calendarList) and the next day's
//! events on the chosen ones (events.list, recurring events expanded by Google with
//! `singleEvents`). Times are asked for in UTC, so every timed event comes back with an offset.
//! Blocking; call it off the UI thread. Tokens are sent only in the Authorization header and never
//! end up in errors.

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::oauth::{self, AccessToken, AuthError, Client, Endpoints, encode};
use super::{Attendee, CalendarInfo, Event, When};

pub const API: &str = "https://www.googleapis.com";
/// How far ahead events are read.
pub const WINDOW_SECONDS: i64 = 24 * 3600;
/// How far back: a meeting that started a few minutes ago still has its row.
const LOOK_BACK_SECONDS: i64 = 10 * 60;
const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BODY: u64 = 8 << 20;
/// Pages per list; a day of events or an account's calendars fit in one.
const MAX_PAGES: usize = 5;
/// Kinds of event that aren't meetings.
const NOT_MEETINGS: [&str; 3] = ["workingLocation", "outOfOffice", "focusTime"];

/// The HTTP client for Google: short timeouts, statuses returned rather than raised, and only a
/// User-Agent besides what each request adds.
pub(super) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(format!("CluelyRS/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

pub(super) fn read_json<T: DeserializeOwned>(response: ureq::http::Response<ureq::Body>) -> Result<T, ()> {
    let body = response.into_body().into_with_config().limit(MAX_BODY).read_to_string().map_err(|_| ())?;
    serde_json::from_str(&body).map_err(|_| ())
}

/// Why a sync didn't finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncError {
    /// The token was refused or revoked: sign in again.
    SignInAgain,
    Offline,
    /// Google's per-user or per-project limit; try again later.
    RateLimited,
    /// The Calendar API isn't enabled for the client ID's Google Cloud project.
    ApiDisabled,
    BadClient,
    Status(u16),
    Invalid,
}

impl SyncError {
    /// A short status for the account row ("Can't sync · offline").
    pub fn message(&self) -> String {
        match self {
            Self::SignInAgain => "Signed out by Google · sign in again".into(),
            Self::Offline => "Can't sync · offline".into(),
            Self::RateLimited => "Can't sync · Google is busy, retrying".into(),
            Self::ApiDisabled => "Calendar API isn't enabled for this client ID".into(),
            Self::BadClient => "Google refused the client ID".into(),
            Self::Status(status) => format!("Can't sync · error {status}"),
            Self::Invalid => "Can't sync · unreadable reply".into(),
        }
    }
}

impl From<AuthError> for SyncError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::SignInAgain | AuthError::MissingScope | AuthError::Denied => Self::SignInAgain,
            AuthError::BadClient => Self::BadClient,
            AuthError::Status(status) => Self::Status(status),
            AuthError::Invalid => Self::Invalid,
            _ => Self::Offline,
        }
    }
}

/// What one sync read.
#[derive(Clone, Debug, PartialEq)]
pub struct Synced {
    pub calendars: Vec<CalendarInfo>,
    pub events: Vec<Event>,
}

impl Synced {
    /// The account's address: its primary calendar's id.
    pub fn account(&self) -> Option<&str> { self.calendars.iter().find(|calendar| calendar.primary).map(|calendar| calendar.id.as_str()) }
}

/// Read the calendars and the chosen ones' events with `access`, refreshing it first when it's
/// missing or stale, and once more when Google refuses it. Returns the token to keep using.
pub fn sync_account(endpoints: &Endpoints, api: &str, client: &Client, refresh_token: &str, access: Option<AccessToken>,
    chosen: &dyn Fn(&CalendarInfo) -> bool, now: i64) -> (Option<AccessToken>, Result<Synced, SyncError>) {
    let mut access = access.filter(AccessToken::fresh);
    for attempt in 0..2 {
        let token = match access.clone() {
            Some(token) => token,
            None => match oauth::refresh(endpoints, client, refresh_token) {
                Ok(token) => { access = Some(token.clone()); token }
                Err(error) => return (None, Err(error.into())),
            },
        };
        match sync(api, &token.token, chosen, now) {
            Err(ApiError::Unauthorized) if attempt == 0 => access = None,
            Err(ApiError::Unauthorized) => return (None, Err(SyncError::SignInAgain)),
            result => return (access, result.map_err(ApiError::into_sync)),
        }
    }
    unreachable!("the second attempt always returns")
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ApiError {
    Unauthorized,
    NotFound,
    Sync(SyncError),
}

impl ApiError {
    fn into_sync(self) -> SyncError {
        match self { Self::Unauthorized => SyncError::SignInAgain, Self::NotFound => SyncError::Status(404), Self::Sync(error) => error }
    }
}

/// One sync with a token known to be current.
fn sync(api: &str, token: &str, chosen: &dyn Fn(&CalendarInfo) -> bool, now: i64) -> Result<Synced, ApiError> {
    let calendars = calendar_list(api, token)?;
    let mut events = Vec::new();
    for calendar in calendars.iter().filter(|calendar| chosen(calendar)) {
        match calendar_events(api, token, &calendar.id, now) {
            Ok(found) => events.extend(found),
            // Unsubscribed since the list was read.
            Err(ApiError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    events.sort_by_key(|event| event.start);
    Ok(Synced { calendars, events })
}

/// GET with the token, through every page, each page's items parsed by `items`.
fn get_pages<T, W: DeserializeOwned + Paged>(url: &str, query: &[(&str, String)], token: &str, mut items: impl FnMut(W) -> Vec<T>) -> Result<Vec<T>, ApiError> {
    let agent = agent();
    let mut found = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = agent.get(url).header("authorization", format!("Bearer {token}")).header("accept", "application/json");
        for (name, value) in query { request = request.query(*name, value); }
        if let Some(page) = &page_token { request = request.query("pageToken", page); }
        let response = request.call().map_err(|_| ApiError::Sync(SyncError::Offline))?;
        let status = response.status().as_u16();
        if status != 200 {
            let body = response.into_body().into_with_config().limit(64 << 10).read_to_string().unwrap_or_default();
            return Err(api_error(status, &body));
        }
        let page: W = read_json(response).map_err(|_| ApiError::Sync(SyncError::Invalid))?;
        page_token = page.next_page().filter(|token| !token.is_empty());
        found.extend(items(page));
        if page_token.is_none() { break; }
    }
    Ok(found)
}

/// A status and error body as what to do about it.
fn api_error(status: u16, body: &str) -> ApiError {
    let has = |reason: &str| body.contains(reason);
    match status {
        401 => ApiError::Unauthorized,
        404 | 410 => ApiError::NotFound,
        429 => ApiError::Sync(SyncError::RateLimited),
        403 if has("accessNotConfigured") || has("SERVICE_DISABLED") => ApiError::Sync(SyncError::ApiDisabled),
        403 if has("ateLimitExceeded") || has("quotaExceeded") || has("RATE_LIMIT_EXCEEDED") => ApiError::Sync(SyncError::RateLimited),
        403 if has("insufficientPermissions") || has("ACCESS_TOKEN_SCOPE_INSUFFICIENT") => ApiError::Sync(SyncError::SignInAgain),
        status => ApiError::Sync(SyncError::Status(status)),
    }
}

trait Paged { fn next_page(&self) -> Option<String>; }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireCalendarList {
    #[serde(default)]
    items: Vec<WireCalendar>,
    #[serde(default)]
    next_page_token: Option<String>,
}

impl Paged for WireCalendarList { fn next_page(&self) -> Option<String> { self.next_page_token.clone() } }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireCalendar {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    summary_override: Option<String>,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    background_color: Option<String>,
    #[serde(default)]
    selected: bool,
    #[serde(default)]
    deleted: bool,
}

fn calendar_list(api: &str, token: &str) -> Result<Vec<CalendarInfo>, ApiError> {
    let url = format!("{api}/calendar/v3/users/me/calendarList");
    let mut calendars = get_pages(&url, &[("minAccessRole", "reader".into()), ("maxResults", "250".into())], token, |page: WireCalendarList| {
        page.items.into_iter().filter(|calendar| !calendar.deleted).map(|calendar| CalendarInfo {
            name: calendar.summary_override.filter(|name| !name.trim().is_empty()).unwrap_or(calendar.summary),
            color: calendar.background_color.as_deref().and_then(parse_color),
            primary: calendar.primary, shown_in_google: calendar.selected, id: calendar.id,
        }).collect()
    })?;
    // The primary calendar first, as Google lists it.
    calendars.sort_by_key(|calendar| !calendar.primary);
    Ok(calendars)
}

/// "#9fe1e7" as 0x9fe1e7.
fn parse_color(hex: &str) -> Option<u32> {
    let digits = hex.strip_prefix('#')?;
    (digits.len() == 6).then(|| u32::from_str_radix(digits, 16).ok()).flatten()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEvents {
    #[serde(default)]
    items: Vec<WireEvent>,
    #[serde(default)]
    next_page_token: Option<String>,
}

impl Paged for WireEvents { fn next_page(&self) -> Option<String> { self.next_page_token.clone() } }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEvent {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    start: Option<WireTime>,
    #[serde(default)]
    end: Option<WireTime>,
    #[serde(default)]
    attendees: Vec<WireAttendee>,
    #[serde(default)]
    hangout_link: Option<String>,
    #[serde(default)]
    conference_data: Option<WireConference>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    transparency: Option<String>,
    #[serde(default)]
    event_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTime {
    #[serde(default)]
    date_time: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    time_zone: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAttendee {
    #[serde(default)]
    email: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default, rename = "self")]
    is_self: bool,
    #[serde(default)]
    response_status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireConference {
    #[serde(default)]
    entry_points: Vec<WireEntryPoint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEntryPoint {
    #[serde(default)]
    entry_point_type: String,
    #[serde(default)]
    uri: String,
}

fn calendar_events(api: &str, token: &str, calendar: &str, now: i64) -> Result<Vec<Event>, ApiError> {
    let url = format!("{api}/calendar/v3/calendars/{}/events", encode(calendar));
    let query = [
        ("timeMin", rfc3339(now - LOOK_BACK_SECONDS)),
        ("timeMax", rfc3339(now + WINDOW_SECONDS)),
        ("singleEvents", "true".into()),
        ("orderBy", "startTime".into()),
        ("maxResults", "250".into()),
        ("timeZone", "UTC".into()),
    ];
    get_pages(&url, &query, token, |page: WireEvents| page.items.into_iter().filter_map(|event| parse_event(event, calendar)).collect())
}

fn rfc3339(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// An API event as CluelyRS keeps it; `None` for cancelled events, ones that aren't meetings, and
/// ones whose times can't be read.
fn parse_event(event: WireEvent, calendar: &str) -> Option<Event> {
    if event.status.as_deref() == Some("cancelled") { return None; }
    if event.event_type.as_deref().is_some_and(|kind| NOT_MEETINGS.contains(&kind)) { return None; }
    let (start, end) = (event.start?, event.end?);
    let time_zone = start.time_zone.clone().filter(|zone| !zone.is_empty());
    let meeting_link = event.hangout_link.into_iter()
        .chain(event.conference_data.into_iter().flat_map(|data| data.entry_points).filter(|entry| entry.entry_point_type == "video").map(|entry| entry.uri))
        .chain(event.location)
        .find(|link| safe_link(link));
    Some(Event {
        id: event.id,
        calendar: calendar.to_owned(),
        title: event.summary.map(|title| title.trim().to_owned()).filter(|title| !title.is_empty()).unwrap_or_else(|| "(No title)".into()),
        start: parse_when(&start)?,
        end: parse_when(&end)?,
        time_zone,
        attendees: event.attendees.into_iter().map(|attendee| Attendee {
            declined: attendee.response_status.as_deref() == Some("declined"),
            email: attendee.email, name: attendee.display_name, is_self: attendee.is_self,
        }).collect(),
        meeting_link,
        free: event.transparency.as_deref() == Some("transparent"),
    })
}

fn parse_when(time: &WireTime) -> Option<When> {
    if let Some(at) = &time.date_time { return chrono::DateTime::parse_from_rfc3339(at).ok().map(|at| When::At(at.timestamp())); }
    chrono::NaiveDate::parse_from_str(time.date.as_deref()?, "%Y-%m-%d").ok().map(When::AllDay)
}

/// A link that may be opened from the row: https, of a sane length, with nothing odd in it.
/// Anyone can send an invitation, so its location is attacker-controlled text.
fn safe_link(link: &str) -> bool {
    link.len() <= 2048 && link.starts_with("https://") && link.len() > "https://".len() && !link.chars().any(|ch| ch.is_whitespace() || ch.is_control())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    /// Serve canned replies on a local port, one connection each, in order; returns the address and
    /// each request (request line, headers and body) as it arrived.
    pub(crate) fn serve_all(replies: Vec<(&str, String)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let replies: Vec<String> = replies.into_iter().map(|(status, body)| format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())).collect();
        let (sender, requests) = mpsc::channel();
        std::thread::spawn(move || for reply in replies {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 { break; }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") { length = value.trim().parse().unwrap_or(0); }
                request.push_str(&line);
                if line == "\r\n" { break; }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            request.push_str(&String::from_utf8_lossy(&body));
            let _ = reader.get_mut().write_all(reply.as_bytes());
            let _ = sender.send(request);
        });
        (address, requests)
    }

    pub(crate) fn serve(status: &str, body: &str) -> (String, mpsc::Receiver<String>) { serve_all(vec![(status, body.to_owned())]) }

    fn wire(json: &str) -> WireEvent { serde_json::from_str(json).unwrap() }

    #[test]
    fn timed_events_are_read_in_any_time_zone() {
        let event = parse_event(wire(r#"{"id":"e1","status":"confirmed","summary":"  Design review  ",
            "start":{"dateTime":"2026-10-20T14:00:00-04:00","timeZone":"America/New_York"},"end":{"dateTime":"2026-10-20T14:30:00-04:00","timeZone":"America/New_York"},
            "hangoutLink":"https://meet.google.com/abc-defg-hij",
            "attendees":[{"email":"me@example.com","self":true,"responseStatus":"accepted"},{"email":"priya@example.com","displayName":"Priya","responseStatus":"needsAction"}]}"#), "primary").unwrap();
        assert_eq!(event.title, "Design review");
        assert_eq!(event.start, When::At(chrono::DateTime::parse_from_rfc3339("2026-10-20T18:00:00Z").unwrap().timestamp()));
        assert_eq!(event.end.at().unwrap() - event.start.at().unwrap(), 30 * 60);
        assert_eq!(event.time_zone.as_deref(), Some("America/New_York"));
        assert_eq!(event.meeting_link.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(event.attendees.len(), 2);
        assert!(event.remindable());
        // The same moment written in UTC and in +05:30 is the same start.
        let utc = parse_event(wire(r#"{"id":"e2","start":{"dateTime":"2026-10-20T18:00:00Z"},"end":{"dateTime":"2026-10-20T18:30:00Z"}}"#), "c").unwrap();
        let india = parse_event(wire(r#"{"id":"e3","start":{"dateTime":"2026-10-20T23:30:00+05:30","timeZone":"Asia/Kolkata"},"end":{"dateTime":"2026-10-21T00:00:00+05:30"}}"#), "c").unwrap();
        assert_eq!((utc.start, india.start), (event.start, event.start));
        assert_eq!(utc.title, "(No title)");
        assert_eq!(utc.time_zone, None);
    }

    #[test]
    fn all_day_cancelled_declined_and_odd_events() {
        let all_day = parse_event(wire(r#"{"id":"h","summary":"Holiday","start":{"date":"2026-11-26"},"end":{"date":"2026-11-27"}}"#), "c").unwrap();
        assert_eq!(all_day.start, When::AllDay(chrono::NaiveDate::from_ymd_opt(2026, 11, 26).unwrap()));
        assert!(!all_day.remindable());
        assert_eq!(parse_event(wire(r#"{"id":"x","status":"cancelled"}"#), "c"), None, "cancelled occurrences have no times");
        assert_eq!(parse_event(wire(r#"{"id":"x","status":"cancelled","start":{"dateTime":"2026-10-20T18:00:00Z"},"end":{"dateTime":"2026-10-20T18:30:00Z"}}"#), "c"), None);
        assert_eq!(parse_event(wire(r#"{"id":"o","eventType":"outOfOffice","start":{"dateTime":"2026-10-20T18:00:00Z"},"end":{"dateTime":"2026-10-20T20:00:00Z"}}"#), "c"), None);
        assert_eq!(parse_event(wire(r#"{"id":"b","start":{"dateTime":"tomorrow"},"end":{"dateTime":"2026-10-20T18:30:00Z"}}"#), "c"), None);
        let declined = parse_event(wire(r#"{"id":"d","start":{"dateTime":"2026-10-20T18:00:00Z"},"end":{"dateTime":"2026-10-20T18:30:00Z"},
            "attendees":[{"email":"me@example.com","self":true,"responseStatus":"declined"}]}"#), "c").unwrap();
        assert!(declined.declined() && !declined.remindable());
        let free = parse_event(wire(r#"{"id":"f","transparency":"transparent","start":{"dateTime":"2026-10-20T18:00:00Z"},"end":{"dateTime":"2026-10-20T18:30:00Z"}}"#), "c").unwrap();
        assert!(free.free && !free.remindable());
    }

    #[test]
    fn meeting_links_are_https_only() {
        let link = |json: &str| parse_event(wire(&format!(r#"{{"id":"l","start":{{"dateTime":"2026-10-20T18:00:00Z"}},"end":{{"dateTime":"2026-10-20T18:30:00Z"}},{json}}}"#)), "c").unwrap().meeting_link;
        assert_eq!(link(r#""conferenceData":{"entryPoints":[{"entryPointType":"phone","uri":"tel:+1-555"},{"entryPointType":"video","uri":"https://zoom.us/j/1"}]}"#).as_deref(), Some("https://zoom.us/j/1"));
        assert_eq!(link(r#""location":"https://teams.microsoft.com/l/meetup-join/x""#).as_deref(), Some("https://teams.microsoft.com/l/meetup-join/x"));
        assert_eq!(link(r#""location":"Room 4""#), None);
        assert_eq!(link(r#""location":"file:///C:/Windows/System32/calc.exe""#), None);
        assert_eq!(link(r#""location":"javascript:alert(1)""#), None);
        assert_eq!(link(r#""location":"https://a.example/x y""#), None);
    }

    #[test]
    fn api_errors_say_what_to_do() {
        assert_eq!(api_error(401, ""), ApiError::Unauthorized);
        assert_eq!(api_error(429, ""), ApiError::Sync(SyncError::RateLimited));
        assert_eq!(api_error(403, r#"{"error":{"errors":[{"reason":"rateLimitExceeded"}]}}"#), ApiError::Sync(SyncError::RateLimited));
        assert_eq!(api_error(403, r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#), ApiError::Sync(SyncError::RateLimited));
        assert_eq!(api_error(403, r#"{"error":{"errors":[{"reason":"accessNotConfigured"}],"status":"PERMISSION_DENIED"}}"#), ApiError::Sync(SyncError::ApiDisabled));
        assert_eq!(api_error(403, r#"{"error":{"status":"PERMISSION_DENIED","details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}"#), ApiError::Sync(SyncError::SignInAgain));
        assert_eq!(api_error(500, ""), ApiError::Sync(SyncError::Status(500)));
    }

    const CALENDARS: &str = r##"{"items":[
        {"id":"team@group.calendar.google.com","summary":"Team","backgroundColor":"#16a765","selected":true},
        {"id":"maya@example.com","summary":"maya@example.com","summaryOverride":"Maya Chen","primary":true,"backgroundColor":"#9a9cff","selected":true},
        {"id":"en.usa#holiday@group.v.calendar.google.com","summary":"Holidays in United States","selected":false}]}"##;

    fn events_page(items: &str) -> String { format!(r#"{{"kind":"calendar#events","items":[{items}]}}"#) }

    fn endpoints(base: &str) -> Endpoints { Endpoints { auth: String::new(), token: format!("{base}/token"), revoke: format!("{base}/revoke") } }

    fn client() -> Client { Client { id: "id.apps.googleusercontent.com".into(), secret: None } }

    fn fresh(token: &str) -> AccessToken { AccessToken { token: token.into(), expires: Instant::now() + Duration::from_secs(3600) } }

    #[test]
    fn a_sync_reads_the_calendars_and_the_chosen_ones_events() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T17:00:00Z").unwrap().timestamp();
        let (api, requests) = serve_all(vec![
            ("200 OK", CALENDARS.into()),
            ("200 OK", r#"{"items":[],"nextPageToken":"p2"}"#.into()),
            ("200 OK", events_page(r#"{"id":"late","summary":"Later","start":{"dateTime":"2026-10-20T20:00:00Z"},"end":{"dateTime":"2026-10-20T21:00:00Z"}}"#)),
            ("200 OK", events_page(r#"{"id":"soon","summary":"Standup","start":{"dateTime":"2026-10-20T17:30:00Z"},"end":{"dateTime":"2026-10-20T17:45:00Z"}}"#)),
        ]);
        let chosen = |calendar: &CalendarInfo| calendar.shown_in_google;
        let (access, result) = sync_account(&endpoints(&api), &api, &client(), "1//r", Some(fresh("ya29.ok")), &chosen, now);
        let synced = result.unwrap();
        assert_eq!(access.map(|token| token.token).as_deref(), Some("ya29.ok"), "a fresh token is kept, not refreshed");
        assert_eq!(synced.account(), Some("maya@example.com"));
        let names: Vec<&str> = synced.calendars.iter().map(|calendar| calendar.name.as_str()).collect();
        assert_eq!(names, ["Maya Chen", "Team", "Holidays in United States"], "primary first, with the user's own names");
        assert_eq!(synced.calendars[0].color, Some(0x9a9cff));
        let titles: Vec<&str> = synced.events.iter().map(|event| event.title.as_str()).collect();
        assert_eq!(titles, ["Standup", "Later"], "soonest first, across calendars");

        let list = requests.recv().unwrap();
        assert!(list.starts_with("GET /calendar/v3/users/me/calendarList?minAccessRole=reader&maxResults=250 HTTP/1.1"), "{list}");
        assert!(list.to_ascii_lowercase().contains("authorization: bearer ya29.ok"));
        // The primary calendar is read first (both pages), then Team; the holidays aren't read.
        let first = requests.recv().unwrap();
        assert!(first.starts_with("GET /calendar/v3/calendars/maya%40example.com/events?timeMin=2026-10-20T16%3A50%3A00Z&timeMax=2026-10-21T17%3A00%3A00Z&singleEvents=true&orderBy=startTime&maxResults=250&timeZone=UTC HTTP/1.1"), "{first}");
        assert!(requests.recv().unwrap().contains("pageToken=p2"));
        assert!(requests.recv().unwrap().starts_with("GET /calendar/v3/calendars/team%40group.calendar.google.com/events?"));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn a_stale_or_refused_token_is_refreshed_once() {
        // No token yet: refreshed first.
        let (server, requests) = serve_all(vec![
            ("200 OK", r#"{"access_token":"ya29.new","expires_in":3599}"#.into()),
            ("200 OK", r#"{"items":[]}"#.into()),
        ]);
        let (access, result) = sync_account(&endpoints(&server), &server, &client(), "1//r", None, &|_| true, 0);
        assert_eq!(result, Ok(Synced { calendars: vec![], events: vec![] }));
        assert_eq!(access.unwrap().token, "ya29.new");
        assert!(requests.recv().unwrap().starts_with("POST /token"));
        // Refused (401): refreshed, then retried with the new token.
        let (server, requests) = serve_all(vec![
            ("401 Unauthorized", r#"{"error":{"code":401}}"#.into()),
            ("200 OK", r#"{"access_token":"ya29.second","expires_in":3599}"#.into()),
            ("200 OK", r#"{"items":[]}"#.into()),
        ]);
        let (access, result) = sync_account(&endpoints(&server), &server, &client(), "1//r", Some(fresh("ya29.old")), &|_| true, 0);
        assert!(result.is_ok());
        assert_eq!(access.unwrap().token, "ya29.second");
        let _ = requests.recv();
        let _ = requests.recv();
        assert!(requests.recv().unwrap().to_ascii_lowercase().contains("bearer ya29.second"));
        // Refused again after refreshing: sign in again.
        let (server, _) = serve_all(vec![
            ("401 Unauthorized", "{}".into()),
            ("200 OK", r#"{"access_token":"ya29.third","expires_in":3599}"#.into()),
            ("401 Unauthorized", "{}".into()),
        ]);
        let (access, result) = sync_account(&endpoints(&server), &server, &client(), "1//r", Some(fresh("ya29.old")), &|_| true, 0);
        assert_eq!((access.is_none(), result), (true, Err(SyncError::SignInAgain)));
        // A revoked refresh token: sign in again.
        let (server, _) = serve("400 Bad Request", r#"{"error":"invalid_grant"}"#);
        assert_eq!(sync_account(&endpoints(&server), &server, &client(), "1//r", None, &|_| true, 0).1, Err(SyncError::SignInAgain));
    }

    #[test]
    fn rate_limits_disabled_apis_and_offline() {
        let (server, _) = serve("429 Too Many Requests", "{}");
        assert_eq!(sync_account(&endpoints(&server), &server, &client(), "r", Some(fresh("t")), &|_| true, 0).1, Err(SyncError::RateLimited));
        let (server, _) = serve("403 Forbidden", r#"{"error":{"code":403,"message":"Google Calendar API has not been used in project 1 before or it is disabled.","errors":[{"reason":"accessNotConfigured"}]}}"#);
        assert_eq!(sync_account(&endpoints(&server), &server, &client(), "r", Some(fresh("t")), &|_| true, 0).1, Err(SyncError::ApiDisabled));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let (access, result) = sync_account(&endpoints(&address), &address, &client(), "r", Some(fresh("t")), &|_| true, 0);
        assert_eq!(result, Err(SyncError::Offline));
        assert!(access.is_some(), "the token is kept while offline");
        assert_eq!(sync_account(&endpoints(&address), &address, &client(), "r", None, &|_| true, 0).1, Err(SyncError::Offline));
    }

    #[test]
    fn errors_never_carry_tokens() {
        for error in [SyncError::SignInAgain, SyncError::Offline, SyncError::RateLimited, SyncError::ApiDisabled, SyncError::Status(500)] {
            assert!(!error.message().contains("ya29"));
        }
    }
}
