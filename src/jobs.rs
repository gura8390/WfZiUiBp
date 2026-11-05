use crate::{
    document,
    engine::{Engine, OcrOptions, TextBlock},
    layout::{self, LayoutBlock},
    qr,
};
use anyhow::{Context, Result, ensure};
use image::{DynamicImage, RgbaImage};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff", "gif", "ico", "pnm", "ppm", "pgm",
];
pub const DOC_EXTS: &[&str] = &["pdf", "cbz", "xps", "epub", "mobi", "fb2"];

pub fn read_image(path: &Path) -> Result<DynamicImage> {
    Ok(image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("无法读取 {}", path.display()))?)
}

pub fn clipboard_image() -> Result<DynamicImage> {
    let mut clipboard = arboard::Clipboard::new()?;
    let image = clipboard.get_image().context("剪贴板里没有可读图片")?;
    let buffer = RgbaImage::from_raw(
        u32::try_from(image.width)?,
        u32::try_from(image.height)?,
        image.bytes.into_owned(),
    )
    .context("剪贴板图片尺寸无效")?;
    Ok(buffer.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Capture,
    Image,
    Document,
    Barcode,
}

pub enum Source {
    File(PathBuf),
    Memory(String, DynamicImage),
}

#[derive(Clone, Copy)]
pub enum PdfMode {
    Searchable,
    TextOnly,
}

#[derive(Clone)]
pub struct Options {
    pub ocr: OcrOptions,
    pub parser: String,
    pub ignore: Vec<[[i32; 2]; 4]>,
    pub dpi: u32,
    pub native_text: bool,
    pub password: String,
}

pub struct Request {
    pub inputs: Vec<Source>,
    pub options: Options,
    pub kind: Kind,
    pub pdf: Option<(PathBuf, PdfMode)>,
    pub control: Arc<AtomicU8>,
}

pub struct ResultItem {
    pub title: String,
    pub text: String,
    pub blocks: Vec<LayoutBlock>,
    pub preview: Option<DynamicImage>,
    pub image_size: [u32; 2],
    pub elapsed: Duration,
}

pub enum Update {
    Progress(String),
    Result(ResultItem),
    Error(String),
    Done,
}

pub fn start(
    root: PathBuf,
    context: eframe::egui::Context,
) -> (mpsc::Sender<Request>, mpsc::Receiver<Update>) {
    let (requests, work) = mpsc::channel::<Request>();
    let (output, updates) = mpsc::channel();
    thread::spawn(move || {
        let mut engine = None;
        let send = |event| {
            let _ = output.send(event);
            context.request_repaint();
        };
        for request in work {
            let count = request.inputs.len();
            for (index, input) in request.inputs.into_iter().enumerate() {
                if check_control(&request.control).is_err() {
                    break;
                }
                let title = match &input {
                    Source::File(path) => path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                    Source::Memory(title, _) => title.clone(),
                };
                send(Update::Progress(format!(
                    "{}/{} · {title}",
                    index + 1,
                    count
                )));
                let timer = Instant::now();
                let result = match (request.kind, input) {
                    (Kind::Document, Source::File(path)) => run_document(
                        &root,
                        &mut engine,
                        &path,
                        &request.options,
                        request.pdf.as_ref(),
                        &request.control,
                        |page| {
                            send(Update::Progress(format!(
                                "{}/{} · {title} · 第 {page} 页",
                                index + 1,
                                count
                            )));
                        },
                    ),
                    (kind, source) => {
                        let image = match source {
                            Source::File(path) => read_image(&path),
                            Source::Memory(_, image) => Ok(image),
                        };
                        image.and_then(|image| {
                            run_image(&root, &mut engine, image, kind, &request.options)
                        })
                    }
                };
                match result {
                    Ok(mut item) => {
                        item.title = title;
                        item.elapsed = timer.elapsed();
                        send(Update::Result(item));
                    }
                    Err(_) if request.control.load(Ordering::Relaxed) == 2 => {
                        send(Update::Progress("任务已停止".into()));
                        break;
                    }
                    Err(error) => send(Update::Error(format!("{title} · {error:#}"))),
                }
            }
            send(Update::Done);
        }
    });
    (requests, updates)
}

fn check_control(control: &AtomicU8) -> Result<()> {
    while control.load(Ordering::Relaxed) == 1 {
        thread::sleep(Duration::from_millis(40));
    }
    ensure!(control.load(Ordering::Relaxed) != 2, "任务已停止");
    Ok(())
}

fn recognize(
    root: &Path,
    engine: &mut Option<Engine>,
    image: &DynamicImage,
    options: &OcrOptions,
) -> Result<Vec<TextBlock>> {
    if engine.is_none() {
        *engine = Some(Engine::new(root, options)?);
    }
    engine.as_mut().unwrap().recognize(image, options)
}

fn run_image(
    root: &Path,
    engine: &mut Option<Engine>,
    image: DynamicImage,
    kind: Kind,
    options: &Options,
) -> Result<ResultItem> {
    let (text, blocks) = if kind == Kind::Barcode {
        let codes = qr::scan(&image)?;
        let text = codes
            .iter()
            .map(|code| format!("{}\n{}", code.format, code.text))
            .collect::<Vec<_>>()
            .join("\n\n");
        (text, Vec::new())
    } else {
        let blocks = layout::process(
            recognize(root, engine, &image, &options.ocr)?,
            &options.parser,
            &options.ignore,
        );
        (layout::text(&blocks), blocks)
    };
    let image_size = [image.width(), image.height()];
    Ok(ResultItem {
        title: String::new(),
        text,
        blocks,
        preview: Some(if matches!(kind, Kind::Capture | Kind::Barcode) {
            image
        } else {
            image.thumbnail(1000, 1000)
        }),
        image_size,
        elapsed: Duration::ZERO,
    })
}

fn run_document(
    root: &Path,
    engine: &mut Option<Engine>,
    path: &Path,
    options: &Options,
    output: Option<&(PathBuf, PdfMode)>,
    control: &AtomicU8,
    mut progress: impl FnMut(u32),
) -> Result<ResultItem> {
    let mut pdf = match output.map(|(_, mode)| mode) {
        Some(PdfMode::Searchable) => Some(document::SearchablePdf::new()),
        Some(PdfMode::TextOnly) => Some(document::SearchablePdf::new_text_only()?),
        None => None,
    };
    let mut texts = Vec::new();
    let mut preview = None;
    let mut image_size = [0, 0];
    let native = options.native_text && options.ignore.is_empty();
    document::visit_with_password(path, options.dpi, native, &options.password, |page| {
        check_control(control)?;
        progress(page.number);
        let (text, blocks) = match &page.text {
            Some(text) => (text.clone(), Vec::new()),
            None => {
                let blocks = layout::process(
                    recognize(root, engine, &page.image, &options.ocr)?,
                    &options.parser,
                    &options.ignore,
                );
                (layout::text(&blocks), blocks)
            }
        };
        if preview.is_none() {
            preview = Some(page.image.thumbnail(1000, 1000));
            image_size = [page.image.width(), page.image.height()];
        }
        if let Some(pdf) = &mut pdf {
            let boxes = blocks
                .into_iter()
                .map(|block| block.block)
                .collect::<Vec<_>>();
            pdf.add(&page, &boxes, &text)?;
        }
        texts.push(text);
        Ok(())
    })?;
    check_control(control)?;
    if let (Some(pdf), Some((folder, mode))) = (pdf, output) {
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let suffix = match mode {
            PdfMode::Searchable => "OCR",
            PdfMode::TextOnly => "Text",
        };
        let mut output = folder.join(format!("{name}-{suffix}.pdf"));
        for number in 2.. {
            if !output.exists() {
                break;
            }
            output = folder.join(format!("{name}-{suffix}-{number}.pdf"));
        }
        pdf.save(&output)
            .with_context(|| format!("无法保存 {}", output.display()))?;
    }
    Ok(ResultItem {
        title: String::new(),
        text: texts.join("\n\n"),
        blocks: Vec::new(),
        preview,
        image_size,
        elapsed: Duration::ZERO,
    })
}

pub fn export(path: &Path, results: &[ResultItem]) -> Result<()> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("txt")
        .to_ascii_lowercase();
    let data = match extension.as_str() {
        "jsonl" => {
            #[derive(Serialize)]
            struct Record<'a> {
                name: &'a str,
                text: &'a str,
                blocks: &'a [LayoutBlock],
            }
            let rows = results
                .iter()
                .map(|item| {
                    serde_json::to_string(&Record {
                        name: &item.title,
                        text: &item.text,
                        blocks: &item.blocks,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            rows.join("\n") + "\n"
        }
        "csv" => {
            fn quote(value: &str) -> String {
                format!("\"{}\"", value.replace('"', "\""))
            }
            let mut output = String::from("\u{feff}文件,文本\r\n");
            for item in results {
                output.push_str(&format!("{},{}\r\n", quote(&item.title), quote(&item.text)));
            }
            output
        }
        "md" => results
            .iter()
            .map(|item| format!("## {}\n\n{}", item.title, item.text))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ if results.len() == 1 => results[0].text.clone(),
        _ => results
            .iter()
            .map(|item| format!("{}\n{}", item.title, item.text))
            .collect::<Vec<_>>()
            .join("\n\n"),
    };
    fs::write(path, data)?;
    Ok(())
}
