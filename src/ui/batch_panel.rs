use super::workspace::tool;
use super::workspace::{Palette, name_accessible_text, panel, set_accessible_text};
use super::*;
use std::collections::{BTreeMap, HashSet};

const QUEUE_LIMIT: usize = 10_000;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
enum Destination {
    #[default]
    Source,
    Folder,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
enum PdfOutput {
    #[default]
    Disabled,
    Searchable,
    TextOnly,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
struct BatchPrefs {
    recursive: bool,
    destination: Destination,
    folder: Option<PathBuf>,
    txt: bool,
    plain: bool,
    separate: bool,
    markdown: bool,
    csv: bool,
    jsonl: bool,
    pdf: PdfOutput,
    skip_empty: bool,
}

impl Default for BatchPrefs {
    fn default() -> Self {
        Self {
            recursive: false,
            destination: Destination::Source,
            folder: None,
            txt: true,
            plain: false,
            separate: false,
            markdown: false,
            csv: false,
            jsonl: false,
            pdf: PdfOutput::Disabled,
            skip_empty: true,
        }
    }
}

impl BatchPrefs {
    fn has_text_output(&self) -> bool {
        self.txt || self.plain || self.separate || self.markdown || self.csv || self.jsonl
    }

    fn folder_for(&self, path: &Path) -> Option<PathBuf> {
        match self.destination {
            Destination::Source => path.parent().map(Path::to_owned),
            Destination::Folder => self.folder.clone(),
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Preferences {
    images: BatchPrefs,
    documents: BatchPrefs,
}

#[derive(Default)]
pub(super) struct BatchUi {
    images: BatchPage,
    documents: BatchPage,
    preferences_loaded: bool,
}

#[derive(Default)]
struct BatchPage {
    prefs: BatchPrefs,
    rows: Vec<Row>,
    row_keys: HashSet<PathBuf>,
    selected: Option<usize>,
    record_tab: bool,
    run: Option<Run>,
    preview: Option<Preview>,
}

struct Run {
    total: usize,
    current: Option<usize>,
    prefs: BatchPrefs,
}

struct Row {
    path: PathBuf,
    state: RowState,
    elapsed: Option<Duration>,
    pages: u32,
    result_index: Option<usize>,
    problem: Option<String>,
}

impl Row {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: RowState::Ready,
            elapsed: None,
            pages: 0,
            result_index: None,
            problem: None,
        }
    }

    fn name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or(self.path.as_os_str())
            .to_string_lossy()
            .into_owned()
    }

    fn reset(&mut self) {
        self.state = RowState::Ready;
        self.elapsed = None;
        self.pages = 0;
        self.result_index = None;
        self.problem = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowState {
    Ready,
    Active,
    Complete,
    Empty,
    Failed,
    Stopped,
}

impl RowState {
    fn label(self) -> &'static str {
        match self {
            Self::Ready => "待处理",
            Self::Active => "进行中",
            Self::Complete => "已完成",
            Self::Empty => "无文字",
            Self::Failed => "失败",
            Self::Stopped => "已停止",
        }
    }
}

struct Preview {
    title: String,
    load: Option<mpsc::Receiver<Result<(DynamicImage, [u32; 2])>>>,
    image: Option<egui::TextureHandle>,
    image_size: [u32; 2],
    error: Option<String>,
    edit_ignore: bool,
    drag_start: Option<egui::Pos2>,
    coordinates: [i32; 4],
    focus_on_open: bool,
}

impl BatchUi {
    fn page(&self, page: Page) -> &BatchPage {
        if page == Page::Documents {
            &self.documents
        } else {
            &self.images
        }
    }

    fn page_mut(&mut self, page: Page) -> &mut BatchPage {
        if page == Page::Documents {
            &mut self.documents
        } else {
            &mut self.images
        }
    }

    fn preference_path(settings: &Path) -> PathBuf {
        settings.with_file_name("batch-settings.json")
    }

    fn load_preferences(&mut self, settings: &Path) {
        if self.preferences_loaded {
            return;
        }
        self.preferences_loaded = true;
        if let Ok(bytes) = fs::read(Self::preference_path(settings)) {
            if let Ok(prefs) = serde_json::from_slice::<Preferences>(&bytes) {
                self.images.prefs = prefs.images;
                self.documents.prefs = prefs.documents;
            }
        }
    }

    fn save_preferences(&self, settings: &Path) -> Result<()> {
        let path = Self::preference_path(settings);
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        let prefs = Preferences {
            images: self.images.prefs.clone(),
            documents: self.documents.prefs.clone(),
        };
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&prefs)?)?;
        fs::rename(&temporary, &path)?;
        Ok(())
    }
}

impl App {
    pub(super) fn stage_files(&mut self, page: Page, paths: Vec<PathBuf>) {
        if !matches!(page, Page::Images | Page::Documents) {
            return;
        }
        self.batch.load_preferences(&self.settings_path);
        let group = self.batch.page_mut(page);
        if group.run.is_some() {
            self.error = Some("请等待本批任务结束后再添加文件".into());
            return;
        }
        let extensions = batch_extensions(page);
        let available = QUEUE_LIMIT.saturating_sub(group.rows.len());
        let files = collect_batch_files(paths, extensions, group.prefs.recursive, available);
        let mut added = 0;
        for path in files {
            if group.row_keys.insert(path.clone()) {
                group.rows.push(Row::new(path));
                added += 1;
            }
        }
        if added > 0 {
            self.open_page(page);
            self.status = format!("已添加 {added} 个文件，等待开始");
            self.error = None;
        } else if available == 0 {
            self.error = Some(format!("待处理列表最多保留 {QUEUE_LIMIT} 个文件"));
        } else {
            self.status = "未找到新的受支持文件".into();
        }
    }

    fn start_batch(&mut self, page: Page) {
        if self.busy.is_some() {
            return;
        }
        let group = self.batch.page(page);
        if group.rows.is_empty() {
            return;
        }
        let prefs = group.prefs.clone();
        let count = group.rows.len();
        let paths: Vec<_> = group.rows.iter().map(|row| row.path.clone()).collect();
        if (prefs.has_text_output() || prefs.pdf != PdfOutput::Disabled)
            && prefs.destination == Destination::Folder
            && !prefs.folder.as_ref().is_some_and(|path| path.is_dir())
        {
            self.error = Some("请先选择有效的保存目录".into());
            return;
        }
        let pdf = if page == Page::Documents && prefs.pdf != PdfOutput::Disabled {
            let mut folders = paths.iter().filter_map(|path| prefs.folder_for(path));
            let Some(folder) = folders.next() else {
                self.error = Some("无法确定 PDF 保存目录".into());
                return;
            };
            if folders.any(|candidate| candidate != folder) {
                self.error = Some("文档来自多个目录，请在设置中指定统一的 PDF 保存目录".into());
                return;
            }
            let mode = match prefs.pdf {
                PdfOutput::Searchable => PdfMode::Searchable,
                PdfOutput::TextOnly => PdfMode::TextOnly,
                PdfOutput::Disabled => unreachable!(),
            };
            Some((folder, mode))
        } else {
            None
        };
        self.run_jobs(page, paths.into_iter().map(Source::File).collect(), pdf);
        if self.busy == Some(page) {
            let group = self.batch.page_mut(page);
            for row in &mut group.rows {
                row.reset();
            }
            group.run = Some(Run {
                total: count,
                current: None,
                prefs,
            });
        }
    }

    pub(super) fn batch_observe(&mut self, page: Page, update: &Update) {
        let group = self.batch.page_mut(page);
        let Some(run) = group.run.as_mut() else {
            return;
        };
        match update {
            Update::Progress(progress) => {
                if let Some(index) = progress
                    .split_once('/')
                    .and_then(|(left, _)| left.parse::<usize>().ok())
                    .and_then(|index| index.checked_sub(1))
                    .filter(|&index| index < run.total)
                {
                    run.current = Some(index);
                    let row = &mut group.rows[index];
                    row.state = RowState::Active;
                    if page == Page::Documents {
                        if let Some(number) = progress
                            .rsplit_once(" · 第 ")
                            .and_then(|(_, suffix)| suffix.strip_suffix(" 页"))
                            .and_then(|number| number.parse::<u32>().ok())
                        {
                            row.pages = number;
                        }
                    }
                }
            }
            Update::Result(result) => {
                if let Some(index) = run.current {
                    let row = &mut group.rows[index];
                    row.state = if result.text.trim().is_empty() {
                        RowState::Empty
                    } else {
                        RowState::Complete
                    };
                    row.elapsed = Some(result.elapsed);
                    row.result_index = Some(self.results[page.index()].len());
                    group.selected = Some(index);
                }
            }
            Update::Error(error) => {
                if let Some(index) = run.current {
                    let row = &mut group.rows[index];
                    row.state = RowState::Failed;
                    row.problem = Some(error.clone());
                }
            }
            Update::Done => {
                let cancelled = self.control.load(Ordering::Relaxed) == 2;
                let run = group.run.take().expect("batch run was present");
                if cancelled {
                    for row in &mut group.rows {
                        if matches!(row.state, RowState::Ready | RowState::Active) {
                            row.state = RowState::Stopped;
                        }
                    }
                }
                group.record_tab = true;
                let completed = group
                    .rows
                    .iter()
                    .filter(|row| row.result_index.is_some())
                    .count();
                let errors = group
                    .rows
                    .iter()
                    .filter(|row| row.state == RowState::Failed)
                    .count();
                self.status = format!(
                    "{} · {completed}/{} 已处理 · {errors} 失败",
                    if cancelled {
                        "已停止"
                    } else {
                        "批量任务结束"
                    },
                    run.total
                );
                if !cancelled && run.prefs.has_text_output() {
                    match export_planned(&run.prefs, &group.rows, &self.results[page.index()]) {
                        Ok(saved) if saved > 0 => {
                            self.status.push_str(&format!(" · 已保存 {saved} 个文件"))
                        }
                        Ok(_) => {}
                        Err(error) => self.error = Some(format!("批量保存失败 · {error:#}")),
                    }
                }
            }
        }
    }

    pub(super) fn original_batch(&mut self, ui: &mut egui::Ui, page: Page) {
        self.batch.load_preferences(&self.settings_path);
        let palette = Palette::from_theme(self.settings.theme_id());
        ui.columns(2, |panels| {
            let [left, right] = panels else {
                unreachable!()
            };
            panel(left, palette, |ui| self.batch_table(ui, page, palette));
            panel(right, palette, |ui| self.batch_side(ui, page));
        });
        self.batch_preview(ui.ctx(), page);
    }

    fn batch_table(&mut self, ui: &mut egui::Ui, page: Page, palette: Palette) {
        let running = self.busy == Some(page);
        let idle = self.busy.is_none();
        let group = self.batch.page(page);
        let selected = group.selected;
        let count = group.rows.len();
        let completed = group
            .rows
            .iter()
            .filter(|row| {
                matches!(
                    row.state,
                    RowState::Complete | RowState::Empty | RowState::Failed
                )
            })
            .count();
        let control = self.control.load(Ordering::Relaxed);
        let mut start = false;
        let mut preview_selected = false;
        ui.horizontal_wrapped(|ui| {
            start = tool(
                ui,
                "run",
                "开始任务",
                "开始识别列表中的文件",
                palette.good,
                true,
                idle && count > 0,
            )
            .clicked();
            if running {
                let (icon, label) = if control == 1 {
                    ("run", "继续")
                } else {
                    ("pause", "暂停")
                };
                if tool(ui, icon, label, label, palette.text, true, control < 2).clicked() {
                    self.control
                        .store(u8::from(control == 0), Ordering::Relaxed);
                }
                if tool(
                    ui,
                    "stop",
                    "停止",
                    "停止当前批次",
                    palette.bad,
                    true,
                    control < 2,
                )
                .clicked()
                {
                    self.control.store(2, Ordering::Relaxed);
                    self.status = "正在停止当前批次…".into();
                }
            }
            preview_selected = tool(
                ui,
                "open_image",
                "预览",
                "预览选中的文件",
                palette.text,
                true,
                selected.is_some(),
            )
            .clicked();
        });
        if start {
            self.start_batch(page);
        }
        if preview_selected && let Some(index) = selected {
            self.open_batch_preview(ui.ctx(), page, index);
        }
        let progress = ui.add(
            egui::ProgressBar::new(if count > 0 {
                completed as f32 / count as f32
            } else {
                0.0
            })
            .text(format!("{completed} / {count}"))
            .desired_width(ui.available_width())
            .animate(running && control == 0),
        );
        progress.ctx.accesskit_node_builder(progress.id, |node| {
            node.set_role(egui::accesskit::Role::ProgressIndicator);
            node.set_label(if page == Page::Documents {
                "文档识别进度"
            } else {
                "图片识别进度"
            });
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(count.max(1) as f64);
            node.set_numeric_value(completed as f64);
        });
        ui.add_space(8.0);
        let group = self.batch.page(page);
        let mut activate = None;
        let mut preview = None;
        let width = (ui.available_width() - 146.0).max(75.0);
        let document = page == Page::Documents;
        batch_header(ui, width, document);
        ui.separator();
        let table_height = (ui.available_height() - 74.0).max(72.0);
        let scroll = egui::ScrollArea::vertical()
            .id_salt(("batch_queue", page.index()))
            .max_height(table_height)
            .min_scrolled_height(table_height)
            .auto_shrink([false, false]);
        if group.rows.is_empty() {
            scroll.show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((table_height * 0.33).max(12.0));
                    ui.label(if document {
                        "拖入文档或文件夹"
                    } else {
                        "拖入图片或文件夹"
                    });
                    ui.weak("准备好后，点击开始任务");
                });
            });
        } else {
            scroll.show_rows(ui, 25.0, group.rows.len(), |ui, visible| {
                for index in visible {
                    let row = &group.rows[index];
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            let name = row.name();
                            let name = ui
                                .add_sized(
                                    [width, 25.0],
                                    egui::Button::selectable(
                                        selected == Some(index),
                                        egui::RichText::new(name),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(format!("{}\n双击预览", row.path.display()));
                            let detail = if document && row.pages > 0 {
                                format!("，{} 页", row.pages)
                            } else if let Some(time) = row.elapsed {
                                format!("，耗时 {:.0} 毫秒", time.as_secs_f64() * 1000.0)
                            } else {
                                String::new()
                            };
                            name.ctx.accesskit_node_builder(name.id, |node| {
                                node.set_label(format!(
                                    "{}，{}{}",
                                    row.name(),
                                    row.state.label(),
                                    detail
                                ));
                                if let Some(error) = &row.problem {
                                    node.set_description(error.clone());
                                }
                            });
                            if name.clicked() {
                                activate = Some(index);
                            }
                            if name.double_clicked() {
                                preview = Some(index);
                            }
                            if document {
                                queue_status(ui, row);
                                queue_cell(
                                    ui,
                                    if row.pages == 0 {
                                        "—".into()
                                    } else {
                                        row.pages.to_string()
                                    },
                                    52.0,
                                );
                            } else {
                                queue_cell(
                                    ui,
                                    row.elapsed.map_or("—".into(), |time| {
                                        format!("{:.0} ms", time.as_secs_f64() * 1000.0)
                                    }),
                                    64.0,
                                );
                                queue_status(ui, row);
                            }
                        });
                    });
                }
            });
        }
        if let Some(index) = activate {
            let group = self.batch.page_mut(page);
            group.selected = Some(index);
            if let Some(result) = group.rows[index].result_index {
                if self.results[page.index()].get(result).is_some() {
                    self.selected[page.index()] = result;
                    self.preview = None;
                    self.active_box = None;
                    group.record_tab = true;
                }
            }
        }
        if let Some(index) = preview {
            self.open_batch_preview(ui.ctx(), page, index);
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let editable = !running;
            let open = if document {
                "打开文档"
            } else {
                "打开图片"
            };
            if tool(ui, "open_image", open, open, palette.text, true, editable).clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter(
                        if document { "文档" } else { "图片" },
                        batch_extensions(page),
                    )
                    .pick_files()
                {
                    self.stage_files(page, paths);
                }
            }
            if tool(
                ui,
                "folder",
                "打开文件夹",
                "从文件夹加入文件",
                palette.text,
                true,
                editable,
            )
            .clicked()
            {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.stage_files(page, vec![folder]);
                }
            }
            let group = self.batch.page_mut(page);
            if tool(
                ui,
                "no",
                "移除",
                "移除选中的文件",
                palette.text,
                true,
                editable && group.selected.is_some(),
            )
            .clicked()
            {
                if let Some(index) = group.selected.take() {
                    let row = group.rows.remove(index);
                    group.row_keys.remove(&row.path);
                }
            }
            if tool(
                ui,
                "clear",
                "清空",
                "清空待处理列表",
                palette.text,
                true,
                editable && !group.rows.is_empty(),
            )
            .clicked()
            {
                group.rows.clear();
                group.row_keys.clear();
                group.selected = None;
                self.status = "已清空待处理列表".into();
            }
        });
        let status = ui
            .add(
                egui::Label::new(RichText::new(&self.status).small().color(palette.muted))
                    .truncate(),
            )
            .on_hover_text(&self.status);
        status.ctx.accesskit_node_builder(status.id, |node| {
            node.set_live(egui::accesskit::Live::Polite);
        });
    }

    fn batch_side(&mut self, ui: &mut egui::Ui, page: Page) {
        let tab = &mut self.batch.page_mut(page).record_tab;
        ui.horizontal(|ui| {
            ui.selectable_value(tab, false, "设置");
            ui.selectable_value(tab, true, "记录");
        });
        ui.separator();
        if *tab {
            self.batch_records(ui, page);
        } else {
            self.batch_settings(ui, page);
        }
    }

    fn batch_settings(&mut self, ui: &mut egui::Ui, page: Page) {
        let mut prefs = self.batch.page(page).prefs.clone();
        let mut changed = false;
        let mut global_changed = false;
        let mut open_ignore = false;
        let document = page == Page::Documents;
        let editable = self.busy != Some(page);
        egui::ScrollArea::vertical()
            .id_salt(("batch_settings", page.index()))
            .show(ui, |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    ui.label(RichText::new("文字识别").strong());
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label("语言 / 模型库");
                        let name = LANGUAGES
                            .iter()
                            .find(|(id, _)| *id == self.settings.ocr.language)
                            .map_or(self.settings.ocr.language.as_str(), |(_, name)| *name);
                        egui::ComboBox::from_id_salt(("batch_model", page.index()))
                            .selected_text(name)
                            .show_ui(ui, |ui| {
                                for &(id, name) in LANGUAGES {
                                    global_changed |= ui
                                        .selectable_value(
                                            &mut self.settings.ocr.language,
                                            id.to_owned(),
                                            name,
                                        )
                                        .changed();
                                }
                            })
                            .response
                            .labelled_by(label.id);
                    });
                    global_changed |= ui
                        .checkbox(&mut self.settings.ocr.classify, "纠正文本方向")
                        .changed();
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label("限制图像边长");
                        let side = &mut self.settings.ocr.side_limit;
                        egui::ComboBox::from_id_salt(("batch_side_limit", page.index()))
                            .selected_text(if *side == 0 {
                                "不缩小".into()
                            } else {
                                side.to_string()
                            })
                            .show_ui(ui, |ui| {
                                for (value, name) in [
                                    (960, "960（默认）"),
                                    (2880, "2880"),
                                    (4320, "4320"),
                                    (0, "不缩小"),
                                ] {
                                    global_changed |=
                                        ui.selectable_value(side, value, name).changed();
                                }
                            })
                            .response
                            .labelled_by(label.id);
                    });
                    ui.separator();
                    ui.label(RichText::new("OCR 文本后处理").strong());
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label("排版解析");
                        let name = layout::MODES
                            .iter()
                            .find(|(id, _)| *id == self.settings.parser)
                            .map_or("原始文本框", |(_, name)| *name);
                        egui::ComboBox::from_id_salt(("batch_parser", page.index()))
                            .selected_text(name)
                            .show_ui(ui, |ui| {
                                for &(id, name) in layout::MODES {
                                    global_changed |= ui
                                        .selectable_value(
                                            &mut self.settings.parser,
                                            id.to_owned(),
                                            name,
                                        )
                                        .changed();
                                }
                            })
                            .response
                            .labelled_by(label.id);
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("忽略区域 · {} 处", self.settings.ignore.len()));
                        open_ignore = ui
                            .add_enabled(
                                !self.batch.page(page).rows.is_empty(),
                                egui::Button::new("设置忽略区域"),
                            )
                            .on_disabled_hover_text("请先添加待识别的图片或文档")
                            .clicked();
                    });
                    if document {
                        ui.separator();
                        ui.label(RichText::new("文档处理").strong());
                        ui.horizontal_wrapped(|ui| {
                            let label = ui.label("内容提取");
                            let mode = &mut self.settings.native_text;
                            egui::ComboBox::from_id_salt(("batch_native", page.index()))
                                .selected_text(if *mode {
                                    "原文本优先"
                                } else {
                                    "整页强制 OCR"
                                })
                                .show_ui(ui, |ui| {
                                    global_changed |=
                                        ui.selectable_value(mode, true, "原文本优先").changed();
                                    global_changed |=
                                        ui.selectable_value(mode, false, "整页强制 OCR").changed();
                                })
                                .response
                                .labelled_by(label.id);
                        });
                        ui.horizontal_wrapped(|ui| {
                            let label = ui.label("页面分辨率 DPI");
                            global_changed |= ui
                                .add(
                                    egui::DragValue::new(&mut self.settings.dpi)
                                        .range(72..=350)
                                        .suffix(" DPI"),
                                )
                                .labelled_by(label.id)
                                .changed();
                        });
                        egui::CollapsingHeader::new("PDF 密码").show(ui, |ui| {
                            let label = ui.label("PDF 打开密码");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.document_password)
                                    .password(true)
                                    .hint_text("仅用于本次会话"),
                            )
                            .labelled_by(label.id);
                        });
                    }
                    ui.separator();
                    ui.label(RichText::new("批量任务").strong());
                    changed |= ui
                        .checkbox(&mut prefs.recursive, "递归读取子文件夹")
                        .changed();
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label("保存到");
                        egui::ComboBox::from_id_salt(("batch_destination", page.index()))
                            .selected_text(match prefs.destination {
                                Destination::Source => {
                                    if document {
                                        "文档原目录"
                                    } else {
                                        "图片原目录"
                                    }
                                }
                                Destination::Folder => "指定目录",
                            })
                            .show_ui(ui, |ui| {
                                changed |= ui
                                    .selectable_value(
                                        &mut prefs.destination,
                                        Destination::Source,
                                        "原文件目录",
                                    )
                                    .changed();
                                changed |= ui
                                    .selectable_value(
                                        &mut prefs.destination,
                                        Destination::Folder,
                                        "指定目录",
                                    )
                                    .changed();
                            })
                            .response
                            .labelled_by(label.id);
                    });
                    if prefs.destination == Destination::Folder {
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("选择保存目录").clicked() {
                                if let Some(folder) = rfd::FileDialog::new()
                                    .set_title("选择 OCR 结果保存目录")
                                    .pick_folder()
                                {
                                    prefs.folder = Some(folder);
                                    changed = true;
                                }
                            }
                            if let Some(folder) = &prefs.folder {
                                ui.label(folder.display().to_string());
                            } else {
                                ui.weak("尚未选择");
                            }
                        });
                    }
                    egui::CollapsingHeader::new("保存文件类型")
                        .id_salt(("batch_outputs", page.index()))
                        .default_open(true)
                        .show(ui, |ui| {
                            changed |= ui.checkbox(&mut prefs.txt, "TXT 标准格式").changed();
                            changed |= ui.checkbox(&mut prefs.plain, "p.txt 纯文字格式").changed();
                            if !document {
                                changed |=
                                    ui.checkbox(&mut prefs.separate, "TXT 单独文件").changed();
                                changed |= ui
                                    .checkbox(&mut prefs.markdown, "Markdown 图文混排")
                                    .changed();
                            }
                            changed |= ui.checkbox(&mut prefs.csv, "CSV 表格文件").changed();
                            changed |= ui.checkbox(&mut prefs.jsonl, "JSONL 原始信息").changed();
                            if document {
                                ui.horizontal_wrapped(|ui| {
                                    let label = ui.label("PDF 导出类型");
                                    egui::ComboBox::from_id_salt("batch_pdf_output")
                                        .selected_text(match prefs.pdf {
                                            PdfOutput::Disabled => "不保存",
                                            PdfOutput::Searchable => "双层可搜索",
                                            PdfOutput::TextOnly => "单层纯文本",
                                        })
                                        .show_ui(ui, |ui| {
                                            changed |= ui
                                                .selectable_value(
                                                    &mut prefs.pdf,
                                                    PdfOutput::Disabled,
                                                    "不保存",
                                                )
                                                .changed();
                                            changed |= ui
                                                .selectable_value(
                                                    &mut prefs.pdf,
                                                    PdfOutput::Searchable,
                                                    "双层可搜索",
                                                )
                                                .changed();
                                            changed |= ui
                                                .selectable_value(
                                                    &mut prefs.pdf,
                                                    PdfOutput::TextOnly,
                                                    "单层纯文本",
                                                )
                                                .changed();
                                        })
                                        .response
                                        .labelled_by(label.id);
                                });
                            }
                        });
                    changed |= ui
                        .checkbox(
                            &mut prefs.skip_empty,
                            if document {
                                "输出忽略空白文档"
                            } else {
                                "输出忽略空白图片"
                            },
                        )
                        .changed();
                });
            });
        if changed {
            self.batch.page_mut(page).prefs = prefs;
            self.apply(self.batch.save_preferences(&self.settings_path));
        }
        if global_changed {
            self.save_settings();
        }
        if open_ignore {
            let index = self.batch.page(page).selected.unwrap_or(0);
            self.open_batch_preview(ui.ctx(), page, index);
            if let Some(preview) = &mut self.batch.page_mut(page).preview {
                preview.edit_ignore = true;
            }
        }
    }

    fn batch_records(&mut self, ui: &mut egui::Ui, page: Page) {
        let index = page.index();
        let count = self.results[index].len();
        if count == 0 {
            ui.vertical_centered(|ui| {
                ui.add_space(44.0);
                ui.label("批量识别记录");
                ui.weak("结果会随任务进度出现在这里。");
            });
            return;
        }
        let selected = &mut self.selected[index];
        *selected = (*selected).min(count - 1);
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("识别记录");
            egui::ComboBox::from_id_salt(("batch_history", index))
                .width(180.0)
                .selected_text(&self.results[index][*selected].title)
                .show_ui(ui, |ui| {
                    for (record, item) in self.results[index].iter().enumerate().rev() {
                        ui.selectable_value(
                            selected,
                            record,
                            format!("{} · {}", record + 1, item.title),
                        );
                    }
                })
                .response
                .labelled_by(label.id);
            ui.weak(format!("{count} 条"));
        });
        let mut export = None;
        let mut clear = false;
        ui.horizontal_wrapped(|ui| {
            if ui.button("复制文字").clicked() {
                ui.ctx()
                    .copy_text(self.results[index][*selected].text.clone());
                self.status = "已复制文字".into();
            }
            if ui.button("导出本条").clicked() {
                export = Some(false);
            }
            if ui.button("导出记录").clicked() {
                export = Some(true);
            }
            clear = ui
                .add_enabled(self.busy != Some(page), egui::Button::new("清空记录"))
                .clicked();
        });
        ui.separator();
        let item = &mut self.results[index][*selected];
        ui.weak(format!(
            "{} × {} 像素　·　{:.0} ms",
            item.image_size[0],
            item.image_size[1],
            item.elapsed.as_secs_f64() * 1000.0
        ));
        egui::ScrollArea::vertical()
            .id_salt(("batch_record_text", index))
            .show(ui, |ui| {
                ui.label("识别文字，可编辑");
                let id = ui.id().with(("batch_record_editor", index, *selected));
                let changed = set_accessible_text(ui, id, &mut item.text);
                let mut response = ui.add(
                    egui::TextEdit::multiline(&mut item.text)
                        .id(id)
                        .hint_text("本条结果为空")
                        .desired_rows(12)
                        .desired_width(f32::INFINITY),
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
            });
        if let Some(all) = export {
            self.save_results(all);
        }
        if clear {
            self.results[index].clear();
            self.selected[index] = 0;
            self.preview = None;
            self.active_box = None;
            for row in &mut self.batch.page_mut(page).rows {
                row.result_index = None;
            }
            self.status = "已清空本页记录".into();
        }
    }

    fn open_batch_preview(&mut self, context: &egui::Context, page: Page, index: usize) {
        let row = &self.batch.page(page).rows[index];
        let mut preview = Preview {
            title: row.name(),
            load: None,
            image: None,
            image_size: [0, 0],
            error: None,
            edit_ignore: false,
            drag_start: None,
            coordinates: [0; 4],
            focus_on_open: true,
        };
        if let Some((image, image_size)) = row
            .result_index
            .and_then(|index| self.results[page.index()].get(index))
            .and_then(|result| Some((result.preview.as_ref()?, result.image_size)))
        {
            preview.image = Some(texture(context, image, "batch_preview"));
            preview.image_size = image_size;
        } else {
            let path = row.path.clone();
            let password = self.document_password.clone();
            let context = context.clone();
            let dpi = self.settings.dpi;
            let (send, recv) = mpsc::channel();
            thread::spawn(move || {
                let image = if page == Page::Documents {
                    first_page(&path, dpi, &password)
                } else {
                    jobs::read_image(&path)
                };
                let _ = send.send(image.map(|image| {
                    let size = [image.width(), image.height()];
                    (image.thumbnail(1400, 1400), size)
                }));
                context.request_repaint();
            });
            preview.load = Some(recv);
        }
        self.batch.page_mut(page).preview = Some(preview);
    }

    fn batch_preview(&mut self, context: &egui::Context, page: Page) {
        let group = self.batch.page_mut(page);
        let Some(preview) = &mut group.preview else {
            return;
        };
        if context.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            group.preview = None;
            return;
        }
        if let Some(load) = &preview.load {
            match load.try_recv() {
                Ok(Ok((image, size))) => {
                    preview.image = Some(texture(context, &image, "batch_preview"));
                    preview.image_size = size;
                    preview.load = None;
                }
                Ok(Err(error)) => {
                    preview.error = Some(format!("{error:#}"));
                    preview.load = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    preview.error = Some("预览加载已中断".into());
                    preview.load = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut open = true;
        let mut close = false;
        let mut new_region = None;
        let mut remove_region = false;
        egui::Window::new(format!(
            "{} · {}",
            if preview.edit_ignore {
                "忽略区域"
            } else {
                "预览"
            },
            preview.title
        ))
        .id(egui::Id::new(("batch_preview", page.index())))
        .open(&mut open)
        .default_size([520.0, 320.0])
        .show(context, |ui| {
            let close_button = ui.button("关闭预览");
            if std::mem::take(&mut preview.focus_on_open) {
                close_button.request_focus();
            }
            close = close_button.clicked();
            if let Some(texture) = &preview.image {
                if preview.edit_ignore {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("拖动画框，保存为原图像素坐标");
                        remove_region = ui
                            .add_enabled(
                                !self.settings.ignore.is_empty(),
                                egui::Button::new("撤销最后一处"),
                            )
                            .clicked();
                    });
                    ui.horizontal_wrapped(|ui| {
                        let [width, height] = preview.image_size.map(|value| value as i32);
                        for (index, (name, limit)) in [
                            ("左边界", width),
                            ("上边界", height),
                            ("右边界", width),
                            ("下边界", height),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let label = ui.label(name);
                            ui.add(
                                egui::DragValue::new(&mut preview.coordinates[index])
                                    .range(0..=limit),
                            )
                            .labelled_by(label.id);
                        }
                        let [left, top, right, bottom] = preview.coordinates;
                        if ui
                            .add_enabled(
                                right > left && bottom > top,
                                egui::Button::new("添加忽略区域"),
                            )
                            .clicked()
                        {
                            new_region =
                                Some([[left, top], [right, top], [right, bottom], [left, bottom]]);
                        }
                    });
                }
                let image = egui::Image::new(texture)
                    .alt_text(format!("原图预览：{}", preview.title))
                    .max_size(ui.available_size());
                let response = if preview.edit_ignore {
                    ui.add(image.sense(egui::Sense::drag()))
                } else {
                    ui.add(image)
                };
                if preview.edit_ignore {
                    let dragged = edit_regions(
                        ui,
                        &response,
                        preview.image_size,
                        &self.settings.ignore,
                        &mut preview.drag_start,
                    );
                    new_region = new_region.or(dragged);
                }
            } else if let Some(error) = &preview.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            } else {
                ui.spinner();
            }
        });
        if !open || close {
            group.preview = None;
        }
        if remove_region {
            self.settings.ignore.pop();
        }
        if let Some(region) = new_region {
            self.settings.ignore.push(region);
        }
        if remove_region || new_region.is_some() {
            self.save_settings();
        }
    }
}

fn edit_regions(
    ui: &egui::Ui,
    image: &egui::Response,
    size: [u32; 2],
    regions: &[[[i32; 2]; 4]],
    anchor: &mut Option<egui::Pos2>,
) -> Option<[[i32; 2]; 4]> {
    let rect = image.rect;
    if size.contains(&0) || !rect.is_positive() {
        return None;
    }
    let scale = rect.size() / egui::vec2(size[0] as f32, size[1] as f32);
    let painter = ui.painter_at(rect);
    let draw = |area: egui::Rect| {
        painter.rect_filled(area, 0.0, Color32::from_rgba_unmultiplied(202, 72, 45, 52));
        painter.rect_stroke(
            area,
            0.0,
            egui::Stroke::new(1.4, Color32::from_rgb(202, 72, 45)),
            egui::StrokeKind::Inside,
        );
    };
    for region in regions {
        let position = |[x, y]: [i32; 2]| rect.min + egui::vec2(x as f32, y as f32) * scale;
        draw(egui::Rect::from_two_pos(
            position(region[0]),
            position(region[2]),
        ));
    }
    if image.drag_started() {
        *anchor = ui
            .input(|input| input.pointer.press_origin())
            .filter(|&point| rect.contains(point));
    }
    let pointer = image
        .interact_pointer_pos()
        .or_else(|| ui.ctx().pointer_latest_pos());
    let area = anchor
        .zip(pointer)
        .map(|(start, current)| egui::Rect::from_two_pos(rect.clamp(start), rect.clamp(current)));
    if let Some(area) = area {
        draw(area);
    }
    if !image.drag_stopped() {
        return None;
    }
    *anchor = None;
    let area = area?;
    let first = (area.min - rect.min) / scale;
    let last = (area.max - rect.min) / scale;
    let left = first.x.floor() as i32;
    let top = first.y.floor() as i32;
    let right = (last.x.ceil() as i32).min(size[0] as i32);
    let bottom = (last.y.ceil() as i32).min(size[1] as i32);
    (right - left > 1 && bottom - top > 1).then_some([
        [left, top],
        [right, top],
        [right, bottom],
        [left, bottom],
    ])
}

fn first_page(path: &Path, dpi: u32, password: &str) -> Result<DynamicImage> {
    let mut image = None;
    let result = crate::document::visit_with_password(path, dpi, false, password, |page| {
        image = Some(page.image);
        anyhow::bail!("预览已完成");
    });
    if let Some(image) = image {
        Ok(image)
    } else {
        result?;
        anyhow::bail!("文档没有可预览的页面")
    }
}

fn batch_header(ui: &mut egui::Ui, width: f32, document: bool) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [width, 24.0],
            egui::Label::new(RichText::new(if document { "文档" } else { "图片" }).strong()),
        );
        ui.add_sized(
            [64.0, 24.0],
            egui::Label::new(RichText::new(if document { "状态" } else { "耗时" }).strong()),
        );
        ui.add_sized(
            [54.0, 24.0],
            egui::Label::new(RichText::new(if document { "页数" } else { "状态" }).strong()),
        );
    });
}

fn queue_cell(ui: &mut egui::Ui, text: String, width: f32) {
    ui.add_sized([width, 25.0], egui::Label::new(text).truncate());
}

fn queue_status(ui: &mut egui::Ui, row: &Row) {
    let label = RichText::new(row.state.label());
    let label = if row.state == RowState::Failed {
        label.color(ui.visuals().error_fg_color)
    } else {
        label
    };
    let response = ui.add_sized([60.0, 25.0], egui::Label::new(label).truncate());
    if let Some(problem) = &row.problem {
        response.on_hover_text(problem);
    }
}

fn batch_extensions(page: Page) -> &'static [&'static str] {
    if page == Page::Documents {
        jobs::DOC_EXTS
    } else {
        jobs::IMAGE_EXTS
    }
}

fn collect_batch_files(
    paths: Vec<PathBuf>,
    extensions: &[&str],
    recursive: bool,
    limit: usize,
) -> Vec<PathBuf> {
    let mut pending: Vec<_> = paths.into_iter().rev().map(|path| (path, 0)).collect();
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    while let Some((path, depth)) = pending.pop() {
        if files.len() >= limit {
            break;
        }
        let Ok(metadata) = path.symlink_metadata() else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() && (depth == 0 || recursive) && depth < 64 {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                continue;
            }
            let Ok(entries) = fs::read_dir(path) else {
                continue;
            };
            let mut children: Vec<_> = entries
                .flatten()
                .take(QUEUE_LIMIT)
                .map(|entry| entry.path())
                .collect();
            children.sort();
            pending.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
        } else if metadata.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    extensions
                        .iter()
                        .any(|allowed| ext.eq_ignore_ascii_case(allowed))
                })
        {
            if let (Ok(absolute), Ok(canonical)) = (std::path::absolute(&path), path.canonicalize())
                && seen.insert(canonical)
            {
                // Keep the native input spelling for the PDF StorageFile API.
                files.push(absolute);
            }
        }
    }
    files
}

fn export_planned(prefs: &BatchPrefs, rows: &[Row], results: &[ResultItem]) -> Result<usize> {
    let mut grouped = BTreeMap::<PathBuf, Vec<(&Row, &ResultItem)>>::new();
    for row in rows {
        let Some(item) = row.result_index.and_then(|index| results.get(index)) else {
            continue;
        };
        if prefs.skip_empty && item.text.trim().is_empty() {
            continue;
        }
        let folder = prefs.folder_for(&row.path).context("保存目录不可用")?;
        grouped.entry(folder).or_default().push((row, item));
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let mut saved = 0;
    for (directory, entries) in grouped {
        let stem = if entries.len() == 1 {
            entries[0].0.path.file_stem()
        } else {
            directory.file_name()
        }
        .and_then(|name| name.to_str())
        .unwrap_or("批量结果");
        let base = format!("[OCR]_{}_{timestamp}", safe_stem(stem));
        if prefs.txt {
            let text = entries
                .iter()
                .map(|(_, item)| format!("{}\n{}", item.title, item.text))
                .collect::<Vec<_>>()
                .join("\n\n");
            write_new(&directory, &base, "txt", text.as_bytes())?;
            saved += 1;
        }
        if prefs.plain {
            let text = entries
                .iter()
                .map(|(_, item)| item.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            write_new(&directory, &base, "p.txt", text.as_bytes())?;
            saved += 1;
        }
        if prefs.separate {
            for (row, item) in &entries {
                let stem = row
                    .path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("识别结果");
                write_new(
                    &directory,
                    &format!("[OCR]_{}", safe_stem(stem)),
                    "txt",
                    item.text.as_bytes(),
                )?;
                saved += 1;
            }
        }
        if prefs.markdown {
            let text = entries
                .iter()
                .map(|(row, item)| {
                    format!(
                        "## {}\n\n![原图](<{}>)\n\n{}",
                        item.title,
                        markdown_image(&directory, &row.path),
                        item.text,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            write_new(&directory, &base, "md", text.as_bytes())?;
            saved += 1;
        }
        if prefs.csv || prefs.jsonl {
            let records: Vec<_> = entries
                .iter()
                .map(|(_, item)| ResultItem {
                    title: item.title.clone(),
                    text: item.text.clone(),
                    blocks: item.blocks.clone(),
                    preview: None,
                    image_size: item.image_size,
                    elapsed: item.elapsed,
                })
                .collect();
            for (enabled, extension) in [(prefs.csv, "csv"), (prefs.jsonl, "jsonl")] {
                if enabled {
                    let (path, handle) = reserve_path(&directory, &base, extension)?;
                    drop(handle);
                    jobs::export(&path, &records)
                        .with_context(|| format!("无法保存 {}", path.display()))?;
                    saved += 1;
                }
            }
        }
    }
    Ok(saved)
}

fn safe_stem(name: &str) -> String {
    name.chars()
        .take(120)
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect()
}

fn markdown_image(directory: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(directory).ok();
    let display = relative.unwrap_or(path).to_string_lossy();
    let display = display
        .strip_prefix(r"\\?\")
        .unwrap_or(&display)
        .replace('\\', "/");
    let mut encoded = if relative.is_some() {
        String::new()
    } else {
        "file:///".into()
    };
    for byte in display.bytes() {
        if byte.is_ascii_alphanumeric() || b"/:._-~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn reserve_path(directory: &Path, base: &str, extension: &str) -> Result<(PathBuf, fs::File)> {
    for number in 1..=10_000 {
        let name = if number == 1 {
            format!("{base}.{extension}")
        } else {
            format!("{base}-{number}.{extension}")
        };
        let path = directory.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("无法创建 {}", path.display()));
            }
        }
    }
    anyhow::bail!("同名输出文件过多")
}

fn write_new(directory: &Path, base: &str, extension: &str, data: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let (path, mut file) = reserve_path(directory, base, extension)?;
    file.write_all(data)
        .with_context(|| format!("无法保存 {}", path.display()))
}
