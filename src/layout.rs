use crate::engine::TextBlock;
use serde::Serialize;

pub const MODES: &[(&str, &str)] = &[
    ("none", "原始文本框"),
    ("multi_para", "多栏 · 自然段"),
    ("multi_line", "多栏 · 按行"),
    ("multi_none", "多栏 · 连续"),
    ("single_para", "单栏 · 自然段"),
    ("single_line", "单栏 · 按行"),
    ("single_none", "单栏 · 连续"),
    ("single_code", "单栏 · 代码缩进"),
];

#[derive(Clone, Serialize)]
pub struct LayoutBlock {
    #[serde(flatten)]
    pub block: TextBlock,
    pub end: String,
}

#[derive(Clone, Copy)]
struct Bounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Bounds {
    fn of(points: &[[i32; 2]; 4]) -> Self {
        Self {
            left: points.iter().map(|p| p[0]).min().unwrap(),
            top: points.iter().map(|p| p[1]).min().unwrap(),
            right: points.iter().map(|p| p[0]).max().unwrap(),
            bottom: points.iter().map(|p| p[1]).max().unwrap(),
        }
    }

    fn height(self) -> f32 {
        (self.bottom as f32 - self.top as f32).max(1.0)
    }

    fn contains(self, other: Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }
}

pub fn process(blocks: Vec<TextBlock>, parser: &str, ignore: &[[[i32; 2]; 4]]) -> Vec<LayoutBlock> {
    let exclude: Vec<_> = ignore.iter().map(Bounds::of).collect();
    let mut blocks: Vec<_> = blocks
        .into_iter()
        .filter_map(|block| {
            let bounds = Bounds::of(&block.points);
            (!exclude.iter().any(|area| area.contains(bounds))).then_some((bounds, block))
        })
        .collect();
    if !MODES.iter().skip(1).any(|(mode, _)| *mode == parser) || blocks.is_empty() {
        return blocks
            .into_iter()
            .map(|(_, block)| LayoutBlock {
                block,
                end: "\n".into(),
            })
            .collect();
    }
    let mut heights: Vec<_> = blocks.iter().map(|(b, _)| b.height()).collect();
    heights.sort_by(f32::total_cmp);
    let height = heights[heights.len() / 2];
    let origin = blocks.iter().map(|(b, _)| b.left).min().unwrap();
    let mut columns = vec![Vec::new()];
    if parser.starts_with("multi_") {
        blocks.sort_by_key(|(b, _)| b.left);
        let mut right = i32::MIN;
        for item in blocks {
            let bounds = item.0;
            if bounds.left as f32 - right as f32 > height * 1.8
                && !columns.last().unwrap().is_empty()
            {
                columns.push(Vec::new());
            }
            columns.last_mut().unwrap().push(item);
            right = right.max(bounds.right);
        }
    } else {
        columns[0] = blocks;
    }

    let mut output = Vec::new();
    for mut column in columns {
        column.sort_by_key(|(b, _)| (b.top, b.left));
        let mut rows: Vec<Vec<(Bounds, TextBlock)>> = Vec::new();
        for item in column {
            let bounds = item.0;
            if let Some(row) = rows.last_mut() {
                let first = row[0].0;
                if (bounds.top as f32 - first.top as f32).abs()
                    < first.height().min(bounds.height()) * 0.65
                {
                    row.push(item);
                    continue;
                }
            }
            rows.push(vec![item]);
        }
        for row in &mut rows {
            row.sort_by_key(|(b, _)| b.left);
        }
        let starts: Vec<_> = rows.iter().map(|row| row[0].0).collect();
        for (index, row) in rows.into_iter().enumerate() {
            let end = match starts.get(index + 1) {
                None => "\n",
                Some(_) if parser.ends_with("_none") => " ",
                Some(next) if parser.ends_with("_para") => {
                    let here = starts[index];
                    if next.top as f32 - here.bottom as f32 > height * 0.75
                        || next.left as f32 - here.left as f32 > height * 0.9
                    {
                        "\n\n"
                    } else {
                        " "
                    }
                }
                Some(_) => "\n",
            };
            let mut prior = None::<Bounds>;
            let mut blocks = row.into_iter().peekable();
            while let Some((bounds, mut block)) = blocks.next() {
                if parser == "single_code" {
                    let previous = prior.map_or(origin, |b| b.right);
                    let spaces = (((bounds.left as f32 - previous as f32) / (height * 0.5)).round()
                        as usize)
                        .min(160);
                    block.text.insert_str(0, &" ".repeat(spaces));
                }
                let separator = match blocks.peek() {
                    None => end,
                    Some(_) if parser == "single_code" => "",
                    Some((next_bounds, next))
                        if (next_bounds.left as f32 - bounds.right as f32) < height * 1.5 =>
                    {
                        word_gap(&block.text, &next.text)
                    }
                    Some(_) => " ",
                };
                output.push(LayoutBlock {
                    block,
                    end: separator.into(),
                });
                prior = Some(bounds);
            }
        }
    }
    output
}

fn word_gap(left: &str, right: &str) -> &'static str {
    let cjk = |ch: char| matches!(ch as u32, 0x1100..=0x11ff | 0x3000..=0x30ff | 0x3130..=0x318f | 0x3400..=0x9fff | 0xac00..=0xd7af | 0xfe30..=0xfe4f | 0xff00..=0xffef);
    match (left.chars().next_back(), right.chars().next()) {
        (Some('-'), _) => "",
        (Some(left), Some(right))
            if cjk(left) && cjk(right)
                || matches!(
                    right,
                    ',' | '.'
                        | ':'
                        | ';'
                        | '!'
                        | '?'
                        | ')'
                        | ']'
                        | '}'
                        | '，'
                        | '。'
                        | '、'
                        | '：'
                        | '！'
                        | '？'
                ) =>
        {
            ""
        }
        _ => " ",
    }
}

pub fn text(blocks: &[LayoutBlock]) -> String {
    blocks
        .iter()
        .flat_map(|block| [block.block.text.as_str(), block.end.as_str()])
        .collect()
}
