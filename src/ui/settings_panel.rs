use super::*;

const THEMES: [(&str, &str); 5] = [
    ("Default Light", "珍珠白"),
    ("Default Dark", "云墨黑"),
    ("Solarized Light", "Solarized Light"),
    ("Solarized Dark", "Solarized Dark"),
    ("Cyberpunk", "赛博朋克"),
];

const GROUPS: [&str; 6] = ["快捷方式", "界面和外观", "窗口", "截图", "服务", "OCR 推理"];

impl App {
    pub(super) fn original_settings(&mut self, ui: &mut egui::Ui) {
        let id = ui.id().with("global-settings");
        let advanced_id = id.with("advanced");
        let mut advanced = ui
            .ctx()
            .data(|data| data.get_temp::<bool>(advanced_id))
            .unwrap_or(false);
        let mut target = None;
        let mut reset = false;
        let sidebar_max = (ui.available_width() * 0.55).max(104.0);
        egui::SidePanel::left(id.with("categories"))
            .default_width(240.0_f32.min(sidebar_max))
            .width_range(104.0..=sidebar_max)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(10)
                    .inner_margin(7),
            )
            .show_inside(ui, |ui| {
                egui::TopBottomPanel::bottom(id.with("settings-actions"))
                    .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 4)))
                    .show_inside(ui, |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button(RichText::new("重置").color(ui.visuals().error_fg_color))
                                .clicked()
                            {
                                reset = true;
                            }
                            ui.toggle_value(&mut advanced, "高级")
                                .on_hover_text("显示更多高级选项");
                        });
                    });
                egui::ScrollArea::vertical()
                    .id_salt(id.with("groups-scroll"))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for (index, title) in GROUPS.into_iter().enumerate() {
                            if index == 5 && !advanced {
                                continue;
                            }
                            if ui
                                .add_sized([ui.available_width(), 40.0], egui::Button::new(title))
                                .clicked()
                            {
                                target = Some(index);
                            }
                        }
                    });
            });
        ui.ctx()
            .data_mut(|data| data.insert_temp(advanced_id, advanced));

        let reset_id = id.with("reset");
        if reset {
            ui.ctx().data_mut(|data| data.insert_temp(reset_id, true));
        }
        let mut changed = false;
        egui::ScrollArea::vertical()
            .id_salt(id.with("settings-scroll"))
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 7.0;
                for (index, title) in GROUPS.into_iter().enumerate() {
                    if index == 5 && !advanced {
                        continue;
                    }
                    let response = ui
                        .scope(|ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(title);
                            ui.group(|ui| {
                                ui.set_min_width(ui.available_width());
                                changed |= self.settings_group(ui, index);
                            });
                        })
                        .response;
                    if target == Some(index) {
                        response.scroll_to_me(Some(egui::Align::Min));
                    }
                }
            });

        if ui
            .ctx()
            .data(|data| data.get_temp::<bool>(reset_id))
            .unwrap_or(false)
        {
            let mut accept = false;
            let mut cancel = false;
            let modal = egui::Modal::new(reset_id).show(ui.ctx(), |ui| {
                ui.set_min_width(250.0);
                ui.label("要重置本页的设定吗？");
                ui.horizontal(|ui| {
                    accept = ui.button("重置设定").clicked();
                    cancel = ui.button("取消").clicked();
                });
            });
            if accept {
                self.reset_global_settings(ui.ctx());
                changed = true;
            }
            if accept || cancel || modal.should_close() {
                ui.ctx().data_mut(|data| data.remove::<bool>(reset_id));
            }
        }
        if changed {
            self.save_settings();
        }
    }

    fn settings_group(&mut self, ui: &mut egui::Ui, index: usize) -> bool {
        match index {
            0 => {
                let mut startup = self.startup;
                settings_row(ui, "开机自启", |ui, label| {
                    ui.checkbox(&mut startup, "").labelled_by(label);
                });
                if startup != self.startup {
                    self.change_startup(startup);
                }
                false
            }
            1 => {
                let mut theme = self.settings.theme_id().to_owned();
                settings_row(ui, "主题", |ui, label| {
                    let title = THEMES
                        .iter()
                        .find(|(id, _)| *id == theme)
                        .map_or("珍珠白", |(_, title)| *title);
                    egui::ComboBox::from_id_salt("theme")
                        .width(136.0)
                        .selected_text(title)
                        .show_ui(ui, |ui| {
                            for (id, title) in THEMES {
                                ui.selectable_value(&mut theme, id.to_owned(), title);
                            }
                        })
                        .response
                        .labelled_by(label);
                });
                let mut scale = self.settings.ui_scale;
                settings_row(ui, "界面与文字大小", |ui, label| {
                    egui::ComboBox::from_id_salt("ui-scale")
                        .width(96.0)
                        .selected_text(percent(scale))
                        .show_ui(ui, |ui| {
                            for ratio in [0.5, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5] {
                                ui.selectable_value(&mut scale, ratio, percent(ratio));
                            }
                        })
                        .response
                        .labelled_by(label);
                });
                let theme_changed = theme != self.settings.theme_id();
                let scale_changed = scale != self.settings.ui_scale;
                if theme_changed {
                    self.settings.theme = Some(theme);
                    style(ui.ctx(), self.settings.theme_id());
                }
                if scale_changed {
                    self.settings.ui_scale = scale;
                    ui.ctx().set_zoom_factor(scale);
                }
                theme_changed || scale_changed
            }
            2 => {
                let mut changed = settings_row(ui, "启动时缩小到任务栏", |ui, label| {
                    ui.checkbox(&mut self.settings.startup_hidden, "")
                        .labelled_by(label)
                        .changed()
                });
                if settings_row(ui, "窗口置顶", |ui, label| {
                    ui.checkbox(&mut self.settings.window_top, "")
                        .labelled_by(label)
                        .changed()
                }) {
                    set_window_level(ui.ctx(), self.settings.window_top);
                    changed = true;
                }
                changed |= settings_row(ui, "锁定标签栏", |ui, label| {
                    ui.checkbox(&mut self.settings.tabs_locked, "")
                        .labelled_by(label)
                        .changed()
                });
                let mut tray = self.settings.tray;
                settings_row(ui, "关闭主窗口时", |ui, label| {
                    let text = if tray {
                        "最小化到系统托盘"
                    } else {
                        "退出应用"
                    };
                    egui::ComboBox::from_id_salt("on-close")
                        .width(145.0)
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut tray, true, "最小化到系统托盘");
                            ui.selectable_value(&mut tray, false, "退出应用");
                        })
                        .response
                        .labelled_by(label);
                });
                if tray != self.settings.tray {
                    changed |= self.change_tray(tray);
                }
                changed
            }
            3 => {
                let mut hotkey = self.settings.hotkey;
                settings_row(ui, "屏幕截图快捷键", |ui, label| {
                    ui.add_enabled(!self.no_hotkey, egui::Checkbox::new(&mut hotkey, "F4"))
                        .labelled_by(label);
                });
                let mut changed = hotkey != self.settings.hotkey && self.change_hotkey(hotkey);
                if settings_row(ui, "截图前隐藏主窗口", |ui, label| {
                    ui.checkbox(&mut self.settings.hide_before_capture, "")
                        .labelled_by(label)
                        .changed()
                }) {
                    self.set_capture_preference();
                    changed = true;
                }
                changed |= settings_row(ui, "识别后复制结果", |ui, label| {
                    ui.checkbox(&mut self.settings.auto_copy, "")
                        .labelled_by(label)
                        .changed()
                });
                changed
            }
            4 => {
                settings_row(ui, "主机", |ui, _| {
                    ui.label("仅本地 (127.0.0.1)");
                });
                settings_row(ui, "默认端口", |ui, _| {
                    ui.label("1225");
                });
                if settings_row(ui, "命令行启动", |ui, _| {
                    ui.button("复制启动命令").clicked()
                }) {
                    let command = std::env::current_exe().map(|path| {
                        format!(
                            "& '{}' --serve --port 1225",
                            path.to_string_lossy().replace('\'', "''")
                        )
                    });
                    match command {
                        Ok(command) => ui.ctx().copy_text(command),
                        Err(error) => self.apply(Err(error.into())),
                    }
                }
                false
            }
            5 => settings_row(ui, "线程数", |ui, label| {
                ui.add(egui::DragValue::new(&mut self.settings.ocr.threads).range(1..=32))
                    .labelled_by(label)
                    .changed()
            }),
            _ => false,
        }
    }

    fn change_startup(&mut self, enabled: bool) {
        let result = crate::startup::set(enabled).context("无法更新本应用的登录启动项");
        if result.is_ok() {
            self.startup = enabled;
        }
        self.apply(result);
    }

    fn change_tray(&mut self, enabled: bool) -> bool {
        let result = self
            .desktop
            .as_ref()
            .context("Windows 托盘服务不可用")
            .and_then(|desktop| desktop.set_tray(enabled));
        let changed = result.is_ok();
        if changed {
            self.settings.tray = enabled;
        }
        self.apply(result);
        changed
    }

    fn change_hotkey(&mut self, enabled: bool) -> bool {
        let result = self
            .desktop
            .as_mut()
            .context("Windows 截图服务不可用")
            .and_then(|desktop| desktop.set_hotkey(enabled));
        let changed = result.is_ok();
        if changed {
            self.settings.hotkey = enabled;
        }
        self.apply(result);
        changed
    }

    fn reset_global_settings(&mut self, context: &egui::Context) {
        let defaults = Settings::default();
        if self.startup {
            self.change_startup(false);
        }
        if self.settings.tray != defaults.tray {
            self.change_tray(defaults.tray);
        }
        if !self.no_hotkey && self.settings.hotkey != defaults.hotkey {
            self.change_hotkey(defaults.hotkey);
        }
        self.settings.theme = defaults.theme;
        self.settings.dark = defaults.dark;
        self.settings.ui_scale = defaults.ui_scale;
        self.settings.window_top = defaults.window_top;
        self.settings.tabs_locked = defaults.tabs_locked;
        self.settings.startup_hidden = defaults.startup_hidden;
        self.settings.hide_before_capture = defaults.hide_before_capture;
        self.settings.auto_copy = defaults.auto_copy;
        self.settings.ocr.threads = defaults.ocr.threads;
        style(context, self.settings.theme_id());
        context.set_zoom_factor(self.settings.ui_scale);
        set_window_level(context, self.settings.window_top);
        self.set_capture_preference();
    }
}

fn settings_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    value: impl FnOnce(&mut egui::Ui, egui::Id) -> R,
) -> R {
    ui.horizontal(|ui| {
        let width = 180.0_f32.min(ui.available_width() * 0.48);
        let label_id = ui
            .allocate_ui_with_layout(
                egui::vec2(width, 24.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| ui.add(egui::Label::new(label).wrap().selectable(false)).id,
            )
            .inner;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            value(ui, label_id)
        })
        .inner
    })
    .inner
}

fn percent(scale: f32) -> String {
    format!("{:.0}%", scale * 100.0)
}

fn set_window_level(context: &egui::Context, pinned: bool) {
    let level = if pinned {
        egui::WindowLevel::AlwaysOnTop
    } else {
        egui::WindowLevel::Normal
    };
    context.send_viewport_cmd(ViewportCommand::WindowLevel(level));
}
