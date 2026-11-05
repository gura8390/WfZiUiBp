use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Map, Value, json};
use tiny_http::{Header, Method, Request, Response};

use super::{get_options, job_options, read_payload, respond, result};
use crate::document::{self, SearchablePdf};
use crate::engine::{Engine, OcrOptions, TextBlock};
use crate::layout::{self, LayoutBlock};

const MAX_UPLOAD: u64 = 64 * 1024 * 1024;
const MAX_TASKS: usize = 8;
const MAX_PAGES: u32 = 300;
const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

pub struct Documents {
    root: PathBuf,
    tasks: HashMap<String, Arc<Task>>,
    worker: Option<mpsc::SyncSender<Work>>,
}

struct Work {
    task: Arc<Task>,
    options: Options,
}

struct Options {
    ocr: OcrOptions,
    parser: String,
    ignore: Vec<[[i32; 2]; 4]>,
    ignore_range: std::ops::RangeInclusive<u32>,
    pages: HashSet<u32>,
    mode: Mode,
    password: String,
}

#[derive(Clone, Copy)]
enum Mode {
    Mixed,
    FullPage,
    TextOnly,
}

struct Task {
    dir: tempfile::TempDir,
    input: PathBuf,
    filename: String,
    pages_count: usize,
    cancelled: AtomicBool,
    state: Mutex<State>,
}

struct State {
    stage: &'static str,
    results: Vec<PageResult>,
    unread: usize,
    message: String,
    completed: Option<Instant>,
    download: Option<String>,
}

struct PageResult {
    page: u32,
    text: String,
    blocks: Vec<LayoutBlock>,
    timestamp: f64,
    elapsed: f64,
}

impl Documents {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            tasks: HashMap::new(),
            worker: None,
        }
    }

    pub fn dispatch(
        &mut self,
        mut request: Request,
        path: &str,
        ocr: &OcrOptions,
        address: std::net::SocketAddr,
    ) {
        self.tasks.retain(|_, task| {
            task.state
                .lock()
                .unwrap()
                .completed
                .is_none_or(|time| time.elapsed() < RETENTION)
        });
        if request.method() == &Method::Get
            && let Some(suffix) = path.strip_prefix("/api/doc/download/")
        {
            if let Some((id, filename)) = suffix.split_once('/')
                && let Some(file) = self.open_download(id, filename)
            {
                let content_type = if filename.ends_with(".pdf") {
                    "application/pdf"
                } else if filename.ends_with(".zip") {
                    "application/zip"
                } else {
                    "application/octet-stream"
                };
                let response = Response::from_file(file)
                    .with_header(Header::from_bytes("Content-Type", content_type).unwrap())
                    .with_header(
                        Header::from_bytes(
                            "Content-Disposition",
                            format!("attachment; filename=\"{filename}\""),
                        )
                        .unwrap(),
                    );
                let _ = request.respond(response);
                return;
            }
            respond(request, 404, result(404, "下载文件不存在。"));
            return;
        }
        let (status, body) = match (request.method(), path) {
            (&Method::Get, "/api/doc/get_options") => (200, document_options(ocr)),
            (&Method::Post, "/api/doc/upload") => {
                let body = match self.upload(&mut request, ocr) {
                    Ok(id) => result(100, id),
                    Err(error) => result(108, error.to_string()),
                };
                (200, body)
            }
            (&Method::Post, "/api/doc/result" | "/api/doc/download") => {
                let body = match read_payload(&mut request) {
                    Ok(payload) if path.ends_with("/result") => self.read_result(&payload),
                    Ok(payload) => self.make_download(&payload, address),
                    Err(_) => result(101, "请求无法解析为 JSON。"),
                };
                (200, body)
            }
            (&Method::Get, _) if path.starts_with("/api/doc/clear/") => {
                let id = path.trim_start_matches("/api/doc/clear/");
                let body = if let Some(task) = self.tasks.remove(id) {
                    task.cancelled.store(true, Ordering::Release);
                    result(100, "Success")
                } else {
                    result(101, "任务不存在。")
                };
                (200, body)
            }
            _ => (404, result(404, "未找到接口。")),
        };
        respond(request, status, body);
    }

    fn upload(&mut self, request: &mut Request, ocr: &OcrOptions) -> Result<String> {
        ensure!(
            self.tasks.len() < MAX_TASKS,
            "文档任务已满，请清理已完成任务"
        );
        ensure!(
            request
                .body_length()
                .is_none_or(|size| size as u64 <= MAX_UPLOAD),
            "文档请求超过 64 MiB"
        );
        let content_type = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Content-Type"))
            .map(|header| header.value.as_str())
            .context("缺少 Content-Type")?;
        let boundary = multer::parse_boundary(content_type).context("需要 multipart/form-data")?;
        let mut bytes = Vec::new();
        request
            .as_reader()
            .take(MAX_UPLOAD + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_UPLOAD, "文档请求超过 64 MiB");
        let (filename, bytes, settings) =
            futures_executor::block_on(parse_upload(bytes, boundary))?;
        let ext = Path::new(&filename)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        ensure!(
            ["pdf", "cbz"].contains(&ext.as_str()),
            "文档接口支持 PDF 和 CBZ"
        );
        let dir = tempfile::Builder::new()
            .prefix("wf-ocr-doc-")
            .rand_bytes(20)
            .tempdir()?;
        let id = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let input = dir.path().join(format!("input.{ext}"));
        fs::write(&input, bytes)?;
        let password = match settings.get("password") {
            None => "",
            Some(value) => value.as_str().context("文档密码必须是字符串")?,
        };
        let pages = page_count(&input, &ext, password)?;
        ensure!(
            (1..=MAX_PAGES).contains(&pages),
            "文档页数须为 1–{MAX_PAGES}"
        );
        let options = Options::parse(&settings, ocr, pages, password.to_owned())?;
        let task = Arc::new(Task {
            dir,
            input,
            filename,
            pages_count: options.pages.len(),
            cancelled: AtomicBool::new(false),
            state: Mutex::new(State {
                stage: "waiting",
                results: Vec::new(),
                unread: 0,
                message: String::new(),
                completed: None,
                download: None,
            }),
        });
        let worker = self
            .worker
            .get_or_insert_with(|| start_worker(self.root.clone()));
        worker
            .try_send(Work {
                task: task.clone(),
                options,
            })
            .map_err(|_| anyhow!("文档任务队列已满"))?;
        self.tasks.insert(id.clone(), task);
        Ok(id)
    }

    fn lookup(&self, payload: &Map<String, Value>) -> std::result::Result<(&str, &Task), Value> {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| result(102, "未填写 id。"))?;
        let task = self
            .tasks
            .get_key_value(id)
            .ok_or_else(|| result(103, "任务不存在。"))?;
        Ok((task.0, task.1))
    }

    fn read_result(&self, payload: &Map<String, Value>) -> Value {
        let task = match self.lookup(payload) {
            Ok((_, task)) => task,
            Err(error) => return error,
        };
        let mut state = task.state.lock().unwrap();
        let data = if payload
            .get("is_data")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let unread = payload
                .get("is_unread")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let start = if unread { state.unread } else { 0 };
            if unread {
                state.unread = state.results.len();
            }
            let pages = &state.results[start..];
            if payload.get("format").and_then(Value::as_str) == Some("text") {
                json!(
                    pages
                        .iter()
                        .map(|page| page.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            } else {
                json!(
                    pages
                        .iter()
                        .map(|page| page.to_json(&task.filename))
                        .collect::<Vec<_>>()
                )
            }
        } else {
            json!([])
        };
        let mut response = json!({"code":100,"processed_count":state.results.len(),"pages_count":task.pages_count,
            "state":state.stage,"is_done":state.completed.is_some(),"data":data});
        if !state.message.is_empty() {
            response["message"] = json!(state.message);
        }
        response
    }

    fn make_download(&self, payload: &Map<String, Value>, address: std::net::SocketAddr) -> Value {
        let (id, task) = match self.lookup(payload) {
            Ok(task) => task,
            Err(error) => return error,
        };
        let mut state = task.state.lock().unwrap();
        if state.stage != "success" {
            return result(201, "任务尚未成功完成。");
        }
        let types = match payload.get("file_types") {
            None => vec!["pdfLayered"],
            Some(value) => match serde_json::from_value::<Vec<String>>(value.clone()) {
                Ok(values) => {
                    if values.is_empty() || values.len() > 6 {
                        return result(202, "file_types 需要 1–6 个值。");
                    }
                    return build_download(
                        &mut state,
                        task,
                        id,
                        address,
                        values.iter().map(String::as_str).collect(),
                        payload,
                    );
                }
                Err(_) => return result(202, "file_types 必须是字符串数组。"),
            },
        };
        build_download(&mut state, task, id, address, types, payload)
    }

    fn open_download(&self, id: &str, filename: &str) -> Option<File> {
        let task = self.tasks.get(id)?;
        let state = task.state.lock().ok()?;
        if state.download.as_deref() != Some(filename) {
            return None;
        }
        File::open(task.dir.path().join(filename)).ok()
    }
}

fn document_options(ocr: &OcrOptions) -> Value {
    let mut options = get_options(ocr);
    let values = options.as_object_mut().unwrap();
    values.remove("data.format");
    for (key, title, default) in [
        ("pageRangeStart", "OCR 页数起始", 1),
        ("pageRangeEnd", "OCR 页数结束", -1),
        ("tbpu.ignoreRangeStart", "忽略区域起始", 1),
        ("tbpu.ignoreRangeEnd", "忽略区域结束", -1),
    ] {
        values.insert(
            key.to_owned(),
            json!({"title":title,"default":default,"type":"number","isInt":true}),
        );
    }
    values.insert(
        "pageList".to_owned(),
        json!({"title":"OCR 页数列表","default":[],"type":"var"}),
    );
    values.insert(
        "password".to_owned(),
        json!({"title":"PDF 打开密码","default":"","type":"text"}),
    );
    values.insert("doc.extractionMode".to_owned(), json!({"title":"内容提取模式","default":"mixed","type":"enum",
        "optionsList":[["mixed","优先使用已有文本"],["fullPage","整页强制 OCR"],["textOnly","仅提取已有文本"]]}));
    options
}

async fn parse_upload(
    bytes: Vec<u8>,
    boundary: String,
) -> Result<(String, Vec<u8>, Map<String, Value>)> {
    let stream = futures_util::stream::once(async move { Ok::<_, io::Error>(bytes) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut file = None;
    let mut options = Map::new();
    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("file") => {
                ensure!(file.is_none(), "每次只可上传一个文档");
                let filename = field
                    .file_name()
                    .context("文件字段缺少文件名")?
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or_default();
                let filename: String = filename
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(255)
                    .collect();
                file = Some((filename, field.bytes().await?.to_vec()));
            }
            Some("json") => {
                let raw = field.bytes().await?;
                ensure!(raw.len() <= 64 * 1024, "文档选项超过 64 KiB");
                options = serde_json::from_slice(&raw).context("文档选项需要 JSON 字典")?;
            }
            _ => {}
        }
    }
    let (filename, file) = file.context("未上传文件")?;
    Ok((filename, file, options))
}

fn page_count(path: &Path, extension: &str, password: &str) -> Result<u32> {
    if extension == "pdf" {
        return document::pdf_page_count(path, password);
    }
    ensure!(password.is_empty(), "仅 PDF 支持打开密码");
    let archive = zip::ZipArchive::new(File::open(path)?)?;
    let count = archive
        .file_names()
        .filter(|name| {
            !name.starts_with("__MACOSX/")
                && Path::new(name)
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|ext| {
                        ["jpg", "jpeg", "png", "bmp", "webp", "tiff", "gif"]
                            .iter()
                            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
                    })
        })
        .count();
    Ok(u32::try_from(count)?)
}

impl Options {
    fn parse(
        values: &Map<String, Value>,
        default: &OcrOptions,
        total: u32,
        password: String,
    ) -> Result<Self> {
        let job = job_options(values, default)?;
        let ranges = |start: &str, end: &str| -> Result<std::ops::RangeInclusive<u32>> {
            let index = |key: &str, default| -> Result<u32> {
                let position = match values.get(key) {
                    None => default,
                    Some(value) => value.as_i64().context("页数需要整数")?,
                };
                let position = if position < 0 {
                    i64::from(total) + position + 1
                } else {
                    position
                };
                ensure!(
                    (1..=i64::from(total)).contains(&position),
                    "页数超出文档范围"
                );
                Ok(position as u32)
            };
            let first = index(start, 1)?;
            let last = index(end, -1)?;
            ensure!(first <= last, "页数起始值超过结束值");
            Ok(first..=last)
        };
        let page_list: Vec<u32> = values
            .get("pageList")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?
            .unwrap_or_default();
        ensure!(
            page_list.iter().all(|page| (1..=total).contains(page)),
            "pageList 超出文档范围"
        );
        let pages = if page_list.is_empty() {
            ranges("pageRangeStart", "pageRangeEnd")?.collect()
        } else {
            page_list.into_iter().collect()
        };
        let mode = match values.get("doc.extractionMode").and_then(Value::as_str) {
            None | Some("mixed") => Mode::Mixed,
            Some("fullPage") => Mode::FullPage,
            Some("textOnly") => Mode::TextOnly,
            _ => bail!("未支持的 doc.extractionMode"),
        };
        Ok(Self {
            ocr: job.ocr,
            parser: job.parser.to_owned(),
            ignore: job.ignore,
            ignore_range: ranges("tbpu.ignoreRangeStart", "tbpu.ignoreRangeEnd")?,
            pages,
            mode,
            password,
        })
    }
}

fn start_worker(root: PathBuf) -> mpsc::SyncSender<Work> {
    let (sender, receiver) = mpsc::sync_channel::<Work>(MAX_TASKS);
    std::thread::spawn(move || {
        let mut engine = None;
        for Work { task, options } in receiver {
            if task.cancelled.load(Ordering::Acquire) {
                continue;
            }
            task.state.lock().unwrap().stage = "running";
            let result = run_document(&root, &mut engine, &task, &options);
            let mut state = task.state.lock().unwrap();
            state.completed = Some(Instant::now());
            match result {
                Ok(()) => state.stage = "success",
                Err(error) => {
                    state.stage = "failure";
                    state.message = format!("{error:#}");
                }
            }
        }
    });
    sender
}

fn run_document(
    root: &Path,
    engine: &mut Option<Engine>,
    task: &Task,
    options: &Options,
) -> Result<()> {
    let mut pdf = SearchablePdf::new();
    let mut text_pdf = SearchablePdf::new_text_only().ok();
    document::visit_with_password(
        &task.input,
        150,
        !matches!(options.mode, Mode::FullPage),
        &options.password,
        |page| {
            ensure!(!task.cancelled.load(Ordering::Acquire), "任务已停止");
            if !options.pages.contains(&page.number) {
                return Ok(());
            }
            let start = Instant::now();
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let ignore = if options.ignore_range.contains(&page.number) {
                &options.ignore[..]
            } else {
                &[]
            };
            let (text, blocks) = match page.text.as_ref().filter(|_| ignore.is_empty()) {
                Some(native) => (native.clone(), Vec::new()),
                None if matches!(options.mode, Mode::TextOnly) => {
                    (page.text.clone().unwrap_or_default(), Vec::new())
                }
                None => {
                    if engine.is_none() {
                        *engine = Some(Engine::new(root, &options.ocr)?);
                    }
                    let blocks = engine
                        .as_mut()
                        .unwrap()
                        .recognize(&page.image, &options.ocr)?;
                    let blocks = layout::process(blocks, &options.parser, ignore);
                    (layout::text(&blocks), blocks)
                }
            };
            let boxes: Vec<TextBlock> = blocks.iter().map(|block| block.block.clone()).collect();
            pdf.add(&page, &boxes, &text)?;
            if let Some(text_pdf) = &mut text_pdf {
                text_pdf.add(&page, &boxes, &text)?;
            }
            task.state.lock().unwrap().results.push(PageResult {
                page: page.number,
                text,
                blocks,
                timestamp,
                elapsed: start.elapsed().as_secs_f64(),
            });
            Ok(())
        },
    )?;
    pdf.save(&task.dir.path().join("searchable.pdf"))?;
    if let Some(text_pdf) = text_pdf {
        text_pdf.save(&task.dir.path().join("text-only.pdf"))?;
    }
    Ok(())
}

impl PageResult {
    fn to_json(&self, filename: &str) -> Value {
        let name = format!("{filename} - {}", self.page);
        let (code, data) = if self.text.trim().is_empty() {
            (101, json!("No text found in page."))
        } else if self.blocks.is_empty() {
            (100, json!([{"text":self.text,"score":1,"box":[],"end":""}]))
        } else {
            (100, json!(self.blocks))
        };
        json!({"code":code,"data":data,"page":self.page,"fileName":name,"path":name,"time":self.elapsed,"timestamp":self.timestamp})
    }
}

fn build_download(
    state: &mut State,
    task: &Task,
    id: &str,
    address: std::net::SocketAddr,
    types: Vec<&str>,
    payload: &Map<String, Value>,
) -> Value {
    let ignore_blank = match payload.get("ignore_blank").map(Value::as_bool) {
        None => true,
        Some(Some(value)) => value,
        Some(None) => return result(202, "ignore_blank 必须是布尔值。"),
    };
    match write_downloads(task, &state.results, types, ignore_blank) {
        Ok(name) => {
            state.download = Some(name.clone());
            json!({"code":100,"name":name,"data":format!("http://{address}/api/doc/download/{id}/{name}")})
        }
        Err(error) => result(203, format!("{error:#}")),
    }
}

fn write_downloads(
    task: &Task,
    results: &[PageResult],
    types: Vec<&str>,
    ignore_blank: bool,
) -> Result<String> {
    let mut paths = Vec::new();
    let root = task.dir.path();
    let pages: Vec<_> = results
        .iter()
        .filter(|page| !ignore_blank || !page.text.trim().is_empty())
        .collect();
    ensure!(!pages.is_empty(), "没有可下载的页面");
    let mut seen = HashSet::new();
    for kind in types {
        if !seen.insert(kind) {
            continue;
        }
        let filename = match kind {
            "txt" | "txtPlain" => {
                let content = if kind == "txt" {
                    pages
                        .iter()
                        .map(|page| format!("第 {} 页\n{}", page.page, page.text))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                } else {
                    pages
                        .iter()
                        .map(|page| page.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                };
                let name = if kind == "txt" {
                    "ocr.txt"
                } else {
                    "ocr-plain.txt"
                };
                fs::write(root.join(name), content)?;
                name
            }
            "jsonl" => {
                let mut file = File::create(root.join("ocr.jsonl"))?;
                for page in &pages {
                    writeln!(file, "{}", page.to_json(&task.filename))?;
                }
                "ocr.jsonl"
            }
            "csv" => {
                let mut file = File::create(root.join("ocr.csv"))?;
                write!(file, "\u{feff}页数,文本\r\n")?;
                for page in &pages {
                    write!(
                        file,
                        "{},\"{}\"\r\n",
                        page.page,
                        page.text.replace('"', "\"\"")
                    )?;
                }
                "ocr.csv"
            }
            "pdfLayered" | "pdfOneLayer" => {
                let (input, output) = if kind == "pdfLayered" {
                    ("searchable.pdf", "ocr.pdf")
                } else {
                    ("text-only.pdf", "ocr-text.pdf")
                };
                let input = root.join(input);
                ensure!(
                    input.is_file(),
                    "无法生成单层 PDF，未找到允许嵌入的系统字体"
                );
                if pages.len() == results.len() {
                    fs::copy(input, root.join(output))?;
                } else {
                    let mut pdf = lopdf::Document::load(input)?;
                    let blank: Vec<_> = results
                        .iter()
                        .enumerate()
                        .filter(|(_, page)| page.text.trim().is_empty())
                        .map(|(index, _)| index as u32 + 1)
                        .collect();
                    pdf.delete_pages(&blank);
                    pdf.prune_objects();
                    pdf.save(root.join(output))?;
                }
                output
            }
            _ => bail!("未支持的下载格式：{kind}"),
        };
        paths.push(filename);
    }
    if paths.len() == 1 {
        return Ok(paths[0].to_owned());
    }
    let mut archive = zip::ZipWriter::new(File::create(root.join("ocr.zip"))?);
    for filename in paths {
        archive.start_file(filename, zip::write::SimpleFileOptions::default())?;
        io::copy(&mut File::open(root.join(filename))?, &mut archive)?;
    }
    archive.finish()?;
    Ok("ocr.zip".to_owned())
}
