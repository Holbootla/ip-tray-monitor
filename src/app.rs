//! Tray icon, context menu, settings window and the background IP checker.

use std::cell::RefCell;
use std::net::IpAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use native_windows_gui as nwg;

use crate::config::{self, Config, MAX_INTERVAL_SECS, MIN_INTERVAL_SECS};
use crate::net::IpFetcher;
use crate::sys;
use crate::target::{self, Status, Target};

const ICON_APP: usize = 1;
const ICON_GREEN: usize = 10;
const ICON_RED: usize = 11;
const ICON_GRAY: usize = 12;

/// Retry sooner than the normal interval while the IP can't be determined.
const RETRY_ON_ERROR_SECS: u64 = 15;

/// Messages for the background checker thread.
enum Cmd {
    CheckNow,
}

/// Result of one check, handed from the worker thread to the UI thread.
struct Outcome {
    result: Result<IpAddr, String>,
    time: String,
}

#[derive(Default)]
struct State {
    config: Config,
    targets: Vec<Target>,
    ip: Option<IpAddr>,
    error: Option<String>,
    checked_at: Option<String>,
    status: Status,
    /// IP we already warned about, so the same mismatch isn't reported twice.
    notified_ip: Option<IpAddr>,
}

#[derive(Default)]
pub struct App {
    embed: nwg::EmbedResource,
    icon_app: nwg::Icon,
    icon_green: nwg::Icon,
    icon_red: nwg::Icon,
    icon_gray: nwg::Icon,

    /// Hidden top-level window that owns the tray icon. (A message-only window
    /// would not receive the "TaskbarCreated" broadcast after Explorer restarts.)
    host: nwg::Window,
    tray: nwg::TrayNotification,
    menu: nwg::Menu,
    menu_settings: nwg::MenuItem,
    menu_sep: nwg::MenuSeparator,
    menu_exit: nwg::MenuItem,
    notice: nwg::Notice,

    settings: nwg::Window,
    lbl_current: nwg::Label,
    lbl_target: nwg::Label,
    txt_target: nwg::TextInput,
    lbl_hint: nwg::Label,
    lbl_interval: nwg::Label,
    txt_interval: nwg::TextInput,
    chk_autostart: nwg::CheckBox,
    chk_notify: nwg::CheckBox,
    btn_save: nwg::Button,
    btn_cancel: nwg::Button,

    state: RefCell<State>,
    inbox: Arc<Mutex<Option<Outcome>>>,
    interval: Arc<AtomicU64>,
    worker: RefCell<Option<mpsc::Sender<Cmd>>>,
}

impl App {
    pub fn build(config: Config) -> Result<Rc<App>, nwg::NwgError> {
        let mut app = App::default();
        let small = Some(sys::small_icon_size());

        nwg::EmbedResource::builder().build(&mut app.embed)?;
        let load = |id: usize, size| {
            app.embed
                .icon(id, size)
                .ok_or_else(|| nwg::NwgError::resource_create(format!("icon #{id} not found")))
        };
        app.icon_app = load(ICON_APP, None)?;
        app.icon_green = load(ICON_GREEN, small)?;
        app.icon_red = load(ICON_RED, small)?;
        app.icon_gray = load(ICON_GRAY, small)?;

        // ---- tray host + tray icon + menu
        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW)
            .size((1, 1))
            .position((-10_000, -10_000))
            .title("IP Tray Monitor")
            .build(&mut app.host)?;

        nwg::TrayNotification::builder()
            .parent(&app.host)
            .icon(Some(&app.icon_gray))
            .tip(Some("IP Tray Monitor\nChecking public IP…"))
            .build(&mut app.tray)?;

        nwg::Menu::builder()
            .popup(true)
            .parent(&app.host)
            .build(&mut app.menu)?;
        nwg::MenuItem::builder()
            .text("Settings")
            .parent(&app.menu)
            .build(&mut app.menu_settings)?;
        nwg::MenuSeparator::builder()
            .parent(&app.menu)
            .build(&mut app.menu_sep)?;
        nwg::MenuItem::builder()
            .text("Exit")
            .parent(&app.menu)
            .build(&mut app.menu_exit)?;

        nwg::Notice::builder().parent(&app.host).build(&mut app.notice)?;

        // ---- settings window (hidden until requested)
        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW)
            .size((400, 286))
            .center(true)
            .title("IP Tray Monitor — Settings")
            .icon(Some(&app.icon_app))
            .build(&mut app.settings)?;

        // Use the standard dialog colour for the window and every static control,
        // so labels don't show up as grey boxes on a white window.
        let bg = match app.settings.handle.hwnd() {
            Some(hwnd) => sys::use_dialog_background(hwnd),
            None => [240, 240, 240],
        };
        let w = &app.settings;
        nwg::Label::builder()
            .text("Current public IP: —")
            .position((16, 12))
            .size((368, 36))
            .background_color(Some(bg))
            .parent(w)
            .build(&mut app.lbl_current)?;
        nwg::Label::builder()
            .text("Target IP:")
            .position((16, 52))
            .size((368, 20))
            .background_color(Some(bg))
            .parent(w)
            .build(&mut app.lbl_target)?;
        nwg::TextInput::builder()
            .position((16, 74))
            .size((368, 25))
            .parent(w)
            .build(&mut app.txt_target)?;
        nwg::Label::builder()
            .text("Several: 1.2.3.4, 5.6.7.0/24. Empty = just show the IP.")
            .position((16, 103))
            .size((368, 34))
            .background_color(Some(bg))
            .parent(w)
            .build(&mut app.lbl_hint)?;
        nwg::Label::builder()
            .text("Check interval (seconds):")
            .position((16, 148))
            .size((200, 22))
            .background_color(Some(bg))
            .parent(w)
            .build(&mut app.lbl_interval)?;
        nwg::TextInput::builder()
            .position((220, 145))
            .size((80, 25))
            .parent(w)
            .build(&mut app.txt_interval)?;
        nwg::CheckBox::builder()
            .text("Start automatically with Windows")
            .position((16, 180))
            .size((368, 22))
            .background_color(Some(bg))
            .parent(w)
            .build(&mut app.chk_autostart)?;
        nwg::CheckBox::builder()
            .text("Show an alert window when the IP doesn't match")
            .position((16, 204))
            .size((368, 22))
            .background_color(Some(bg))
            .parent(w)
            .build(&mut app.chk_notify)?;
        nwg::Button::builder()
            .text("Save")
            .position((188, 242))
            .size((94, 30))
            .parent(w)
            .build(&mut app.btn_save)?;
        nwg::Button::builder()
            .text("Cancel")
            .position((290, 242))
            .size((94, 30))
            .parent(w)
            .build(&mut app.btn_cancel)?;

        {
            let mut st = app.state.borrow_mut();
            // A bad target in a hand-edited config shouldn't stop the app.
            st.targets = target::parse_targets(&config.target_ip).unwrap_or_default();
            app.interval.store(config.interval_secs(), Ordering::Relaxed);
            st.config = config;
        }

        Ok(Rc::new(app))
    }

    // ------------------------------------------------------------ worker

    pub fn start_worker(&self) {
        let fetcher = IpFetcher::new(&self.state.borrow().config.providers);
        let sender = self.notice.sender();
        let inbox = Arc::clone(&self.inbox);
        let interval = Arc::clone(&self.interval);
        let (tx, rx) = mpsc::channel::<Cmd>();

        thread::spawn(move || loop {
            let result = match &fetcher {
                Ok(f) => f.fetch(),
                Err(e) => Err(format!("TLS init failed: {e}")),
            };
            let failed = result.is_err();
            *inbox.lock().unwrap() = Some(Outcome {
                result,
                time: sys::local_time(),
            });
            sender.notice();

            let mut wait = interval.load(Ordering::Relaxed);
            if failed {
                wait = wait.min(RETRY_ON_ERROR_SECS);
            }
            match rx.recv_timeout(Duration::from_secs(wait)) {
                Ok(Cmd::CheckNow) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // Collapse bursts of requests (e.g. several adapter events) into one check.
            thread::sleep(Duration::from_millis(300));
            while rx.try_recv().is_ok() {}
        });

        // Re-check right away when the network configuration changes.
        let net_tx = tx.clone();
        thread::spawn(move || loop {
            if !sys::wait_for_address_change() {
                thread::sleep(Duration::from_secs(60));
                continue;
            }
            // Give DHCP / VPN routes a moment to settle.
            thread::sleep(Duration::from_secs(3));
            if net_tx.send(Cmd::CheckNow).is_err() {
                break;
            }
        });

        *self.worker.borrow_mut() = Some(tx);
    }

    fn check_now(&self) {
        if let Some(tx) = self.worker.borrow().as_ref() {
            let _ = tx.send(Cmd::CheckNow);
        }
    }

    pub fn stop_worker(&self) {
        self.worker.borrow_mut().take();
    }

    // ------------------------------------------------------------ status

    fn on_check_result(&self) {
        let Some(outcome) = self.inbox.lock().unwrap().take() else {
            return;
        };
        {
            let mut st = self.state.borrow_mut();
            st.checked_at = Some(outcome.time);
            match outcome.result {
                Ok(ip) => {
                    st.ip = Some(ip);
                    st.error = None;
                }
                Err(e) => {
                    st.ip = None;
                    st.error = Some(e);
                }
            }
        }
        self.refresh();
    }

    /// Recomputes the status and updates icon, tooltip and notifications.
    fn refresh(&self) {
        let mut st = self.state.borrow_mut();
        let status = target::evaluate(st.ip.as_ref(), &st.targets);
        st.status = status;

        let ip_text = st.ip.map(|ip| ip.to_string());
        let (icon, mut tip) = match status {
            Status::Unknown => (&self.icon_gray, "IP: unavailable\nNo connection".to_string()),
            Status::NoTarget => (
                &self.icon_green,
                format!("IP: {}\nTarget not set", ip_text.as_deref().unwrap_or("?")),
            ),
            Status::Match => (
                &self.icon_green,
                format!("IP: {}\nMatches target", ip_text.as_deref().unwrap_or("?")),
            ),
            Status::Mismatch => (
                &self.icon_red,
                format!(
                    "IP: {}\nMISMATCH! Expected: {}",
                    ip_text.as_deref().unwrap_or("?"),
                    st.config.target_ip.trim()
                ),
            ),
        };
        if let Some(t) = &st.checked_at {
            tip.push_str(&format!("\nChecked at {t}"));
        }
        self.tray.set_icon(icon);
        self.tray.set_tip(&truncate(&tip, 127));

        // ---- notifications
        let notify = st.config.notifications;
        match status {
            Status::Mismatch => {
                if st.notified_ip != st.ip {
                    st.notified_ip = st.ip;
                    if notify {
                        let msg = format!(
                            "The public IP address doesn't match the target!\n\n\
                             Current IP:\t{}\n\
                             Expected:\t{}\n\
                             Detected at:\t{}\n\n\
                             Traffic may be leaving outside the VPN.",
                            ip_text.as_deref().unwrap_or("?"),
                            st.config.target_ip.trim(),
                            st.checked_at.as_deref().unwrap_or("?")
                        );
                        // A blocking, always-on-top alert window (shown from its own
                        // thread so the tray keeps working while it is open).
                        sys::show_alert(msg);
                    }
                }
            }
            Status::Match => {
                if st.notified_ip.take().is_some() {
                    // The problem is gone: dismiss the alert, leave a quiet toast.
                    sys::close_alert();
                    if notify {
                        let msg = format!("IP {} matches the target again.", ip_text.as_deref().unwrap_or("?"));
                        self.balloon(&msg, "IP address OK", nwg::TrayNotificationFlags::INFO_ICON);
                    }
                }
            }
            Status::NoTarget => {
                if st.notified_ip.take().is_some() {
                    sys::close_alert();
                }
            }
            Status::Unknown => {}
        }

        self.lbl_current.set_text(&current_ip_label(&st));
    }

    fn balloon(&self, text: &str, title: &str, flags: nwg::TrayNotificationFlags) {
        self.tray
            .show(&truncate(text, 255), Some(&truncate(title, 63)), Some(flags), None);
    }

    pub fn welcome(&self) {
        self.balloon(
            "Running in the tray. Right-click the icon → Settings to set the target IP.",
            "IP Tray Monitor",
            nwg::TrayNotificationFlags::INFO_ICON,
        );
    }

    pub fn warn_config(&self, err: &str) {
        self.balloon(
            &format!("config.json is invalid, defaults are used: {err}"),
            "IP Tray Monitor",
            nwg::TrayNotificationFlags::WARNING_ICON,
        );
    }

    // ------------------------------------------------------------ settings

    fn open_settings(&self) {
        {
            let st = self.state.borrow();
            self.txt_target.set_text(&st.config.target_ip);
            self.txt_interval.set_text(&st.config.interval_secs().to_string());
            self.chk_notify.set_check_state(check(st.config.notifications));
        }
        self.chk_autostart.set_check_state(check(sys::autostart_enabled()));
        self.refresh_label_only();
        self.settings.set_visible(true);
        if let Some(hwnd) = self.settings.handle.hwnd() {
            sys::bring_to_front(hwnd);
        }
        self.txt_target.set_focus();
    }

    fn refresh_label_only(&self) {
        self.lbl_current.set_text(&current_ip_label(&self.state.borrow()));
    }

    fn save_settings(&self) {
        let target_text = self.txt_target.text().trim().to_string();
        let targets = match target::parse_targets(&target_text) {
            Ok(t) => t,
            Err(e) => {
                nwg::modal_error_message(&self.settings, "Invalid target IP", &e);
                return;
            }
        };
        let interval = match self.txt_interval.text().trim().parse::<u64>() {
            Ok(v) if (MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&v) => v,
            _ => {
                nwg::modal_error_message(
                    &self.settings,
                    "Invalid interval",
                    &format!("Enter a number of seconds from {MIN_INTERVAL_SECS} to {MAX_INTERVAL_SECS}."),
                );
                return;
            }
        };
        let new_config = Config {
            target_ip: target_text,
            check_interval_secs: interval,
            notifications: self.chk_notify.check_state() == nwg::CheckBoxState::Checked,
            providers: self.state.borrow().config.providers.clone(),
        };

        if let Err(e) = config::save(&new_config) {
            nwg::modal_error_message(&self.settings, "Error", &format!("Could not save settings:\n{e}"));
            return;
        }
        let want_autostart = self.chk_autostart.check_state() == nwg::CheckBoxState::Checked;
        if want_autostart != sys::autostart_enabled() {
            if let Err(e) = sys::set_autostart(want_autostart) {
                nwg::modal_error_message(&self.settings, "Error", &format!("Could not change autostart:\n{e}"));
            }
        }

        let interval_changed =
            self.interval.swap(new_config.interval_secs(), Ordering::Relaxed) != new_config.interval_secs();
        {
            let mut st = self.state.borrow_mut();
            if st.config.target_ip != new_config.target_ip {
                // New target: allow a fresh warning for the current IP.
                st.notified_ip = None;
            }
            st.targets = targets;
            st.config = new_config;
        }
        self.settings.set_visible(false);
        self.refresh();
        if interval_changed {
            self.check_now();
        }
    }

    // ------------------------------------------------------------ events

    fn handle_event(&self, evt: nwg::Event, data: &nwg::EventData, handle: nwg::ControlHandle) {
        use nwg::Event as E;
        match evt {
            E::OnContextMenu if handle == self.tray.handle => {
                let (x, y) = nwg::GlobalCursor::position();
                self.menu.popup(x, y);
            }
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp) if handle == self.tray.handle => {
                self.open_settings();
            }
            E::OnMenuItemSelected if handle == self.menu_settings.handle => self.open_settings(),
            E::OnMenuItemSelected if handle == self.menu_exit.handle => nwg::stop_thread_dispatch(),
            E::OnNotice if handle == self.notice.handle => self.on_check_result(),
            E::OnButtonClick if handle == self.btn_save.handle => self.save_settings(),
            E::OnButtonClick if handle == self.btn_cancel.handle => self.settings.set_visible(false),
            E::OnWindowClose if handle == self.settings.handle => {
                // Closing the settings window just hides it; the app keeps running.
                if let nwg::EventData::OnWindowClose(close) = data {
                    close.close(false);
                }
                self.settings.set_visible(false);
            }
            E::OnKeyPress if handle == self.txt_target.handle || handle == self.txt_interval.handle => {
                if let nwg::EventData::OnKey(key) = data {
                    match *key {
                        nwg::keys::RETURN => self.save_settings(),
                        nwg::keys::ESCAPE => self.settings.set_visible(false),
                        _ => {}
                    }
                }
            }
            E::OnWindowClose if handle == self.host.handle => {
                if let nwg::EventData::OnWindowClose(close) = data {
                    close.close(false);
                }
            }
            _ => {}
        }
    }

    fn on_taskbar_created(&self) {
        if let Some(hwnd) = self.host.handle.hwnd() {
            sys::readd_tray_icon(hwnd, self.icon_gray.handle as _);
        }
        self.refresh();
    }
}

/// Wires window events to the app. Returns handles that must be unbound on exit.
pub struct Bindings {
    handlers: Vec<nwg::EventHandler>,
    raw: Option<nwg::RawEventHandler>,
}

impl Bindings {
    pub fn unbind(self) {
        for h in &self.handlers {
            nwg::unbind_event_handler(h);
        }
        if let Some(r) = &self.raw {
            let _ = nwg::unbind_raw_event_handler(r);
        }
    }
}

pub fn bind(app: &Rc<App>) -> Bindings {
    let mut handlers = Vec::new();
    for window in [&app.host.handle, &app.settings.handle] {
        let weak = Rc::downgrade(app);
        handlers.push(nwg::full_bind_event_handler(window, move |evt, data, handle| {
            if let Some(app) = weak.upgrade() {
                app.handle_event(evt, &data, handle);
            }
        }));
    }

    let taskbar_created = sys::taskbar_created_message();
    let weak = Rc::downgrade(app);
    let raw = nwg::bind_raw_event_handler(&app.host.handle, 0x10001, move |_hwnd, msg, _w, _l| {
        if taskbar_created != 0 && msg == taskbar_created {
            if let Some(app) = weak.upgrade() {
                app.on_taskbar_created();
            }
        }
        None
    })
    .ok();

    Bindings { handlers, raw }
}

fn current_ip_label(st: &State) -> String {
    match (st.ip, &st.error) {
        (Some(ip), _) => format!("Current public IP: {ip}"),
        (None, Some(e)) => truncate(&format!("Current public IP: unavailable ({e})"), 120),
        (None, None) => "Current public IP: checking…".to_string(),
    }
}

fn check(v: bool) -> nwg::CheckBoxState {
    if v {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    }
}

/// Truncates to at most `max` UTF-16 units (Win32 tray strings are fixed-size).
fn truncate(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    for ch in s.chars() {
        units += ch.len_utf16();
        if units > max {
            break;
        }
        out.push(ch);
    }
    out
}
