//! IP Tray Monitor — shows the public IP in the Windows tray.
//!
//! * green icon — IP matches the target (or no target is set)
//! * red icon   — IP differs from the target (+ a Windows notification)
//! * gray icon  — IP can't be determined (no connection)

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod net;
mod target;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod sys;

#[cfg(windows)]
fn main() {
    use native_windows_gui as nwg;

    let Some(_instance) = sys::SingleInstance::acquire() else {
        // Already running — nothing to do.
        return;
    };

    if let Err(e) = nwg::init() {
        eprintln!("Failed to initialise the UI: {e}");
        return;
    }
    let _ = nwg::Font::set_global_family("Segoe UI");

    let (cfg, first_run, config_error) = match config::load() {
        config::LoadResult::Loaded(c) => (c, false, None),
        config::LoadResult::FirstRun(c) => (c, true, None),
        config::LoadResult::Invalid(c, e) => (c, false, Some(e)),
    };

    if first_run {
        // Autostart is on by default; the user can switch it off in Settings.
        let _ = sys::set_autostart(true);
        let _ = config::save(&cfg);
    } else if sys::autostart_enabled() {
        // Keep the registry entry pointing at the current exe location.
        let _ = sys::set_autostart(true);
    }

    let app = match app::App::build(cfg) {
        Ok(app) => app,
        Err(e) => {
            nwg::fatal_message("IP Tray Monitor", &format!("Failed to start: {e}"));
        }
    };
    let bindings = app::bind(&app);

    if first_run {
        app.welcome();
    }
    if let Some(e) = config_error {
        app.warn_config(&e);
    }

    app.start_worker();
    nwg::dispatch_thread_events();

    app.stop_worker();
    bindings.unbind();
    // Dropping `app` removes the tray icon.
}

#[cfg(not(windows))]
fn main() {
    eprintln!("IP Tray Monitor runs on Windows only.");
}
