//! 启发式段落聚类（Spike 1.6 规则的产品化）：垂直间距 + 首行缩进 + 条款编号。
//! 空文本行（印章/噪声）不参与聚类；块类型用启发式可判的 title/paragraph/other。

/// 聚类输入行（文本非空才参与）。
pub struct ClusterLine {
    pub bbox: [f32; 4],
    pub text: String,
}

/// 块类型（schema §5 的子集；table/image/list/header/footer 留给版面模型可选组件）。
/// `Other` 属 schema 词汇表（预留给印章/噪声块等，当前聚类不产生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Title,
    Paragraph,
    #[allow(dead_code)]
    Other,
}

impl BlockKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockKind::Title => "title",
            BlockKind::Paragraph => "paragraph",
            BlockKind::Other => "other",
        }
    }
}

/// 条款/章节编号开头：`1、` `1.` `一、` `一.` 等。
pub fn is_clause_head(text: &str) -> bool {
    let t = text.trim_start();
    let bytes = t.as_bytes();
    if !bytes.is_empty() && bytes[0].is_ascii_digit() {
        let rest = &t[1..];
        if rest.starts_with('、') || rest.starts_with('.') || rest.starts_with('．') {
            return true;
        }
    }
    let mut it = t.chars();
    match it.next() {
        Some(c) if "一二三四五六七八九十".contains(c) => {
            matches!(it.next(), Some('、') | Some('.'))
        }
        _ => false,
    }
}

/// 相邻行"新段"判定（任一满足即切块）：
/// (a) 垂直间距 > 0.85×行高中位数；(b) 首行缩进（x0 右移 > 0.8×行高）；(c) 条款编号开头。
/// 返回块列表（行索引数组，顺序即阅读顺序）。
pub fn cluster(lines: &[ClusterLine]) -> Vec<Vec<usize>> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut hs: Vec<f32> = lines.iter().map(|l| l.bbox[3] - l.bbox[1]).collect();
    hs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let h_med = hs[hs.len() / 2];
    let mut blocks: Vec<Vec<usize>> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let join = match blocks.last().and_then(|b| b.last()) {
            Some(&p) => {
                let prev = &lines[p];
                let gap = l.bbox[1] - prev.bbox[3];
                let indent_right = l.bbox[0] - prev.bbox[0];
                !(gap > 0.85 * h_med || indent_right > 0.8 * h_med || is_clause_head(&l.text))
            }
            None => false,
        };
        if join {
            blocks.last_mut().unwrap().push(i);
        } else {
            blocks.push(vec![i]);
        }
    }
    blocks
}

/// 块类型启发式：单行成块且疑似标题（条款头中文编号、或短行且无句末标点）→ title；
/// 其余 → paragraph。
pub fn classify_block(members: &[usize], lines: &[ClusterLine]) -> BlockKind {
    if members.len() == 1 {
        let t = lines[members[0]].text.trim();
        let short = t.chars().count() <= 20;
        let ends_plain = !t.ends_with(['。', '，', '；', '：', '、', '.', ',', ';', ':']);
        let cn_head = t
            .chars()
            .next()
            .is_some_and(|c| "一二三四五六七八九十".contains(c));
        if cn_head || (short && ends_plain) {
            return BlockKind::Title;
        }
    }
    BlockKind::Paragraph
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(y0: f32, x0: f32, text: &str) -> ClusterLine {
        ClusterLine {
            bbox: [x0, y0, x0 + 500.0, y0 + 40.0],
            text: text.to_string(),
        }
    }

    #[test]
    fn clause_head_detection() {
        assert!(is_clause_head("1、甲方负责"));
        assert!(is_clause_head("一、权利"));
        assert!(is_clause_head("3. 定义"));
        assert!(!is_clause_head("为加强商品房预售管理"));
        assert!(!is_clause_head("1,234,567.89 元"));
    }

    #[test]
    fn cluster_splits_on_gap_indent_and_clause() {
        // 行高 40（y0..y0+40），行距 50 → gap 10；段间距 100 → gap 60
        let lines = vec![
            line(0.0, 84.0, "甲方：某某"),              // 段0
            line(50.0, 84.0, "乙方：某某"),             // 段0
            line(150.0, 150.0, "为加强管理，根据《广"), // 首行缩进 → 新段1
            line(200.0, 84.0, "东省条例》……"),
            line(300.0, 84.0, "1、甲方负责贯彻实施"), // 条款编号 → 新段2
            line(350.0, 84.0, "预售款收存和使用。"),
        ];
        let blocks = cluster(&lines);
        assert_eq!(blocks, vec![vec![0, 1], vec![2, 3], vec![4, 5]]);
    }

    #[test]
    fn classify_title_and_paragraph() {
        let lines = vec![
            line(0.0, 84.0, "一、权利"),
            line(
                50.0,
                84.0,
                "1、甲方负责贯彻实施《条例》和《办法》有关规定，行使商品房",
            ),
            line(100.0, 84.0, "预售款收存和使用的日常监督管理的权利。"),
        ];
        assert_eq!(classify_block(&[0], &lines), BlockKind::Title);
        assert_eq!(classify_block(&[1, 2], &lines), BlockKind::Paragraph);
        // 单行的完整句子不是标题
        assert_eq!(classify_block(&[2], &lines), BlockKind::Paragraph);
    }
}
