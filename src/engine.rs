use std::{borrow::Cow, fs, path::Path};

use anyhow::{Context, Result, anyhow, ensure};
use image::{DynamicImage, Rgb, RgbImage, imageops};
use ort::{
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};
use serde::{Deserialize, Serialize};

pub const LANGUAGES: &[(&str, &str)] = &[
    ("chinese", "简体中文"),
    ("chinese_cht", "繁體中文"),
    ("en", "English"),
    ("japan", "日本語"),
    ("korean", "한국어"),
    ("cyrillic", "Кириллица"),
    ("v6_tiny", "PP-OCRv6 Tiny · 中英"),
];

const DET_PIXEL_LIMIT: usize = 32 * 1024 * 1024;
const MAX_BOXES: usize = 1000;
const REC_BATCH: usize = 6;
const REC_MIN_WIDTH: usize = 320;
const REC_MAX_WIDTH: usize = 8192;
const DET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const DET_SCALE: [f32; 3] = [1.0 / 0.229, 1.0 / 0.224, 1.0 / 0.225];
const TEXT_MEAN: [f32; 3] = [0.5; 3];
const TEXT_SCALE: [f32; 3] = [2.0; 3];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrOptions {
    pub language: String,
    pub threads: usize,
    pub classify: bool,
    pub side_limit: u32,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            language: "chinese".to_owned(),
            threads: 4,
            classify: false,
            side_limit: 960,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    pub score: f32,
    #[serde(rename = "box")]
    pub points: [[i32; 2]; 4],
}

pub struct Engine {
    det: Session,
    cls: Option<Session>,
    rec: Session,
    dictionary: Vec<String>,
    language: String,
    threads: usize,
    root: std::path::PathBuf,
}

impl Engine {
    pub fn new(root: &Path, options: &OcrOptions) -> Result<Self> {
        let language = language_id(&options.language)?;
        ensure!((1..=64).contains(&options.threads), "OCR 线程数应为 1–64");
        let root = root
            .canonicalize()
            .with_context(|| format!("找不到 OCR 模型目录 {}", root.display()))?;
        let runtime = root.join("onnxruntime.dll");
        ensure!(runtime.is_file(), "缺少 ONNX Runtime {}", runtime.display());
        ort::init_from(&runtime)
            .with_context(|| format!("加载 ONNX Runtime {}", runtime.display()))?
            .with_name("WfZiUiBp")
            .commit();
        let (rec, dictionary) = load_recognizer(&root, language, options.threads)?;
        Ok(Self {
            det: load_session(&root.join(detector_file(language)), options.threads)?,
            cls: None,
            rec,
            dictionary,
            language: language.to_owned(),
            threads: options.threads,
            root,
        })
    }

    pub fn recognize(
        &mut self,
        image: &DynamicImage,
        options: &OcrOptions,
    ) -> Result<Vec<TextBlock>> {
        let language = language_id(&options.language)?;
        if options.threads != self.threads {
            *self = Self::new(&self.root, options)?;
        } else if language != self.language {
            let (rec, dictionary) = load_recognizer(&self.root, language, self.threads)?;
            if detector_file(language) != detector_file(&self.language) {
                self.det = load_session(&self.root.join(detector_file(language)), self.threads)?;
            }
            (self.rec, self.dictionary) = (rec, dictionary);
            self.language = language.to_owned();
        }
        ensure!(image.width() > 0 && image.height() > 0, "OCR 图像为空");
        let rgb = match image.as_rgb8() {
            Some(rgb) => Cow::Borrowed(rgb),
            None => Cow::Owned(image.to_rgb8()),
        };
        let quads = self.detect(&rgb, options.side_limit)?;
        if quads.is_empty() {
            return Ok(Vec::new());
        }
        let mut crops = quads
            .iter()
            .map(|quad| warp_crop(&rgb, *quad))
            .collect::<Vec<_>>();
        if options.classify {
            self.classify(&mut crops)?;
        }
        self.read(&crops, &quads)
    }

    fn detect(&mut self, image: &RgbImage, limit: u32) -> Result<Vec<Quad>> {
        let longest = image.width().max(image.height());
        let ratio = if limit == 0 || longest <= limit {
            1.0
        } else {
            limit as f32 / longest as f32
        };
        let width = rounded_stride(image.width(), ratio);
        let height = rounded_stride(image.height(), ratio);
        ensure!(
            width.saturating_mul(height) <= DET_PIXEL_LIMIT,
            "检测图像过大，请调整边长限制"
        );
        let mut data = vec![0.0; 3 * width * height];
        pack(image, &mut data, width, height, width, DET_MEAN, DET_SCALE);
        let input = Tensor::from_array(([1, 3, height, width], data))?;
        let outputs = self.det.run(ort::inputs![input])?;
        let (shape, map) = outputs[0].try_extract_tensor::<f32>()?;
        ensure!(
            shape.len() == 4 && shape[0] == 1 && shape[1] == 1,
            "检测模型输出维度无效"
        );
        let (map_height, map_width) = (shape[2] as usize, shape[3] as usize);
        ensure!(
            map_width > 0 && map_height > 0 && map_width.saturating_mul(map_height) == map.len(),
            "检测模型输出大小无效"
        );
        let mut quads = find_boxes(
            map,
            map_width,
            map_height,
            image.width(),
            image.height(),
            DetectionSettings::for_language(&self.language),
        );
        quads.sort_by(|a, b| a[0][1].cmp(&b[0][1]).then(a[0][0].cmp(&b[0][0])));
        for index in 1..quads.len() {
            let mut pos = index;
            while pos > 0
                && (quads[pos][0][1] - quads[pos - 1][0][1]).abs() < 10
                && quads[pos][0][0] < quads[pos - 1][0][0]
            {
                quads.swap(pos, pos - 1);
                pos -= 1;
            }
        }
        Ok(quads)
    }

    fn classify(&mut self, crops: &mut [RgbImage]) -> Result<()> {
        if self.cls.is_none() {
            self.cls = Some(load_session(&self.root.join("cls.onnx"), self.threads)?);
        }
        let cls = self.cls.as_mut().expect("classifier initialized");
        let mut input = vec![0.0; 3 * 48 * 192];
        for crop in crops {
            input.fill(0.0);
            let width = resized_width(crop, 48).min(192);
            pack(crop, &mut input, width, 48, 192, TEXT_MEAN, TEXT_SCALE);
            let tensor =
                ort::value::TensorRef::from_array_view(([1, 3, 48, 192], input.as_slice()))?;
            let output = cls.run(ort::inputs![tensor])?;
            let (_, scores) = output[0].try_extract_tensor::<f32>()?;
            ensure!(scores.len() == 2, "方向模型输出维度无效");
            if scores[1] > scores[0] && scores[1] > 0.9 {
                imageops::rotate180_in_place(crop);
            }
        }
        Ok(())
    }

    fn read(&mut self, crops: &[RgbImage], quads: &[Quad]) -> Result<Vec<TextBlock>> {
        let traditional = self.language == "chinese_cht";
        let v6 = self.language == "v6_tiny";
        let height = if traditional { 32 } else { 48 };
        let max_width = if v6 { 3200 } else { REC_MAX_WIDTH };
        let padding = if v6 { 0.0 } else { -1.0 };
        let mut order = (0..crops.len()).collect::<Vec<_>>();
        order.sort_by(|&a, &b| {
            (f64::from(crops[a].width()) / f64::from(crops[a].height()))
                .total_cmp(&(f64::from(crops[b].width()) / f64::from(crops[b].height())))
        });
        let mut blocks = Vec::with_capacity(crops.len());
        for indices in order.chunks(REC_BATCH) {
            let width = indices
                .iter()
                .map(|&idx| {
                    (height as f32 * crops[idx].width() as f32 / crops[idx].height() as f32)
                        as usize
                })
                .max()
                .unwrap_or(REC_MIN_WIDTH)
                .clamp(REC_MIN_WIDTH, max_width);
            let stride = 3 * height * width;
            let mut input = vec![padding; stride * indices.len()];
            for (batch, &idx) in indices.iter().enumerate() {
                let crop = &crops[idx];
                pack(
                    crop,
                    &mut input[batch * stride..][..stride],
                    resized_width(crop, height).min(width),
                    height,
                    width,
                    TEXT_MEAN,
                    TEXT_SCALE,
                );
            }
            let tensor = Tensor::from_array(([indices.len(), 3, height, width], input))?;
            let outputs = self.rec.run(ort::inputs![tensor])?;
            let (shape, scores) = outputs[0].try_extract_tensor::<f32>()?;
            ensure!(
                shape.len() == 3
                    && shape[0] as usize == indices.len()
                    && shape[1] > 0
                    && shape[2] as usize == self.dictionary.len() + 1,
                "识别模型与字典不匹配，输出维度为 {shape:?}"
            );
            let classes = shape[2] as usize;
            let sequence = shape[1] as usize * classes;
            for (batch, &idx) in indices.iter().enumerate() {
                let (text, score) = decode(
                    &scores[batch * sequence..][..sequence],
                    classes,
                    &self.dictionary,
                );
                if !text.is_empty() {
                    blocks.push((
                        idx,
                        TextBlock {
                            text,
                            score,
                            points: quads[idx],
                        },
                    ));
                }
            }
        }
        blocks.sort_by_key(|(idx, _)| *idx);
        Ok(blocks.into_iter().map(|(_, block)| block).collect())
    }
}

fn language_id(language: &str) -> Result<&str> {
    let language = language.strip_prefix("models/config_").unwrap_or(language);
    let language = language.strip_suffix(".txt").unwrap_or(language);
    LANGUAGES
        .iter()
        .find(|(id, _)| *id == language)
        .map(|(id, _)| *id)
        .ok_or_else(|| anyhow!("不支持的 OCR 语言 {language}"))
}

fn detector_file(language: &str) -> &'static str {
    if language == "v6_tiny" {
        "det_v6_tiny.onnx"
    } else {
        "det.onnx"
    }
}

fn load_recognizer(root: &Path, language: &str, threads: usize) -> Result<(Session, Vec<String>)> {
    let path = root.join(format!("dict_{language}.txt"));
    let contents =
        fs::read_to_string(&path).with_context(|| format!("读取字典 {}", path.display()))?;
    let mut dictionary = contents.lines().map(str::to_owned).collect::<Vec<_>>();
    dictionary.push(" ".to_owned());
    Ok((
        load_session(&root.join(format!("rec_{language}.onnx")), threads)?,
        dictionary,
    ))
}

fn load_session(path: &Path, threads: usize) -> Result<Session> {
    ensure!(path.is_file(), "缺少 OCR 模型 {}", path.display());
    let mut builder = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|err| anyhow!("{err}"))?
        .with_intra_threads(threads)
        .map_err(|err| anyhow!("{err}"))?
        .with_memory_pattern(false)
        .map_err(|err| anyhow!("{err}"))?
        .with_config_entry("session.intra_op.allow_spinning", "0")
        .map_err(|err| anyhow!("{err}"))?;
    builder
        .commit_from_file(path)
        .with_context(|| format!("加载模型 {}", path.display()))
}

fn decode(scores: &[f32], classes: usize, dictionary: &[String]) -> (String, f32) {
    let mut last = 0;
    let mut sum = 0.0;
    let mut count = 0;
    let mut text = String::new();
    for scores in scores.chunks_exact(classes) {
        let mut best = 0;
        for idx in 1..classes {
            if scores[idx] > scores[best] {
                best = idx;
            }
        }
        if best != 0 && best != last {
            text.push_str(&dictionary[best - 1]);
            sum += scores[best];
            count += 1;
        }
        last = best;
    }
    (text, if count == 0 { 0.0 } else { sum / count as f32 })
}

fn rounded_stride(side: u32, ratio: f32) -> usize {
    ((f32::trunc(side as f32 * ratio) / 32.0).round().max(1.0) * 32.0) as usize
}

fn resized_width(crop: &RgbImage, height: usize) -> usize {
    (height as f32 * crop.width() as f32 / crop.height() as f32).ceil() as usize
}

// Paddle inference uses BGR planes and OpenCV's half-pixel resize coordinates.
fn pack(
    image: &RgbImage,
    output: &mut [f32],
    width: usize,
    height: usize,
    stride: usize,
    mean: [f32; 3],
    scale: [f32; 3],
) {
    let image_width = image.width() as usize;
    let image_height = image.height() as usize;
    let x_scale = image_width as f32 / width as f32;
    let y_scale = image_height as f32 / height as f32;
    let xs = (0..width)
        .map(|x| bilinear_axis((x as f32 + 0.5) * x_scale - 0.5, image_width))
        .collect::<Vec<_>>();
    let plane = stride * height;
    let (blue, output) = output.split_at_mut(plane);
    let (green, red) = output.split_at_mut(plane);
    let pixels = image.as_raw();
    for y in 0..height {
        let (y0, y1, fy) = bilinear_axis((y as f32 + 0.5) * y_scale - 0.5, image_height);
        for (x, &(x0, x1, fx)) in xs.iter().enumerate() {
            let [r, g, b] = sample(pixels, image_width, x0, x1, y0, y1, fx, fy);
            let index = y * stride + x;
            blue[index] = (b as f32 / 255.0 - mean[0]) * scale[0];
            green[index] = (g as f32 / 255.0 - mean[1]) * scale[1];
            red[index] = (r as f32 / 255.0 - mean[2]) * scale[2];
        }
    }
}

fn bilinear_axis(pos: f32, limit: usize) -> (usize, usize, f32) {
    let pos = pos.clamp(0.0, (limit - 1) as f32);
    let base = pos as usize;
    (base, (base + 1).min(limit - 1), pos - base as f32)
}

#[inline]
fn sample(
    pixels: &[u8],
    width: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    fx: f32,
    fy: f32,
) -> [u8; 3] {
    let top_left = (y0 * width + x0) * 3;
    let top_right = (y0 * width + x1) * 3;
    let bottom_left = (y1 * width + x0) * 3;
    let bottom_right = (y1 * width + x1) * 3;
    std::array::from_fn(|channel| {
        let tl = pixels[top_left + channel] as f32;
        let tr = pixels[top_right + channel] as f32;
        let bl = pixels[bottom_left + channel] as f32;
        let br = pixels[bottom_right + channel] as f32;
        let top = tl + (tr - tl) * fx;
        let bottom = bl + (br - bl) * fx;
        (top + (bottom - top) * fy).round() as u8
    })
}

type Quad = [[i32; 2]; 4];
type Point = [f32; 2];

fn warp_crop(image: &RgbImage, quad: Quad) -> RgbImage {
    let [a, b, c, d] = quad.map(|p| p.map(|value| value as f32));
    let width = distance(a, b).max(distance(d, c)).max(1.0);
    let height = distance(a, d).max(distance(b, c)).max(1.0);
    // Use the original quad for mapping and bound temporary crop memory even
    // when a giant source image was first scaled down for detection.
    let scale = (4_194_304.0 / (width * height)).sqrt().min(1.0);
    let width = (width * scale).clamp(1.0, 8192.0) as u32;
    let height = (height * scale).clamp(1.0, 8192.0) as u32;
    let dx1 = b[0] - c[0];
    let dx2 = d[0] - c[0];
    let dy1 = b[1] - c[1];
    let dy2 = d[1] - c[1];
    let sx = a[0] - b[0] + c[0] - d[0];
    let sy = a[1] - b[1] + c[1] - d[1];
    let determinant = dx1 * dy2 - dx2 * dy1;
    let (g, h) = if determinant.abs() < 0.0001 {
        (0.0, 0.0)
    } else {
        (
            (sx * dy2 - dx2 * sy) / determinant,
            (dx1 * sy - sx * dy1) / determinant,
        )
    };
    let ux = b[0] - a[0] + g * b[0];
    let uy = b[1] - a[1] + g * b[1];
    let vx = d[0] - a[0] + h * d[0];
    let vy = d[1] - a[1] + h * d[1];
    let image_width = image.width() as usize;
    let image_height = image.height() as usize;
    let crop = RgbImage::from_fn(width, height, |x, y| {
        let u = x as f32 / width as f32;
        let v = y as f32 / height as f32;
        let denominator = g * u + h * v + 1.0;
        let (x0, x1, fx) = bilinear_axis((ux * u + vx * v + a[0]) / denominator, image_width);
        let (y0, y1, fy) = bilinear_axis((uy * u + vy * v + a[1]) / denominator, image_height);
        Rgb(sample(image.as_raw(), image_width, x0, x1, y0, y1, fx, fy))
    });
    if height as f32 >= width as f32 * 1.5 {
        imageops::rotate270(&crop)
    } else {
        crop
    }
}

fn distance(a: Point, b: Point) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

#[derive(Clone, Copy)]
struct DetectionSettings {
    pixel_threshold: f32,
    box_threshold: f32,
    unclip: f32,
    max_candidates: usize,
}

impl DetectionSettings {
    fn for_language(language: &str) -> Self {
        if language == "v6_tiny" {
            Self {
                pixel_threshold: 0.2,
                box_threshold: 0.4,
                unclip: 1.4,
                max_candidates: 3000,
            }
        } else {
            Self {
                pixel_threshold: 0.3,
                box_threshold: 0.6,
                unclip: 1.5,
                max_candidates: MAX_BOXES,
            }
        }
    }
}

fn find_boxes(
    map: &[f32],
    width: usize,
    height: usize,
    image_width: u32,
    image_height: u32,
    settings: DetectionSettings,
) -> Vec<Quad> {
    let mut bitmap: Vec<_> = map.iter().map(|&p| p > settings.pixel_threshold).collect();
    let mut pending = Vec::new();
    let mut boundary = Vec::new();
    let mut boxes = Vec::new();
    let mut candidates = 0;
    let scale_x = image_width as f32 / width as f32;
    let scale_y = image_height as f32 / height as f32;
    for start in 0..bitmap.len() {
        if !bitmap[start] {
            continue;
        }
        if candidates == settings.max_candidates {
            break;
        }
        candidates += 1;
        boundary.clear();
        bitmap[start] = false;
        pending.push(start);
        while let Some(index) = pending.pop() {
            let x = index % width;
            let y = index / width;
            let left = x.saturating_sub(1);
            let right = (x + 1).min(width - 1);
            let top = y.saturating_sub(1);
            let bottom = (y + 1).min(height - 1);
            if x == left
                || x == right
                || y == top
                || y == bottom
                || [index - 1, index + 1, index - width, index + width]
                    .iter()
                    .any(|&neighbor| map[neighbor] <= settings.pixel_threshold)
            {
                boundary.push([x as f32, y as f32]);
            }
            for ny in top..=bottom {
                for nx in left..=right {
                    let neighbor = ny * width + nx;
                    if bitmap[neighbor] {
                        bitmap[neighbor] = false;
                        pending.push(neighbor);
                    }
                }
            }
        }
        let Some(rect) = min_rect(&mut boundary) else {
            continue;
        };
        let (box_width, box_height) = rect.dimensions();
        if box_width.min(box_height) < 3.0
            || box_score(map, width, height, rect.quad(0.0)) < settings.box_threshold
        {
            continue;
        }
        let expansion =
            (box_width * box_height) * settings.unclip / (2.0 * (box_width + box_height));
        let quad = rect.quad(expansion).map(|[x, y]| {
            [
                ((x.round() * scale_x) as i32).clamp(0, image_width as i32 - 1),
                ((y.round() * scale_y) as i32).clamp(0, image_height as i32 - 1),
            ]
        });
        let quad_float = quad.map(|p| p.map(|v| v as f32));
        if distance(quad_float[0], quad_float[1]) >= 4.0
            && distance(quad_float[0], quad_float[3]) >= 4.0
        {
            boxes.push(quad);
        }
    }
    boxes
}

#[derive(Clone, Copy)]
struct RotatedRect {
    direction: Point,
    min_u: f32,
    max_u: f32,
    min_v: f32,
    max_v: f32,
}

impl RotatedRect {
    fn dimensions(self) -> (f32, f32) {
        (self.max_u - self.min_u, self.max_v - self.min_v)
    }

    fn quad(self, expansion: f32) -> [Point; 4] {
        let [ux, uy] = self.direction;
        let project = |u, v| [u * ux - v * uy, u * uy + v * ux];
        let mut points = [
            project(self.min_u - expansion, self.min_v - expansion),
            project(self.max_u + expansion, self.min_v - expansion),
            project(self.max_u + expansion, self.max_v + expansion),
            project(self.min_u - expansion, self.max_v + expansion),
        ];
        points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
        if points[0][1] > points[1][1] {
            points.swap(0, 1);
        }
        if points[2][1] > points[3][1] {
            points.swap(2, 3);
        }
        [points[0], points[2], points[3], points[1]]
    }
}

fn min_rect(points: &mut [Point]) -> Option<RotatedRect> {
    if points.len() < 4 {
        return None;
    }
    points.sort_unstable_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    let mut hull = Vec::new();
    for (step, iter) in [false, true].into_iter().enumerate() {
        let base = hull.len();
        for index in 0..points.len() {
            let point = points[if iter {
                points.len() - 1 - index
            } else {
                index
            }];
            while hull.len() >= base + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0
            {
                hull.pop();
            }
            hull.push(point);
        }
        if step == 0 {
            hull.pop();
        }
    }
    if hull.len() < 4 {
        return None;
    }
    let mut best = None;
    let mut best_area = f32::INFINITY;
    for edge in hull.windows(2) {
        let [a, b] = [edge[0], edge[1]];
        let length = distance(a, b);
        if length == 0.0 {
            continue;
        }
        let ux = (b[0] - a[0]) / length;
        let uy = (b[1] - a[1]) / length;
        let mut rect = RotatedRect {
            direction: [ux, uy],
            min_u: f32::INFINITY,
            max_u: f32::NEG_INFINITY,
            min_v: f32::INFINITY,
            max_v: f32::NEG_INFINITY,
        };
        for &[x, y] in &hull {
            let u = x * ux + y * uy;
            let v = -x * uy + y * ux;
            rect.min_u = rect.min_u.min(u);
            rect.max_u = rect.max_u.max(u);
            rect.min_v = rect.min_v.min(v);
            rect.max_v = rect.max_v.max(v);
        }
        let (w, h) = rect.dimensions();
        if w * h < best_area {
            best_area = w * h;
            best = Some(rect);
        }
    }
    best
}

fn cross(a: Point, b: Point, p: Point) -> f32 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn box_score(map: &[f32], width: usize, height: usize, quad: [Point; 4]) -> f32 {
    let x_min = quad.iter().map(|p| p[0].floor() as usize).min().unwrap();
    let y_min = quad.iter().map(|p| p[1].floor() as usize).min().unwrap();
    let x_max = quad
        .iter()
        .map(|p| p[0].ceil() as usize)
        .max()
        .unwrap()
        .min(width - 1);
    let y_max = quad
        .iter()
        .map(|p| p[1].ceil() as usize)
        .max()
        .unwrap()
        .min(height - 1);
    let mut sum = 0.0;
    let mut count = 0;
    for y in y_min..=y_max {
        for x in x_min..=x_max {
            let p = [x as f32, y as f32];
            if (0..4).all(|edge| cross(quad[edge], quad[(edge + 1) % 4], p) >= -0.001) {
                sum += map[y * width + x];
                count += 1;
            }
        }
    }
    if count == 0 { 0.0 } else { sum / count as f32 }
}
