use super::*;
use egui::{
    Align, Align2, FontId, Layout, Margin, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, pos2, vec2,
};

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub tab: Color32,
    pub bg: Color32,
    pub panel: Color32,
    pub hover: Color32,
    pub line: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub good: Color32,
    pub bad: Color32,
}

impl Palette {
    pub(super) fn from_theme(theme: &str) -> Self {
        let (tab, bg, text, muted, tint, good, bad) = match theme {
            "Cyberpunk" => (
                0x084a5a, 0x136377, 0xfcec0c, 0xcf9f0f, 0.20, 0x6efc39, 0xff5e5e,
            ),
            "Default Dark" => (
                0x4a4a4a, 0x444444, 0xffffff, 0xaaaaaa, 0.14, 0x6efc39, 0xff2e2e,
            ),
            "Solarized Dark" => (
                0x004052, 0x002b36, 0x93a1a1, 0x839496, 0.10, 0x6efc39, 0xf14c4c,
            ),
            "Solarized Light" => (
                0xd9d2c2, 0xfdf6e3, 0x586e75, 0x657b83, 0.067, 0x48985d, 0xe51d09,
            ),
            _ => (
                0xf3f3f3, 0xffffff, 0x000000, 0x555555, 0.067, 0x00a900, 0xe01c23,
            ),
        };
        let bg = rgb(bg);
        let overlay = if matches!(theme, "Default Dark" | "Solarized Dark") {
            Color32::WHITE
        } else {
            Color32::BLACK
        };
        Self {
            tab: rgb(tab),
            bg,
            panel: mix(bg, overlay, tint),
            hover: mix(bg, overlay, tint + 0.085),
            line: mix(bg, overlay, 0.33),
            text: rgb(text),
            muted: rgb(muted),
            good: rgb(good),
            bad: rgb(bad),
        }
    }
}

fn rgb(color: u32) -> Color32 {
    Color32::from_rgb((color >> 16) as u8, (color >> 8) as u8, color as u8)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let blend = |a: u8, b: u8| (a as f32 * (1.0 - t) + b as f32 * t).round() as u8;
    Color32::from_rgb(
        blend(a.r(), b.r()),
        blend(a.g(), b.g()),
        blend(a.b(), b.b()),
    )
}

pub(super) fn style(ctx: &egui::Context, theme: &str) {
    let p = Palette::from_theme(theme);
    let dark = matches!(theme, "Cyberpunk" | "Default Dark" | "Solarized Dark");
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    let mut style = (*ctx.style()).clone();
    style.visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    style.visuals.panel_fill = p.bg;
    style.visuals.window_fill = p.bg;
    style.visuals.extreme_bg_color = p.bg;
    style.visuals.faint_bg_color = p.panel;
    style.visuals.override_text_color = Some(p.text);
    style.visuals.selection.bg_fill =
        mix(p.bg, if dark { rgb(0x00bfff) } else { rgb(0x4c9ee6) }, 0.32);
    style.visuals.selection.stroke = Stroke::new(1.0, p.text);
    style.visuals.hyperlink_color = if dark { rgb(0x75d2ff) } else { rgb(0x145698) };
    style.visuals.error_fg_color = p.bad;
    style.visuals.warn_fg_color = p.bad;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    for (visual, fill) in [
        (&mut style.visuals.widgets.inactive, p.bg),
        (&mut style.visuals.widgets.hovered, p.hover),
        (&mut style.visuals.widgets.active, p.hover),
        (&mut style.visuals.widgets.open, p.hover),
    ] {
        visual.corner_radius = egui::CornerRadius::same(6);
        visual.bg_fill = fill;
        visual.weak_bg_fill = fill;
        visual.bg_stroke = Stroke::new(1.0, mix(p.bg, p.line, 0.55));
        visual.fg_stroke.color = p.text;
        visual.expansion = 0.0;
    }
    style.spacing.item_spacing = vec2(4.0, 4.0);
    style.spacing.button_padding = vec2(6.0, 2.0);
    style.spacing.interact_size = vec2(34.0, 25.0);
    style.spacing.indent = 12.0;
    for (key, size) in [
        (egui::TextStyle::Body, 17.0),
        (egui::TextStyle::Button, 17.0),
        (egui::TextStyle::Heading, 21.0),
        (egui::TextStyle::Small, 14.0),
    ] {
        style.text_styles.insert(key, FontId::proportional(size));
    }
    ctx.all_styles_mut(|current| current.clone_from(&style));
}

pub(super) fn fonts(ctx: &egui::Context) {
    let system_fonts = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut fonts = egui::FontDefinitions::default();
    if let Some(bytes) = ["msyh.ttc", "simhei.ttf", "simsun.ttc"]
        .into_iter()
        .find_map(|name| fs::read(system_fonts.join(name)).ok())
    {
        fonts
            .font_data
            .insert("Umi UI".into(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "Umi UI".into());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("Umi UI".into());
    }
    let mut bold = fonts.families[&egui::FontFamily::Proportional].clone();
    if let Ok(bytes) = fs::read(system_fonts.join("msyhbd.ttc")) {
        fonts.font_data.insert(
            "Umi UI Bold".into(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        bold.insert(0, "Umi UI Bold".into());
    }
    fonts
        .families
        .insert(egui::FontFamily::Name("Umi Strong".into()), bold);
    ctx.set_fonts(fonts);
}

fn strong(text: &str, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(
        size,
        egui::FontFamily::Name("Umi Strong".into()),
    ))
}

pub(super) fn texture(
    ctx: &egui::Context,
    image: &DynamicImage,
    name: &str,
) -> egui::TextureHandle {
    let rgba = image.to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    );
    ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
}

pub(super) fn panel<R>(ui: &mut Ui, p: Palette, contents: impl FnOnce(&mut Ui) -> R) -> R {
    let available = ui.available_size_before_wrap();
    egui::Frame::new()
        .fill(p.panel)
        .corner_radius(10)
        .inner_margin(Margin::same(7))
        .show(ui, |ui| {
            if available.x.is_finite() {
                ui.set_width((available.x - 14.0).max(0.0));
            }
            if available.y.is_finite() {
                ui.set_min_height((available.y - 14.0).max(0.0));
            }
            contents(ui)
        })
        .inner
}

fn icon(ctx: &egui::Context, name: &str) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("umi-svg", name));
    if let Some(icon) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return Some(icon);
    }
    macro_rules! png {
        ($name:literal) => {
            include_bytes!(concat!("../../assets/ui/", $name, ".png")).as_slice()
        };
    }
    let data = match name {
        "add" => png!("add"),
        "lock" => png!("lock"),
        "pin" => png!("pin"),
        "no" => png!("no"),
        "yes" => png!("yes"),
        "menu" => png!("menu"),
        "screenshot" => png!("screenshot"),
        "paste" => png!("paste"),
        "save" => png!("save"),
        "full_screen" => png!("full_screen"),
        "one_to_one" => png!("one_to_one"),
        "clear" => png!("clear"),
        "split_view" => png!("split_view"),
        "stop" => png!("stop"),
        "folder" => png!("folder"),
        "open_image" => png!("open_image"),
        "run" => png!("run"),
        "pause" => png!("pause"),
        "down" => png!("down"),
        "arrow_to_left" => png!("arrow_to_left"),
        "arrow_to_center" => png!("arrow_to_center"),
        _ => return None,
    };
    let image = image::load_from_memory_with_format(data, image::ImageFormat::Png).ok()?;
    let handle = texture(ctx, &image, name);
    ctx.data_mut(|data| data.insert_temp(id, handle.clone()));
    Some(handle)
}

pub(super) fn tool(
    ui: &mut Ui,
    name: &str,
    label: &str,
    hint: &str,
    color: Color32,
    frame: bool,
    enabled: bool,
) -> Response {
    let image = icon(ui.ctx(), name)
        .map(|handle| egui::Image::new((handle.id(), Vec2::splat(15.0))).tint(color));
    let text = (!label.is_empty()).then(|| RichText::new(label).color(color).into());
    let button = egui::Button::opt_image_and_text(image, text)
        .frame(frame)
        .min_size(vec2(23.0, 24.0));
    let response = ui.add_enabled(enabled, button);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, hint));
    response.on_hover_text(hint)
}

fn tab(ui: &mut Ui, label: &str, selected: bool, p: Palette) -> Response {
    let text = if selected {
        strong(label, 17.0)
    } else {
        RichText::new(label).color(p.muted)
    };
    let response = ui.add(
        egui::Button::new(text)
            .selected(selected)
            .fill(if selected { p.hover } else { p.panel })
            .stroke(Stroke::NONE)
            .min_size(vec2(57.0, 24.0)),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label)
    });
    response
}

pub(super) struct MediaUi {
    right: usize,
    fraction: f32,
    vertical: bool,
    fit: bool,
    scale: f32,
    overlay: bool,
    auto_generate: bool,
    generate_at: Option<f64>,
}

impl Default for MediaUi {
    fn default() -> Self {
        Self {
            right: 0,
            fraction: 0.49,
            vertical: false,
            fit: true,
            scale: 1.0,
            overlay: true,
            auto_generate: true,
            generate_at: None,
        }
    }
}

impl MediaUi {
    pub(super) fn index(page: Page) -> Option<usize> {
        match page {
            Page::Capture => Some(0),
            Page::Barcode => Some(1),
            _ => None,
        }
    }

    pub(super) fn show_history(&mut self) {
        self.right = 1;
    }
}

impl App {
    pub(super) fn open_page(&mut self, page: Page) {
        if !self.tabs.contains(&page) {
            self.tabs.push(page);
        }
        if self.page != page {
            self.active_box = None;
        }
        self.page = page;
        self.persist_tabs();
    }

    fn persist_tabs(&mut self) {
        if self.settings.active_tab != self.page || self.settings.open_tabs != self.tabs {
            self.settings.active_tab = self.page;
            self.settings.open_tabs.clone_from(&self.tabs);
            self.save_settings();
        }
    }

    pub(super) fn workspace(&mut self, ctx: &egui::Context) {
        if !self.settings.tabs_locked {
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::T)) {
                self.open_page(Page::Navigation);
            }
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::W))
                && let Some(index) = self.tabs.iter().position(|page| *page == self.page)
            {
                self.close_tab(index);
            }
        }
        let p = Palette::from_theme(self.settings.theme_id());
        egui::TopBottomPanel::top("umi_tabs")
            .exact_height(30.0)
            .frame(egui::Frame::new().fill(p.tab))
            .show(ctx, |ui| self.tab_strip(ui, p));
        if let Some(error) = &self.error {
            let mut dismiss = false;
            egui::TopBottomPanel::bottom("error_strip")
                .frame(
                    egui::Frame::new()
                        .fill(p.panel)
                        .inner_margin(Margin::symmetric(7, 3)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(error).color(p.bad));
                        dismiss = tool(ui, "no", "", "关闭提示", p.muted, false, true).clicked();
                    });
                });
            if dismiss {
                self.error = None;
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(7)))
            .show(ctx, |ui| match self.page {
                Page::Capture | Page::Barcode => self.media_page(ui, p, self.page),
                Page::Images | Page::Documents => self.original_batch(ui, self.page),
                Page::Settings => self.original_settings(ui),
                Page::Navigation => self.navigation(ui, p),
                Page::About => self.about(ui, p),
            });
    }

    fn tab_strip(&mut self, ui: &mut Ui, p: Palette) {
        let mut close = None;
        let mut open = None;
        let bar = ui.available_rect_before_wrap();
        let lock_rect = Rect::from_min_max(pos2(bar.right() - 30.0, bar.top()), bar.right_bottom());
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(lock_rect)
                .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
            |ui| {
                let color = if self.settings.tabs_locked {
                    p.good
                } else {
                    p.text
                };
                if tool(
                    ui,
                    "lock",
                    "",
                    if self.settings.tabs_locked {
                        "解锁标签页"
                    } else {
                        "锁定标签页"
                    },
                    color,
                    false,
                    true,
                )
                .clicked()
                {
                    self.settings.tabs_locked = !self.settings.tabs_locked;
                    self.save_settings();
                }
            },
        );
        let tabs_rect = Rect::from_min_max(bar.min, pos2(lock_rect.left() - 2.0, bar.bottom()));
        let mut tabs_ui = ui.new_child(UiBuilder::new().max_rect(tabs_rect));
        egui::ScrollArea::horizontal()
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut tabs_ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    let pin = if self.settings.window_top {
                        p.good
                    } else {
                        p.text
                    };
                    if tool(ui, "pin", "", "窗口置顶", pin, false, true).clicked() {
                        self.settings.window_top = !self.settings.window_top;
                        ui.ctx().send_viewport_cmd(ViewportCommand::WindowLevel(
                            if self.settings.window_top {
                                egui::WindowLevel::AlwaysOnTop
                            } else {
                                egui::WindowLevel::Normal
                            },
                        ));
                        self.save_settings();
                    }
                    for (i, &page) in self.tabs.iter().enumerate() {
                        let selected = self.page == page;
                        let mut response = None;
                        let frame = egui::Frame::new()
                            .fill(if selected { p.bg } else { p.tab })
                            .inner_margin(Margin::symmetric(5, 0));
                        let group = frame.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let title = if selected {
                                    strong(page.title(), 17.0)
                                } else {
                                    RichText::new(page.title()).color(p.muted)
                                };
                                let r = ui.add(
                                    egui::Button::new(title)
                                        .frame(false)
                                        .min_size(vec2(71.0, 29.0)),
                                );
                                r.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::Button,
                                        true,
                                        selected,
                                        page.title(),
                                    )
                                });
                                if r.clicked() {
                                    open = Some(page);
                                }
                                if !self.settings.tabs_locked && r.middle_clicked() {
                                    close = Some(i);
                                }
                                response = Some(r);
                                if !self.settings.tabs_locked {
                                    if tool(
                                        ui,
                                        "no",
                                        "",
                                        &format!("关闭{}标签页", page.title()),
                                        p.muted,
                                        false,
                                        true,
                                    )
                                    .clicked()
                                    {
                                        close = Some(i);
                                    }
                                }
                            })
                        });
                        if let Some(response) = response {
                            response.on_hover_text(page.title());
                        }
                        ui.painter().vline(
                            group.response.rect.right(),
                            (group.response.rect.top() + 6.0)
                                ..=(group.response.rect.bottom() - 5.0),
                            Stroke::new(1.0, p.line),
                        );
                    }
                    if !self.settings.tabs_locked
                        && tool(ui, "add", "", "新建标签页（Ctrl+T）", p.text, false, true)
                            .clicked()
                    {
                        open = Some(Page::Navigation);
                    }
                });
            });
        if let Some(index) = close {
            self.close_tab(index);
        } else if let Some(page) = open {
            self.open_page(page);
        }
    }

    fn close_tab(&mut self, index: usize) {
        let was_active = self.tabs[index] == self.page;
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(Page::Navigation);
        }
        if was_active {
            self.open_page(self.tabs[index.min(self.tabs.len() - 1)]);
        } else {
            self.persist_tabs();
        }
    }

    fn navigation(&mut self, ui: &mut Ui, p: Palette) {
        let mut hover = None;
        let mut picked = None;
        let available = ui.available_rect_before_wrap();
        let split = (available.width() * 0.32).clamp(155.0, 245.0);
        let left = Rect::from_min_size(available.min, vec2(split, available.height()));
        let right = Rect::from_min_max(
            pos2(left.right() + 7.0, available.top()),
            available.right_bottom(),
        );
        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
            panel(ui, p, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("功能页").color(p.muted));
                });
                ui.add_space(5.0);
                for (page, title) in Page::ALL {
                    let button =
                        ui.add_sized([ui.available_width(), 37.0], egui::Button::new(title));
                    if button.hovered() {
                        hover = Some(page);
                    }
                    if button.clicked() {
                        picked = Some(page);
                    }
                }
            })
        });
        ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
            panel(ui, p, |ui| {
                ui.add_space(7.0);
                let (heading, detail) = match hover {
                    Some(Page::Capture) => (
                        "截图OCR",
                        "按 F4 框选屏幕，或拖入、粘贴图片。识别结果可编辑、复制和导出。",
                    ),
                    Some(Page::Images) => (
                        "批量OCR",
                        "添加图片或文件夹，在表格中确认队列后开始识别。支持多种结果格式。",
                    ),
                    Some(Page::Documents) => (
                        "批量文档",
                        "导入 PDF、XPS、CBZ 或电子书，导出文本、双层 PDF 或纯文本 PDF。",
                    ),
                    Some(Page::Barcode) => {
                        ("二维码", "截图、粘贴或导入图片，扫描和生成二维码、条形码。")
                    }
                    Some(Page::Settings) => {
                        ("全局设置", "修改主题、窗口行为、托盘、F4 和启动偏好。")
                    }
                    Some(Page::About) => ("关于", "查看版本、模型来源和开源许可。"),
                    _ => ("欢迎使用文字识别", "请选择左侧的功能页。"),
                };
                ui.heading(heading);
                ui.add_space(9.0);
                ui.label(detail);
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("当前版本  •  v{}", env!("CARGO_PKG_VERSION")))
                        .color(p.muted),
                );
            })
        });
        if let Some(page) = picked {
            if let Some(index) = self.tabs.iter().position(|tab| *tab == Page::Navigation) {
                if self.tabs.contains(&page) {
                    self.tabs.remove(index);
                } else {
                    self.tabs[index] = page;
                }
            }
            self.open_page(page);
        }
    }

    fn about(&self, ui: &mut Ui, p: Palette) {
        panel(ui, p, |ui| {
            ui.heading("文字识别 WfZiUiBp");
            ui.label(format!("当前版本  •  v{}", env!("CARGO_PKG_VERSION")));
            ui.add_space(9.0);
            ui.label("离线截图、图片和文档 OCR，支持二维码与条码。");
            ui.label("界面参考 Umi-OCR 2.1.5，识别使用 PaddleOCR 模型与 ONNX Runtime。");
            ui.add_space(9.0);
            ui.hyperlink_to("Umi-OCR 上游项目", "https://github.com/hiroi-sora/Umi-OCR");
            ui.hyperlink_to("PaddleOCR 模型文档", "https://www.paddleocr.ai/");
        });
    }

    fn media_page(&mut self, ui: &mut Ui, p: Palette, page: Page) {
        let index = MediaUi::index(page).unwrap();
        let mut view = std::mem::take(&mut self.media[index]);
        let vertical = view.vertical || ui.available_width() < 565.0;
        let (left, right) = split(ui, p, page.title(), &mut view.fraction, vertical);
        ui.scope_builder(
            UiBuilder::new().max_rect(left).id_salt("image-side"),
            |ui| panel(ui, p, |ui| self.image_panel(ui, p, page, &mut view)),
        );
        ui.scope_builder(
            UiBuilder::new().max_rect(right).id_salt("record-side"),
            |ui| panel(ui, p, |ui| self.media_side(ui, p, page, &mut view)),
        );
        self.media[index] = view;
    }

    fn image_panel(&mut self, ui: &mut Ui, p: Palette, page: Page, view: &mut MediaUi) {
        let available = self.busy.is_none();
        let group = page.index();
        let index = self.selected[group];
        let generated = page == Page::Barcode && view.right == 2;
        if !generated
            && self
                .preview
                .as_ref()
                .is_none_or(|(old_page, old_index, _)| *old_page != page || *old_index != index)
        {
            self.preview = self.results[group]
                .get(index)
                .and_then(|item| item.preview.as_ref())
                .map(|image| (page, index, texture(ui.ctx(), image, "preview")));
        }
        let preview = if generated {
            self.qr_generated
                .as_ref()
                .map(|(_, texture)| texture.clone())
        } else {
            self.preview
                .as_ref()
                .and_then(|(old_page, old_index, texture)| {
                    (*old_page == page && *old_index == index).then(|| texture.clone())
                })
        };
        let wide = ui.available_width() >= 342.0;
        ui.horizontal(|ui| {
            if tool(
                ui,
                "screenshot",
                if wide { "截图" } else { "" },
                if page == Page::Capture {
                    "屏幕截图（F4）"
                } else {
                    "截图扫描"
                },
                p.text,
                true,
                available,
            )
            .clicked()
            {
                if generated {
                    view.right = 1;
                }
                if page == Page::Barcode {
                    self.capture_qr();
                } else {
                    self.capture(false);
                }
            }
            if tool(
                ui,
                "paste",
                if wide { "粘贴" } else { "" },
                "粘贴剪贴板图片",
                p.text,
                true,
                available,
            )
            .clicked()
            {
                if generated {
                    view.right = 1;
                }
                self.clipboard(page);
            }
            if self.busy == Some(page)
                && tool(ui, "stop", "", "停止任务", p.bad, false, true).clicked()
            {
                self.control.store(2, Ordering::Relaxed);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{:.0}%", view.scale * 100.0)).color(p.muted));
                if tool(
                    ui,
                    "one_to_one",
                    "",
                    "图片大小：实际",
                    p.muted,
                    false,
                    preview.is_some(),
                )
                .clicked()
                {
                    view.fit = false;
                    view.scale = 1.0;
                }
                if tool(
                    ui,
                    "full_screen",
                    "",
                    "图片大小：适应窗口",
                    p.muted,
                    false,
                    preview.is_some(),
                )
                .clicked()
                {
                    view.fit = true;
                }
                if tool(
                    ui,
                    "save",
                    "",
                    "保存图片",
                    p.muted,
                    false,
                    preview.is_some(),
                )
                .clicked()
                {
                    self.save_media_image(page, generated);
                }
                let menu = tool(ui, "menu", "", "图像菜单", p.muted, false, true);
                egui::Popup::menu(&menu)
                    .show(|ui| self.image_menu(ui, page, view, generated, preview.is_some()));
                if page == Page::Capture
                    && tool(
                        ui,
                        if view.overlay { "yes" } else { "no" },
                        if wide { "文字" } else { "" },
                        "在图像上显示识别文字",
                        if view.overlay { p.good } else { p.muted },
                        false,
                        true,
                    )
                    .clicked()
                {
                    view.overlay = !view.overlay;
                }
            });
        });
        let size = ui.available_size_before_wrap().max(vec2(1.0, 1.0));
        let (canvas, pointer) = ui.allocate_exact_size(size, Sense::click());
        ui.painter().rect(
            canvas,
            0,
            p.bg,
            Stroke::new(1.0, p.line),
            StrokeKind::Inside,
        );
        let canvas_name = if preview.is_some() {
            if generated {
                "二维码预览"
            } else {
                "原图预览"
            }
        } else if self.busy == Some(page) {
            "正在识别"
        } else if generated {
            "在右侧输入内容生成二维码"
        } else {
            "截图、拖入或粘贴图片"
        };
        pointer
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, canvas_name));
        pointer.context_menu(|ui| self.image_menu(ui, page, view, generated, preview.is_some()));
        if let Some(texture) = preview {
            let image_size = if generated {
                texture.size_vec2()
            } else {
                self.results[group]
                    .get(index)
                    .map_or(texture.size_vec2(), |result| {
                        vec2(result.image_size[0] as f32, result.image_size[1] as f32)
                    })
            };
            let bounds = canvas.shrink(2.0);
            if view.fit {
                view.scale = (bounds.width() / image_size.x.max(1.0))
                    .min(bounds.height() / image_size.y.max(1.0))
                    .min(1.0)
                    .max(0.01);
            }
            if pointer.hovered() {
                let wheel = ui.input(|input| {
                    if input.modifiers.ctrl {
                        input.raw_scroll_delta.y
                    } else {
                        0.0
                    }
                });
                if wheel != 0.0 {
                    view.fit = false;
                    view.scale = (view.scale * (wheel * 0.0019).exp()).clamp(0.05, 6.0);
                }
            }
            let display = image_size * view.scale;
            let mut canvas_ui = ui.new_child(UiBuilder::new().max_rect(bounds).id_salt("canvas"));
            canvas_ui.set_clip_rect(bounds);
            egui::ScrollArea::both()
                .id_salt((page.title(), index))
                .auto_shrink([false, false])
                .show(&mut canvas_ui, |ui| {
                    let pad = (bounds.size() - display).max(Vec2::ZERO) * 0.5;
                    let (outer, _) = ui.allocate_exact_size(display + pad * 2.0, Sense::hover());
                    let rect = Rect::from_min_size(outer.min + pad, display);
                    let response = ui.interact(rect, ui.id().with("image-pixels"), Sense::click());
                    ui.painter().image(
                        texture.id(),
                        rect,
                        Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    if page == Page::Capture
                        && view.overlay
                        && let Some(result) = self.results[group].get(index)
                    {
                        let click = response
                            .interact_pointer_pos()
                            .filter(|_| response.clicked());
                        for (box_index, item) in result.blocks.iter().enumerate() {
                            let points: Vec<_> = item
                                .block
                                .points
                                .iter()
                                .map(|point| {
                                    rect.min + vec2(point[0] as f32, point[1] as f32) * view.scale
                                })
                                .collect();
                            let area = Rect::from_points(&points);
                            if click.is_some_and(|pos| area.contains(pos)) {
                                self.active_box = Some(box_index);
                            }
                            let current = self.active_box == Some(box_index);
                            let edge = if current {
                                rgb(0xffb400)
                            } else {
                                rgb(0x179f87)
                            };
                            ui.painter().add(egui::Shape::convex_polygon(
                                points,
                                Color32::WHITE,
                                Stroke::new(1.0, edge),
                            ));
                            if area.height() >= 8.0 {
                                let size = (area.height() * 0.78).clamp(7.0, 36.0);
                                let font = FontId::proportional(size);
                                let galley = ui.painter().layout_no_wrap(
                                    item.block.text.clone(),
                                    font,
                                    Color32::BLACK,
                                );
                                let scale = ((area.width() - 2.0) / galley.size().x.max(1.0))
                                    .clamp(0.25, 1.0);
                                ui.painter()
                                    .with_clip_rect(area.intersect(ui.clip_rect()))
                                    .text(
                                        area.left_center() + vec2(1.0, 0.0),
                                        Align2::LEFT_CENTER,
                                        &item.block.text,
                                        FontId::proportional(size * scale),
                                        Color32::BLACK,
                                    );
                            }
                        }
                    }
                });
        } else {
            let tip = if self.busy == Some(page) {
                "正在识别…"
            } else if generated {
                "在右侧输入内容生成二维码"
            } else {
                "截图、拖入或粘贴图片"
            };
            ui.painter().text(
                canvas.center(),
                Align2::CENTER_CENTER,
                tip,
                FontId::proportional(16.0),
                p.text,
            );
        }
    }

    fn image_menu(
        &mut self,
        ui: &mut Ui,
        page: Page,
        view: &mut MediaUi,
        generated: bool,
        image: bool,
    ) {
        if ui.button("打开图片…").clicked() {
            self.choose(page, None);
            ui.close();
        }
        if page == Page::Capture && ui.button("重复截图").clicked() {
            self.capture(true);
            ui.close();
        }
        if ui
            .add_enabled(image, egui::Button::new("保存图片…"))
            .clicked()
        {
            self.save_media_image(page, generated);
            ui.close();
        }
        if page == Page::Capture {
            let group = page.index();
            if let Some(item) = self.results[group].get(self.selected[group])
                && !item.blocks.is_empty()
            {
                ui.menu_button("按文本框复制", |ui| {
                    for (index, block) in item.blocks.iter().enumerate() {
                        let label: String = block.block.text.chars().take(32).collect();
                        if ui.button(format!("{:02}  {label}", index + 1)).clicked() {
                            ui.ctx().copy_text(block.block.text.clone());
                            ui.close();
                        }
                    }
                });
            }
        }
        if ui
            .button(if view.vertical {
                "切换为左右布局"
            } else {
                "切换为上下布局"
            })
            .clicked()
        {
            view.vertical = !view.vertical;
            ui.close();
        }
    }

    fn save_media_image(&mut self, page: Page, generated: bool) {
        let group = page.index();
        let image = if generated {
            self.qr_generated.as_ref().map(|(image, _)| image)
        } else {
            self.results[group]
                .get(self.selected[group])
                .and_then(|item| item.preview.as_ref())
        };
        if let Some(image) = image
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name(if generated {
                    "二维码.png"
                } else {
                    "截图.png"
                })
                .add_filter("PNG", &["png"])
                .save_file()
        {
            let result = image.save(path).map_err(Into::into);
            self.apply(result);
        }
    }

    fn media_side(&mut self, ui: &mut Ui, p: Palette, page: Page, view: &mut MediaUi) {
        ui.horizontal(|ui| {
            let tabs: &[&str] = if page == Page::Barcode {
                &["设置", "记录", "生成"]
            } else {
                &["设置", "记录"]
            };
            for (index, &label) in tabs.iter().enumerate() {
                if tab(ui, label, view.right == index, p).clicked() {
                    view.right = index;
                }
            }
            if view.right == 0 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("重置").color(p.bad)).frame(false))
                        .clicked()
                    {
                        if page == Page::Capture {
                            let defaults = Settings::default();
                            self.settings.ocr.language = defaults.ocr.language;
                            self.settings.ocr.side_limit = defaults.ocr.side_limit;
                            self.settings.ocr.classify = defaults.ocr.classify;
                            self.settings.parser = defaults.parser;
                            self.settings.auto_copy = defaults.auto_copy;
                            self.save_settings();
                        } else {
                            self.qr_format = "QRCode";
                            self.qr_size = [400, 400];
                            self.qr_margin = -1;
                        }
                    }
                });
            }
        });
        match view.right {
            1 => self.media_history(ui, p, page),
            2 => self.qr_input(ui, p, view),
            _ if page == Page::Barcode => self.qr_settings(ui, p, view),
            _ => self.capture_settings(ui, p),
        }
    }

    fn capture_settings(&mut self, ui: &mut Ui, p: Palette) {
        let mut changed = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                section(ui, p, "文字识别（PaddleOCR）", |ui| {
                    changed |= option(ui, "纠正文本方向", |ui, _| {
                        toggle(ui, p, &mut self.settings.ocr.classify, "纠正文本方向").changed()
                    });
                    changed |= option(ui, "语言/模型库", |ui, label| {
                        let current = LANGUAGES
                            .iter()
                            .find(|(key, _)| *key == self.settings.ocr.language)
                            .map_or(self.settings.ocr.language.as_str(), |(_, label)| *label);
                        let mut changed = false;
                        egui::ComboBox::from_id_salt("capture-language")
                            .width(175.0)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for &(id, name) in LANGUAGES {
                                    changed |= ui
                                        .selectable_value(
                                            &mut self.settings.ocr.language,
                                            id.into(),
                                            name,
                                        )
                                        .changed();
                                }
                            })
                            .response
                            .labelled_by(label);
                        changed
                    });
                    changed |= option(ui, "限制图像边长", |ui, label| {
                        let mut changed = false;
                        let value = self.settings.ocr.side_limit;
                        let display = if value == 960 {
                            "960（默认）".into()
                        } else {
                            value.to_string()
                        };
                        egui::ComboBox::from_id_salt("capture-limit")
                            .width(175.0)
                            .selected_text(display)
                            .show_ui(ui, |ui| {
                                for size in [320, 640, 960, 1280, 1600, 1920, 2560] {
                                    changed |= ui
                                        .selectable_value(
                                            &mut self.settings.ocr.side_limit,
                                            size,
                                            size.to_string(),
                                        )
                                        .changed();
                                }
                            })
                            .response
                            .labelled_by(label);
                        changed
                    });
                });
                section(ui, p, "OCR文本后处理", |ui| {
                    changed |= option(ui, "排版解析方案", |ui, label| {
                        let mut changed = false;
                        let display = layout::MODES
                            .iter()
                            .find(|(id, _)| *id == self.settings.parser)
                            .map_or("不处理", |(_, label)| *label);
                        egui::ComboBox::from_id_salt("capture-parser")
                            .width(175.0)
                            .selected_text(display)
                            .show_ui(ui, |ui| {
                                for &(id, label) in layout::MODES {
                                    changed |= ui
                                        .selectable_value(
                                            &mut self.settings.parser,
                                            id.into(),
                                            label,
                                        )
                                        .changed();
                                }
                            })
                            .response
                            .labelled_by(label);
                        changed
                    });
                });
                section(ui, p, "快捷键", |ui| {
                    option(ui, "屏幕截图", |ui, _| {
                        ui.label(
                            RichText::new(if self.settings.hotkey && !self.no_hotkey {
                                "F4"
                            } else {
                                "未启用"
                            })
                            .color(p.muted),
                        )
                    });
                    option(ui, "粘贴图片", |ui, _| {
                        ui.label(RichText::new("Ctrl+V").color(p.muted))
                    });
                    option(ui, "重复截图", |ui, _| {
                        if ui.button("重截上次区域").clicked() {
                            self.capture(true);
                        }
                    });
                });
                section(ui, p, "识图后的操作", |ui| {
                    changed |= option(ui, "复制结果", |ui, _| {
                        toggle(ui, p, &mut self.settings.auto_copy, "复制结果").changed()
                    });
                });
                section(ui, p, "其他", |ui| {
                    changed |= option(ui, "截图前隐藏主窗口", |ui, _| {
                        let changed = toggle(
                            ui,
                            p,
                            &mut self.settings.hide_before_capture,
                            "截图前隐藏主窗口",
                        )
                        .changed();
                        if changed {
                            self.set_capture_preference();
                        }
                        changed
                    });
                    option(ui, "窗口和托盘", |ui, _| {
                        if ui.button("打开全局设置").clicked() {
                            self.open_page(Page::Settings);
                        }
                    });
                });
            });
        if changed {
            self.save_settings();
        }
    }

    fn media_history(&mut self, ui: &mut Ui, p: Palette, page: Page) {
        let group = page.index();
        let count = self.results[group].len();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(count != 0, egui::Button::new("复制全部"))
                .clicked()
            {
                let text = self.results[group]
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                ui.ctx().copy_text(text);
            }
            if tool(ui, "save", "", "导出全部结果", p.muted, false, count != 0).clicked() {
                self.save_results(true);
            }
            if tool(ui, "clear", "", "清空记录", p.muted, false, count != 0).clicked() {
                self.results[group].clear();
                self.selected[group] = 0;
                self.preview = None;
                self.active_box = None;
            }
            if let Some(text) = self.active_box.and_then(|box_index| {
                self.results[group]
                    .get(self.selected[group])
                    .and_then(|item| item.blocks.get(box_index))
            }) && ui
                .small_button("复制文本框")
                .on_hover_text(format!("置信度 {:.0}%", text.block.score * 100.0))
                .clicked()
            {
                ui.ctx().copy_text(text.block.text.clone());
            }
        });
        if self.results[group].is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("暂无记录").color(p.muted)));
            return;
        }
        let mut remove = None;
        let mut pick = None;
        egui::ScrollArea::vertical()
            .id_salt((page.title(), "history"))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (index, item) in self.results[group].iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        egui::Frame::new()
                            .fill(p.bg)
                            .corner_radius(7)
                            .inner_margin(Margin::same(6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let name = RichText::new(format!(
                                        "{}  ·  {:.2}s",
                                        item.title,
                                        item.elapsed.as_secs_f64()
                                    ))
                                    .size(13.0)
                                    .color(p.muted);
                                    if ui
                                        .selectable_label(self.selected[group] == index, name)
                                        .clicked()
                                    {
                                        pick = Some(index);
                                    }
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if tool(ui, "no", "", "移除此条记录", p.muted, false, true)
                                            .clicked()
                                        {
                                            remove = Some(index);
                                        }
                                        if tool(
                                            ui,
                                            "paste",
                                            "",
                                            "复制本条文字",
                                            p.muted,
                                            false,
                                            !item.text.is_empty(),
                                        )
                                        .clicked()
                                        {
                                            ui.ctx().copy_text(item.text.clone());
                                        }
                                    });
                                });
                                let id = ui.id().with("history-edit");
                                let changed = set_accessible_text(ui, id, &mut item.text);
                                let mut response = ui.add(
                                    egui::TextEdit::multiline(&mut item.text)
                                        .id(id)
                                        .hint_text("未识别到内容")
                                        .desired_rows(2)
                                        .desired_width(f32::INFINITY)
                                        .frame(false),
                                );
                                if changed {
                                    response.mark_changed();
                                }
                                name_accessible_text(
                                    ui,
                                    &response,
                                    &format!("识别文字：{}", item.title),
                                    &item.text,
                                );
                                if response.has_focus() {
                                    pick = Some(index);
                                }
                            })
                    });
                }
            });
        if let Some(index) = remove {
            self.results[group].remove(index);
            let selected = self.selected[group];
            self.selected[group] = if selected > index {
                selected - 1
            } else {
                selected.min(self.results[group].len().saturating_sub(1))
            };
            self.preview = None;
            self.active_box = None;
        } else if let Some(index) = pick
            && self.selected[group] != index
        {
            self.selected[group] = index;
            self.active_box = None;
        }
    }

    fn qr_settings(&mut self, ui: &mut Ui, p: Palette, view: &mut MediaUi) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                section(ui, p, "扫描二维码 / 条形码", |ui| {
                    ui.label("通过截图、粘贴、拖入或打开图片开始扫描。");
                    if ui.button("打开图片…").clicked() {
                        self.choose(Page::Barcode, None);
                    }
                });
                section(ui, p, "生成二维码 / 条形码", |ui| {
                    option(ui, "码制", |ui, label| {
                        egui::ComboBox::from_id_salt("barcode-format")
                            .width(165.0)
                            .selected_text(self.qr_format)
                            .show_ui(ui, |ui| {
                                for format in [
                                    "QRCode",
                                    "DataMatrix",
                                    "Aztec",
                                    "PDF417",
                                    "Code128",
                                    "Code39",
                                    "EAN13",
                                    "EAN8",
                                    "ITF",
                                    "UPCA",
                                    "UPCE",
                                    "Codabar",
                                ] {
                                    ui.selectable_value(&mut self.qr_format, format, format);
                                }
                            })
                            .response
                            .labelled_by(label);
                    });
                    option(ui, "图像尺寸", |ui, label| {
                        ui.add(egui::DragValue::new(&mut self.qr_size[0]).range(32..=2048))
                            .labelled_by(label);
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut self.qr_size[1]).range(32..=2048));
                    });
                    option(ui, "留白", |ui, label| {
                        ui.add(egui::DragValue::new(&mut self.qr_margin).range(-1..=64))
                            .labelled_by(label)
                            .on_hover_text("-1 使用当前码制的默认值");
                    });
                    if ui.button("输入文字并生成").clicked() {
                        view.right = 2;
                    }
                });
            });
    }

    fn qr_input(&mut self, ui: &mut Ui, p: Palette, view: &mut MediaUi) {
        let mut refresh = false;
        ui.horizontal(|ui| {
            if ui.button("设置").clicked() {
                view.right = 0;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh = ui.button("刷新").clicked();
                ui.checkbox(&mut view.auto_generate, "自动刷新");
            });
        });
        let now = ui.input(|input| input.time);
        let id = ui.id().with("qr-content");
        let changed = set_accessible_text(ui, id, &mut self.qr_text);
        let mut response = ui.add_sized(
            ui.available_size_before_wrap().max(Vec2::splat(1.0)),
            egui::TextEdit::multiline(&mut self.qr_text)
                .id(id)
                .hint_text("输入要生成的文字或网址"),
        );
        if changed {
            response.mark_changed();
        }
        name_accessible_text(ui, &response, "二维码内容", &self.qr_text);
        if response.changed() && view.auto_generate {
            view.generate_at = Some(now + 0.5);
        }
        if !view.auto_generate {
            view.generate_at = None;
        }
        if let Some(when) = view.generate_at {
            if now >= when {
                refresh = true;
            } else {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(when - now));
            }
        }
        if refresh {
            view.generate_at = None;
            if self.qr_text.is_empty() {
                self.qr_generated = None;
                return;
            }
            match qr::generate_with(
                &self.qr_text,
                self.qr_format,
                self.qr_size,
                self.qr_margin,
                -1,
            ) {
                Ok(image) => {
                    let tex = texture(ui.ctx(), &image, "generated-qr");
                    self.qr_generated = Some((image, tex));
                    self.error = None;
                    view.fit = true;
                    ui.ctx().request_repaint();
                }
                Err(error) => {
                    self.error = Some(format!("生成失败 · {error:#}"));
                    ui.ctx().request_repaint();
                }
            }
        }
        let _ = p;
    }
}

pub(super) fn set_accessible_text(ui: &Ui, id: egui::Id, text: &mut String) -> bool {
    use egui::accesskit::{Action, ActionData};
    let mut changed = false;
    ui.input_mut(|input| {
        input.consume_accesskit_action_requests(id, |request| {
            if request.action == Action::SetValue
                && let Some(ActionData::Value(value)) = &request.data
            {
                if text.as_str() != value.as_ref() {
                    *text = value.to_string();
                    changed = true;
                }
                return true;
            }
            false
        })
    });
    changed
}

pub(super) fn name_accessible_text(ui: &Ui, response: &Response, name: &str, value: &str) {
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label(name);
        node.set_value(value);
        node.add_action(egui::accesskit::Action::SetValue);
    });
}

pub(super) fn section(ui: &mut Ui, p: Palette, title: &str, contents: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    ui.label(RichText::new(title).color(p.text));
    egui::Frame::new()
        .fill(p.bg)
        .corner_radius(9)
        .inner_margin(Margin::symmetric(7, 3))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            contents(ui);
        });
}

pub(super) fn option<R>(
    ui: &mut Ui,
    label: &str,
    contents: impl FnOnce(&mut Ui, egui::Id) -> R,
) -> R {
    ui.horizontal(|ui| {
        let id = ui.label(label).id;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| contents(ui, id))
            .inner
    })
    .inner
}

fn toggle(ui: &mut Ui, p: Palette, value: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(34.0, 17.0), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *value, label)
    });
    let fill = if *value {
        p.good
    } else if response.hovered() {
        p.hover
    } else {
        p.bg
    };
    ui.painter().rect(
        rect,
        5,
        fill,
        Stroke::new(1.5, if *value { p.good } else { p.line }),
        StrokeKind::Inside,
    );
    let center = rect.center();
    if *value {
        ui.painter().add(egui::Shape::line(
            vec![
                center + vec2(-5.0, 0.0),
                center + vec2(-1.5, 3.0),
                center + vec2(5.0, -3.5),
            ],
            Stroke::new(2.0, p.bg),
        ));
    } else {
        ui.painter().hline(
            (center.x - 4.0)..=(center.x + 4.0),
            center.y,
            Stroke::new(1.0, p.bad),
        );
    }
    response
}

fn split(ui: &mut Ui, p: Palette, id: &str, ratio: &mut f32, vertical: bool) -> (Rect, Rect) {
    let whole = ui.available_rect_before_wrap();
    let gap = 7.0;
    let divider = |ratio: f32| {
        if vertical {
            Rect::from_min_size(
                pos2(whole.left(), whole.top() + (whole.height() - gap) * ratio),
                vec2(whole.width(), gap),
            )
        } else {
            Rect::from_min_size(
                pos2(whole.left() + (whole.width() - gap) * ratio, whole.top()),
                vec2(gap, whole.height()),
            )
        }
    };
    let response = ui
        .interact(divider(*ratio), ui.id().with(("split", id)), Sense::drag())
        .on_hover_cursor(if vertical {
            egui::CursorIcon::ResizeVertical
        } else {
            egui::CursorIcon::ResizeHorizontal
        });
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let next = if vertical {
            (pos.y - whole.top()) / whole.height().max(1.0)
        } else {
            (pos.x - whole.left()) / whole.width().max(1.0)
        };
        *ratio = next.clamp(0.27, 0.73);
    }
    let bar = divider(*ratio);
    if response.hovered() || response.dragged() {
        ui.painter().rect_filled(bar.shrink(2.0), 1, p.line);
    }
    if vertical {
        (
            Rect::from_min_max(whole.min, pos2(whole.right(), bar.top())),
            Rect::from_min_max(pos2(whole.left(), bar.bottom()), whole.max),
        )
    } else {
        (
            Rect::from_min_max(whole.min, pos2(bar.left(), whole.bottom())),
            Rect::from_min_max(pos2(bar.right(), whole.top()), whole.max),
        )
    }
}
