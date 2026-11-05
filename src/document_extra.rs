use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek},
    path::Path,
    ptr,
};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use image::{DynamicImage, ImageReader, RgbImage};
use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        Graphics::{
            Imaging::GUID_WICPixelFormat32bppPBGRA,
            Printing::{
                CLSID_XPSRASTERIZER_FACTORY, IXpsRasterizationFactory,
                XPSRAS_RENDERING_MODE_ANTIALIASED,
            },
        },
        Storage::Xps::{
            IXpsOMCanvas, IXpsOMGlyphs, IXpsOMObjectFactory, IXpsOMVisualCollection,
            XpsOMObjectFactory,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
    },
    core::{Interface, PCWSTR},
};
use windows_sys::Win32::{
    Foundation::{RECT, SIZE},
    Graphics::Gdi::{
        ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC,
        CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_NOPREFIX, DT_SINGLELINE,
        DeleteDC, DeleteObject, DrawTextW, GdiFlush, GetTextExtentExPointW, HBITMAP, HDC, HFONT,
        HGDIOBJ, SelectObject, SetBkMode, TRANSPARENT,
    },
};

use crate::document::Page;

const MAX_PAGES: u32 = 10_000;
const MAX_DOCUMENT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

pub fn visit(
    path: &Path,
    dpi: u32,
    native_text: bool,
    page: impl FnMut(Page) -> Result<()>,
) -> Result<()> {
    ensure!(
        path.metadata()?.len() <= MAX_DOCUMENT_BYTES,
        "文档超过 256 MiB"
    );
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "xps" => visit_xps(path, dpi, native_text, page),
        "epub" => visit_epub(path, Reflow::new(dpi, native_text, page)?),
        "fb2" => visit_fb2(path, Reflow::new(dpi, native_text, page)?),
        "mobi" => visit_mobi(path, Reflow::new(dpi, native_text, page)?),
        _ => bail!("不支持的文档格式"),
    }
}

struct ComApartment(bool);

impl ComApartment {
    fn new() -> Result<Self> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result == RPC_E_CHANGED_MODE {
            // The caller already initialized a different COM apartment.
            return Ok(Self(false));
        }
        result.ok()?;
        Ok(Self(true))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn visit_xps(
    path: &Path,
    dpi: u32,
    native_text: bool,
    mut callback: impl FnMut(Page) -> Result<()>,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let _apartment = ComApartment::new()?;
    let full_path = std::path::absolute(path)?;
    let wide: Vec<_> = full_path.as_os_str().encode_wide().chain([0]).collect();

    // The COM interfaces retain the package and its resources through rendering.
    let factory: IXpsOMObjectFactory =
        unsafe { CoCreateInstance(&XpsOMObjectFactory, None, CLSCTX_INPROC_SERVER)? };
    let rasterizer: IXpsRasterizationFactory = unsafe {
        CoCreateInstance(&CLSID_XPSRASTERIZER_FACTORY, None, CLSCTX_INPROC_SERVER)
            .context("Windows XPS 栅格化组件无法启动")?
    };
    let package = unsafe { factory.CreatePackageFromFile(PCWSTR(wide.as_ptr()), false) }
        .context("无法打开 XPS 文档")?;
    let documents = unsafe { package.GetDocumentSequence()?.GetDocuments()? };
    let mut number = 0;
    for document in 0..unsafe { documents.GetCount()? } {
        let pages = unsafe { documents.GetAt(document)?.GetPageReferences()? };
        for index in 0..unsafe { pages.GetCount()? } {
            number += 1;
            ensure!(number <= MAX_PAGES, "文档页数超过 {MAX_PAGES}");
            let reference = unsafe { pages.GetAt(index)? };
            let page = unsafe { reference.GetPage()? };
            let size = unsafe { page.GetPageDimensions()? };
            ensure!(
                size.width.is_finite()
                    && size.height.is_finite()
                    && size.width > 0.0
                    && size.height > 0.0,
                "XPS 页面尺寸无效"
            );
            let scale =
                (dpi.clamp(72, 350) as f32 / 96.0).min(4200.0 / size.width.max(size.height));
            let width = (size.width * scale).ceil().max(1.0) as u32;
            let height = (size.height * scale).ceil().max(1.0) as u32;
            let bitmap = unsafe {
                rasterizer
                    .CreateRasterizer(
                        &page,
                        96.0 * scale,
                        XPSRAS_RENDERING_MODE_ANTIALIASED,
                        XPSRAS_RENDERING_MODE_ANTIALIASED,
                    )?
                    .RasterizeRect(0, 0, width as i32, height as i32, None)?
            };
            ensure!(
                unsafe { bitmap.GetPixelFormat()? } == GUID_WICPixelFormat32bppPBGRA,
                "XPS 栅格化组件返回了不支持的像素格式"
            );
            let mut data = vec![0; (width * height * 4) as usize];
            unsafe { bitmap.CopyPixels(ptr::null(), 4 * width, &mut data)? };
            let rgb = data
                .chunks_exact(4)
                .flat_map(|pixel| {
                    let background = 255 - pixel[3];
                    [pixel[2], pixel[1], pixel[0]].map(|channel| channel.saturating_add(background))
                })
                .collect();
            let image = RgbImage::from_raw(width, height, rgb).context("XPS 页面位图无效")?;
            let text = if native_text {
                let mut strings = Vec::new();
                xps_text(&unsafe { page.GetVisuals()? }, &mut strings, 0)?;
                (!strings.is_empty()).then(|| strings.join("\n"))
            } else {
                None
            };
            drop(page);
            unsafe { reference.DiscardPage()? };
            callback(Page {
                number,
                image: DynamicImage::ImageRgb8(image),
                text,
                size: [size.width * 0.75, size.height * 0.75],
            })?;
        }
    }
    ensure!(number > 0, "XPS 中没有页面");
    Ok(())
}

fn xps_text(
    visuals: &IXpsOMVisualCollection,
    strings: &mut Vec<String>,
    depth: usize,
) -> Result<()> {
    ensure!(depth < 100, "XPS 画布嵌套过深");
    for index in 0..unsafe { visuals.GetCount()? } {
        let visual = unsafe { visuals.GetAt(index)? };
        if let Ok(glyphs) = visual.cast::<IXpsOMGlyphs>() {
            let pointer = unsafe { glyphs.GetUnicodeString()? };
            if !pointer.is_null() {
                let string = unsafe { pointer.to_string() };
                unsafe { CoTaskMemFree(Some(pointer.0.cast())) };
                let string = string?;
                if !string.trim().is_empty() {
                    strings.push(string);
                }
            }
        } else if let Ok(canvas) = visual.cast::<IXpsOMCanvas>() {
            xps_text(&unsafe { canvas.GetVisuals()? }, strings, depth + 1)?;
        }
    }
    Ok(())
}

struct GdiPage {
    dc: HDC,
    bitmap: HBITMAP,
    font: HFONT,
    old_bitmap: HGDIOBJ,
    old_font: HGDIOBJ,
    pixels: *mut std::ffi::c_void,
    width: i32,
    height: i32,
    margin: i32,
    line_height: i32,
}

impl GdiPage {
    fn new(dpi: u32) -> Result<Self> {
        let scale = dpi.clamp(72, 260) as f32 / 72.0;
        let points = |value: f32| (value * scale).round() as i32;
        let mut page = Self {
            dc: unsafe { CreateCompatibleDC(ptr::null_mut()) },
            bitmap: ptr::null_mut(),
            font: ptr::null_mut(),
            old_bitmap: ptr::null_mut(),
            old_font: ptr::null_mut(),
            pixels: ptr::null_mut(),
            width: points(595.0),
            height: points(842.0),
            margin: points(36.0),
            line_height: points(18.0),
        };
        ensure!(!page.dc.is_null(), "无法创建电子书排版画布");
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: page.width,
                biHeight: -page.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        page.bitmap = unsafe {
            CreateDIBSection(
                page.dc,
                &info,
                DIB_RGB_COLORS,
                &mut page.pixels,
                ptr::null_mut(),
                0,
            )
        };
        ensure!(
            !page.bitmap.is_null() && !page.pixels.is_null(),
            "无法创建电子书页面位图"
        );
        page.old_bitmap = unsafe { SelectObject(page.dc, page.bitmap) };
        ensure!(!page.old_bitmap.is_null(), "无法选用电子书页面位图");
        let font: Vec<_> = "Microsoft YaHei UI".encode_utf16().chain([0]).collect();
        page.font = unsafe {
            CreateFontW(
                -points(12.0),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                ANTIALIASED_QUALITY as u32,
                0,
                font.as_ptr(),
            )
        };
        ensure!(!page.font.is_null(), "无法创建电子书排版字体");
        page.old_font = unsafe { SelectObject(page.dc, page.font) };
        ensure!(!page.old_font.is_null(), "无法选用电子书排版字体");
        unsafe { SetBkMode(page.dc, TRANSPARENT as i32) };
        page.clear();
        Ok(page)
    }

    fn clear(&mut self) {
        unsafe {
            GdiFlush();
            ptr::write_bytes(
                self.pixels.cast::<u8>(),
                255,
                (self.width * self.height * 4) as usize,
            );
        }
    }

    fn pixels(&self) -> Result<DynamicImage> {
        unsafe { GdiFlush() };
        let pixels = unsafe {
            std::slice::from_raw_parts(
                self.pixels.cast::<u8>(),
                (self.width * self.height * 4) as usize,
            )
        };
        let rgb = pixels
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect();
        let image = RgbImage::from_raw(self.width as u32, self.height as u32, rgb)
            .context("电子书排版位图尺寸无效")?;
        Ok(DynamicImage::ImageRgb8(image))
    }

    fn fit(&self, text: &[u16]) -> Result<usize> {
        let text = &text[..text.len().min(8192)];
        let mut fit = 0;
        let mut size = SIZE::default();
        ensure!(
            unsafe {
                GetTextExtentExPointW(
                    self.dc,
                    text.as_ptr(),
                    i32::try_from(text.len())?,
                    self.width - 2 * self.margin,
                    &mut fit,
                    ptr::null_mut(),
                    &mut size,
                )
            } != 0,
            "无法测量电子书文本宽度"
        );
        let mut length = (fit as usize).clamp(1, text.len());
        if length < text.len() {
            if (0xD800..=0xDBFF).contains(&text[length - 1]) {
                length = if length == 1 { 2 } else { length - 1 };
            }
            if let Some(space) = text[..length]
                .iter()
                .rposition(|&unit| unit == u16::from(b' '))
            {
                if space >= length / 2 {
                    length = space.max(1);
                }
            }
        }
        Ok(length)
    }

    fn paint(&self, line: &[u16], top: i32) -> Result<()> {
        let mut rect = RECT {
            left: self.margin,
            top,
            right: self.width - self.margin,
            bottom: top + self.line_height,
        };
        ensure!(
            unsafe {
                DrawTextW(
                    self.dc,
                    line.as_ptr(),
                    line.len() as i32,
                    &mut rect,
                    DT_SINGLELINE | DT_NOPREFIX,
                )
            } > 0,
            "无法排版电子书文本"
        );
        Ok(())
    }
}

impl Drop for GdiPage {
    fn drop(&mut self) {
        unsafe {
            if !self.old_font.is_null() {
                SelectObject(self.dc, self.old_font);
            }
            if !self.old_bitmap.is_null() {
                SelectObject(self.dc, self.old_bitmap);
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.dc.is_null() {
                DeleteDC(self.dc);
            }
        }
    }
}

struct Reflow<F> {
    canvas: GdiPage,
    native_text: bool,
    number: u32,
    next_line: i32,
    current_text: String,
    pending: String,
    callback: F,
}

impl<F: FnMut(Page) -> Result<()>> Reflow<F> {
    fn new(dpi: u32, native_text: bool, callback: F) -> Result<Self> {
        let canvas = GdiPage::new(dpi)?;
        let next_line = canvas.margin;
        Ok(Self {
            canvas,
            native_text,
            number: 0,
            next_line,
            current_text: String::new(),
            pending: String::new(),
            callback,
        })
    }

    fn emit(&mut self, image: DynamicImage, text: Option<String>, size: [f32; 2]) -> Result<()> {
        self.number += 1;
        ensure!(self.number <= MAX_PAGES, "文档页数超过 {MAX_PAGES}");
        (self.callback)(Page {
            number: self.number,
            image,
            text,
            size,
        })
    }

    fn page_break(&mut self) -> Result<()> {
        if !self.current_text.is_empty() {
            let text = std::mem::take(&mut self.current_text);
            let image = self.canvas.pixels()?;
            self.emit(image, self.native_text.then_some(text), [595.0, 842.0])?;
            self.canvas.clear();
        }
        self.next_line = self.canvas.margin;
        Ok(())
    }

    fn flush_text(&mut self) -> Result<()> {
        for paragraph in std::mem::take(&mut self.pending).lines() {
            let line = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
            let text: Vec<_> = line.encode_utf16().collect();
            let mut rest = text.as_slice();
            while !rest.is_empty() {
                if self.next_line + self.canvas.line_height
                    > self.canvas.height - self.canvas.margin
                {
                    self.page_break()?;
                }
                let length = self.canvas.fit(rest)?;
                let (line, next) = rest.split_at(length);
                self.canvas.paint(line, self.next_line)?;
                self.current_text.push_str(&String::from_utf16_lossy(line));
                self.current_text.push('\n');
                self.next_line += self.canvas.line_height;
                rest = next.strip_prefix(&[u16::from(b' ')]).unwrap_or(next);
            }
        }
        Ok(())
    }

    fn picture(&mut self, image: DynamicImage) -> Result<()> {
        self.flush_text()?;
        self.page_break()?;
        let scale = (840.0 / image.width().max(image.height()) as f32).min(0.75);
        let size = [image.width() as f32 * scale, image.height() as f32 * scale];
        self.emit(image, None, size)
    }

    fn finish(mut self) -> Result<()> {
        self.flush_text()?;
        self.page_break()?;
        ensure!(self.number > 0, "电子书没有可排版的文字或受支持的位图");
        Ok(())
    }
}

fn archive_entry(
    archive: &mut zip::ZipArchive<impl Read + Seek>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let entry = archive
        .by_name(name)
        .with_context(|| format!("EPUB 缺少内容 {name}"))?;
    ensure!(entry.size() <= limit, "EPUB 内容过大：{name}");
    let mut bytes = Vec::new();
    entry.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "EPUB 内容过大：{name}");
    Ok(bytes)
}

fn markup_string(data: &[u8]) -> Result<std::borrow::Cow<'_, str>> {
    let encoding = encoding_rs::Encoding::for_bom(data)
        .map(|(encoding, _)| encoding)
        .or_else(|| {
            if data.starts_with(b"<\0?\0") {
                Some(encoding_rs::UTF_16LE)
            } else if data.starts_with(b"\0<\0?") {
                Some(encoding_rs::UTF_16BE)
            } else {
                None
            }
        })
        .or_else(|| {
            let head = data.get(..data.len().min(192))?;
            let end = head.windows(2).position(|part| part == b"?>")?;
            let declaration = std::str::from_utf8(&head[..end]).ok()?;
            let (_, attribute) = declaration.split_once("encoding")?;
            let attribute = attribute.trim_start().strip_prefix('=')?.trim_start();
            let quote = attribute
                .chars()
                .next()
                .filter(|&c| c == '\'' || c == '"')?;
            let (name, _) = attribute[1..].split_once(quote)?;
            encoding_rs::Encoding::for_label(name.as_bytes())
        })
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, malformed) = encoding.decode(data);
    ensure!(!malformed, "电子书文本编码无效");
    Ok(text)
}

fn archive_path(base: &str, reference: &str) -> Option<String> {
    let reference = reference.split(['#', '?']).next()?.trim();
    if reference.is_empty() || reference.starts_with("//") {
        return None;
    }
    let bytes = reference.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = |c: u8| char::from(c).to_digit(16).map(|d| d as u8);
            decoded.push(hex(*bytes.get(index + 1)?)? * 16 + hex(*bytes.get(index + 2)?)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = std::str::from_utf8(&decoded).ok()?;
    if decoded.contains(['\\', ':', '\0']) || decoded.starts_with("//") {
        return None;
    }
    let mut segments: Vec<_> = if decoded.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map(|(folder, _)| folder.split('/').collect())
            .unwrap_or_default()
    };
    for segment in decoded.split('/') {
        match segment {
            "" | "." => (),
            ".." => {
                segments.pop()?;
            }
            segment => segments.push(segment),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

fn raster_image(data: &[u8]) -> Result<Option<DynamicImage>> {
    ensure!(
        data.len() as u64 <= MAX_IMAGE_BYTES,
        "电子书位图超过 32 MiB"
    );
    let mut reader = ImageReader::new(std::io::Cursor::new(data)).with_guessed_format()?;
    if reader.format().is_none() {
        return Ok(None);
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map(Some).context("电子书位图无法解码")
}

fn base64_image(data: &str) -> Result<Option<DynamicImage>> {
    ensure!(
        data.len() as u64 <= MAX_IMAGE_BYTES * 2,
        "电子书位图超过 32 MiB"
    );
    let data: Vec<_> = data
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let image = base64::engine::general_purpose::STANDARD.decode(data)?;
    raster_image(&image)
}

fn visit_epub<F: FnMut(Page) -> Result<()>>(path: &Path, mut pages: Reflow<F>) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    ensure!(archive.len() <= 30_000, "EPUB 包含过多文件");
    let container = archive_entry(&mut archive, "META-INF/container.xml", 1024 * 1024)?;
    let container = markup_string(&container)?;
    let container = roxmltree::Document::parse(&container).context("EPUB 容器配置无效")?;
    let opf = container
        .descendants()
        .find(|node| node.has_tag_name("rootfile"))
        .and_then(|node| node.attribute("full-path"))
        .and_then(|reference| archive_path("", reference))
        .context("EPUB 未指定书籍目录")?;
    let package = archive_entry(&mut archive, &opf, 4 * 1024 * 1024)?;
    let package = markup_string(&package)?;
    let package = roxmltree::Document::parse(&package).context("EPUB 书籍目录无效")?;
    let manifest: HashMap<_, _> = package
        .descendants()
        .filter(|node| node.has_tag_name("item"))
        .filter_map(|node| {
            Some((
                node.attribute("id")?,
                (node.attribute("href")?, node.attribute("media-type")?),
            ))
        })
        .collect();
    let mut budget = MAX_TEXT_BYTES;
    for item in package
        .descendants()
        .filter(|node| node.has_tag_name("itemref"))
    {
        let id = item
            .attribute("idref")
            .context("EPUB 阅读顺序缺少内容标识")?;
        let (reference, media_type) = manifest.get(id).context("EPUB 阅读顺序引用了未声明内容")?;
        let entry = archive_path(&opf, reference).context("EPUB 内容路径无效")?;
        if media_type.starts_with("image/") {
            if let Some(image) =
                raster_image(&archive_entry(&mut archive, &entry, MAX_IMAGE_BYTES)?)?
            {
                pages.picture(image)?;
            }
        } else if matches!(
            *media_type,
            "application/xhtml+xml" | "text/html" | "application/x-dtbook+xml"
        ) {
            let bytes = archive_entry(&mut archive, &entry, budget.min(16 * 1024 * 1024))?;
            budget -= bytes.len() as u64;
            let text = markup_string(&bytes)?;
            consume_html(&text, &mut pages, |reference, _| {
                if let Some((header, data)) = reference
                    .strip_prefix("data:image/")
                    .and_then(|value| value.split_once(','))
                {
                    if header.ends_with(";base64") {
                        return base64_image(data);
                    }
                    return Ok(None);
                }
                let Some(name) = archive_path(&entry, reference) else {
                    return Ok(None);
                };
                if archive.index_for_name(&name).is_none() {
                    return Ok(None);
                }
                raster_image(&archive_entry(&mut archive, &name, MAX_IMAGE_BYTES)?)
            })?;
            pages.pending.push('\n');
            pages.flush_text()?;
        }
    }
    pages.finish()
}

fn visit_fb2<F: FnMut(Page) -> Result<()>>(path: &Path, mut pages: Reflow<F>) -> Result<()> {
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() as u64 <= MAX_TEXT_BYTES, "FB2 超过 64 MiB");
    let text = markup_string(&bytes)?;
    let book = roxmltree::Document::parse(&text).context("FB2 XML 无效")?;
    let root = book.root_element();
    ensure!(
        root.has_tag_name("FictionBook"),
        "FB2 缺少 FictionBook 根元素"
    );
    let binary: HashMap<_, _> = root
        .children()
        .filter(|node| node.has_tag_name("binary"))
        .filter_map(|node| Some((node.attribute("id")?, node.text()?)))
        .collect();
    for body in root.children().filter(|node| node.has_tag_name("body")) {
        fb2_node(body, &binary, &mut pages, 0)?;
        pages.flush_text()?;
    }
    pages.finish()
}

fn fb2_node<F: FnMut(Page) -> Result<()>>(
    node: roxmltree::Node<'_, '_>,
    binary: &HashMap<&str, &str>,
    pages: &mut Reflow<F>,
    depth: usize,
) -> Result<()> {
    ensure!(depth < 100, "FB2 正文嵌套过深");
    if node.is_text() {
        push_inline(&mut pages.pending, node.text().unwrap_or(""));
    } else if node.has_tag_name("image") {
        let href = node
            .attribute(("http://www.w3.org/1999/xlink", "href"))
            .or_else(|| node.attribute("href"));
        if let Some(data) = href
            .and_then(|value| value.strip_prefix('#'))
            .and_then(|id| binary.get(id))
        {
            if let Some(image) = base64_image(data)? {
                pages.picture(image)?;
            }
        }
    } else if node.is_element() {
        let block = matches!(
            node.tag_name().name(),
            "p" | "title"
                | "subtitle"
                | "section"
                | "v"
                | "stanza"
                | "poem"
                | "text-author"
                | "empty-line"
                | "body"
        );
        if block {
            pages.pending.push('\n');
        }
        for child in node.children() {
            fb2_node(child, binary, pages, depth + 1)?;
        }
        if block {
            pages.pending.push('\n');
        }
    }
    Ok(())
}

fn push_inline(output: &mut String, text: &str) {
    for character in text.chars() {
        if character.is_whitespace() {
            if !output.ends_with([' ', '\n']) {
                output.push(' ');
            }
        } else if !character.is_control() {
            output.push(character);
        }
    }
}

fn consume_html<F: FnMut(Page) -> Result<()>>(
    text: &str,
    pages: &mut Reflow<F>,
    mut picture: impl FnMut(&str, Option<usize>) -> Result<Option<DynamicImage>>,
) -> Result<()> {
    use quick_xml::{Reader, escape::resolve_predefined_entity, events::Event};

    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = false;
    reader.config_mut().allow_unmatched_ends = true;
    let mut hidden = None;
    loop {
        match reader.read_event().context("电子书 HTML 内容无效")? {
            Event::Eof => break,
            Event::Text(event) if hidden.is_none() => {
                push_inline(&mut pages.pending, &event.decode()?)
            }
            Event::CData(event) if hidden.is_none() => {
                push_inline(&mut pages.pending, &event.decode()?)
            }
            Event::GeneralRef(reference) if hidden.is_none() => {
                if let Some(character) = reference.resolve_char_ref()? {
                    push_inline(&mut pages.pending, character.encode_utf8(&mut [0; 4]));
                } else {
                    let name = reference.decode()?;
                    if let Some(decoded) = resolve_predefined_entity(&name) {
                        push_inline(&mut pages.pending, decoded);
                    } else {
                        pages.pending.push('&');
                        pages.pending.push_str(&name);
                        pages.pending.push(';');
                    }
                }
            }
            Event::End(event) => {
                let tag = event.local_name().as_ref().to_ascii_lowercase();
                if hidden.as_deref() == Some(tag.as_slice()) {
                    hidden = None;
                } else if hidden.is_none() && html_line_break(&tag) {
                    pages.pending.push('\n');
                }
            }
            Event::Start(event) | Event::Empty(event) => {
                let tag = event.local_name().as_ref().to_ascii_lowercase();
                if tag == b"body" {
                    hidden = None;
                }
                if hidden.is_some() {
                    continue;
                }
                if matches!(tag.as_slice(), b"head" | b"script" | b"style" | b"template") {
                    hidden = Some(tag);
                } else if matches!(tag.as_slice(), b"img" | b"image") {
                    let mut href = String::new();
                    let mut alt = String::new();
                    let mut recindex = None;
                    for item in event.html_attributes() {
                        let Ok(item) = item else { continue };
                        let value = item
                            .decode_and_unescape_value(reader.decoder())
                            .unwrap_or_else(|_| String::from_utf8_lossy(&item.value));
                        match item
                            .key
                            .local_name()
                            .as_ref()
                            .to_ascii_lowercase()
                            .as_slice()
                        {
                            b"src" | b"href" => href = value.into_owned(),
                            b"alt" => alt = value.into_owned(),
                            b"recindex" => recindex = value.parse().ok(),
                            _ => (),
                        }
                    }
                    if let Some(image) = picture(&href, recindex)? {
                        pages.picture(image)?;
                    } else if !alt.is_empty() {
                        push_inline(&mut pages.pending, &alt);
                    }
                } else if html_line_break(&tag) {
                    pages.pending.push('\n');
                }
            }
            _ => (),
        }
    }
    Ok(())
}

fn html_line_break(tag: &[u8]) -> bool {
    matches!(
        tag,
        b"p" | b"br"
            | b"div"
            | b"h1"
            | b"h2"
            | b"h3"
            | b"h4"
            | b"h5"
            | b"h6"
            | b"li"
            | b"tr"
            | b"blockquote"
            | b"section"
            | b"article"
            | b"pre"
            | b"pagebreak"
    )
}

fn visit_mobi<F: FnMut(Page) -> Result<()>>(path: &Path, mut pages: Reflow<F>) -> Result<()> {
    use mobi::headers::{Compression, Encryption, TextEncoding};

    ensure!(
        path.metadata()?.len() <= 32 * 1024 * 1024,
        "MOBI 超过 32 MiB"
    );
    let book = std::panic::catch_unwind(|| mobi::Mobi::from_path(path))
        .map_err(|_| anyhow::anyhow!("MOBI 书籍目录无效"))??;
    ensure!(book.encryption() == Encryption::No, "MOBI 带有 DRM 加密");
    ensure!(
        book.metadata.mobi.format_version < 8 && book.compression() != Compression::Huff,
        "当前支持未加密的经典 MOBI（未压缩或 PalmDOC 压缩）"
    );
    let records = &book.metadata.records.records;
    let record = |index: usize| -> Option<&[u8]> {
        let start = records.get(index)?.offset as usize;
        let end = records
            .get(index + 1)
            .map(|record| record.offset as usize)
            .unwrap_or(book.content.len());
        book.content.get(start..end)
    };
    let first = (book.metadata.mobi.first_content_record as usize).max(1);
    let count = book.metadata.palmdoc.record_count as usize;
    let length = book.metadata.palmdoc.text_length as usize;
    ensure!(
        count > 0 && first + count <= records.len() && length as u64 <= MAX_TEXT_BYTES,
        "MOBI 正文记录数或大小无效"
    );
    let mut content = Vec::with_capacity(length);
    let record_size = book.metadata.palmdoc.record_size as usize;
    ensure!((1..=65_535).contains(&record_size), "MOBI 记录长度无效");
    for index in first..first + count {
        let raw = record(index).context("MOBI 正文记录索引无效")?;
        let limit = (length - content.len()).min(record_size);
        if book.compression() == Compression::PalmDoc {
            append_palmdoc(raw, &mut content, limit)?;
        } else {
            content.extend_from_slice(&raw[..raw.len().min(limit)]);
        }
    }
    ensure!(content.len() == length, "MOBI 正文数据读取不完整");
    let text = if book.text_encoding() == TextEncoding::CP1252 {
        encoding_rs::WINDOWS_1252.decode(&content).0
    } else {
        String::from_utf8_lossy(&content)
    };
    let first_image = book.metadata.mobi.first_image_index as usize;
    consume_html(&text, &mut pages, |_, index| {
        let Some(index) = index.and_then(|index| index.checked_sub(1)) else {
            return Ok(None);
        };
        let Some(raw) = first_image.checked_add(index).and_then(record) else {
            return Ok(None);
        };
        raster_image(raw)
    })?;
    pages.finish()
}

fn append_palmdoc(raw: &[u8], output: &mut Vec<u8>, limit: usize) -> Result<()> {
    let start = output.len();
    let end = start + limit;
    let mut index = 0;
    while let Some(&byte) = raw.get(index).filter(|_| output.len() < end) {
        index += 1;
        match byte {
            1..=8 => {
                let next = index + byte as usize;
                let run = raw.get(index..next).context("MOBI PalmDOC 文本截断")?;
                output.extend_from_slice(&run[..run.len().min(end - output.len())]);
                index = next;
            }
            0x80..=0xBF => {
                let next = *raw.get(index).context("MOBI PalmDOC 引用截断")?;
                index += 1;
                let pair = u16::from_be_bytes([byte, next]);
                let distance = ((pair >> 3) & 0x7FF) as usize;
                let count = ((pair & 7) + 3) as usize;
                ensure!(
                    distance > 0 && distance <= output.len() - start,
                    "MOBI PalmDOC 引用无效"
                );
                for _ in 0..count.min(end - output.len()) {
                    output.push(output[output.len() - distance]);
                }
            }
            0xC0..=0xFF => {
                output.push(b' ');
                if output.len() < end {
                    output.push(byte ^ 0x80);
                }
            }
            _ => output.push(byte),
        }
    }
    Ok(())
}
