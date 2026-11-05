use std::io::{Cursor, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, ImageReader};
use serde_json::{Map, Value, json};
use tiny_http::{Header, Method, Request, Response};

use crate::engine::{Engine, OcrOptions};
use crate::{layout, qr};

#[path = "server_document.rs"]
mod document_api;

const MAX_BODY: u64 = 100 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;
const PARSERS: [(&str, &str); 8] = [
    ("multi_para", "多栏-按自然段换行"),
    ("multi_line", "多栏-总是换行"),
    ("multi_none", "多栏-无换行"),
    ("single_para", "单栏-按自然段换行"),
    ("single_line", "单栏-总是换行"),
    ("single_none", "单栏-无换行"),
    ("single_code", "单栏-保留缩进"),
    ("none", "不做处理"),
];

pub fn serve(address: SocketAddr, root: PathBuf, options: OcrOptions) -> Result<()> {
    let server = tiny_http::Server::http(address)
        .map_err(|error| anyhow!("监听 {address} 失败：{error}"))?;
    let mut engine = None;
    let mut documents = document_api::Documents::new(root.clone());
    eprintln!("http://{}", server.server_addr());
    for mut request in server.incoming_requests() {
        let path = request.url().split('?').next().unwrap_or_default();
        if path.starts_with("/api/doc/") {
            let path = path.to_owned();
            documents.dispatch(request, &path, &options, address);
            continue;
        }
        let (status, body): (u16, Value) = match (request.method(), path) {
            (&Method::Get, "/" | "/umiocr") => (200, json!("文识别")),
            (&Method::Get, "/api/ocr/get_options") => (200, get_options(&options)),
            (&Method::Post, "/api/ocr") => {
                let result = match read_payload(&mut request) {
                    Ok(payload) => recognize(&payload, &mut engine, &root, &options),
                    Err(error) => error,
                };
                (200, result)
            }
            (&Method::Post, "/api/qrcode") => {
                let result = match read_payload(&mut request) {
                    Ok(payload) => barcode(&payload),
                    Err(error) => error,
                };
                (200, result)
            }
            _ => (404, result(404, "未找到接口。")),
        };
        respond(request, status, body);
    }
    Ok(())
}

fn respond(request: Request, status: u16, body: Value) {
    let response = Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(
            Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap(),
        );
    if let Err(error) = request.respond(response) {
        eprintln!("HTTP 响应失败：{error}");
    }
}

fn result(code: u16, data: impl serde::Serialize) -> Value {
    json!({"code": code, "data": data})
}

fn get_options(default: &OcrOptions) -> Value {
    let language = if default.language.starts_with("models/") {
        default.language.clone()
    } else {
        format!("models/config_{}.txt", default.language)
    };
    let languages: Vec<_> = crate::engine::LANGUAGES
        .iter()
        .map(|(id, name)| (format!("models/config_{id}.txt"), *name))
        .collect();
    json!({
        "ocr.language": {"title":"语言/模型库", "type":"enum", "default":language,
            "optionsList":languages},
        "ocr.cls": {"title":"纠正文本方向", "type":"boolean", "default":default.classify},
        "ocr.limit_side_len": {"title":"限制图像边长", "type":"enum", "default":default.side_limit,
            "optionsList":[[960,"960"],[2880,"2880"],[4320,"4320"],[999999,"无限制"]]},
        "tbpu.parser": {"title":"排版解析方案", "type":"enum", "default":"multi_para", "optionsList":PARSERS},
        "tbpu.ignoreArea": {"title":"忽略区域", "type":"var", "default":[], "toolTip":"[[[左上角x,y],[右下角x,y]],…]"},
        "data.format": {"title":"数据返回格式", "type":"enum", "default":"dict",
            "optionsList":[["dict","含有位置等信息的字典"],["text","纯文本"]]}
    })
}

fn read_payload(request: &mut Request) -> std::result::Result<Map<String, Value>, Value> {
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| result(800, format!("读取请求失败：{error}")))?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(result(800, "请求正文超过 100 MiB。"));
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(result(801, "请求为空。"));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) if !object.is_empty() => Ok(object),
        Ok(Value::Object(_)) | Ok(Value::Null) => Err(result(801, "请求为空。")),
        Ok(_) => Err(result(800, "请求必须是 JSON 字典。")),
        Err(error) => Err(result(800, format!("请求无法解析为 JSON：{error}"))),
    }
}

fn decode_image(encoded: &Value) -> Result<DynamicImage> {
    let encoded = encoded
        .as_str()
        .ok_or_else(|| anyhow!("base64 必须是字符串"))?;
    let bytes = STANDARD.decode(encoded)?;
    let reader = ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?;
    let (width, height) = reader.into_dimensions()?;
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        bail!("图像超过 {MAX_IMAGE_PIXELS} 像素");
    }
    Ok(image::load_from_memory(&bytes)?)
}

struct JobOptions<'a> {
    ocr: OcrOptions,
    parser: &'a str,
    ignore: Vec<[[i32; 2]; 4]>,
    as_text: bool,
}

fn job_options<'a>(opts: &'a Map<String, Value>, default: &OcrOptions) -> Result<JobOptions<'a>> {
    let mut ocr = default.clone();
    if let Some(value) = opts.get("ocr.language") {
        ocr.language = value
            .as_str()
            .ok_or_else(|| anyhow!("ocr.language 必须是字符串"))?
            .to_owned();
    }
    if let Some(value) = opts.get("ocr.cls") {
        ocr.classify = value
            .as_bool()
            .ok_or_else(|| anyhow!("ocr.cls 必须是布尔值"))?;
    }
    if let Some(value) = opts.get("ocr.limit_side_len") {
        ocr.side_limit = value
            .as_u64()
            .filter(|value| (32..=999999).contains(value))
            .ok_or_else(|| anyhow!("ocr.limit_side_len 必须是 32 至 999999 的整数"))?
            as u32;
    }
    let parser = match opts.get("tbpu.parser") {
        None => "multi_para",
        Some(value) => value
            .as_str()
            .filter(|name| PARSERS.iter().any(|(id, _)| id == name))
            .ok_or_else(|| anyhow!("未知的 tbpu.parser"))?,
    };
    let as_text = match opts.get("data.format").map(Value::as_str) {
        None | Some(Some("dict")) => false,
        Some(Some("text")) => true,
        _ => bail!("data.format 必须是 dict 或 text"),
    };
    let ignore = if let Some(value) = opts.get("tbpu.ignoreArea") {
        let rectangles: Vec<[[f64; 2]; 2]> = serde_json::from_value(value.clone())?;
        rectangles
            .into_iter()
            .map(|[[left, top], [right, bottom]]| {
                let [left, top] = [left.floor() as i32, top.floor() as i32];
                let [right, bottom] = [right.ceil() as i32, bottom.ceil() as i32];
                [[left, top], [right, top], [right, bottom], [left, bottom]]
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(JobOptions {
        ocr,
        parser,
        ignore,
        as_text,
    })
}

fn recognize(
    payload: &Map<String, Value>,
    engine: &mut Option<Engine>,
    root: &Path,
    default: &OcrOptions,
) -> Value {
    let Some(encoded) = payload.get("base64") else {
        return result(802, "请求中缺少 base64 字段。");
    };
    let empty = Map::new();
    let opts = match payload.get("options") {
        None => &empty,
        Some(Value::Object(options)) => options,
        Some(_) => return result(803, "请求中 options 字段必须为字典。"),
    };
    let job = match job_options(opts, default) {
        Ok(job) => job,
        Err(error) => return result(804, error.to_string()),
    };
    let start = Instant::now();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let image = match decode_image(encoded) {
        Ok(image) => image,
        Err(error) => return result(202, format!("图片读取失败：{error}")),
    };
    if engine.is_none() {
        match Engine::new(root, &job.ocr) {
            Ok(new_engine) => *engine = Some(new_engine),
            Err(error) => return result(901, error.to_string()),
        }
    }
    let blocks = match engine.as_mut().unwrap().recognize(&image, &job.ocr) {
        Ok(blocks) => layout::process(blocks, job.parser, &job.ignore),
        Err(error) => return result(902, error.to_string()),
    };
    let (code, data) = if blocks.is_empty() {
        (101, json!("No text found in image."))
    } else if job.as_text {
        (100, json!(layout::text(&blocks)))
    } else {
        (100, json!(blocks))
    };
    json!({"code": code, "data": data, "time": start.elapsed().as_secs_f64(), "timestamp": timestamp})
}

fn barcode(payload: &Map<String, Value>) -> Value {
    if let Some(encoded) = payload.get("base64") {
        let image = match decode_image(encoded) {
            Ok(image) => image,
            Err(error) => return result(202, format!("图片读取失败：{error}")),
        };
        return match qr::scan(&image) {
            Ok(codes) if codes.is_empty() => result(101, "QR code not found in the image."),
            Ok(codes) => result(100, codes.into_iter().map(|code| {
                json!({"text":code.text,"format":code.format,"box":code.points,"score":1,"end":"\n","orientation":code.orientation})
            }).collect::<Vec<_>>()),
            Err(error) => result(204, error.to_string()),
        };
    }
    let Some(text) = payload.get("text") else {
        return result(802, "指令中不存在 base64 或 text。");
    };
    match generate_barcode(text, payload.get("options")) {
        Ok(encoded) => result(100, encoded),
        Err(error) => result(200, error.to_string()),
    }
}

fn generate_barcode(text: &Value, options: Option<&Value>) -> Result<String> {
    let text = text.as_str().ok_or_else(|| anyhow!("text 必须是字符串"))?;
    if let Some(options) = options.filter(|options| !options.is_object()) {
        bail!("options 必须是字典：{options}");
    }
    let option = |name: &str| options.and_then(|options| options.get(name));
    let format = match option("format") {
        None => "QRCode",
        Some(value) => value
            .as_str()
            .ok_or_else(|| anyhow!("format 必须是字符串"))?,
    };
    let size = |name: &str| -> Result<u32> {
        match option(name) {
            None => Ok(0),
            Some(value) => value
                .as_u64()
                .filter(|value| *value <= 8192)
                .map(|value| value as u32)
                .ok_or_else(|| anyhow!("{name} 必须是 0 至 8192 的整数")),
        }
    };
    let number = |name: &str| -> Result<i32> {
        match option(name) {
            None => Ok(-1),
            Some(value) => value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| anyhow!("{name} 必须是整数")),
        }
    };
    let image = qr::generate_with(
        text,
        format,
        [size("w")?, size("h")?],
        number("quiet_zone")?,
        number("ec_level")?,
    )?;
    let mut buffer = Cursor::new(Vec::new());
    image.write_to(&mut buffer, ImageFormat::Jpeg)?;
    Ok(STANDARD.encode(buffer.into_inner()))
}
