//! Small Win32 helpers: single instance, autostart, local time, network-change wait.

use std::io;
use std::ptr;

const APP_NAME: &str = "IpTrayMonitor";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Holds the named mutex for the lifetime of the process.
pub struct SingleInstance(winapi::um::winnt::HANDLE);

impl SingleInstance {
    /// Returns `None` if another instance is already running in this session.
    pub fn acquire() -> Option<SingleInstance> {
        use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
        use winapi::um::errhandlingapi::GetLastError;
        use winapi::um::synchapi::CreateMutexW;

        let name = wide(r"Local\IpTrayMonitor.SingleInstance");
        unsafe {
            let handle = CreateMutexW(ptr::null_mut(), 0, name.as_ptr());
            if handle.is_null() {
                // Can't tell — better to run than to silently do nothing.
                return Some(SingleInstance(handle));
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                winapi::um::handleapi::CloseHandle(handle);
                return None;
            }
            Some(SingleInstance(handle))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { winapi::um::handleapi::CloseHandle(self.0) };
        }
    }
}

// ---------------------------------------------------------------- autostart

fn exe_command() -> io::Result<String> {
    let exe = std::env::current_exe()?;
    Ok(format!("\"{}\"", exe.display()))
}

pub fn autostart_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(APP_NAME))
        .is_ok()
}

pub fn set_autostart(enable: bool) -> io::Result<()> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    if enable {
        let (key, _) = hkcu.create_subkey(RUN_KEY)?;
        key.set_value(APP_NAME, &exe_command()?)
    } else {
        match hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
            Ok(key) => match key.delete_value(APP_NAME) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------- misc

/// Local wall-clock time as `HH:MM:SS`.
pub fn local_time() -> String {
    unsafe {
        let mut st: winapi::um::minwinbase::SYSTEMTIME = std::mem::zeroed();
        winapi::um::sysinfoapi::GetLocalTime(&mut st);
        format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond)
    }
}

/// Blocks until any IP address on any local interface changes
/// (Wi-Fi switch, VPN connect/disconnect, cable plugged in, resume, ...).
pub fn wait_for_address_change() -> bool {
    unsafe { winapi::um::iphlpapi::NotifyAddrChange(ptr::null_mut(), ptr::null_mut()) == 0 }
}

/// Size of a small icon (tray / title bar) for the current DPI.
pub fn small_icon_size() -> (u32, u32) {
    use winapi::um::winuser::{GetSystemMetrics, SM_CXSMICON, SM_CYSMICON};
    unsafe {
        let cx = GetSystemMetrics(SM_CXSMICON).max(16) as u32;
        let cy = GetSystemMetrics(SM_CYSMICON).max(16) as u32;
        (cx, cy)
    }
}

pub fn bring_to_front(hwnd: winapi::shared::windef::HWND) {
    unsafe {
        winapi::um::winuser::ShowWindow(hwnd, winapi::um::winuser::SW_SHOWNORMAL);
        winapi::um::winuser::SetForegroundWindow(hwnd);
    }
}

pub fn taskbar_created_message() -> u32 {
    let name = wide("TaskbarCreated");
    unsafe { winapi::um::winuser::RegisterWindowMessageW(name.as_ptr()) }
}

/// Re-registers the tray icon after Explorer restarts. nwg always uses
/// uID = 0 and callback message WM_USER + 102 for its tray icon.
pub fn readd_tray_icon(hwnd: winapi::shared::windef::HWND, icon: winapi::shared::windef::HICON) {
    use winapi::um::shellapi::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIM_ADD, NOTIFYICONDATAW};
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 0;
        data.uFlags = NIF_ICON | NIF_MESSAGE;
        data.uCallbackMessage = winapi::um::winuser::WM_USER + 102;
        data.hIcon = icon;
        Shell_NotifyIconW(NIM_ADD, &mut data);
    }
}

/// Paints the window with the system dialog colour (COLOR_BTNFACE) and
/// returns that colour as RGB for the child controls.
pub fn use_dialog_background(hwnd: winapi::shared::windef::HWND) -> [u8; 3] {
    use winapi::um::winuser::{GetSysColor, GetSysColorBrush, SetClassLongPtrW, COLOR_BTNFACE, GCLP_HBRBACKGROUND};
    unsafe {
        let brush = GetSysColorBrush(COLOR_BTNFACE);
        SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush as isize);
        let c = GetSysColor(COLOR_BTNFACE);
        [(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8]
    }
}

// ---------------------------------------------------------------- alert

const ALERT_TITLE: &str = "IP Tray Monitor \u{2014} IP address mismatch";

/// Shows a modal, always-on-top warning box with a sound. Runs on its own
/// thread so the UI thread (tray icon, menu) is never blocked. A previous
/// alert that is still open is closed first, so they never pile up.
pub fn show_alert(text: String) {
    use winapi::um::winuser::{MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_SYSTEMMODAL, MB_TOPMOST};
    close_alert();
    std::thread::spawn(move || {
        let text = wide(&text);
        let title = wide(ALERT_TITLE);
        unsafe {
            MessageBoxW(
                ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND | MB_SYSTEMMODAL,
            );
        }
    });
}

/// Closes any open mismatch alert (e.g. once the IP is back to normal).
pub fn close_alert() {
    use winapi::um::winuser::{FindWindowExW, PostMessageW, WM_CLOSE};
    let class = wide("#32770"); // standard dialog class used by MessageBox
    let title = wide(ALERT_TITLE);
    unsafe {
        let mut after = ptr::null_mut();
        for _ in 0..8 {
            let hwnd = FindWindowExW(ptr::null_mut(), after, class.as_ptr(), title.as_ptr());
            if hwnd.is_null() {
                break;
            }
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
            after = hwnd;
        }
    }
}
