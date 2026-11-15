//! Update check: a newer GitHub release shows the UPDATE chip.

use crate::*;
use std::time::Duration;

/// GitHub repo checked for newer releases, and where the update chip leads.
pub(crate) const UPDATE_REPO: &str = "mroldzeppelin/tx-open-studio";

pub(crate) const RELEASES_URL: &str = "https://github.com/mroldzeppelin/tx-open-studio/releases/latest";

/// update-informer caches the answer on disk and asks GitHub at most this
/// often (the first run only creates the cache).
pub(crate) const UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Looks for a newer GitHub release in a background thread and, if there
/// is one, shows the update chip. Any failure (offline, no release yet,
/// rate limit) just means no chip; the UI thread never waits for it.
/// Disabled by setting TX_NO_UPDATE_CHECK.
pub(crate) fn start_update_check(ui: slint::Weak<AppWindow>) {
    // Dev hook: TX_FAKE_UPDATE=0.2.0 shows the chip without any network.
    if let Ok(fake) = std::env::var("TX_FAKE_UPDATE") {
        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_update_version(format!("v{fake}").into()));
        return;
    }
    if std::env::var_os("TX_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        use update_informer::{registry, Check};
        let informer = update_informer::new(registry::GitHub, UPDATE_REPO, env!("CARGO_PKG_VERSION"))
            .interval(UPDATE_INTERVAL);
        if let Ok(Some(version)) = informer.check_version() {
            let label = version.to_string(); // "v1.2.3"
            let _ = ui.upgrade_in_event_loop(move |ui| ui.set_update_version(label.into()));
        }
    });
    if let Err(e) = spawned {
        eprintln!("update check not started: {e}");
    }
}

/// Opens the latest release page in the default browser.
pub(crate) fn open_releases_page() {
    match std::process::Command::new("xdg-open").arg(RELEASES_URL).spawn() {
        // Reap it in the background so it doesn't linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("xdg-open {RELEASES_URL} failed: {e}"),
    }
}
