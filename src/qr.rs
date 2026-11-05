use anyhow::{Result, bail, ensure};
use image::{DynamicImage, GrayImage, Luma};
use rxing::{
    BarcodeFormat, EncodeHints, MultiFormatWriter, RXingResultMetadataType,
    RXingResultMetadataValue, Writer, helpers,
};
use serde::Serialize;

#[derive(Serialize)]
pub struct Barcode {
    pub text: String,
    pub format: String,
    pub points: Vec<[i32; 2]>,
    pub orientation: i32,
}

pub fn scan(image: &DynamicImage) -> Result<Vec<Barcode>> {
    let codes = match helpers::detect_multiple_in_image(image.clone()) {
        Ok(codes) => codes,
        Err(rxing::Exceptions::NotFoundException(_)) => {
            match helpers::detect_in_image_filtered(image.clone(), None) {
                Ok(code) => vec![code],
                Err(rxing::Exceptions::NotFoundException(_)) => Vec::new(),
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    Ok(codes
        .into_iter()
        .map(|code| {
            let format = *code.getBarcodeFormat();
            let points: Vec<_> = code
                .getPoints()
                .iter()
                .map(|p| [p.x.round() as i32, p.y.round() as i32])
                .collect();
            let orientation = match code
                .getRXingResultMetadata()
                .get(&RXingResultMetadataType::ORIENTATION)
            {
                Some(RXingResultMetadataValue::Orientation(degrees)) => *degrees,
                _ => match (format, points.as_slice()) {
                    (BarcodeFormat::QR_CODE, [_, a, b, ..]) => {
                        (f64::from(b[1] - a[1])
                            .atan2(f64::from(b[0] - a[0]))
                            .to_degrees()
                            / 90.0)
                            .round() as i32
                            * 90
                    }
                    _ => 0,
                },
            };
            Barcode {
                text: code.getText().into(),
                format: format_name(format),
                points,
                orientation,
            }
        })
        .collect())
}

pub fn generate(text: &str) -> Result<DynamicImage> {
    generate_with(text, "QRCode", [400, 400], -1, -1)
}

pub fn generate_with(
    text: &str,
    format: &str,
    size: [u32; 2],
    margin: i32,
    correction: i32,
) -> Result<DynamicImage> {
    ensure!(!text.is_empty(), "请输入二维码内容");
    ensure!(
        size.into_iter().all(|pixels| pixels <= 8192),
        "图像尺寸须为 0–8192"
    );
    ensure!((-1..=64).contains(&margin), "quiet_zone 须为 -1–64");
    let format = BarcodeFormat::from(format);
    let error = match (format, correction) {
        (_, -1) => None,
        (BarcodeFormat::QR_CODE, 0..=3) => Some(["M", "L", "H", "Q"][correction as usize].into()),
        (BarcodeFormat::AZTEC, 0..=3) => Some([15, 7, 30, 25][correction as usize].to_string()),
        (BarcodeFormat::PDF_417, 0..=8) => Some(correction.to_string()),
        (BarcodeFormat::QR_CODE | BarcodeFormat::AZTEC | BarcodeFormat::PDF_417, _) => {
            bail!("无效的纠错级别")
        }
        _ => None,
    };
    let hints = EncodeHints {
        CharacterSet: Some("UTF-8".into()),
        ErrorCorrection: error,
        Margin: (margin >= 0).then(|| margin.to_string()),
        ..Default::default()
    };
    let matrix = MultiFormatWriter.encode_with_hints(
        text,
        &format,
        size[0] as i32,
        size[1] as i32,
        &hints,
    )?;
    Ok(
        GrayImage::from_fn(matrix.getWidth(), matrix.getHeight(), |x, y| {
            Luma([if matrix.get(x, y) { 0 } else { 255 }])
        })
        .into(),
    )
}

fn format_name(format: BarcodeFormat) -> String {
    match format {
        BarcodeFormat::AZTEC => "Aztec",
        BarcodeFormat::CODABAR => "Codabar",
        BarcodeFormat::CODE_39 => "Code39",
        BarcodeFormat::CODE_93 => "Code93",
        BarcodeFormat::CODE_128 => "Code128",
        BarcodeFormat::DATA_MATRIX => "DataMatrix",
        BarcodeFormat::EAN_8 => "EAN8",
        BarcodeFormat::EAN_13 => "EAN13",
        BarcodeFormat::ITF => "ITF",
        BarcodeFormat::MAXICODE => "MaxiCode",
        BarcodeFormat::PDF_417 => "PDF417",
        BarcodeFormat::QR_CODE => "QRCode",
        BarcodeFormat::MICRO_QR_CODE => "MicroQRCode",
        BarcodeFormat::RECTANGULAR_MICRO_QR_CODE => "RMQRCode",
        BarcodeFormat::RSS_14 => "DataBar",
        BarcodeFormat::RSS_EXPANDED => "DataBarExpanded",
        BarcodeFormat::UPC_A => "UPCA",
        BarcodeFormat::UPC_E => "UPCE",
        _ => return format.to_string(),
    }
    .to_owned()
}
