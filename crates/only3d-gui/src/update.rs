//! Telling people about new versions: at most once a day the window asks GitHub for only3d's
//! newest release, and if it's newer, a chip in the status bar links to it. Nothing about you or
//! your projects is sent; Preferences turns it off.

use std::sync::{Arc, Mutex};

use eframe::egui;
use serde_json::Value;

const API: &str = "https://api.github.com/repos/onatyap/only3d/releases?per_page=10";
/// Where the releases are, for people.
pub const RELEASES: &str = "https://github.com/onatyap/only3d/releases";

/// A release newer than this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Newer {
    pub version: String,
    pub url: String,
}

/// Filled in by the background check (`None` until then, or when this build is current).
pub type Slot = Arc<Mutex<Option<Newer>>>;

/// Ask in the background; the window redraws when the answer is in.
pub fn check_in_background(ctx: egui::Context) -> Slot {
    let slot: Slot = Arc::default();
    let s = slot.clone();
    std::thread::spawn(move || {
        let agent = format!("User-Agent: only3d/{}", env!("CARGO_PKG_VERSION"));
        let Ok(out) = only3d_model::proc::command("curl").args(["-fsS", "-m", "8", "-H", "Accept: application/vnd.github+json", "-H", &agent, API]).output() else { return };
        if let Some(n) = newest(&String::from_utf8_lossy(&out.stdout), env!("CARGO_PKG_VERSION")) {
            *s.lock().unwrap_or_else(|p| p.into_inner()) = Some(n);
            ctx.request_repaint();
        }
    });
    slot
}

/// The newest release in GitHub's release list that is newer than `current` (drafts aside;
/// pre-releases count, since every only3d release is one for now).
pub fn newest(json: &str, current: &str) -> Option<Newer> {
    let cur = version(current)?;
    let list: Value = serde_json::from_str(json).ok()?;
    list.as_array()?
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            Some((version(tag)?, tag.trim_start_matches('v').to_string(), r["html_url"].as_str()?.to_string()))
        })
        .filter(|(v, _, _)| *v > cur)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, version, url)| Newer { version, url })
}

/// "v0.2.1-beta" -> (0, 2, 1)
fn version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|x| x.parse::<u64>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// Seconds since the Unix epoch.
pub fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_newer_release_counts() {
        let json = r#"[
            {"tag_name": "v0.3.0", "draft": true, "html_url": "https://x/v0.3.0"},
            {"tag_name": "v0.2.1", "draft": false, "prerelease": true, "html_url": "https://x/v0.2.1"},
            {"tag_name": "v0.1.0", "draft": false, "html_url": "https://x/v0.1.0"},
            {"tag_name": "nightly", "draft": false, "html_url": "https://x/nightly"}
        ]"#;
        assert_eq!(newest(json, "0.1.0"), Some(Newer { version: "0.2.1".into(), url: "https://x/v0.2.1".into() }));
        assert_eq!(newest(json, "0.2.1"), None, "the same version");
        assert_eq!(newest(json, "0.10.0"), None, "compared as numbers, not text");
        assert_eq!(newest("not json", "0.1.0"), None);
        assert_eq!(newest(r#"{"message": "Not Found"}"#, "0.1.0"), None, "a private repo answers 404");
    }
}
