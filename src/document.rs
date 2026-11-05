use crate::engine::TextBlock;
use anyhow::{Context, Result, ensure};
use image::{DynamicImage, GenericImageView, codecs::jpeg::JpegEncoder};
use lopdf::{
    Dictionary, Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};
use windows::{
    Data::Pdf::{PdfDocument, PdfPageRenderOptions},
    Storage::{
        StorageFile,
        Streams::{DataReader, InMemoryRandomAccessStream},
    },
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::HSTRING,
};

struct Apartment;

impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

pub struct Page {
    pub number: u32,
    pub image: DynamicImage,
    pub text: Option<String>,
    pub size: [f32; 2],
}

pub fn visit_with_password(
    path: &Path,
    dpi: u32,
    native_text: bool,
    password: &str,
    mut page: impl FnMut(Page) -> Result<()>,
) -> Result<()> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "cbz" => visit_cbz(path, page),
        "pdf" => {
            let _apartment = Apartment::new()?;
            let pdf = open_pdf(path, password)?;
            let source = native_text
                .then(|| -> Result<Document> {
                    let mut doc = Document::load(path)?;
                    if doc.is_encrypted() {
                        doc.decrypt(password)?;
                    }
                    Ok(doc)
                })
                .and_then(Result::ok);
            for index in 0..pdf.PageCount()? {
                let handle = pdf.GetPage(index)?;
                let size = handle.Size()?;
                let scale = dpi.clamp(72, 350) as f32 / 96.0;
                let max = size.Width.max(size.Height);
                ensure!(max.is_finite() && max > 0.0, "无效的 PDF 页面尺寸");
                let scale = scale.min(4200.0 / max);
                let settings = PdfPageRenderOptions::new()?;
                settings.SetDestinationWidth((size.Width * scale).round().max(1.0) as u32)?;
                settings.SetDestinationHeight((size.Height * scale).round().max(1.0) as u32)?;
                let stream = InMemoryRandomAccessStream::new()?;
                handle
                    .RenderWithOptionsToStreamAsync(&stream, &settings)?
                    .join()?;
                let count = u32::try_from(stream.Size()?)?;
                ensure!(count <= 120_000_000, "PDF 页面图像超出大小限制");
                let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
                use windows::core::Interface;
                use windows_future::IAsyncOperation;
                let operation: IAsyncOperation<u32> = reader.LoadAsync(count)?.cast()?;
                ensure!(operation.join()? == count, "PDF 页面图像读取不完整");
                let mut bytes = vec![0; count as usize];
                reader.ReadBytes(&mut bytes)?;
                let text = source
                    .as_ref()
                    .and_then(|doc| doc.extract_text(&[index + 1]).ok())
                    .filter(|text| !text.trim().is_empty());
                page(Page {
                    number: index + 1,
                    image: image::load_from_memory(&bytes)?,
                    text,
                    size: [size.Width * 0.75, size.Height * 0.75],
                })?;
            }
            Ok(())
        }
        "xps" | "epub" | "fb2" | "mobi" => {
            crate::document_extra::visit(path, dpi, native_text, page)
        }
        _ => anyhow::bail!("文档格式不受支持"),
    }
}

fn open_pdf(path: &Path, password: &str) -> Result<PdfDocument> {
    let absolute = std::path::absolute(path)?;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(absolute.as_os_str()))?.join()?;
    let operation = if password.is_empty() {
        PdfDocument::LoadFromFileAsync(&file)?
    } else {
        PdfDocument::LoadFromFileWithPasswordAsync(&file, &HSTRING::from(password))?
    };
    operation.join().context("无法打开 PDF，请检查文件或密码")
}

pub fn pdf_page_count(path: &Path, password: &str) -> Result<u32> {
    let _apartment = Apartment::new()?;
    Ok(open_pdf(path, password)?.PageCount()?)
}

fn visit_cbz(path: &Path, mut page: impl FnMut(Page) -> Result<()>) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    let mut names: Vec<_> = archive
        .file_names()
        .filter(|name| {
            Path::new(name)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    ["jpg", "jpeg", "png", "bmp", "webp", "tiff", "gif"]
                        .iter()
                        .any(|ext| value.eq_ignore_ascii_case(ext))
                })
                && !name.starts_with("__MACOSX/")
        })
        .map(str::to_owned)
        .collect();
    names.sort_by_key(|name| natural_key(name));
    ensure!(!names.is_empty(), "CBZ 中没有受支持的图片");
    for (index, name) in names.into_iter().enumerate() {
        let mut entry = archive.by_name(&name)?;
        ensure!(entry.size() <= 64_000_000, "CBZ 中的页面过大：{name}");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        let image =
            image::load_from_memory(&bytes).with_context(|| format!("无法读取 CBZ 页面 {name}"))?;
        let scale = (840.0 / image.width().max(image.height()) as f32).min(0.75);
        let size = [image.width() as f32 * scale, image.height() as f32 * scale];
        page(Page {
            number: index as u32 + 1,
            image,
            text: None,
            size,
        })?;
    }
    Ok(())
}

fn natural_key(name: &str) -> Vec<(String, u64)> {
    name.split_inclusive(|character: char| !character.is_ascii_digit())
        .map(|part| {
            let digits = part.trim_end_matches(|character: char| !character.is_ascii_digit());
            (
                part[digits.len()..].to_ascii_lowercase(),
                digits.parse().unwrap_or(0),
            )
        })
        .collect()
}

pub struct SearchablePdf {
    document: Document,
    pages: (u32, u16),
    children: Vec<Object>,
    display_font: Option<DisplayFont>,
}

struct DisplayFont {
    bytes: Vec<u8>,
    descriptor: (u32, u16),
    name: &'static str,
}

impl SearchablePdf {
    pub fn new() -> Self {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        Self {
            document,
            pages,
            children: Vec::new(),
            display_font: None,
        }
    }

    pub fn new_text_only() -> Result<Self> {
        use ttf_parser::{Face, Permissions};
        let mut pdf = Self::new();
        let folder = std::env::var_os("WINDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "C:\\Windows".into())
            .join("Fonts");
        for (filename, name) in [
            ("simhei.ttf", "SimHei"),
            ("simkai.ttf", "KaiTi"),
            ("arial.ttf", "Arial"),
        ] {
            let Ok(bytes) = std::fs::read(folder.join(filename)) else {
                continue;
            };
            let Ok(face) = Face::parse(&bytes, 0) else {
                continue;
            };
            if !face.is_outline_embedding_allowed()
                || matches!(face.permissions(), Some(Permissions::Restricted))
            {
                continue;
            }
            let scale = 1000.0 / face.units_per_em() as f32;
            let bounds = face.global_bounding_box();
            let font_bounds = [bounds.x_min, bounds.y_min, bounds.x_max, bounds.y_max]
                .map(|value| (value as f32 * scale).round() as i32);
            let ascent = (face.ascender() as f32 * scale).round() as i32;
            let descent = (face.descender() as f32 * scale).round() as i32;
            let font_file = pdf.document.add_object(Stream::new(
                dictionary! { "Length1" => bytes.len() as i64 },
                bytes.clone(),
            ));
            let descriptor = pdf.document.add_object(dictionary! {
                "Type" => "FontDescriptor", "FontName" => name, "Flags" => 4,
                "FontBBox" => font_bounds.into_iter().map(Object::from).collect::<Vec<_>>(),
                "ItalicAngle" => 0, "Ascent" => ascent, "Descent" => descent, "CapHeight" => ascent, "StemV" => 80, "FontFile2" => font_file,
            });
            pdf.display_font = Some(DisplayFont {
                bytes,
                descriptor,
                name,
            });
            return Ok(pdf);
        }
        anyhow::bail!("找不到允许嵌入 PDF 的 TrueType 字体")
    }

    pub fn add(&mut self, page: &Page, blocks: &[TextBlock], text: &str) -> Result<()> {
        let (width, height) = page.image.dimensions();
        let [pw, ph] = page.size;
        let mut operations = Vec::new();
        let mut xobjects = Dictionary::new();
        if self.display_font.is_none() {
            let mut jpeg = Vec::new();
            JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&page.image.to_rgb8())?;
            let image = self.document.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => width, "Height" => height,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
                },
                jpeg,
            ));
            xobjects.set("PageImage", image);
            operations.extend([
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![pw.into(), 0.into(), 0.into(), ph.into(), 0.into(), 0.into()],
                ),
                Operation::new("Do", vec![Object::Name(b"PageImage".to_vec())]),
                Operation::new("Q", vec![]),
            ]);
        }
        let mut fonts = Dictionary::new();
        let sx = pw / width as f32;
        let sy = ph / height as f32;
        let mut runs: Vec<_> = blocks
            .iter()
            .map(|block| {
                let [left, top] = block.points[0];
                let [right, bottom] = block.points[2];
                (
                    block.text.as_str(),
                    left as f32 * sx,
                    ph - bottom as f32 * sy,
                    ((bottom - top) as f32 * sy).max(2.0),
                    ((right - left) as f32 * sx).max(2.0),
                )
            })
            .collect();
        if runs.is_empty() {
            runs.extend(text.lines().enumerate().map(|(index, line)| {
                (
                    line,
                    16.0,
                    (ph - 20.0 - index as f32 * 11.0).max(5.0),
                    10.0,
                    (pw - 32.0).max(10.0),
                )
            }));
        }
        let glyphs: BTreeMap<char, u16> = runs
            .iter()
            .flat_map(|run| run.0.chars())
            .filter(|character| !character.is_control())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(u16::MAX as usize - 1)
            .enumerate()
            .map(|(index, character)| (character, index as u16 + 1))
            .collect();
        if !glyphs.is_empty() {
            let (font, advances) =
                pdf_font(&mut self.document, &glyphs, self.display_font.as_ref())?;
            fonts.set("OCR", font);
            for (text, x, y, size, target_width) in runs {
                let codes: Vec<_> = text.chars().filter_map(|ch| glyphs.get(&ch)).collect();
                if codes.is_empty() {
                    continue;
                }
                let advance: i32 = codes.iter().map(|code| advances[**code as usize]).sum();
                let mut stretch = target_width / (size * (advance.max(1) as f32 / 1000.0));
                if blocks.is_empty() {
                    stretch = stretch.min(1.0);
                }
                let data = codes
                    .into_iter()
                    .flat_map(|code| code.to_be_bytes())
                    .collect();
                operations.extend([
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec![Object::Name(b"OCR".to_vec()), size.into()]),
                    Operation::new(
                        "Tr",
                        vec![if self.display_font.is_some() {
                            0.into()
                        } else {
                            3.into()
                        }],
                    ),
                    Operation::new(
                        "Tm",
                        vec![
                            stretch.into(),
                            0.into(),
                            0.into(),
                            1.into(),
                            x.into(),
                            y.into(),
                        ],
                    ),
                    Operation::new(
                        "Tj",
                        vec![Object::String(data, lopdf::StringFormat::Hexadecimal)],
                    ),
                    Operation::new("ET", vec![]),
                ]);
            }
        }
        let contents = self.document.add_object(Stream::new(
            dictionary! {},
            Content { operations }.encode()?,
        ));
        let child = self.document.add_object(dictionary! {
            "Type" => "Page", "Parent" => self.pages, "MediaBox" => vec![0.into(), 0.into(), pw.into(), ph.into()],
            "Resources" => dictionary! { "XObject" => xobjects, "Font" => fonts },
            "Contents" => contents,
        });
        self.children.push(child.into());
        Ok(())
    }

    pub fn save(mut self, path: &Path) -> Result<()> {
        ensure!(!self.children.is_empty(), "没有可导出的页面");
        let count = self.children.len() as i64;
        self.document.objects.insert(
            self.pages,
            dictionary! { "Type" => "Pages", "Kids" => self.children, "Count" => count }.into(),
        );
        let catalog = self
            .document
            .add_object(dictionary! { "Type" => "Catalog", "Pages" => self.pages });
        self.document.trailer.set("Root", catalog);
        self.document.compress();
        self.document.save(path)?;
        Ok(())
    }
}

fn pdf_font(
    doc: &mut Document,
    glyphs: &BTreeMap<char, u16>,
    display: Option<&DisplayFont>,
) -> Result<((u32, u16), Vec<i32>)> {
    use std::fmt::Write as _;
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /OCR-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<_> = glyphs.iter().collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for (ch, code) in chunk {
            let _ = write!(cmap, "<{code:04X}> <");
            for unit in ch.encode_utf16(&mut [0; 2]) {
                let _ = write!(cmap, "{unit:04X}");
            }
            cmap.push_str(">\n");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    let unicode = doc.add_object(Stream::new(dictionary! {}, cmap.into_bytes()));
    let mut advances = vec![600; glyphs.len() + 1];
    let (name, descriptor, mapping) = if let Some(display) = display {
        let face = ttf_parser::Face::parse(&display.bytes, 0)
            .map_err(|error| anyhow::anyhow!("无法读取 PDF 字体：{error:?}"))?;
        let mut ids = vec![0u8; advances.len() * 2];
        for (ch, code) in glyphs {
            let gid = face.glyph_index(*ch).unwrap_or(ttf_parser::GlyphId(0));
            let index = *code as usize;
            ids[index * 2..index * 2 + 2].copy_from_slice(&gid.0.to_be_bytes());
            if let Some(advance) = face.glyph_hor_advance(gid) {
                advances[index] =
                    (advance as f32 * 1000.0 / face.units_per_em() as f32).round() as i32;
            }
        }
        (
            display.name,
            display.descriptor,
            doc.add_object(Stream::new(dictionary! {}, ids)).into(),
        )
    } else {
        let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "OCRLayer", "Flags" => 4, "FontBBox" => vec![0.into(), (-200).into(), 600.into(), 800.into()],
        "ItalicAngle" => 0, "Ascent" => 800, "Descent" => -200, "CapHeight" => 800, "StemV" => 80,
        });
        ("OCRLayer", descriptor, Object::Name(b"Identity".to_vec()))
    };
    let descendant = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => name, "FontDescriptor" => descriptor,
        "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 },
        "DW" => 600, "CIDToGIDMap" => mapping,
        "W" => vec![1.into(), Object::Array(advances.iter().skip(1).copied().map(Object::from).collect())],
    });
    let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type0", "BaseFont" => name, "Encoding" => "Identity-H", "DescendantFonts" => vec![descendant.into()], "ToUnicode" => unicode });
    Ok((font, advances))
}
