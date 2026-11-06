use crate::{
    desktop::{Desktop, DesktopCommand, DesktopEvent},
    engine::{LANGUAGES, OcrOptions},
    jobs::{self, Kind, PdfMode, ResultItem, Source, Update},
    layout, qr,
};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, RichText, ViewportCommand};
use image::DynamicImage;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

mod batch_panel;
mod event_loop;
mod settings_panel;
mod workspace;

use workspace::{MediaUi, fonts, style, texture};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    ocr: OcrOptions,
    parser: String,
    ignore: Vec<[[i32; 2]; 4]>,
    dpi: u32,
    native_text: bool,
    hotkey: bool,
    tray: bool,
    auto_copy: bool,
    dark: bool,
    #[serde(default)]
    theme: Option<String>,
    ui_scale: f32,
    window_top: bool,
    tabs_locked: bool,
    startup_hidden: bool,
    hide_before_capture: bool,
    open_tabs: Vec<Page>,
    active_tab: Page,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ocr: OcrOptions {
                language: "v6_tiny".into(),
                ..OcrOptions::default()
            },
            parser: "multi_para".into(),
            ignore: Vec::new(),
            dpi: 160,
            native_text: true,
            hotkey: true,
            tray: true,
            auto_copy: true,
            dark: false,
            theme: Some("Cyberpunk".into()),
            ui_scale: 1.0,
            window_top: false,
            tabs_locked: false,
            startup_hidden: false,
            hide_before_capture: true,
            open_tabs: vec![Page::Capture],
            active_tab: Page::Capture,
        }
    }
}

impl Settings {
    fn theme_id(&self) -> &str {
        self.theme.as_deref().unwrap_or(if self.dark {
            "Default Dark"
        } else {
            "Cyberpunk"
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Page {
    Capture,
    Images,
    Documents,
    Barcode,
    Settings,
    Navigation,
    About,
}

impl Page {
    const ALL: [(Self, &str); 6] = [
        (Self::Capture, "截图OCR"),
        (Self::Images, "批量OCR"),
        (Self::Documents, "批量文档"),
        (Self::Barcode, "二维码"),
        (Self::Settings, "全局设置"),
        (Self::About, "关于"),
    ];

    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(page, _)| *page == self)
            .map_or("新标签页", |(_, title)| *title)
    }
    fn kind(self) -> Kind {
        match self {
            Self::Capture => Kind::Capture,
            Self::Documents => Kind::Document,
            Self::Barcode => Kind::Barcode,
            _ => Kind::Image,
        }
    }
}

pub struct Launch {
    pub root: PathBuf,
    pub settings: PathBuf,
    pub initial: DesktopCommand,
    pub hidden: bool,
    pub use_startup_preference: bool,
    pub no_hotkey: bool,
    pub paths: Vec<PathBuf>,
    pub capture: Option<DynamicImage>,
    pub capture_is_qr: bool,
}

struct NativeDesktop {
    desktop: Option<Desktop>,
    events: mpsc::Receiver<DesktopEvent>,
    tray_registered: bool,
    error: Option<String>,
}

fn load_settings(path: &Path) -> Settings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .unwrap_or_default()
}

pub fn run(mut launch: Launch) -> Result<()> {
    let mut settings = load_settings(&launch.settings);
    let (sender, events) = mpsc::channel();
    let (desktop, error) =
        match Desktop::start(sender, settings.hotkey && !launch.no_hotkey, settings.tray) {
            Ok(desktop) => (Some(desktop), None),
            Err(error) => (None, Some(format!("Windows 截图服务启动失败 · {error:#}"))),
        };
    let mut native = NativeDesktop {
        desktop,
        events,
        tray_registered: false,
        error,
    };
    if launch.hidden || launch.use_startup_preference && settings.startup_hidden {
        launch.initial = DesktopCommand::Show;
        if let Some(desktop) = &mut native.desktop {
            loop {
                match native.events.recv().context("Windows 桌面线程已退出")? {
                    DesktopEvent::Show => break,
                    DesktopEvent::Clipboard => {
                        launch.initial = DesktopCommand::Clipboard;
                        break;
                    }
                    DesktopEvent::Capture(image) => {
                        launch.capture = Some(image);
                        break;
                    }
                    DesktopEvent::QrCapture(image) => {
                        launch.capture = Some(image);
                        launch.capture_is_qr = true;
                        break;
                    }
                    DesktopEvent::Paths(paths) => {
                        launch.paths.extend(paths);
                        break;
                    }
                    DesktopEvent::Exit => return Ok(()),
                    DesktopEvent::Reload => {
                        settings = load_settings(&launch.settings);
                        if let Err(error) = desktop
                            .set_hotkey(settings.hotkey && !launch.no_hotkey)
                            .and_then(|()| desktop.set_tray(settings.tray))
                        {
                            native.error = Some(format!("{error:#}"));
                        }
                    }
                    DesktopEvent::TrayStatus(registered) => native.tray_registered = registered,
                    DesktopEvent::Error(error) => native.error = Some(error),
                    DesktopEvent::CaptureCancelled | DesktopEvent::Hide => {}
                }
            }
        }
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1040.0, 640.0])
        .with_min_inner_size([570.0, 370.0])
        .with_drag_and_drop(true);
    if let Some(icon) = crate::desktop::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    event_loop::run(
        "文字识别 · WfZiUiBp",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, launch, settings, native)))),
    )
}

struct App {
    settings: Settings,
    settings_path: PathBuf,
    no_hotkey: bool,
    desktop: Option<Desktop>,
    desktop_events: mpsc::Receiver<DesktopEvent>,
    tray_registered: bool,
    requests: mpsc::Sender<jobs::Request>,
    updates: mpsc::Receiver<Update>,
    control: Arc<AtomicU8>,
    busy: Option<Page>,
    status: String,
    error: Option<String>,
    page: Page,
    tabs: Vec<Page>,
    batch: batch_panel::BatchUi,
    media: [MediaUi; 2],
    results: [Vec<ResultItem>; 4],
    selected: [usize; 4],
    preview: Option<(Page, usize, egui::TextureHandle)>,
    active_box: Option<usize>,
    exit: bool,
    initial: Option<DesktopCommand>,
    initial_paths: Vec<PathBuf>,
    pending_capture: Option<(Page, DynamicImage)>,
    queued_imports: VecDeque<(Page, Vec<Source>)>,
    qr_text: String,
    qr_format: &'static str,
    qr_size: [u32; 2],
    qr_margin: i32,
    qr_generated: Option<(DynamicImage, egui::TextureHandle)>,
    startup: bool,
    document_password: String,
    native_handle: Option<isize>,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        launch: Launch,
        settings: Settings,
        native: NativeDesktop,
    ) -> Self {
        fonts(&cc.egui_ctx);
        style(&cc.egui_ctx, settings.theme_id());
        cc.egui_ctx
            .set_zoom_factor(settings.ui_scale.clamp(0.75, 1.75));
        if settings.window_top {
            cc.egui_ctx
                .send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        }
        let NativeDesktop {
            desktop,
            events,
            tray_registered,
            error,
        } = native;
        let (native_ui, desktop_events) = mpsc::channel();
        let context = cc.egui_ctx.clone();
        let handle = cc
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            });
        thread::spawn(move || {
            for event in events {
                let wake_window = matches!(
                    &event,
                    DesktopEvent::Show
                        | DesktopEvent::Clipboard
                        | DesktopEvent::Capture(_)
                        | DesktopEvent::QrCapture(_)
                        | DesktopEvent::Paths(_)
                        | DesktopEvent::Exit
                );
                if native_ui.send(event).is_err() {
                    break;
                }
                if wake_window && let Some(handle) = handle {
                    use windows_sys::Win32::UI::WindowsAndMessaging::{
                        GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
                        SW_SHOWNOACTIVATE, ShowWindowAsync,
                    };
                    let window = handle as windows_sys::Win32::Foundation::HWND;
                    // An invisible Win32 window does not deliver the redraw that egui requests.
                    let mut process = 0;
                    unsafe {
                        GetWindowThreadProcessId(window, &mut process);
                        if process == std::process::id() {
                            if IsIconic(window) != 0 {
                                ShowWindowAsync(window, SW_RESTORE);
                            } else if IsWindowVisible(window) == 0 {
                                ShowWindowAsync(window, SW_SHOWNOACTIVATE);
                            }
                        }
                    }
                }
                context.request_repaint();
            }
        });
        let (requests, updates) = jobs::start(launch.root, cc.egui_ctx.clone());
        let mut tabs = Vec::new();
        for &tab in &settings.open_tabs {
            if !tabs.contains(&tab) {
                tabs.push(tab);
            }
        }
        if tabs.is_empty() {
            tabs.push(Page::Capture);
        }
        let page = if tabs.contains(&settings.active_tab) {
            settings.active_tab
        } else {
            tabs[0]
        };
        let mut app = Self {
            settings,
            settings_path: launch.settings,
            no_hotkey: launch.no_hotkey,
            desktop,
            desktop_events,
            tray_registered,
            requests,
            updates,
            control: Arc::new(AtomicU8::new(0)),
            busy: None,
            status: "等待输入".into(),
            error,
            page,
            tabs,
            batch: batch_panel::BatchUi::default(),
            media: std::array::from_fn(|_| MediaUi::default()),
            results: std::array::from_fn(|_| Vec::new()),
            selected: [0; 4],
            preview: None,
            active_box: None,
            exit: false,
            initial: Some(launch.initial),
            initial_paths: launch.paths,
            pending_capture: launch.capture.map(|image| {
                (
                    if launch.capture_is_qr {
                        Page::Barcode
                    } else {
                        Page::Capture
                    },
                    image,
                )
            }),
            queued_imports: VecDeque::new(),
            qr_text: String::new(),
            qr_format: "QRCode",
            qr_size: [400, 400],
            qr_margin: -1,
            qr_generated: None,
            startup: crate::startup::enabled(),
            document_password: String::new(),
            native_handle: handle,
        };
        app.set_capture_preference();
        app
    }

    fn options(&self) -> jobs::Options {
        jobs::Options {
            ocr: self.settings.ocr.clone(),
            parser: self.settings.parser.clone(),
            ignore: self.settings.ignore.clone(),
            dpi: self.settings.dpi,
            native_text: self.settings.native_text,
            password: self.document_password.clone(),
        }
    }

    fn run_jobs(&mut self, page: Page, inputs: Vec<Source>, pdf: Option<(PathBuf, PdfMode)>) {
        if inputs.is_empty() {
            return;
        }
        if self.busy.is_some() {
            self.error = Some("当前任务还在运行，请等待或停止后重试".into());
            return;
        }
        self.error = None;
        self.open_page(page);
        self.control.store(0, Ordering::Relaxed);
        let request = jobs::Request {
            inputs,
            options: self.options(),
            kind: page.kind(),
            pdf,
            control: self.control.clone(),
        };
        if self.requests.send(request).is_ok() {
            self.busy = Some(page);
            self.status = "正在准备识别…".into();
        } else {
            self.error = Some("识别线程已退出，请重启应用".into());
        }
    }

    fn apply(&mut self, result: Result<()>) {
        if let Err(error) = result {
            self.error = Some(format!("{error:#}"));
        }
    }

    fn capture(&mut self, repeat: bool) {
        let result = self
            .desktop
            .as_ref()
            .context("Windows 截图服务不可用")
            .and_then(|desktop| {
                if repeat {
                    desktop.request_repeat_capture()
                } else {
                    desktop.request_capture()
                }
            });
        self.apply(result);
    }

    fn capture_qr(&mut self) {
        let result = self
            .desktop
            .as_ref()
            .context("Windows 截图服务不可用")
            .and_then(Desktop::request_qr_capture);
        self.apply(result);
    }

    fn clipboard(&mut self, page: Page) {
        match jobs::clipboard_image() {
            Ok(image) => {
                self.run_jobs(page, vec![Source::Memory("剪贴板图片".into(), image)], None)
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    fn choose(&mut self, page: Page, pdf_mode: Option<PdfMode>) {
        let (name, extensions) = if page == Page::Documents {
            ("文档", jobs::DOC_EXTS)
        } else {
            ("图片", jobs::IMAGE_EXTS)
        };
        if let Some(files) = rfd::FileDialog::new()
            .add_filter(name, extensions)
            .pick_files()
        {
            let output = if let Some(mode) = pdf_mode {
                let Some(folder) = rfd::FileDialog::new()
                    .set_title("选择 PDF 的保存目录")
                    .pick_folder()
                else {
                    return;
                };
                Some((folder, mode))
            } else {
                None
            };
            self.run_jobs(page, files.into_iter().map(Source::File).collect(), output);
        }
    }

    fn save_results(&mut self, all: bool) {
        let Some(items) = self.results.get(self.page.index()) else {
            return;
        };
        let items = if all {
            items.as_slice()
        } else {
            items
                .get(self.selected[self.page.index()])
                .map(std::slice::from_ref)
                .unwrap_or(&[])
        };
        if items.is_empty() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("识别结果.txt")
            .add_filter("纯文本", &["txt"])
            .add_filter("JSON Lines", &["jsonl"])
            .add_filter("Markdown", &["md"])
            .add_filter("CSV", &["csv"])
            .save_file()
        {
            let result = jobs::export(&path, items);
            if result.is_ok() {
                self.status = format!("已保存到 {}", path.display());
            }
            self.apply(result);
        }
    }

    fn save_settings(&mut self) {
        let result = (|| -> Result<()> {
            if let Some(parent) = self.settings_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec_pretty(&self.settings)?;
            let temporary = self.settings_path.with_extension("json.tmp");
            fs::write(&temporary, bytes)?;
            fs::rename(temporary, &self.settings_path)?;
            Ok(())
        })();
        self.apply(result);
    }

    fn command(&mut self, context: &egui::Context, command: DesktopCommand) {
        match command {
            DesktopCommand::Capture => self.capture(false),
            DesktopCommand::RepeatCapture => self.capture(true),
            DesktopCommand::Show => {
                context.send_viewport_cmd(ViewportCommand::Visible(true));
                context.send_viewport_cmd(ViewportCommand::Minimized(false));
                context.send_viewport_cmd(ViewportCommand::Focus);
            }
            DesktopCommand::Hide => context.send_viewport_cmd(ViewportCommand::Visible(false)),
            DesktopCommand::Clipboard => {
                self.command(context, DesktopCommand::Show);
                self.clipboard(Page::Capture);
            }
            DesktopCommand::Exit => {
                self.exit = true;
                context.send_viewport_cmd(ViewportCommand::Close);
            }
            DesktopCommand::Reload => {
                self.status = "重新载入设置".into();
                if let Ok(data) = fs::read(&self.settings_path).and_then(|bytes| {
                    serde_json::from_slice::<Settings>(&bytes).map_err(std::io::Error::other)
                }) {
                    self.settings = data;
                    style(context, self.settings.theme_id());
                    context.set_zoom_factor(self.settings.ui_scale.clamp(0.75, 1.75));
                    context.send_viewport_cmd(ViewportCommand::WindowLevel(
                        if self.settings.window_top {
                            egui::WindowLevel::AlwaysOnTop
                        } else {
                            egui::WindowLevel::Normal
                        },
                    ));
                    self.set_capture_preference();
                    if let Some(desktop) = &mut self.desktop {
                        let result = desktop
                            .set_hotkey(self.settings.hotkey && !self.no_hotkey)
                            .and_then(|()| desktop.set_tray(self.settings.tray));
                        self.apply(result);
                    }
                }
            }
        }
    }

    fn events(&mut self, context: &egui::Context) {
        if let Some(command) = self.initial.take() {
            self.command(context, command);
        }
        let paths = std::mem::take(&mut self.initial_paths);
        if !paths.is_empty() {
            self.import(paths);
        }
        if self.busy.is_none()
            && let Some((page, image)) = self.pending_capture.take()
        {
            self.run_jobs(page, vec![Source::Memory("屏幕选区".into(), image)], None);
        }
        while let Ok(event) = self.desktop_events.try_recv() {
            match event {
                DesktopEvent::Capture(image) => self.captured(context, Page::Capture, image),
                DesktopEvent::QrCapture(image) => self.captured(context, Page::Barcode, image),
                DesktopEvent::Show => self.command(context, DesktopCommand::Show),
                DesktopEvent::Hide => self.command(context, DesktopCommand::Hide),
                DesktopEvent::Exit => self.command(context, DesktopCommand::Exit),
                DesktopEvent::Reload => self.command(context, DesktopCommand::Reload),
                DesktopEvent::Clipboard => self.command(context, DesktopCommand::Clipboard),
                DesktopEvent::Paths(paths) => {
                    self.command(context, DesktopCommand::Show);
                    self.import(paths);
                }
                DesktopEvent::CaptureCancelled => {}
                DesktopEvent::TrayStatus(registered) => self.tray_registered = registered,
                DesktopEvent::Error(error) => {
                    self.error = Some(error);
                }
            }
        }
        while let Ok(update) = self.updates.try_recv() {
            if let Some(page @ (Page::Images | Page::Documents)) = self.busy {
                self.batch_observe(page, &update);
            }
            match update {
                Update::Progress(status) => self.status = status,
                Update::Error(error) => self.error = Some(error),
                Update::Result(item) => {
                    let page = self.busy.unwrap_or(Page::Capture);
                    let list = &mut self.results[page.index()];
                    let limit = if matches!(page, Page::Capture | Page::Barcode) {
                        6
                    } else {
                        24
                    };
                    if list.len() >= limit {
                        let older = list.len() - limit;
                        list[older].preview = None;
                    }
                    self.status = format!(
                        "{} · {:.0} 毫秒",
                        if item.text.trim().is_empty() {
                            "未识别到内容"
                        } else {
                            "识别完成"
                        },
                        item.elapsed.as_secs_f64() * 1000.0
                    );
                    if page == Page::Capture
                        && self.settings.auto_copy
                        && !item.text.trim().is_empty()
                    {
                        context.copy_text(item.text.clone());
                    }
                    list.push(item);
                    self.selected[page.index()] = list.len() - 1;
                    self.preview = None;
                    self.active_box = None;
                    if let Some(index) = MediaUi::index(page) {
                        self.media[index].show_history();
                    }
                }
                Update::Done => {
                    self.busy = None;
                    if let Some((page, image)) = self.pending_capture.take() {
                        self.run_jobs(page, vec![Source::Memory("屏幕选区".into(), image)], None);
                    } else {
                        self.run_next_import();
                    }
                }
            }
        }
    }

    fn captured(&mut self, context: &egui::Context, page: Page, image: DynamicImage) {
        self.command(context, DesktopCommand::Show);
        if self.busy.is_some() {
            self.pending_capture = Some((page, image));
            self.status = "已保留最新截图，当前任务结束后识别".into();
        } else {
            self.run_jobs(page, vec![Source::Memory("屏幕选区".into(), image)], None);
        }
    }

    fn import(&mut self, paths: Vec<PathBuf>) {
        let selected_page = self.page;
        let (documents, images): (Vec<_>, Vec<_>) = paths.into_iter().partition(|path| {
            path.is_dir() && selected_page == Page::Documents
                || path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|ext| {
                        jobs::DOC_EXTS
                            .iter()
                            .any(|value| ext.eq_ignore_ascii_case(value))
                    })
        });
        if !documents.is_empty() {
            self.stage_files(Page::Documents, documents);
        }
        if !images.is_empty() {
            match selected_page {
                Page::Capture | Page::Barcode if images.iter().all(|path| path.is_file()) => {
                    self.queued_imports.push_back((
                        selected_page,
                        images.into_iter().map(Source::File).collect(),
                    ));
                }
                _ => self.stage_files(Page::Images, images),
            }
        }
        self.run_next_import();
    }

    fn set_capture_preference(&mut self) {
        if let (Some(desktop), Some(handle)) = (&self.desktop, self.native_handle) {
            let result = desktop.set_capture_window(handle, self.settings.hide_before_capture);
            self.apply(result);
        }
    }

    fn run_next_import(&mut self) {
        if self.busy.is_none()
            && let Some((page, inputs)) = self.queued_imports.pop_front()
        {
            self.run_jobs(page, inputs, None);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.events(context);
        if context.input(|input| input.viewport().close_requested())
            && self.settings.tray
            && self.tray_registered
            && !self.exit
        {
            context.send_viewport_cmd(ViewportCommand::CancelClose);
            context.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        let paths = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect::<Vec<_>>()
        });
        if !paths.is_empty() {
            self.import(paths);
        }
        if context.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::V))
            && matches!(self.page, Page::Capture | Page::Barcode)
            && !context.wants_keyboard_input()
        {
            self.clipboard(self.page);
        }
        self.workspace(context);
        if self.busy.is_some() {
            context.request_repaint_after(Duration::from_millis(110));
        }
    }
}
