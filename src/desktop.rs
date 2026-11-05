use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, RgbImage};
use std::cell::{Cell, RefCell};
use std::ffi::{OsString, c_void};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{self, PathBuf};
use std::ptr::{null, null_mut};
use std::slice;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    GetSystemMetricsForDpi, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const MSG_CAPTURE: u32 = WM_APP + 1;
const MSG_TRAY: u32 = WM_APP + 2;
const MSG_TRAY_ENABLED: u32 = WM_APP + 3;
const MSG_COMMAND: u32 = WM_APP + 4;
const MSG_CANCEL_CAPTURE: u32 = WM_APP + 5;
const MSG_CAPTURE_WINDOW: u32 = WM_APP + 6;
const HOTKEY_ID: i32 = 1;
const TRAY_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY; // shellapi.h macro
const TRAY_RETRY_TIMER: usize = 1;
const CAPTURE_HIDE_TIMER: usize = 2;
const CAPTURE_HIDE_DELAY_MS: u32 = 200;
const CAPTURE_OUTLINE_COLOR: u32 = 0x0000_d4ff; // COLORREF for #ffd400
const APP_ICON_ID: usize = 1; // RT_GROUP_ICON from build.rs
const MENU_CAPTURE: usize = 1;
const MENU_CLIPBOARD: usize = 2;
const MENU_QR_CAPTURE: usize = 3;
const MENU_SHOW: usize = 4;
const MENU_EXIT: usize = 5;
const CONTROL_CLASS: &str = "WfZiUiBp_Desktop";
const PATH_MESSAGE: usize = 0x5746_5A50;
const MAX_PATH_PAYLOAD: usize = 65_536;

pub fn window_icon() -> Option<eframe::egui::IconData> {
    let rgba = image::load_from_memory_with_format(
        include_bytes!("../assets/umiocr.ico"),
        image::ImageFormat::Ico,
    )
    .ok()?
    .into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(eframe::egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

#[derive(Clone, Copy)]
#[repr(usize)]
pub enum DesktopCommand {
    Show = 1,
    Hide,
    Capture,
    RepeatCapture,
    Exit,
    Reload,
    Clipboard,
}

pub enum DesktopEvent {
    Capture(DynamicImage),
    QrCapture(DynamicImage),
    CaptureCancelled,
    Paths(Vec<PathBuf>),
    TrayStatus(bool),
    Show,
    Hide,
    Exit,
    Reload,
    Clipboard,
    Error(String),
}

pub fn forward_existing(action: DesktopCommand) -> bool {
    unsafe {
        let target = control_window();
        !target.is_null() && PostMessageW(target, MSG_COMMAND, action as usize, 0) != 0
    }
}

pub fn forward_paths(paths: &[PathBuf]) -> bool {
    let target = control_window();
    if paths.is_empty() || target.is_null() {
        return false;
    }
    let mut process_id = 0;
    if unsafe { GetWindowThreadProcessId(target, &mut process_id) } == 0
        || process_id == std::process::id()
    {
        return false;
    }
    let mut payload = Vec::new();
    for path in paths {
        let Ok(path) = path::absolute(path) else {
            return false;
        };
        payload.extend(path.as_os_str().encode_wide());
        payload.push(0);
        if payload.len() > MAX_PATH_PAYLOAD {
            return false;
        }
    }
    let data = COPYDATASTRUCT {
        dwData: PATH_MESSAGE,
        cbData: (payload.len() * size_of::<u16>()) as u32,
        lpData: payload.as_mut_ptr().cast(),
    };
    let mut accepted = 0;
    let delivered = unsafe {
        SendMessageTimeoutW(
            target,
            WM_COPYDATA,
            0,
            (&raw const data) as LPARAM,
            SMTO_BLOCK | SMTO_ABORTIFHUNG,
            1_000,
            &mut accepted,
        )
    };
    delivered != 0 && accepted == 1
}

fn control_window() -> HWND {
    let class = wide(CONTROL_CLASS);
    unsafe { FindWindowW(class.as_ptr(), class.as_ptr()) }
}

unsafe fn received_paths(lparam: LPARAM) -> Option<Vec<PathBuf>> {
    let data = unsafe { (lparam as *const COPYDATASTRUCT).as_ref() }?;
    let bytes = data.cbData as usize;
    let payload = data.lpData.cast::<u16>();
    if data.dwData != PATH_MESSAGE
        || bytes == 0
        || bytes > MAX_PATH_PAYLOAD * size_of::<u16>()
        || !bytes.is_multiple_of(size_of::<u16>())
        || payload.is_null()
        || !payload.is_aligned()
    {
        return None;
    }
    // Win32 owns the marshalled buffer for the duration of WM_COPYDATA.
    let units = unsafe { slice::from_raw_parts(payload, bytes / size_of::<u16>()) };
    decode_paths(units)
}

fn decode_paths(units: &[u16]) -> Option<Vec<PathBuf>> {
    units
        .strip_suffix(&[0])?
        .split(|unit| *unit == 0)
        .map(|path| (!path.is_empty()).then(|| PathBuf::from(OsString::from_wide(path))))
        .collect()
}

pub fn capture_monitor_region(screen: usize, rect: [i32; 4]) -> Result<DynamicImage> {
    let _dpi = DpiContext::physical_pixels();
    let monitors = Monitor::list()?;
    let monitor = monitors
        .get(screen)
        .ok_or_else(|| anyhow!("未找到编号为 {screen} 的显示器"))?;
    let area = monitor.region(rect)?;
    Screen::crop_desktop(area)
}

pub struct Desktop {
    // HWND is sent as an integer. Only the desktop thread owns the window and its state.
    control: usize,
    overlay: usize,
    worker: Option<JoinHandle<()>>,
    hotkey: Option<Hotkey>,
}

impl Desktop {
    pub fn start(events: Sender<DesktopEvent>, hotkey: bool, tray: bool) -> Result<Self> {
        let (ready, initialized) = mpsc::sync_channel(1);
        let worker_events = events.clone();
        let worker = thread::Builder::new()
            .name("desktop".into())
            .spawn(move || run_desktop(worker_events, tray, ready))?;
        let (control, overlay) = initialized.recv().context("桌面线程未能启动")??;
        let mut desktop = Self {
            control,
            overlay,
            worker: Some(worker),
            hotkey: None,
        };
        if let Err(error) = desktop.set_hotkey(hotkey) {
            let _ = events.send(DesktopEvent::Error(error.to_string()));
        }
        Ok(desktop)
    }

    pub fn request_capture(&self) -> Result<()> {
        self.post(MSG_CAPTURE, 0).context("无法打开截图选区")
    }

    pub fn request_qr_capture(&self) -> Result<()> {
        self.post(MSG_CAPTURE, CaptureKind::Qr as usize)
            .context("无法打开二维码截图选区")
    }

    pub fn request_repeat_capture(&self) -> Result<()> {
        self.post(MSG_COMMAND, DesktopCommand::RepeatCapture as usize)
            .context("无法重复截图")
    }

    pub fn set_capture_window(&self, hwnd: isize, hide: bool) -> Result<()> {
        if hide && !own_window(hwnd as HWND) {
            bail!("截图前需要隐藏的窗口不属于本应用");
        }
        let target = if hide { hwnd as usize } else { 0 };
        self.post(MSG_CAPTURE_WINDOW, target)
            .context("无法更新截图前的窗口隐藏设置")
    }

    pub fn set_hotkey(&mut self, enabled: bool) -> Result<()> {
        if enabled && self.hotkey.is_none() {
            self.hotkey = Some(Hotkey::start(self.control, self.overlay)?);
        } else if !enabled {
            self.hotkey.take();
        }
        Ok(())
    }

    pub fn set_tray(&self, enabled: bool) -> Result<()> {
        self.post(MSG_TRAY_ENABLED, usize::from(enabled))
            .context("无法更新系统托盘")
    }

    fn post(&self, message: u32, parameter: usize) -> Result<()> {
        if unsafe { PostMessageW(self.control as HWND, message, parameter, 0) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.hotkey.take();
        if let Some(worker) = self.worker.take() {
            if self.post(WM_CLOSE, 0).is_ok() || worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

fn own_window(window: HWND) -> bool {
    let mut process_id = 0;
    let thread_id = unsafe { GetWindowThreadProcessId(window, &mut process_id) };
    thread_id != 0 && process_id == std::process::id()
}

struct Hotkey {
    id: u32,
    worker: Option<JoinHandle<()>>,
}

impl Hotkey {
    fn start(control: usize, overlay: usize) -> Result<Self> {
        let (ready, initialized) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("f4-hotkey".into())
            .spawn(move || run_hotkey(control, overlay, ready))?;
        let id = initialized.recv().context("F4 快捷键线程未能启动")??;
        Ok(Self {
            id,
            worker: Some(worker),
        })
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            if unsafe { PostThreadMessageW(self.id, WM_QUIT, 0, 0) } != 0 || worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum F4Press {
    Up,
    Passed,
    Captured,
}

impl F4Press {
    fn update(&mut self, down: bool, modified: bool, trigger: impl FnOnce() -> bool) -> bool {
        if !down {
            let captured = *self == Self::Captured;
            *self = Self::Up;
            return captured;
        }
        if *self == Self::Up {
            *self = if !modified && trigger() {
                Self::Captured
            } else {
                Self::Passed
            };
        }
        *self == Self::Captured
    }
}

thread_local! {
    static HOTKEY_TARGET: Cell<HWND> = const { Cell::new(null_mut()) };
    static ESCAPE_OVERLAY: Cell<HWND> = const { Cell::new(null_mut()) };
    static F4_PRESS: Cell<F4Press> = const { Cell::new(F4Press::Up) };
    static ESCAPE_PRESS: Cell<F4Press> = const { Cell::new(F4Press::Up) };
}

fn run_hotkey(control: usize, overlay: usize, ready: mpsc::SyncSender<Result<u32>>) {
    unsafe {
        let mut message = MSG::default();
        // The queue must exist before the owner can post WM_QUIT.
        PeekMessageW(&mut message, null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
        HOTKEY_TARGET.set(control as HWND);
        ESCAPE_OVERLAY.set(overlay as HWND);
        if GetAsyncKeyState(VK_F4 as i32) < 0 {
            F4_PRESS.set(F4Press::Passed);
        }
        let hook = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_proc),
            GetModuleHandleW(null()),
            0,
        );
        let registered = if hook.is_null() {
            let hook_error = io::Error::last_os_error();
            if RegisterHotKey(null_mut(), HOTKEY_ID, MOD_NOREPEAT, VK_F4 as u32) == 0 {
                let registration_error = io::Error::last_os_error();
                let _ = ready.send(Err(anyhow!(
                    "无法启用 F4。键盘钩子：{hook_error}。系统快捷键：{registration_error}。"
                )));
                return;
            }
            true
        } else {
            false
        };
        if ready.send(Ok(GetCurrentThreadId())).is_ok() {
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                if registered
                    && message.message == WM_HOTKEY
                    && message.wParam == HOTKEY_ID as usize
                {
                    PostMessageW(control as HWND, MSG_CAPTURE, 0, 0);
                }
            }
        }
        if registered {
            UnregisterHotKey(null_mut(), HOTKEY_ID);
        } else {
            UnhookWindowsHookEx(hook);
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, kind: WPARAM, event: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let key = &*(event as *const KBDLLHOOKSTRUCT);
            let kind = kind as u32;
            let down = matches!(kind, WM_KEYDOWN | WM_SYSKEYDOWN);
            if down || matches!(kind, WM_KEYUP | WM_SYSKEYUP) {
                if key.vkCode == VK_F4 as u32 {
                    let mut press = F4_PRESS.get();
                    let modified = down
                        && press == F4Press::Up
                        && (key.flags & LLKHF_ALTDOWN != 0
                            || [VK_MENU, VK_SHIFT, VK_CONTROL, VK_LWIN, VK_RWIN]
                                .into_iter()
                                .any(|modifier| GetAsyncKeyState(modifier as i32) < 0));
                    let swallow = press.update(down, modified, || {
                        PostMessageW(HOTKEY_TARGET.get(), MSG_CAPTURE, 0, 0) != 0
                    });
                    F4_PRESS.set(press);
                    if swallow {
                        return 1;
                    }
                } else if key.vkCode == VK_ESCAPE as u32 {
                    let mut press = ESCAPE_PRESS.get();
                    let swallow = press.update(down, false, || {
                        IsWindowVisible(ESCAPE_OVERLAY.get()) != 0
                            && PostMessageW(HOTKEY_TARGET.get(), MSG_CANCEL_CAPTURE, 0, 0) != 0
                    });
                    ESCAPE_PRESS.set(press);
                    if swallow {
                        return 1;
                    }
                }
            }
        }
        CallNextHookEx(null_mut(), code, kind, event)
    }
}

#[derive(Clone, Copy)]
enum CaptureRequest {
    Region(CaptureKind),
    Repeat,
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum CaptureKind {
    Text = 0,
    Qr = 1,
}

#[derive(Clone, Copy)]
struct HiddenWindow {
    window: HWND,
    was_foreground: bool,
}

struct DesktopState {
    events: Sender<DesktopEvent>,
    control: Cell<HWND>,
    overlay: Cell<HWND>,
    screen: RefCell<Option<Screen>>,
    capture_kind: Cell<CaptureKind>,
    anchor: Cell<Option<POINT>>,
    cursor: Cell<POINT>,
    last_selection: RefCell<Option<LastSelection>>,
    capture_window: Cell<HWND>,
    pending_capture: Cell<Option<CaptureRequest>>,
    hidden_window: Cell<Option<HiddenWindow>>,
    tray_enabled: Cell<bool>,
    tray_registered: Cell<bool>,
    tray_v4: Cell<bool>,
    taskbar_created: u32,
}

impl DesktopState {
    fn start_capture(&self, request: CaptureRequest) -> Result<()> {
        if self.pending_capture.get().is_some() || self.screen.borrow().is_some() {
            return match request {
                CaptureRequest::Region(_) => Ok(()),
                CaptureRequest::Repeat => bail!("截图选区正在使用"),
            };
        }

        let main = self.capture_window.get();
        unsafe {
            if own_window(main) && IsWindowVisible(main) != 0 && IsIconic(main) == 0 {
                let was_foreground = GetForegroundWindow() == main;
                if ShowWindowAsync(main, SW_HIDE) == 0 {
                    return Err(io::Error::last_os_error()).context("无法暂时隐藏主窗口");
                }
                self.hidden_window.set(Some(HiddenWindow {
                    window: main,
                    was_foreground,
                }));
                self.pending_capture.set(Some(request));
                if SetTimer(
                    self.control.get(),
                    CAPTURE_HIDE_TIMER,
                    CAPTURE_HIDE_DELAY_MS,
                    None,
                ) == 0
                {
                    let error = io::Error::last_os_error();
                    self.pending_capture.set(None);
                    self.restore_window();
                    return Err(error).context("无法等待主窗口隐藏");
                }
                return Ok(());
            }
        }
        self.capture_now(request)
    }

    fn capture_now(&self, request: CaptureRequest) -> Result<()> {
        match request {
            CaptureRequest::Region(kind) => {
                let result = self.show_overlay(kind);
                if result.is_err() {
                    self.restore_window();
                }
                result
            }
            CaptureRequest::Repeat => {
                let image = self.repeat_image();
                self.restore_window();
                let _ = self.events.send(DesktopEvent::Capture(image?));
                Ok(())
            }
        }
    }

    fn restore_window(&self) {
        if let Some(hidden) = self.hidden_window.take()
            && own_window(hidden.window)
        {
            unsafe {
                ShowWindowAsync(
                    hidden.window,
                    if hidden.was_foreground {
                        SW_SHOW
                    } else {
                        SW_SHOWNA
                    },
                );
            }
        }
    }

    fn show_overlay(&self, kind: CaptureKind) -> Result<()> {
        let screen = Screen::capture()?;
        let overlay = self.overlay.get();
        unsafe {
            if SetWindowPos(
                overlay,
                HWND_TOPMOST,
                screen.x,
                screen.y,
                screen.width,
                screen.height,
                SWP_NOACTIVATE,
            ) == 0
            {
                return Err(io::Error::last_os_error()).context("无法定位截图选区");
            }
            self.anchor.set(None);
            self.capture_kind.set(kind);
            self.screen.replace(Some(screen));
            ShowWindow(overlay, SW_SHOW);
            SetForegroundWindow(overlay);
            InvalidateRect(overlay, null(), 0);
        }
        Ok(())
    }

    fn point(&self, lparam: LPARAM) -> Option<POINT> {
        let frame = self.screen.borrow();
        let screen = frame.as_ref()?;
        let (x, y) = if screen.width <= i16::MAX as i32 && screen.height <= i16::MAX as i32 {
            (lparam as i16 as i32, (lparam >> 16) as i16 as i32)
        } else {
            let mut point = POINT::default();
            if unsafe { GetCursorPos(&mut point) } == 0 {
                return None;
            }
            (point.x - screen.x, point.y - screen.y)
        };
        Some(POINT {
            x: x.clamp(0, screen.width),
            y: y.clamp(0, screen.height),
        })
    }

    fn close_overlay(&self) -> bool {
        self.anchor.set(None);
        let was_pending = self.pending_capture.take().is_some();
        let was_visible = self.screen.replace(None).is_some();
        if was_pending {
            unsafe { KillTimer(self.control.get(), CAPTURE_HIDE_TIMER) };
        }
        if was_visible {
            let overlay = self.overlay.get();
            unsafe {
                if GetCapture() == overlay {
                    ReleaseCapture();
                }
                ShowWindow(overlay, SW_HIDE);
            }
        }
        self.restore_window();
        was_pending || was_visible
    }

    fn cancel_capture(&self) {
        if self.close_overlay() {
            let _ = self.events.send(DesktopEvent::CaptureCancelled);
        }
    }

    fn finish_capture(&self, lparam: LPARAM) {
        let Some(rect) = self
            .anchor
            .get()
            .zip(self.point(lparam))
            .map(|(first, last)| selection_rect(first, last))
            .filter(|rect| rect.right > rect.left && rect.bottom > rect.top)
        else {
            self.cancel_capture();
            return;
        };
        let cropped = (|| {
            let frame = self.screen.borrow();
            let screen = frame.as_ref().ok_or_else(|| anyhow!("截图选区已关闭"))?;
            let image = screen.crop(rect)?;
            let area = translate_rect(rect, screen.x, screen.y);
            self.last_selection.replace(Some(LastSelection::new(area)));
            Ok(image)
        })();
        let kind = self.capture_kind.get();
        self.close_overlay();
        match cropped {
            Ok(image) => {
                let event = match kind {
                    CaptureKind::Text => DesktopEvent::Capture(image),
                    CaptureKind::Qr => DesktopEvent::QrCapture(image),
                };
                let _ = self.events.send(event);
            }
            Err(error) => self.report(&error),
        }
    }

    fn repeat_image(&self) -> Result<DynamicImage> {
        let selection = self.last_selection.borrow();
        let area = selection
            .as_ref()
            .ok_or_else(|| anyhow!("尚未记录截图区域"))?
            .resolve()?;
        Screen::crop_desktop(area)
    }

    fn command(&self, action: usize) -> Result<()> {
        let event = match action {
            action if action == DesktopCommand::Show as usize => DesktopEvent::Show,
            action if action == DesktopCommand::Hide as usize => {
                self.hidden_window.take();
                DesktopEvent::Hide
            }
            action if action == DesktopCommand::Capture as usize => {
                return self.start_capture(CaptureRequest::Region(CaptureKind::Text));
            }
            action if action == DesktopCommand::RepeatCapture as usize => {
                return self.start_capture(CaptureRequest::Repeat);
            }
            action if action == DesktopCommand::Exit as usize => DesktopEvent::Exit,
            action if action == DesktopCommand::Reload as usize => DesktopEvent::Reload,
            action if action == DesktopCommand::Clipboard as usize => DesktopEvent::Clipboard,
            _ => return Ok(()),
        };
        let _ = self.events.send(event);
        Ok(())
    }

    fn report(&self, error: &anyhow::Error) {
        let _ = self.events.send(DesktopEvent::Error(format!("{error:#}")));
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.control.get(),
            uID: TRAY_ID,
            ..Default::default()
        }
    }

    fn add_tray_icon(&self) -> bool {
        let mut data = self.tray_data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = MSG_TRAY;
        let title = wide("文字识别 WfZiUiBp");
        data.szTip[..title.len()].copy_from_slice(&title);
        unsafe {
            let dpi = GetDpiForSystem();
            data.hIcon = LoadImageW(
                GetModuleHandleW(null()),
                APP_ICON_ID as *const u16,
                IMAGE_ICON,
                GetSystemMetricsForDpi(SM_CXSMICON, dpi),
                GetSystemMetricsForDpi(SM_CYSMICON, dpi),
                0,
            );
            if data.hIcon.is_null() {
                return false;
            }
            let added =
                Shell_NotifyIconW(NIM_ADD, &data) != 0 || Shell_NotifyIconW(NIM_MODIFY, &data) != 0;
            DestroyIcon(data.hIcon);
            if added {
                let mut version = self.tray_data();
                version.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                self.tray_v4
                    .set(Shell_NotifyIconW(NIM_SETVERSION, &version) != 0);
            }
            added
        }
    }

    fn set_tray(&self, enabled: bool) {
        self.tray_enabled.set(enabled);
        if enabled && !self.tray_registered.get() {
            if !self.register_tray() {
                self.report(&anyhow!("系统托盘图标暂不可用，正在重试"));
            }
        } else if !enabled {
            unsafe {
                KillTimer(self.control.get(), TRAY_RETRY_TIMER);
                if self.tray_registered.replace(false) {
                    Shell_NotifyIconW(NIM_DELETE, &self.tray_data());
                }
            }
        }
        self.notify_tray_status();
    }

    fn register_tray(&self) -> bool {
        let registered = self.add_tray_icon();
        self.tray_registered.set(registered);
        unsafe {
            if registered {
                KillTimer(self.control.get(), TRAY_RETRY_TIMER);
            } else {
                SetTimer(self.control.get(), TRAY_RETRY_TIMER, 1_500, None);
            }
        }
        registered
    }

    fn notify_tray_status(&self) {
        let _ = self
            .events
            .send(DesktopEvent::TrayStatus(self.tray_registered.get()));
    }

    fn tray_event(&self, wparam: WPARAM, lparam: LPARAM) {
        let v4 = self.tray_v4.get();
        let event = if v4 {
            if (lparam as usize >> 16) as u16 != TRAY_ID as u16 {
                return;
            }
            u32::from(lparam as u16)
        } else {
            if wparam as u32 != TRAY_ID {
                return;
            }
            lparam as u32
        };
        match event {
            WM_LBUTTONUP | NIN_SELECT | NIN_KEYSELECT => {
                let _ = self.events.send(DesktopEvent::Show);
            }
            WM_CONTEXTMENU | WM_RBUTTONUP if !v4 => self.tray_menu(None),
            WM_CONTEXTMENU => self.tray_menu(Some(POINT {
                x: wparam as i16 as i32,
                y: (wparam >> 16) as i16 as i32,
            })),
            _ => {}
        }
    }

    fn tray_menu(&self, anchor: Option<POINT>) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            for (id, text) in [
                (MENU_CAPTURE, "屏幕截图"),
                (MENU_CLIPBOARD, "粘贴图片"),
                (MENU_QR_CAPTURE, "扫描二维码"),
                (MENU_SHOW, "打开主窗口"),
                (MENU_EXIT, "退出文字识别"),
            ] {
                AppendMenuW(menu, MF_STRING, id, wide(text).as_ptr());
            }
            let control = self.control.get();
            let point = anchor
                .filter(|point| point.x != -1 || point.y != -1)
                .unwrap_or_else(|| {
                    let mut point = POINT::default();
                    GetCursorPos(&mut point);
                    point
                });
            SetForegroundWindow(control);
            let command = TrackPopupMenuEx(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                control,
                null(),
            );
            DestroyMenu(menu);
            PostMessageW(control, WM_NULL, 0, 0);
            match command as usize {
                MENU_SHOW => {
                    let _ = self.events.send(DesktopEvent::Show);
                }
                MENU_CAPTURE => {
                    PostMessageW(control, MSG_CAPTURE, 0, 0);
                }
                MENU_CLIPBOARD => {
                    let _ = self.events.send(DesktopEvent::Clipboard);
                }
                MENU_QR_CAPTURE => {
                    PostMessageW(control, MSG_CAPTURE, CaptureKind::Qr as usize, 0);
                }
                MENU_EXIT => {
                    let _ = self.events.send(DesktopEvent::Exit);
                }
                _ => {
                    Shell_NotifyIconW(NIM_SETFOCUS, &self.tray_data());
                }
            }
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn run_desktop(
    events: Sender<DesktopEvent>,
    tray: bool,
    ready: mpsc::SyncSender<Result<(usize, usize)>>,
) {
    unsafe {
        let _dpi = DpiContext::physical_pixels();
        let state = DesktopState {
            events,
            control: Cell::new(null_mut()),
            overlay: Cell::new(null_mut()),
            screen: RefCell::new(None),
            capture_kind: Cell::new(CaptureKind::Text),
            anchor: Cell::new(None),
            cursor: Cell::new(POINT::default()),
            last_selection: RefCell::new(None),
            capture_window: Cell::new(null_mut()),
            pending_capture: Cell::new(None),
            hidden_window: Cell::new(None),
            tray_enabled: Cell::new(false),
            tray_registered: Cell::new(false),
            tray_v4: Cell::new(false),
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        };
        let control = match create_windows(&state) {
            Ok(window) => window,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        state.set_tray(tray);
        if ready
            .send(Ok((control as usize, state.overlay.get() as usize)))
            .is_ok()
        {
            let mut message = MSG::default();
            let mut next = GetMessageW(&mut message, null_mut(), 0, 0);
            while next > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
                next = GetMessageW(&mut message, null_mut(), 0, 0);
            }
            if next < 0 {
                state.report(&anyhow!("桌面消息循环失败：{}", io::Error::last_os_error()));
            }
        }
        state.close_overlay();
        state.set_tray(false);
        DestroyWindow(state.overlay.get());
        DestroyWindow(control);
    }
}

fn create_windows(state: &DesktopState) -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(null()) };
    let control_name = wide(CONTROL_CLASS);
    let overlay_name = wide("WfZiUiBp_Capture");
    register_class(instance, &control_name, Some(control_proc), false)?;
    register_class(instance, &overlay_name, Some(overlay_proc), true)?;
    let parameter = (state as *const DesktopState).cast();
    unsafe {
        let control = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            control_name.as_ptr(),
            control_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            parameter,
        );
        if control.is_null() {
            return Err(io::Error::last_os_error()).context("无法创建托盘窗口");
        }
        let overlay = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            overlay_name.as_ptr(),
            wide("截图选区").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            parameter,
        );
        if overlay.is_null() {
            let error = io::Error::last_os_error();
            DestroyWindow(control);
            return Err(error).context("无法创建截图选区窗口");
        }
        state.control.set(control);
        state.overlay.set(overlay);
        Ok(control)
    }
}

fn register_class(instance: *mut c_void, name: &[u16], proc: WNDPROC, cross: bool) -> Result<()> {
    unsafe {
        let class = WNDCLASSW {
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            lpfnWndProc: proc,
            hCursor: LoadCursorW(null_mut(), if cross { IDC_CROSS } else { IDC_ARROW }),
            ..WNDCLASSW::default()
        };
        if RegisterClassW(&class) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error()
                != Some(windows_sys::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS as i32)
            {
                return Err(error).context("无法注册桌面窗口类");
            }
        }
    }
    Ok(())
}

// The owner keeps DesktopState alive until both of its windows have been destroyed.
unsafe fn state_ptr(window: HWND, message: u32, param: LPARAM) -> *const DesktopState {
    unsafe {
        if message == WM_NCCREATE {
            let creation = &*(param as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, creation.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *const DesktopState;
        if message == WM_NCDESTROY {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        }
        pointer
    }
}

unsafe extern "system" fn control_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if let Some(state) = state_ptr(window, message, lparam).as_ref() {
            match message {
                MSG_CAPTURE => {
                    let kind = if wparam == CaptureKind::Qr as usize {
                        CaptureKind::Qr
                    } else {
                        CaptureKind::Text
                    };
                    if let Err(error) = state.start_capture(CaptureRequest::Region(kind)) {
                        state.report(&error);
                    }
                    return 0;
                }
                MSG_TRAY => {
                    state.tray_event(wparam, lparam);
                    return 0;
                }
                MSG_TRAY_ENABLED => {
                    state.set_tray(wparam != 0);
                    return 0;
                }
                MSG_CAPTURE_WINDOW => {
                    let main = wparam as HWND;
                    if main.is_null() || own_window(main) {
                        state.capture_window.set(main);
                    }
                    return 0;
                }
                WM_TIMER if wparam == TRAY_RETRY_TIMER => {
                    if state.tray_enabled.get()
                        && !state.tray_registered.get()
                        && state.register_tray()
                    {
                        state.notify_tray_status();
                    }
                    return 0;
                }
                WM_TIMER if wparam == CAPTURE_HIDE_TIMER => {
                    KillTimer(window, CAPTURE_HIDE_TIMER);
                    if let Some(request) = state.pending_capture.take() {
                        if state
                            .hidden_window
                            .get()
                            .is_some_and(|hidden| IsWindowVisible(hidden.window) != 0)
                        {
                            state.restore_window();
                            state.report(&anyhow!("主窗口尚未隐藏，请重试截图"));
                        } else if let Err(error) = state.capture_now(request) {
                            state.report(&error);
                        }
                    }
                    return 0;
                }
                MSG_COMMAND => {
                    if let Err(error) = state.command(wparam) {
                        state.report(&error);
                    }
                    return 0;
                }
                MSG_CANCEL_CAPTURE => {
                    state.cancel_capture();
                    return 0;
                }
                WM_COPYDATA => {
                    return received_paths(lparam)
                        .is_some_and(|paths| state.events.send(DesktopEvent::Paths(paths)).is_ok())
                        as LRESULT;
                }
                WM_CLOSE => {
                    // Let run_desktop delete the icon and then destroy both windows.
                    EndMenu();
                    PostQuitMessage(0);
                    return 0;
                }
                message if state.taskbar_created != 0 && message == state.taskbar_created => {
                    state.tray_registered.set(false);
                    state.set_tray(state.tray_enabled.get());
                    return 0;
                }
                _ => {}
            }
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

unsafe extern "system" fn overlay_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if let Some(state) = state_ptr(window, message, lparam).as_ref() {
            match message {
                WM_PAINT => {
                    let mut paint = PAINTSTRUCT::default();
                    let dc = BeginPaint(window, &mut paint);
                    if let Some(screen) = state.screen.borrow().as_ref() {
                        let rect = state
                            .anchor
                            .get()
                            .map(|anchor| selection_rect(anchor, state.cursor.get()));
                        let outline_width = ((GetDpiForWindow(window) + 24) / 48).max(2) as i32;
                        screen.paint(dc, rect, outline_width);
                    }
                    EndPaint(window, &paint);
                    return 0;
                }
                WM_ERASEBKGND => return 1,
                WM_LBUTTONDOWN => {
                    if let Some(point) = state.point(lparam) {
                        state.anchor.set(Some(point));
                        state.cursor.set(point);
                        SetCapture(window);
                    }
                    return 0;
                }
                WM_MOUSEMOVE => {
                    if let Some(anchor) = state.anchor.get()
                        && let Some(point) = state.point(lparam)
                    {
                        let old = selection_rect(anchor, state.cursor.replace(point));
                        let new = selection_rect(anchor, point);
                        let invalid = RECT {
                            left: old.left.min(new.left) - 1,
                            top: old.top.min(new.top) - 1,
                            right: old.right.max(new.right) + 1,
                            bottom: old.bottom.max(new.bottom) + 1,
                        };
                        InvalidateRect(window, &invalid, 0);
                    }
                    return 0;
                }
                WM_LBUTTONUP => {
                    if state.anchor.get().is_some() {
                        state.finish_capture(lparam);
                    }
                    return 0;
                }
                WM_KEYDOWN if wparam == VK_ESCAPE as usize => {
                    state.cancel_capture();
                    return 0;
                }
                WM_RBUTTONUP | WM_CANCELMODE | WM_CLOSE | WM_DISPLAYCHANGE => {
                    state.cancel_capture();
                    return 0;
                }
                WM_CAPTURECHANGED if state.anchor.get().is_some() => {
                    state.cancel_capture();
                    return 0;
                }
                _ => {}
            }
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

fn selection_rect(first: POINT, last: POINT) -> RECT {
    RECT {
        left: first.x.min(last.x),
        right: first.x.max(last.x),
        top: first.y.min(last.y),
        bottom: first.y.max(last.y),
    }
}

fn paint_selection_outline(target: HDC, mut rect: RECT, width: i32) {
    unsafe {
        // A dark edge keeps the yellow frame visible over yellow content.
        let edge = RECT {
            left: rect.left - 1,
            top: rect.top - 1,
            right: rect.right + 1,
            bottom: rect.bottom + 1,
        };
        FrameRect(target, &edge, GetStockObject(BLACK_BRUSH));
        let previous = SetDCBrushColor(target, CAPTURE_OUTLINE_COLOR);
        let brush = GetStockObject(DC_BRUSH);
        for _ in 0..width {
            if rect.left >= rect.right || rect.top >= rect.bottom {
                break;
            }
            FrameRect(target, &rect, brush);
            rect.left += 1;
            rect.top += 1;
            rect.right -= 1;
            rect.bottom -= 1;
        }
        SetDCBrushColor(target, previous);
    }
}

fn translate_rect(rect: RECT, x: i32, y: i32) -> RECT {
    RECT {
        left: rect.left + x,
        top: rect.top + y,
        right: rect.right + x,
        bottom: rect.bottom + y,
    }
}

fn contains_rect(outer: RECT, inner: RECT) -> bool {
    inner.left >= outer.left
        && inner.top >= outer.top
        && inner.right <= outer.right
        && inner.bottom <= outer.bottom
}

struct DpiContext(*mut c_void);

impl DpiContext {
    fn physical_pixels() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for DpiContext {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

struct Monitor {
    name: [u16; 32],
    bounds: RECT,
    primary: bool,
}

impl Monitor {
    fn list() -> Result<Vec<Self>> {
        let mut monitors = Vec::<Self>::new();
        let ok = unsafe {
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(collect_monitor),
                (&raw mut monitors) as LPARAM,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error()).context("无法读取显示器列表");
        }
        if monitors.is_empty() {
            bail!("当前桌面没有可截取的显示区域");
        }
        monitors.sort_by_key(|monitor| !monitor.primary);
        Ok(monitors)
    }

    fn desktop_bounds(monitors: &[Self]) -> RECT {
        monitors
            .iter()
            .fold(monitors[0].bounds, |bounds, monitor| RECT {
                left: bounds.left.min(monitor.bounds.left),
                top: bounds.top.min(monitor.bounds.top),
                right: bounds.right.max(monitor.bounds.right),
                bottom: bounds.bottom.max(monitor.bounds.bottom),
            })
    }

    fn region(&self, [x, y, width, height]: [i32; 4]) -> Result<RECT> {
        let max_x = self.bounds.right - self.bounds.left;
        let max_y = self.bounds.bottom - self.bounds.top;
        if !(0..max_x).contains(&x) || !(0..max_y).contains(&y) {
            bail!("截图区域起点超出显示器范围");
        }
        let rect = RECT {
            left: x,
            top: y,
            right: if width > 0 {
                x.saturating_add(width).min(max_x)
            } else {
                max_x
            },
            bottom: if height > 0 {
                y.saturating_add(height).min(max_y)
            } else {
                max_y
            },
        };
        Ok(translate_rect(rect, self.bounds.left, self.bounds.top))
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> i32 {
    unsafe {
        let monitors = &mut *(data as *mut Vec<Monitor>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, &mut info.monitorInfo) != 0 {
            monitors.push(Monitor {
                name: info.szDevice,
                bounds: info.monitorInfo.rcMonitor,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        1
    }
}

struct LastSelection {
    area: RECT,
    monitor: Option<[u16; 32]>,
}

impl LastSelection {
    fn new(area: RECT) -> Self {
        if let Ok(monitors) = Monitor::list()
            && let Some(monitor) = monitors
                .iter()
                .find(|monitor| contains_rect(monitor.bounds, area))
        {
            return Self {
                area: translate_rect(area, -monitor.bounds.left, -monitor.bounds.top),
                monitor: Some(monitor.name),
            };
        }
        Self {
            area,
            monitor: None,
        }
    }

    fn resolve(&self) -> Result<RECT> {
        let monitors = Monitor::list()?;
        if let Some(name) = self.monitor {
            let monitor = monitors
                .iter()
                .find(|monitor| monitor.name == name)
                .ok_or_else(|| anyhow!("上次截图使用的显示器已断开"))?;
            let area = translate_rect(self.area, monitor.bounds.left, monitor.bounds.top);
            if !contains_rect(monitor.bounds, area) {
                bail!("上次截图区域超出当前显示器范围");
            }
            Ok(area)
        } else if contains_rect(Monitor::desktop_bounds(&monitors), self.area) {
            Ok(self.area)
        } else {
            bail!("上次截图区域超出当前桌面范围")
        }
    }
}

struct Bitmap {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Bitmap {
    unsafe fn create(compatible: HDC, allocate: impl FnOnce(HDC) -> HBITMAP) -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(compatible);
            if dc.is_null() {
                return Err(io::Error::last_os_error()).context("无法创建截图绘图上下文");
            }
            let mut resource = Self {
                dc,
                bitmap: null_mut(),
                previous: null_mut(),
            };
            resource.bitmap = allocate(dc);
            if resource.bitmap.is_null() {
                return Err(io::Error::last_os_error()).context("无法分配截图位图");
            }
            resource.previous = SelectObject(dc, resource.bitmap);
            if resource.previous.is_null() || resource.previous as isize == -1 {
                resource.previous = null_mut();
                return Err(io::Error::last_os_error()).context("无法选择截图位图");
            }
            Ok(resource)
        }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            DeleteDC(self.dc);
        }
    }
}

struct Screen {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    frame: Bitmap,
    black: Bitmap,
    bits: *const u8,
    byte_len: usize,
}

impl Screen {
    fn capture() -> Result<Self> {
        let monitors = Monitor::list()?;
        Self::capture_area(Monitor::desktop_bounds(&monitors))
    }

    fn crop_desktop(area: RECT) -> Result<DynamicImage> {
        let screen = Self::capture_area(area)?;
        screen.crop(RECT {
            left: 0,
            top: 0,
            right: screen.width,
            bottom: screen.height,
        })
    }

    fn capture_area(area: RECT) -> Result<Self> {
        unsafe {
            let x = area.left;
            let y = area.top;
            let width = area.right - area.left;
            let height = area.bottom - area.top;
            if width <= 0 || height <= 0 {
                bail!("当前桌面没有可截取的显示区域");
            }
            let byte_len = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .filter(|bytes| *bytes <= isize::MAX as usize)
                .ok_or_else(|| anyhow!("虚拟桌面尺寸过大"))?;
            let dc = GetDC(null_mut());
            if dc.is_null() {
                return Err(io::Error::last_os_error()).context("无法读取桌面");
            }
            let captured = Self::copy_screen(dc, x, y, width, height, byte_len);
            ReleaseDC(null_mut(), dc);
            captured
        }
    }

    unsafe fn copy_screen(
        dc: HDC,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        byte_len: usize,
    ) -> Result<Self> {
        unsafe {
            let mut info = BITMAPINFO::default();
            info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bits = null_mut();
            let frame = Bitmap::create(dc, |memory| {
                CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0)
            })?;
            if bits.is_null()
                || (BitBlt(
                    frame.dc,
                    0,
                    0,
                    width,
                    height,
                    dc,
                    x,
                    y,
                    SRCCOPY | CAPTUREBLT,
                ) == 0
                    && BitBlt(frame.dc, 0, 0, width, height, dc, x, y, SRCCOPY) == 0)
            {
                return Err(io::Error::last_os_error()).context("无法读取虚拟桌面像素");
            }
            let black = Bitmap::create(dc, |_| CreateCompatibleBitmap(dc, 1, 1))?;
            if PatBlt(black.dc, 0, 0, 1, 1, BLACKNESS) == 0 {
                return Err(io::Error::last_os_error()).context("无法创建截图遮罩");
            }
            GdiFlush();
            Ok(Self {
                x,
                y,
                width,
                height,
                frame,
                black,
                bits: bits.cast(),
                byte_len,
            })
        }
    }

    fn crop(&self, rect: RECT) -> Result<DynamicImage> {
        let pixels = unsafe {
            GdiFlush();
            slice::from_raw_parts(self.bits, self.byte_len)
        };
        crop_pixels(pixels, self.width as usize, rect)
    }

    fn paint(&self, target: HDC, selection: Option<RECT>, outline_width: i32) {
        unsafe {
            BitBlt(
                target,
                0,
                0,
                self.width,
                self.height,
                self.frame.dc,
                0,
                0,
                SRCCOPY,
            );
            AlphaBlend(
                target,
                0,
                0,
                self.width,
                self.height,
                self.black.dc,
                0,
                0,
                1,
                1,
                BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 112,
                    AlphaFormat: 0,
                },
            );
            if let Some(rect) = selection
                && rect.left < rect.right
                && rect.top < rect.bottom
            {
                BitBlt(
                    target,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    self.frame.dc,
                    rect.left,
                    rect.top,
                    SRCCOPY,
                );
                paint_selection_outline(target, rect, outline_width);
            }
        }
    }
}

fn crop_pixels(pixels: &[u8], frame_width: usize, rect: RECT) -> Result<DynamicImage> {
    let width = (rect.right - rect.left) as usize;
    let height = (rect.bottom - rect.top) as usize;
    let mut output = Vec::with_capacity(width * height * 3);
    for y in rect.top as usize..rect.bottom as usize {
        let start = (y * frame_width + rect.left as usize) * 4;
        for pixel in pixels[start..start + width * 4].chunks_exact(4) {
            output.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    let image = RgbImage::from_raw(width as u32, height as u32, output)
        .ok_or_else(|| anyhow!("无法组装截图选区像素"))?;
    Ok(DynamicImage::ImageRgb8(image))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icon_uses_the_original_color_asset() {
        let icon = window_icon().expect("decode the original Umi-OCR icon");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
    }

    #[test]
    fn f4_swallows_one_complete_press() {
        let mut state = F4Press::Up;
        assert!(state.update(true, false, || true));
        assert!(state.update(true, true, || panic!("autorepeat must not trigger")));
        assert!(state.update(false, true, || panic!("release must not trigger")));
        assert!(state.update(true, false, || true));
    }

    #[test]
    fn alt_f4_remains_with_the_foreground_app() {
        let mut state = F4Press::Up;
        assert!(!state.update(true, true, || panic!("modified F4 must pass through")));
        assert!(!state.update(true, false, || panic!("releasing Alt must not trigger")));
        assert!(!state.update(false, false, || panic!("release must not trigger")));
        assert!(state.update(true, false, || true));
    }

    #[test]
    fn failed_dispatch_does_not_consume_f4() {
        let mut state = F4Press::Up;
        assert!(!state.update(true, false, || false));
        assert!(!state.update(false, false, || panic!("release must not trigger")));
    }

    #[test]
    fn crop_keeps_top_down_order_and_swaps_bgr_channels() {
        let frame = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        let rect = selection_rect(POINT { x: 2, y: 2 }, POINT { x: 1, y: 0 });
        let result = crop_pixels(&frame, 2, rect).unwrap().into_rgb8();
        assert_eq!(result.dimensions(), (1, 2));
        assert_eq!(result.as_raw(), &[6, 5, 4, 12, 11, 10]);
    }

    #[test]
    fn selection_paints_yellow_edges_without_changing_the_capture() {
        let (width, height) = (24, 18);
        let (source, preview) = unsafe {
            let dc = GetDC(null_mut());
            assert!(!dc.is_null());
            let source = Bitmap::create(dc, |_| CreateCompatibleBitmap(dc, width, height));
            let preview = Bitmap::create(dc, |_| CreateCompatibleBitmap(dc, width, height));
            ReleaseDC(null_mut(), dc);
            (source.unwrap(), preview.unwrap())
        };
        let screen = unsafe {
            assert_ne!(PatBlt(source.dc, 0, 0, width, height, WHITENESS), 0);
            Screen::copy_screen(
                source.dc,
                0,
                0,
                width,
                height,
                (width * height * 4) as usize,
            )
            .unwrap()
        };
        let rect = selection_rect(POINT { x: 18, y: 14 }, POINT { x: 5, y: 4 });
        screen.paint(preview.dc, Some(rect), 3);
        for (x, y) in [(5, 8), (7, 8), (15, 8), (17, 8), (10, 6), (10, 11)] {
            assert_eq!(unsafe { GetPixel(preview.dc, x, y) }, CAPTURE_OUTLINE_COLOR);
        }
        assert_eq!(unsafe { GetPixel(preview.dc, 4, 8) }, 0);
        assert_eq!(unsafe { GetPixel(preview.dc, 10, 9) }, 0x00ff_ffff);
        let masked = unsafe { GetPixel(preview.dc, 3, 8) };
        assert!(masked > 0 && masked < 0x00ff_ffff);
        let capture = screen.crop(rect).unwrap().into_rgb8();
        assert_eq!(capture.dimensions(), (13, 10));
        assert!(capture.as_raw().iter().all(|channel| *channel == 255));

        let tiny = selection_rect(POINT { x: 1, y: 1 }, POINT { x: 0, y: 0 });
        screen.paint(preview.dc, Some(tiny), 3);
        assert_eq!(unsafe { GetPixel(preview.dc, 0, 0) }, CAPTURE_OUTLINE_COLOR);
        assert_eq!(unsafe { GetPixel(preview.dc, 5, 8) }, masked);
    }

    #[test]
    fn paths_ipc_preserves_windows_utf16_and_rejects_invalid_payloads() {
        let raw = wide("D:\\素材\\文字.png\0F:\\扫描 件.pdf");
        assert_eq!(
            decode_paths(&raw).unwrap(),
            [
                PathBuf::from("D:\\素材\\文字.png"),
                PathBuf::from("F:\\扫描 件.pdf")
            ]
        );
        assert!(decode_paths(&raw[..raw.len() - 1]).is_none());
        assert!(decode_paths(&[0]).is_none());
        let unpaired = [0xd800, 0];
        assert_eq!(
            decode_paths(&unpaired).unwrap()[0]
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>(),
            [0xd800]
        );
    }
}
