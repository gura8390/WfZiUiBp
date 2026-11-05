#![windows_subsystem = "windows"]

mod desktop;
mod document;
mod document_extra;
mod engine;
mod jobs;
mod layout;
mod qr;
mod server;
mod startup;
mod ui;

use anyhow::{Context, Result, bail, ensure};
use desktop::DesktopCommand;
use engine::{Engine, OcrOptions};
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

#[derive(Default)]
struct Args {
    action: Option<DesktopCommand>,
    paths: Vec<PathBuf>,
    model_root: Option<PathBuf>,
    profile: Option<PathBuf>,
    ocr: OcrOptions,
    parser: Option<String>,
    port: Option<u16>,
    serve: bool,
    no_hotkey: bool,
    help: bool,
    cli_images: bool,
    qr_create: Option<(String, PathBuf)>,
    qr_read: Option<PathBuf>,
    rect: Option<[i32; 4]>,
    screen: Option<usize>,
    output: Option<PathBuf>,
    append: Option<PathBuf>,
    clip: bool,
    json: bool,
}

impl Args {
    fn read() -> Result<Self> {
        let mut options = Self::default();
        let mut input = std::env::args_os().skip(1);
        while let Some(token) = input.next() {
            let flag = token.to_string_lossy();
            let mut value = || input.next().with_context(|| format!("{flag} 需要参数"));
            let string = |value: OsString| {
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("参数不是有效的 Unicode 文本"))
            };
            match flag.as_ref() {
                "--help" | "-h" => options.help = true,
                "--serve" => options.serve = true,
                "--port" => options.port = Some(string(value()?)?.parse()?),
                "--models" => options.model_root = Some(value()?.into()),
                "--profile" => options.profile = Some(value()?.into()),
                "--no-hotkey" => options.no_hotkey = true,
                "--threads" => options.ocr.threads = string(value()?)?.parse()?,
                "--language" => options.ocr.language = string(value()?)?,
                "--side-limit" => options.ocr.side_limit = string(value()?)?.parse()?,
                "--classify" => options.ocr.classify = true,
                "--parser" => options.parser = Some(string(value()?)?),
                "--path" => options.cli_images = true,
                "--show" => options.action = Some(DesktopCommand::Show),
                "--hide" | "--silent" => options.action = Some(DesktopCommand::Hide),
                "--quit" => options.action = Some(DesktopCommand::Exit),
                "--reload" => options.action = Some(DesktopCommand::Reload),
                "--screenshot" => options.action = Some(DesktopCommand::Capture),
                "--repeat" => options.action = Some(DesktopCommand::RepeatCapture),
                "--clipboard" => options.action = Some(DesktopCommand::Clipboard),
                "--qrcode_create" => options.qr_create = Some((string(value()?)?, value()?.into())),
                "--qrcode_read" => options.qr_read = Some(value()?.into()),
                "--output" | "-->" => options.output = Some(value()?.into()),
                "--output_append" | "-->>" => options.append = Some(value()?.into()),
                "--clip" => options.clip = true,
                "--json" => options.json = true,
                _ if flag.starts_with("rect=") => {
                    let parts: Vec<i32> = flag[5..]
                        .split(',')
                        .map(str::parse)
                        .collect::<Result<_, _>>()?;
                    options.rect = Some(
                        parts
                            .try_into()
                            .map_err(|_| anyhow::anyhow!("rect 需要 x,y,w,h 四个数值"))?,
                    );
                }
                _ if flag.starts_with("screen=") => options.screen = Some(flag[7..].parse()?),
                "--" => {
                    options.paths.extend(input.map(PathBuf::from));
                    break;
                }
                _ if flag.starts_with('-') => bail!("未知参数 {flag}"),
                _ => options.paths.push(token.into()),
            }
        }
        ensure!(
            (1..=32).contains(&options.ocr.threads),
            "线程数须为 1 到 32"
        );
        ensure!(
            (32..=4096).contains(&options.ocr.side_limit),
            "检测边长须为 32 到 4096"
        );
        Ok(options)
    }
}

fn main() {
    if let Err(error) = run() {
        let message = format!("{error:#}");
        if std::env::args_os().len() > 1 {
            attach_console();
            eprintln!("{message}");
        } else {
            let title: Vec<_> = "文字识别".encode_utf16().chain([0]).collect();
            let body: Vec<_> = message.encode_utf16().chain([0]).collect();
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    body.as_ptr(),
                    title.as_ptr(),
                    16,
                );
            }
        }
        std::process::exit(1);
    }
}

fn attach_console() {
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

fn run() -> Result<()> {
    let args = Args::read()?;
    if args.help {
        attach_console();
        println!(
            "文字识别 WfZiUiBp {}\n\n  --screenshot [screen=0 rect=x,y,w,h]\n  --clipboard [--output FILE | --clip]\n  --path IMAGE ... [--language chinese | v6_tiny] [--json]\n  --qrcode_create TEXT IMAGE\n  --qrcode_read IMAGE\n  --serve [--port 1225] [--threads 4] [--models DIR]\n  --show | --hide | --quit | --reload | --repeat\n  --profile FILE --no-hotkey",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if let Some((value, path)) = &args.qr_create {
        return Ok(qr::generate(value)?.save(path)?);
    }
    if let Some(path) = &args.qr_read {
        let codes = qr::scan(&jobs::read_image(path)?)?;
        let data = if args.json {
            serde_json::to_string(&codes)?
        } else {
            codes
                .into_iter()
                .map(|code| code.text)
                .collect::<Vec<_>>()
                .join("\n")
        };
        return emit(&args, &data);
    }
    let model_root = args.model_root.clone().unwrap_or_else(default_models);
    if args.serve {
        attach_console();
        return server::serve(
            SocketAddr::from((Ipv4Addr::LOCALHOST, args.port.unwrap_or(1225))),
            model_root,
            args.ocr,
        );
    }
    if args.cli_images
        || args.rect.is_some()
        || args.screen.is_some()
        || matches!(args.action, Some(DesktopCommand::Clipboard))
            && (args.output.is_some() || args.append.is_some() || args.clip || args.json)
    {
        let mut engine = Engine::new(&model_root, &args.ocr)?;
        let mut results = Vec::new();
        let mut recognize = |image: image::DynamicImage| -> Result<()> {
            let blocks = engine.recognize(&image, &args.ocr)?;
            results.push(layout::process(
                blocks,
                args.parser.as_deref().unwrap_or("single_line"),
                &[],
            ));
            Ok(())
        };
        if args.rect.is_some() || args.screen.is_some() {
            recognize(desktop::capture_monitor_region(
                args.screen.unwrap_or(0),
                args.rect.unwrap_or([0, 0, 0, 0]),
            )?)?;
        } else if matches!(args.action, Some(DesktopCommand::Clipboard)) {
            recognize(jobs::clipboard_image()?)?;
        } else {
            ensure!(!args.paths.is_empty(), "请指定一张或多张图片");
            for path in &args.paths {
                recognize(jobs::read_image(path)?)?;
            }
        }
        let result = if args.json {
            serde_json::to_string(&results)?
        } else {
            results
                .iter()
                .map(|blocks| layout::text(blocks))
                .collect::<Vec<_>>()
                .join("\n")
        };
        return emit(&args, &result);
    }
    let use_startup_preference = args.action.is_none() && args.paths.is_empty();
    let action = args.action.unwrap_or(DesktopCommand::Show);
    if args.profile.is_none() {
        let forwarded = if args.paths.is_empty() {
            desktop::forward_existing(action)
        } else {
            desktop::forward_paths(&args.paths)
        };
        if forwarded {
            return Ok(());
        }
    }
    if matches!(action, DesktopCommand::Exit | DesktopCommand::Reload) {
        return Ok(());
    }
    let (action, capture) = if matches!(action, DesktopCommand::Capture) {
        let Some(image) = capture_before_launch()? else {
            return Ok(());
        };
        (DesktopCommand::Show, Some(image))
    } else {
        (action, None)
    };
    let hidden = matches!(action, DesktopCommand::Hide);
    let settings = args.profile.unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("WfZiUiBp")
            .join("settings.json")
    });
    ui::run(ui::Launch {
        root: model_root,
        settings,
        initial: action,
        hidden,
        use_startup_preference,
        no_hotkey: args.no_hotkey,
        paths: args.paths,
        capture,
        capture_is_qr: false,
    })
}

fn capture_before_launch() -> Result<Option<image::DynamicImage>> {
    use desktop::{Desktop, DesktopEvent};
    let (sender, events) = std::sync::mpsc::channel();
    let desktop = Desktop::start(sender, false, false)?;
    desktop.request_capture()?;
    for event in events {
        match event {
            DesktopEvent::Capture(image) => return Ok(Some(image)),
            DesktopEvent::CaptureCancelled | DesktopEvent::Exit => return Ok(None),
            DesktopEvent::Error(error) => bail!("{error}"),
            _ => {}
        }
    }
    Ok(None)
}

fn emit(args: &Args, result: &str) -> Result<()> {
    if let Some(path) = &args.output {
        fs::write(path, result)?;
    }
    if let Some(path) = &args.append {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(result.as_bytes())?;
        if !result.ends_with('\n') {
            file.write_all(b"\n")?;
        }
    }
    if args.clip {
        arboard::Clipboard::new()?.set_text(result)?;
    }
    if args.output.is_none() && args.append.is_none() {
        attach_console();
        writeln!(io::stdout().lock(), "{result}")?;
    }
    Ok(())
}

fn default_models() -> PathBuf {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe
        .as_deref()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    let candidates = [
        exe_dir.join("models"),
        PathBuf::from("models"),
        exe_dir.join("..").join("..").join("models"),
    ];
    candidates
        .iter()
        .find(|path| path.is_dir())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}
