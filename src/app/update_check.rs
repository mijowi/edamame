//! GitHub latest-release check for the About modal.
//!
//! A worker thread GETs the repository's `releases/latest` endpoint,
//! extracts the `tag_name`, and reports back on the main mpsc channel
//! as [`super::AppEvent::ReleaseCheckResult`] — the same fire-and-send
//! shape as the image decode workers.  The App caches the outcome for
//! the rest of the session ([`super::App`]'s `latest_release` field) so
//! reopening the About modal never re-hits the network.
//!
//! `tag_name` is extracted with a small string scan rather than a JSON
//! crate: the response shape is stable, the field is a plain string,
//! and a malformed body just degrades to `Failed` ("unavailable" in the
//! modal).  Not worth a `serde_json` dependency.

use std::sync::mpsc;
use std::time::Duration;

use super::AppEvent;

/// Project homepage, opened by the About modal's footer button.
pub(crate) const GITHUB_URL: &str = "https://github.com/gorgonian/edamame";

const RELEASES_API_URL: &str = "https://api.github.com/repos/gorgonian/edamame/releases/latest";

/// Bound on connect / response / body phases so a slow or unreachable
/// endpoint can't pin the worker thread for the whole session.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of the release check as the About modal consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReleaseStatus {
    /// No result yet — the modal shows a spinner.
    Pending,
    /// The fetch succeeded; carries the release `tag_name` (e.g. `v0.2.0`).
    Available(String),
    /// The fetch failed (network, HTTP error, malformed body) — the
    /// modal shows "unavailable".
    Failed,
}

/// Spawn the release-check worker.  The result lands on `tx` as
/// [`AppEvent::ReleaseCheckResult`]; a dropped receiver is ignored
/// (the app is shutting down).
pub(crate) fn spawn_release_check(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        let result = fetch_latest_tag(RELEASES_API_URL);
        let _ = tx.send(AppEvent::ReleaseCheckResult(result));
    });
}

/// Blocking GET + tag extraction.  Runs on the worker thread only.
fn fetch_latest_tag(url: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(FETCH_TIMEOUT))
        .timeout_recv_response(Some(FETCH_TIMEOUT))
        .timeout_recv_body(Some(FETCH_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        // GitHub's API rejects requests without a User-Agent.
        .header("User-Agent", concat!("edamame/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| e.to_string())?;
    let bytes = response
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let body = String::from_utf8_lossy(&bytes);
    parse_tag_name(&body).ok_or_else(|| "no tag_name in release response".to_owned())
}

/// Extract the `"tag_name": "…"` value from a GitHub release JSON
/// body.  Returns `None` when the field is absent or empty.  GitHub
/// tag names never contain escaped quotes, so a plain scan to the next
/// `"` is sufficient.
pub(crate) fn parse_tag_name(body: &str) -> Option<String> {
    let rest = &body[body.find("\"tag_name\"")? + "\"tag_name\"".len()..];
    let rest = rest[rest.find(':')? + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let tag = &rest[..rest.find('"')?];
    (!tag.is_empty()).then(|| tag.to_owned())
}

/// Annotation appended to the fetched release in the About modal's
/// version block, comparing it against the installed version.  Both
/// sides tolerate a leading `v`.  Unparseable versions fall back to
/// string equality (matching → up to date, otherwise no annotation
/// rather than a guess).
pub(crate) fn release_suffix(installed: &str, latest: &str) -> &'static str {
    use std::cmp::Ordering;
    match (parse_version(installed), parse_version(latest)) {
        (Some(a), Some(b)) => match b.cmp(&a) {
            Ordering::Greater => " (update available)",
            Ordering::Equal => " (up to date)",
            Ordering::Less => " (ahead of release)",
        },
        _ if installed.trim_start_matches('v') == latest.trim_start_matches('v') => " (up to date)",
        _ => "",
    }
}

/// Parse `v1.2.3` / `1.2.3` into numeric segments for ordering.
fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_name_extracts_from_release_json() {
        let body = r#"{"url":"https://api.github.com/…","tag_name":"v0.2.1","name":"v0.2.1"}"#;
        assert_eq!(parse_tag_name(body), Some("v0.2.1".to_owned()));
    }

    #[test]
    fn parse_tag_name_tolerates_whitespace_around_colon() {
        let body = "{\n  \"tag_name\" :  \"v1.0.0\"\n}";
        assert_eq!(parse_tag_name(body), Some("v1.0.0".to_owned()));
    }

    #[test]
    fn parse_tag_name_rejects_missing_or_empty_field() {
        assert_eq!(parse_tag_name(r#"{"message":"Not Found"}"#), None);
        assert_eq!(parse_tag_name(r#"{"tag_name":""}"#), None);
        assert_eq!(parse_tag_name(""), None);
        assert_eq!(parse_tag_name(r#"{"tag_name":42}"#), None);
    }

    #[test]
    fn release_suffix_compares_numeric_versions() {
        assert_eq!(release_suffix("0.1.0", "v0.1.0"), " (up to date)");
        assert_eq!(release_suffix("0.1.0", "v0.2.0"), " (update available)");
        assert_eq!(release_suffix("0.1.0", "v0.1.1"), " (update available)");
        assert_eq!(release_suffix("0.2.0", "v0.1.9"), " (ahead of release)");
        // Numeric, not lexicographic: 0.10.0 > 0.9.0.
        assert_eq!(release_suffix("0.9.0", "v0.10.0"), " (update available)");
        // A longer tuple with the same prefix is newer.
        assert_eq!(release_suffix("1.0", "v1.0.1"), " (update available)");
    }

    #[test]
    fn release_suffix_falls_back_to_string_equality() {
        assert_eq!(release_suffix("0.1.0-beta", "v0.1.0-beta"), " (up to date)");
        assert_eq!(release_suffix("0.1.0", "nightly"), "");
    }
}
