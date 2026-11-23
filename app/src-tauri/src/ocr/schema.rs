//! OCR 输出 JSON Schema：坐标 0–1000 归一化，
//! text_flow/flow_map 的构建与反向定位。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrJson {
    pub version: u32,
    pub engine: String,
    pub lang: String,
    pub page_count: usize,
    pub pages: Vec<PageJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageJson {
    pub page_idx: usize,
    /// 渲染页像素尺寸（坐标归一化的基准）
    pub width: u32,
    pub height: u32,
    /// doc_ori 施加的扶正角度（0/90/180/270）
    pub rotation: u32,
    /// deskew 施加的矫正角（度；未触发矫正为 0.0）
    pub deskew_angle: f32,
    pub blocks: Vec<BlockJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockJson {
    #[serde(rename = "type")]
    pub kind: String,
    pub bbox: [i32; 4],
    /// 块内行文本按阅读顺序直接拼接（中文无分隔符）
    pub text_flow: String,
    /// 跨行拼接边界映射：{text_flow 偏移(字符), 块内行索引, 行内字符起点, 长度}
    pub flow_map: Vec<FlowMapEntry>,
    pub lines: Vec<LineJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlowMapEntry {
    pub offset: usize,
    pub line: usize,
    pub char_start: usize,
    pub char_len: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineJson {
    pub text: String,
    pub bbox: [i32; 4],
    pub chars: Vec<CharJson>,
    /// 命中可疑金额模式时标注（不改正文）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_suspect: Option<bool>,
    /// 行级 rec 平均置信度（旧版产物可能缺失，反序列化为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharJson {
    pub c: String,
    pub bbox: [i32; 4],
}

/// 像素 bbox → 0–1000 归一化（四舍五入，裁剪到界内）。
pub fn norm_bbox(b: [f32; 4], width: u32, height: u32) -> [i32; 4] {
    let (w, h) = (width as f32, height as f32);
    let nx = |v: f32| ((v / w) * 1000.0).round().clamp(0.0, 1000.0) as i32;
    let ny = |v: f32| ((v / h) * 1000.0).round().clamp(0.0, 1000.0) as i32;
    [nx(b[0]), ny(b[1]), nx(b[2]), ny(b[3])]
}

/// 构建块 text_flow（行文本直接拼接）与 flow_map（仅跨行边界：
/// 第 k 行（k≥1）在 text_flow 中的起始偏移 + 行内区间 [0, len)）。
/// 偏移与区间单位均为字符数。
pub fn build_text_flow(lines: &[String]) -> (String, Vec<FlowMapEntry>) {
    let mut flow = String::new();
    let mut map = Vec::new();
    let mut offset = 0usize;
    for (i, text) in lines.iter().enumerate() {
        let len = text.chars().count();
        if i > 0 {
            map.push(FlowMapEntry {
                offset,
                line: i,
                char_start: 0,
                char_len: len,
            });
        }
        flow.push_str(text);
        offset += len;
    }
    (flow, map)
}

/// 由 flow_map 反查 text_flow 偏移（字符）对应的 (块内行索引, 行内字符偏移)。
/// 行 0 隐式覆盖 [0, 首条 entry.offset)；越界返回 None。
/// （Phase 4.4 quote 命中坐标回写的核心查询。）
pub fn locate(
    flow_map: &[FlowMapEntry],
    total_len: usize,
    offset: usize,
) -> Option<(usize, usize)> {
    if offset >= total_len {
        return None;
    }
    let line0_len = flow_map.first().map(|e| e.offset).unwrap_or(total_len);
    if offset < line0_len {
        return Some((0, offset));
    }
    for e in flow_map {
        if offset >= e.offset && offset < e.offset + e.char_len {
            return Some((e.line, e.char_start + (offset - e.offset)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_bbox_scales_and_clamps() {
        assert_eq!(
            norm_bbox([0.0, 0.0, 1654.0, 2339.0], 1654, 2339),
            [0, 0, 1000, 1000]
        );
        assert_eq!(
            norm_bbox([827.0, 1169.5, 100.0, 200.0], 1654, 2339),
            [500, 500, 60, 86]
        );
        // 越界裁剪
        assert_eq!(
            norm_bbox([-10.0, -5.0, 2000.0, 3000.0], 1654, 2339),
            [0, 0, 1000, 1000]
        );
    }

    #[test]
    fn text_flow_concat_and_flow_map_boundaries() {
        let lines = vec![
            "……本合".to_string(),
            "同金额：1,234,567.89 元".to_string(),
            "整。".to_string(),
        ];
        let (flow, map) = build_text_flow(&lines);
        assert_eq!(flow, "……本合同金额：1,234,567.89 元整。");
        assert_eq!(
            map,
            vec![
                FlowMapEntry {
                    offset: 4,
                    line: 1,
                    char_start: 0,
                    char_len: 18
                },
                FlowMapEntry {
                    offset: 22,
                    line: 2,
                    char_start: 0,
                    char_len: 2
                },
            ]
        );
        // 边界映射一致性：offset → (行, 字符) 反查
        let total = flow.chars().count();
        assert_eq!(total, 24);
        assert_eq!(locate(&map, total, 0), Some((0, 0)));
        assert_eq!(locate(&map, total, 3), Some((0, 3)));
        // 跨行边界：offset 4 = 行 1 的"同"
        assert_eq!(locate(&map, total, 4), Some((1, 0)));
        assert_eq!(locate(&map, total, 18), Some((1, 14)));
        assert_eq!(locate(&map, total, 21), Some((1, 17)));
        assert_eq!(locate(&map, total, 22), Some((2, 0)));
        assert_eq!(locate(&map, total, 23), Some((2, 1)));
        assert_eq!(locate(&map, total, 24), None);
        // 用 map 还原每行文本
        let chars: Vec<char> = flow.chars().collect();
        let mut bounds: Vec<(usize, usize)> = vec![(0, map[0].offset)];
        for (i, e) in map.iter().enumerate() {
            let end = map.get(i + 1).map(|n| n.offset).unwrap_or(total);
            bounds.push((e.offset, end));
        }
        for (i, text) in lines.iter().enumerate() {
            let (s, e) = bounds[i];
            assert_eq!(chars[s..e].iter().collect::<String>(), *text);
        }
    }

    #[test]
    fn locate_single_line_block() {
        let (flow, map) = build_text_flow(&["单行".to_string()]);
        assert!(map.is_empty());
        assert_eq!(locate(&map, flow.chars().count(), 1), Some((0, 1)));
    }
}
