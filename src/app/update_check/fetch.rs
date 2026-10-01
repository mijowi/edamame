//! The network half: one detached worker, one GET, one event back.  The main thread never
//! blocks on this, and what crosses the channel is already parsed and bounded by
//! [`super::parse`] — a small fixed-shape value, not a response body.

use std::sync::mpsc;
use std::time::Duration;

use super::parse;
use super::status::ReleaseInfo;
use crate::app::AppEvent;

/// Project homepage, opened by the About modal's footer button.
pub(crate) const GITHUB_URL: &str = "https://github.com/mijowi/edamame";

/// The one endpoint this feature talks to.  A compile-time constant, never derived from the
/// open document or from config — see `docs/dev/security-invariants.md`.
const RELEASES_API_URL: &str = "https://api.github.com/repos/mijowi/edamame/releases/latest";

/// Bounds connect / response / body phases so an unreachable endpoint can't pin the worker.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Body cap, well under ureq's 10 MB default.  A release object is a few kilobytes; refusing
/// early keeps a pathological reply from being materialized at all.
const BODY_CAP_BYTES: u64 = 256 * 1024;

/// Browser URL for a specific release.  Built from the tag rather than the response's
/// `html_url` so the parser stays limited to the two fields it needs.
pub(crate) fn release_url(tag: &str) -> String {
    format!("{GITHUB_URL}/releases/tag/{tag}")
}

/// Spawn the release-check worker.  The result lands on `tx` as
/// [`AppEvent::ReleaseCheckResult`]; a dropped receiver is ignored (the app is shutting down).
pub(crate) fn spawn_release_check(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        let result = fetch_release(RELEASES_API_URL);
        let _ = tx.send(AppEvent::ReleaseCheckResult(result));
    });
}

/// Blocking GET + parse.  Runs on the worker thread only.
fn fetch_release(url: &str) -> Result<ReleaseInfo, String> {
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
        .with_config()
        .limit(BODY_CAP_BYTES)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let body = String::from_utf8_lossy(&bytes);
    parse::parse_release(&body).ok_or_else(|| "no tag_name in release response".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_url_names_the_tag() {
        assert_eq!(
            release_url("v0.2.0"),
            "https://github.com/mijowi/edamame/releases/tag/v0.2.0"
        );
    }
}
