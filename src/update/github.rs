//! Asks GitHub for the repository's releases: one unauthenticated GET, sent with only a
//! User-Agent and the usual Accept headers (no identifiers, no telemetry). GitHub allows 60 such
//! requests an hour per address; a rate-limited, failed or unreadable reply is a calm "couldn't
//! check", never an error dialog.

use std::time::Duration;

use serde::Deserialize;

use super::{Asset, Outcome, Release, Version};

const API: &str = "https://api.github.com";
const REPO: &str = "jayamartinez/cluely-rs";
/// Release pages live under this; anything else in a reply opens the releases list instead.
const RELEASES_PAGE: &str = "https://github.com/jayamartinez/cluely-rs/releases";
const TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Ten releases with their notes and assets are far below this.
const MAX_BODY: u64 = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// No connection, DNS failure or timeout.
    Offline,
    /// GitHub's hourly limit for unauthenticated requests (403 or 429).
    RateLimited,
    /// Any other unexpected status.
    Status(u16),
    /// A reply that isn't the expected JSON.
    Invalid,
}

/// Check GitHub for a release newer than `current`. Blocking; call it off the UI thread.
pub fn check(current: &Version) -> Result<Outcome, CheckError> {
    check_at(API, current)
}

/// `check` against another API address: the tests' local server.
fn check_at(api: &str, current: &Version) -> Result<Outcome, CheckError> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(format!("CluelyRS/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let response = agent.get(format!("{api}/repos/{REPO}/releases?per_page=10"))
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .call()
        .map_err(|_| CheckError::Offline)?;
    match response.status().as_u16() {
        200 => {}
        // The repository can't be seen (renamed or private): nothing published to offer.
        404 => return Ok(Outcome::NoReleases),
        403 | 429 => return Err(CheckError::RateLimited),
        status => return Err(CheckError::Status(status)),
    }
    let body = response.into_body().into_with_config().limit(MAX_BODY).read_to_string().map_err(|error| match error {
        ureq::Error::BodyExceedsLimit(_) => CheckError::Invalid,
        _ => CheckError::Offline,
    })?;
    Ok(Outcome::of(latest_release(&body)?, current))
}

#[derive(Deserialize)]
struct WireRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<WireAsset>,
}

#[derive(Deserialize)]
struct WireAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// The newest published release in GitHub's release list: drafts, prereleases (flagged or
/// versioned as one) and tags that aren't versions are skipped. `None` when nothing is left.
fn latest_release(json: &str) -> Result<Option<Release>, CheckError> {
    let releases: Vec<WireRelease> = serde_json::from_str(json).map_err(|_| CheckError::Invalid)?;
    Ok(releases.into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| Some((Version::parse(&release.tag_name).filter(|version| !version.is_prerelease())?, release)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(version, release)| Release {
            version,
            notes: release.body.unwrap_or_default(),
            page: if release.html_url.starts_with(&format!("{RELEASES_PAGE}/")) { release.html_url } else { RELEASES_PAGE.to_string() },
            published: release.published_at.and_then(|at| at.get(..10).map(str::to_string)),
            assets: release.assets.into_iter()
                .map(|asset| Asset { name: asset.name, url: asset.browser_download_url, size: asset.size })
                .collect(),
        }))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;

    fn wire(tag: &str, draft: bool, prerelease: bool) -> String {
        format!(r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":{prerelease},"html_url":"{RELEASES_PAGE}/tag/{tag}",
            "body":"Notes for {tag}","published_at":"2026-10-07T12:00:00Z",
            "assets":[{{"name":"CluelyRS-{tag}.dmg","browser_download_url":"https://github.com/{REPO}/releases/download/{tag}/CluelyRS.dmg","size":42}}]}}"#)
    }

    #[test]
    fn the_newest_published_release_is_picked_from_the_list() {
        let json = format!("[{},{},{},{},{},{}]",
            wire("v0.4.0", true, false), wire("v0.3.0", false, true), wire("v0.3.1-rc.1", false, false),
            wire("v0.2.0", false, false), wire("nightly", false, false), wire("v0.10.0", false, false));
        let latest = latest_release(&json).unwrap().expect("a release");
        assert_eq!(latest.version.to_string(), "0.10.0", "draft, prereleases and non-versions skipped; numeric order");
        assert_eq!(latest.page, format!("{RELEASES_PAGE}/tag/v0.10.0"));
        assert_eq!((latest.notes.as_str(), latest.published.as_deref()), ("Notes for v0.10.0", Some("2026-10-07")));
        assert_eq!(latest.assets[0], Asset { name: "CluelyRS-v0.10.0.dmg".into(),
            url: format!("https://github.com/{REPO}/releases/download/v0.10.0/CluelyRS.dmg"), size: 42 });
    }

    #[test]
    fn no_releases_only_drafts_or_odd_replies_are_handled() {
        assert_eq!(latest_release("[]"), Ok(None));
        assert_eq!(latest_release(&format!("[{}]", wire("v1.0.0", true, false))), Ok(None), "drafts aren't published");
        assert_eq!(latest_release(&format!("[{}]", wire("v1.0.0", false, true))), Ok(None));
        assert_eq!(latest_release(r#"{"message":"Not Found"}"#), Err(CheckError::Invalid));
        assert_eq!(latest_release("<html>"), Err(CheckError::Invalid));
        // Missing optional fields, and a page outside the repository's releases.
        let minimal = latest_release(r#"[{"tag_name":"0.2.0","html_url":"https://evil.example/x"}]"#).unwrap().unwrap();
        assert_eq!((minimal.page.as_str(), minimal.notes.as_str(), minimal.published, minimal.assets.len()), (RELEASES_PAGE, "", None, 0));
    }

    /// Serve one canned reply on a local port; returns the API address and the request it got.
    fn serve(status: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let reply = format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        let (sender, requests) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            while reader.read_line(&mut request).is_ok_and(|read| read > 0) && !request.ends_with("\r\n\r\n") {}
            let _ = reader.get_mut().write_all(reply.as_bytes());
            let _ = sender.send(request);
        });
        (address, requests)
    }

    #[test]
    fn a_check_against_a_local_server_reads_each_reply() {
        let current = Version::parse("0.1.0").unwrap();
        let (api, requests) = serve("200 OK", &format!("[{}]", wire("v0.2.0", false, false)));
        let Ok(Outcome::Available(release)) = check_at(&api, &current) else { panic!("an update") };
        assert_eq!(release.version.to_string(), "0.2.0");
        let request = requests.recv().unwrap().to_ascii_lowercase();
        assert!(request.starts_with(&format!("get /repos/{REPO}/releases?per_page=10 http/1.1")), "{request}");
        assert!(request.contains(&format!("user-agent: cluelyrs/{}", env!("CARGO_PKG_VERSION"))), "{request}");
        assert!(request.contains("accept: application/vnd.github+json"));
        assert!(!request.contains("authorization") && !request.contains("cookie"), "nothing identifying is sent");

        let (api, _) = serve("200 OK", "[]");
        assert_eq!(check_at(&api, &current), Ok(Outcome::NoReleases), "no releases yet is not an error");
        let (api, _) = serve("200 OK", &format!("[{}]", wire("v0.1.0", false, false)));
        assert_eq!(check_at(&api, &current), Ok(Outcome::UpToDate));
        let (api, _) = serve("404 Not Found", r#"{"message":"Not Found"}"#);
        assert_eq!(check_at(&api, &current), Ok(Outcome::NoReleases));
        let (api, _) = serve("403 Forbidden", r#"{"message":"API rate limit exceeded"}"#);
        assert_eq!(check_at(&api, &current), Err(CheckError::RateLimited));
        let (api, _) = serve("429 Too Many Requests", "{}");
        assert_eq!(check_at(&api, &current), Err(CheckError::RateLimited));
        let (api, _) = serve("502 Bad Gateway", "");
        assert_eq!(check_at(&api, &current), Err(CheckError::Status(502)));
        let (api, _) = serve("200 OK", "not json");
        assert_eq!(check_at(&api, &current), Err(CheckError::Invalid));

        // Nothing listening: offline.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        assert_eq!(check_at(&address, &current), Err(CheckError::Offline));
    }
}
