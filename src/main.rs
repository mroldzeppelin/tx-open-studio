mod amp;
mod catalog;
mod fx;
mod global_controls;
mod link;
mod midi;
mod preset_tones;
mod presets;
mod protocol;
mod store;
mod sync;
mod tempo;
mod tone_select;
mod update;
mod wiring;
#[cfg(test)]
mod test_util;

use amp::*;
use global_controls::*;
use link::*;
use midi::*;
use presets::*;
use sync::*;
use tempo::*;
use update::*;
use protocol::{AmpKnob, Event, PresetMeta, PRESET_COUNT};

slint::include_modules!();

/// Desktop-entry id (`tx-open-studio.desktop`). GNOME on Wayland matches the
/// window's app_id against .desktop basenames to pick the dock/taskbar icon;
/// X11 shells use WM_CLASS the same way.
const APP_ID: &str = "tx-open-studio";

/// Selects the winit backend with a hook that names the window `APP_ID`
/// (Wayland app_id and X11 WM_CLASS). Must run before the first window.
fn select_backend() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attrs| {
            #[cfg(target_os = "linux")]
            let attrs = {
                use slint::winit_030::winit::platform::{wayland, x11};
                let attrs = wayland::WindowAttributesExtWayland::with_name(attrs, APP_ID, APP_ID);
                x11::WindowAttributesExtX11::with_name(attrs, APP_ID, APP_ID)
            };
            attrs
        })
        .select()
}

fn main() -> Result<(), slint::PlatformError> {
    select_backend()?;
    let ui = AppWindow::new()?;
    let version = env!("CARGO_PKG_VERSION");
    ui.set_app_version(if cfg!(debug_assertions) { format!("{version}-debug") } else { version.to_string() }.into());
    let ctx = SyncCtx::default();

    ui.set_preset(DEFAULT_PRESET);
    fx::set_cab_model_options(&ui, 0);
    show_list(&ui, &ctx.names);

    wiring::wire(&ui, &ctx);
    ctx.load_tone_cache(&ui);
    start_update_check(ui.as_weak());

    // Connect now (pedal present: same as a normal start), then keep
    // polling while any link is down (no pedal: sits in RECONNECTING).
    ctx.poll(&ui);
    let reconnect_timer = slint::Timer::default();
    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        reconnect_timer.start(slint::TimerMode::Repeated, RECONNECT_POLL, move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if ctx.link().wants_poll() {
                ctx.poll(&ui);
            }
        });
    }

    ui.run()
}
