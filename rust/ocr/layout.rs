//! Layout analysis for plain-text OCR ("版面分析").
//!
//! OCR lines with coordinates → blocks in reading order (columns, sections) →
//! visual rows → paragraphs → one paragraph per output line. Wrapped lines of
//! a paragraph are joined; headings, list items, short lines and separate
//! columns stay apart. Chinese and Japanese join without spaces.

use super::table::{Rect, TextLine};
use super::text::{is_unspaced_script, push_wrapped};

/// Paragraph text from positioned OCR lines, one paragraph per line.
pub(super) fn analyze(lines: &[TextLine]) -> String {
    let lines: Vec<&TextLine> = lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let height = typical_height(&lines);
    let mut paragraphs = Vec::new();
    for block in blocks(lines, height) {
        paragraphs.extend(block_paragraphs(&block, height));
    }
    paragraphs.join("\n")
}

/// Paragraphs from text without coordinates (cloud OCR): blank lines, list items,
/// short lines and sentence ends at a shorter-than-usual line end a paragraph.
pub fn paragraphs_from_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let widths: Vec<usize> = lines.iter().map(|line| display_width(line)).collect();
    let mut sorted: Vec<usize> = widths.iter().copied().filter(|&w| w > 0).collect();
    sorted.sort_unstable();
    // The usual full line length, robust to one long outlier.
    let full = sorted
        .get(sorted.len().saturating_sub(1) * 9 / 10)
        .copied()
        .unwrap_or(0);
    let mut output: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut open = false;
    for (index, &line) in lines.iter().enumerate() {
        if line.is_empty() {
            if open {
                output.push(std::mem::take(&mut current));
                open = false;
            }
            continue;
        }
        if open && (starts_list_item(line) || is_structural(line)) {
            output.push(std::mem::take(&mut current));
        }
        push_wrapped(&mut current, line);
        open = true;
        let width = widths[index];
        let ends = is_structural(line)
            || (lines.len() >= 3 && full > 0 && width * 10 < full * 6)
            || (ends_sentence(line) && full > 0 && width * 10 < full * 9);
        if ends {
            output.push(std::mem::take(&mut current));
            open = false;
        }
    }
    if open {
        output.push(current);
    }
    output.join("\n")
}

fn typical_height(lines: &[&TextLine]) -> f32 {
    let mut heights: Vec<f32> = lines
        .iter()
        .filter(|line| !is_vertical(line))
        .map(|line| line_height(line))
        .collect();
    if heights.is_empty() {
        heights = lines.iter().map(|line| width(&line.rect)).collect();
    }
    heights.sort_by(f32::total_cmp);
    heights[heights.len() / 2].max(1.0)
}

fn line_height(line: &TextLine) -> f32 {
    (line.rect.bottom - line.rect.top).max(1.0)
}

fn width(rect: &Rect) -> f32 {
    (rect.right - rect.left).max(1.0)
}

fn center_y(rect: &Rect) -> f32 {
    (rect.top + rect.bottom) / 2.0
}

/// Text written top to bottom (a narrow, tall box).
fn is_vertical(line: &TextLine) -> bool {
    line_height(line) > width(&line.rect) * 1.5 && line.text.trim().chars().count() > 1
}

// ---------------------------------------------------------------------------
// Blocks in reading order (recursive XY cut)
// ---------------------------------------------------------------------------

fn blocks(lines: Vec<&TextLine>, height: f32) -> Vec<Vec<&TextLine>> {
    let mut output = Vec::new();
    cut(lines, height, &mut output, 0);
    output
}

fn cut<'a>(lines: Vec<&'a TextLine>, height: f32, output: &mut Vec<Vec<&'a TextLine>>, depth: u32) {
    if lines.len() <= 1 || depth > 24 {
        if !lines.is_empty() {
            output.push(lines);
        }
        return;
    }
    // Columns first: a gutter read top to bottom on each side.
    if let Some(x) = gutter(&lines, height) {
        let (left, right): (Vec<_>, Vec<_>) = lines
            .into_iter()
            .partition(|line| (line.rect.left + line.rect.right) / 2.0 < x);
        cut(left, height, output, depth + 1);
        cut(right, height, output, depth + 1);
        return;
    }
    // Then sections separated by clear vertical space, or where a full-width
    // part (a title) meets a part laid out in columns.
    if let Some(y) = section_break(&lines, height) {
        let (upper, lower): (Vec<_>, Vec<_>) =
            lines.into_iter().partition(|line| center_y(&line.rect) < y);
        cut(upper, height, output, depth + 1);
        cut(lower, height, output, depth + 1);
        return;
    }
    output.push(lines);
}

/// Covered spans of the lines' projections onto one axis, merged.
fn spans(mut ranges: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// A vertical whitespace channel splitting the lines into two text columns.
/// Label/value pairs that share rows (a form) are not columns.
fn gutter(lines: &[&TextLine], height: f32) -> Option<f32> {
    let covered = spans(
        lines
            .iter()
            .map(|line| (line.rect.left, line.rect.right))
            .collect(),
    );
    let mut best: Option<(f32, f32)> = None;
    for pair in covered.windows(2) {
        let gap = pair[1].0 - pair[0].1;
        if gap >= height * 1.2 && best.is_none_or(|(_, size)| gap > size) {
            best = Some(((pair[0].1 + pair[1].0) / 2.0, gap));
        }
    }
    let (x, _) = best?;
    let (left, right): (Vec<&TextLine>, Vec<&TextLine>) = lines
        .iter()
        .copied()
        .partition(|line| (line.rect.left + line.rect.right) / 2.0 < x);
    if left.is_empty() || right.is_empty() {
        return None;
    }
    // One line on either side beside a block is a caption or a label: read
    // row by row unless both sides really are stacks of lines.
    if rows(&left, height).len() < 2 || rows(&right, height).len() < 2 {
        return (!shares_rows(&left, &right, height)).then_some(x);
    }
    let paired = left
        .iter()
        .filter(|a| right.iter().any(|b| same_row(a, b, height)))
        .count();
    let mut widths: Vec<f32> = left.iter().map(|line| width(&line.rect)).collect();
    widths.sort_by(f32::total_cmp);
    let short_labels = widths[widths.len() / 2] < height * 6.0;
    // Mostly row-aligned short labels with values beside them: a form, read
    // row by row. Two text columns may align too, but their lines are long.
    if paired * 10 >= left.len() * 6 && short_labels {
        return None;
    }
    Some(x)
}

fn shares_rows(a: &[&TextLine], b: &[&TextLine], height: f32) -> bool {
    a.iter().any(|x| b.iter().any(|y| same_row(x, y, height)))
}

fn same_row(a: &TextLine, b: &TextLine, height: f32) -> bool {
    let overlap = a.rect.bottom.min(b.rect.bottom) - a.rect.top.max(b.rect.top);
    overlap > line_height(a).min(line_height(b)) * 0.5
        || (center_y(&a.rect) - center_y(&b.rect)).abs() < height * 0.3
}

/// A horizontal cut: the widest clear band of at least one line height, or the
/// first gap below which (or above which) the lines form columns.
fn section_break(lines: &[&TextLine], height: f32) -> Option<f32> {
    let covered = spans(
        lines
            .iter()
            .map(|line| (line.rect.top, line.rect.bottom))
            .collect(),
    );
    if covered.len() < 2 {
        return None;
    }
    let gaps: Vec<(f32, f32)> = covered
        .windows(2)
        .map(|pair| ((pair[0].1 + pair[1].0) / 2.0, pair[1].0 - pair[0].1))
        .collect();
    if let Some(&(y, _)) = gaps
        .iter()
        .filter(|(_, size)| *size >= height * 1.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        return Some(y);
    }
    gaps.iter().map(|&(y, _)| y).find(|&y| {
        let (upper, lower): (Vec<&TextLine>, Vec<&TextLine>) = lines
            .iter()
            .copied()
            .partition(|line| center_y(&line.rect) < y);
        gutter(&lower, height).is_some() || gutter(&upper, height).is_some()
    })
}

// ---------------------------------------------------------------------------
// Rows and paragraphs inside one block
// ---------------------------------------------------------------------------

struct Row {
    text: String,
    rect: Rect,
    height: f32,
    /// Fragments far apart on one row (`姓名      张三`): a form or table row,
    /// never part of flowing text.
    tabular: bool,
}

fn rows<'a>(lines: &[&'a TextLine], height: f32) -> Vec<Vec<&'a TextLine>> {
    let mut ordered = lines.to_vec();
    ordered.sort_by(|a, b| center_y(&a.rect).total_cmp(&center_y(&b.rect)));
    let mut rows: Vec<Vec<&TextLine>> = Vec::new();
    for line in ordered {
        match rows.last_mut() {
            Some(row) if same_row(row[0], line, height) => row.push(line),
            _ => rows.push(vec![line]),
        }
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.rect.left.total_cmp(&b.rect.left));
    }
    rows
}

fn build_row(lines: &[&TextLine], height: f32) -> Row {
    let mut text = String::new();
    let mut previous: Option<&TextLine> = None;
    let mut tabular = false;
    for line in lines {
        // Keep the recognized line as it is, apart from repeated spaces.
        let fragment = line.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(before) = previous {
            let gap = line.rect.left - before.rect.right;
            tabular |= gap > height * 1.5;
            let joined = matches!(
                (text.chars().next_back(), fragment.chars().next()),
                (Some(last), Some(first))
                    if is_unspaced_script(last) && is_unspaced_script(first)
            ) && gap < height * 1.0;
            if !joined {
                text.push(' ');
            }
        }
        text.push_str(&fragment);
        previous = Some(line);
    }
    let rect = lines.iter().skip(1).fold(lines[0].rect, |acc, line| Rect {
        left: acc.left.min(line.rect.left),
        top: acc.top.min(line.rect.top),
        right: acc.right.max(line.rect.right),
        bottom: acc.bottom.max(line.rect.bottom),
    });
    let mut heights: Vec<f32> = lines
        .iter()
        .filter(|line| !is_vertical(line))
        .map(|line| line_height(line))
        .collect();
    heights.sort_by(f32::total_cmp);
    let row_height = heights
        .get(heights.len() / 2)
        .copied()
        .unwrap_or(rect.bottom - rect.top);
    Row {
        text,
        rect,
        height: row_height.max(1.0),
        tabular,
    }
}

fn block_paragraphs(block: &[&TextLine], height: f32) -> Vec<String> {
    let rows: Vec<Row> = rows(block, height)
        .iter()
        .map(|row| build_row(row, height))
        .collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let left = rows
        .iter()
        .map(|row| row.rect.left)
        .fold(f32::INFINITY, f32::min);
    let right = rows
        .iter()
        .map(|row| row.rect.right)
        .fold(f32::NEG_INFINITY, f32::max);
    let block_width = (right - left).max(1.0);
    // A block a few characters wide (a menu, a list of labels) holds separate
    // items, not a paragraph wrapped that narrowly.
    let narrow = rows.len() > 1 && block_width < height * 6.0;
    // The block's ordinary line spacing.
    let mut gaps: Vec<f32> = rows
        .windows(2)
        .map(|pair| pair[1].rect.top - pair[0].rect.bottom)
        .collect();
    gaps.sort_by(f32::total_cmp);
    let leading = gaps.get(gaps.len() / 2).copied().unwrap_or(0.0).max(0.0);

    let mut paragraphs = Vec::new();
    let mut current = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            let previous = &rows[index - 1];
            let size = previous.height.max(row.height);
            let gap = row.rect.top - previous.rect.bottom;
            let spaced = gap > (leading * 1.6).max(leading + size * 0.5);
            let resized = previous.height.max(row.height) > previous.height.min(row.height) * 1.3;
            // A line ending well before the block's right edge ends its paragraph.
            let short = right - previous.rect.right > (size * 2.0).max(block_width * 0.15);
            // First-line indentation starts one.
            let indented =
                row.rect.left - left > size * 1.2 && previous.rect.left - left < size * 0.6;
            if narrow
                || spaced
                || resized
                || short
                || indented
                || previous.tabular
                || row.tabular
                || starts_list_item(&row.text)
            {
                paragraphs.push(std::mem::take(&mut current));
            }
        }
        push_wrapped(&mut current, &row.text);
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs.retain(|paragraph| !paragraph.is_empty());
    paragraphs
}

// ---------------------------------------------------------------------------
// Text cues
// ---------------------------------------------------------------------------

/// `•`, `-`, `1.`, `(2)`, `③`, `一、`, `a)`, `第三条` and similar list markers.
fn starts_list_item(text: &str) -> bool {
    let text = text.trim_start();
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if matches!(
        first,
        '•' | '·' | '●' | '○' | '■' | '□' | '◆' | '▪' | '►' | '▶' | '✓' | '✔' | '※'
    ) || ('\u{2460}'..='\u{24FF}').contains(&first)
        || ('\u{2776}'..='\u{2793}').contains(&first)
        || ('\u{3220}'..='\u{3243}').contains(&first)
    {
        return true;
    }
    if matches!(first, '-' | '*' | '+') {
        return chars.next().is_some_and(char::is_whitespace);
    }
    if text.starts_with('第') {
        let rest: String = text.chars().skip(1).take(6).collect();
        if let Some(end) = rest.find(['条', '章', '节', '款']) {
            return rest[..end]
                .chars()
                .all(|c| c.is_ascii_digit() || is_chinese_numeral(c))
                && end > 0;
        }
    }
    // Numbered: 1. 1) 1、 (1) （一） 一、 a) A.
    let body = text.trim_start_matches(['(', '（', '[', '【']);
    let marker: String = body
        .chars()
        .take_while(|&c| c.is_ascii_digit() || is_chinese_numeral(c))
        .collect();
    let count = marker.chars().count();
    let (marker_len, after) = if count > 0 && count <= 3 {
        (marker.len(), body[marker.len()..].chars().next())
    } else {
        let mut letters = body.chars();
        match (letters.next(), letters.next()) {
            (Some(letter), Some(next))
                if letter.is_ascii_alphabetic() && matches!(next, ')' | '.') =>
            {
                // `a)` or `A.` followed by a space; not words like `e.g.`.
                return body.chars().nth(2).is_some_and(char::is_whitespace);
            }
            _ => return false,
        }
    };
    let bracketed = body.len() != text.len();
    match after {
        Some(')' | '）' | ']' | '】') => bracketed || marker.chars().all(|c| c.is_ascii_digit()),
        Some('、') => true,
        Some('.' | '．') => {
            // `1. Item`, not a decimal `1.5`.
            !body[marker_len..]
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_digit())
                && marker.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

fn is_chinese_numeral(c: char) -> bool {
    matches!(
        c,
        '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十' | '百' | '零'
    )
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().chars().next_back().is_some_and(|c| {
        matches!(
            c,
            '。' | '！'
                | '？'
                | '.'
                | '!'
                | '?'
                | '…'
                | '：'
                | ':'
                | '；'
                | ';'
                | '」'
                | '』'
                | '”'
                | '"'
        )
    })
}

/// Markdown-like structure from model OCR: headings, tables, code fences.
fn is_structural(line: &str) -> bool {
    line.starts_with('#')
        || line.starts_with('|')
        || line.starts_with("```")
        || line.starts_with('>')
}

/// Width in "Latin character" units: CJK characters count double.
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| if is_unspaced_script(c) { 2 } else { 1 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, left: f32, top: f32, right: f32, bottom: f32) -> TextLine {
        TextLine {
            text: text.into(),
            rect: Rect {
                left,
                top,
                right,
                bottom,
            },
        }
    }

    /// Lines of `height` 20 and spacing 28, starting at `top`.
    fn stack(texts: &[(&str, f32, f32)], top: f32) -> Vec<TextLine> {
        texts
            .iter()
            .enumerate()
            .map(|(i, &(text, left, right))| {
                let y = top + i as f32 * 28.0;
                line(text, left, y, right, y + 20.0)
            })
            .collect()
    }

    #[test]
    fn wrapped_lines_merge_and_short_last_lines_end_paragraphs() {
        let mut lines = stack(
            &[
                ("版面分析会把同一段落中因宽度换行的多行文字", 10.0, 600.0),
                (
                    "合并为一行，中文之间不插入空格，直到段落结束。",
                    10.0,
                    598.0,
                ),
                ("这是第一段的最后一行。", 10.0, 230.0),
                ("第二段从这里开始，同样会被合并成一个完整的段", 10.0, 600.0),
                ("落。", 10.0, 50.0),
            ],
            10.0,
        );
        lines.extend(stack(
            &[("Layout analysis also joins English", 10.0, 590.0)],
            200.0,
        ));
        lines.extend(stack(&[("lines with a space.", 10.0, 260.0)], 228.0));
        assert_eq!(
            analyze(&lines),
            "版面分析会把同一段落中因宽度换行的多行文字合并为一行，中文之间不插入空格，直到段落结束。这是第一段的最后一行。\n\
             第二段从这里开始，同样会被合并成一个完整的段落。\n\
             Layout analysis also joins English lines with a space."
        );
    }

    #[test]
    fn indentation_headings_and_lists_start_new_paragraphs() {
        let mut lines = vec![line("第一章 总则", 200.0, 0.0, 420.0, 32.0)];
        lines.extend(stack(
            &[
                (
                    "    首行缩进表示新段落的开始，这一行很长很长很长",
                    50.0,
                    600.0,
                ),
                (
                    "这一行仍属于同一段落，并且一直写到右侧边缘为止",
                    10.0,
                    600.0,
                ),
                ("第二段同样首行缩进，所以单独成为一段文字内容", 50.0, 600.0),
                ("继续写满整行宽度的文字内容继续写满整行宽度的", 10.0, 600.0),
                ("1. 列表第一项写满整行宽度的文字内容继续写满整", 10.0, 600.0),
                ("2. 列表第二项", 10.0, 200.0),
            ],
            50.0,
        ));
        let text = analyze(&lines);
        let paragraphs: Vec<&str> = text.lines().collect();
        assert_eq!(paragraphs.len(), 5, "{text}");
        assert_eq!(paragraphs[0], "第一章 总则");
        assert!(paragraphs[1].ends_with("一直写到右侧边缘为止"), "{text}");
        assert!(paragraphs[3].starts_with("1. 列表第一项"), "{text}");
        assert_eq!(paragraphs[4], "2. 列表第二项");
    }

    #[test]
    fn two_columns_read_left_then_right_below_a_full_width_title() {
        let mut lines = vec![line(
            "Two Column Article Title Spanning Both",
            10.0,
            0.0,
            700.0,
            26.0,
        )];
        lines.extend(stack(
            &[
                ("Left column first line of", 10.0, 330.0),
                ("text continues here and", 10.0, 330.0),
                ("ends.", 10.0, 80.0),
            ],
            40.0,
        ));
        lines.extend(stack(
            &[
                ("Right column starts at the", 380.0, 700.0),
                ("top and wraps to a second", 380.0, 700.0),
                ("line.", 380.0, 440.0),
            ],
            40.0,
        ));
        assert_eq!(
            analyze(&lines),
            "Two Column Article Title Spanning Both\n\
             Left column first line of text continues here and ends.\n\
             Right column starts at the top and wraps to a second line."
        );
    }

    #[test]
    fn form_labels_and_values_stay_on_their_rows() {
        let lines = [
            line("姓名", 10.0, 0.0, 50.0, 20.0),
            line("张三", 200.0, 0.0, 240.0, 20.0),
            line("电话", 10.0, 40.0, 50.0, 60.0),
            line("123456", 200.0, 40.0, 260.0, 60.0),
            line("地址", 10.0, 80.0, 50.0, 100.0),
            line("北京市", 200.0, 80.0, 260.0, 100.0),
        ];
        assert_eq!(analyze(&lines), "姓名 张三\n电话 123456\n地址 北京市");
    }

    #[test]
    fn long_form_rows_and_menu_items_stay_separate() {
        let lines = [
            line("地址", 10.0, 0.0, 50.0, 20.0),
            line("北京市海淀区中关村大街1号", 200.0, 0.0, 460.0, 20.0),
            line("邮箱", 10.0, 40.0, 50.0, 60.0),
            line("zhangsan@example.com", 200.0, 40.0, 420.0, 60.0),
        ];
        assert_eq!(
            analyze(&lines),
            "地址 北京市海淀区中关村大街1号\n邮箱 zhangsan@example.com"
        );
        let menu = stack(
            &[
                ("首页", 10.0, 50.0),
                ("文档", 10.0, 50.0),
                ("下载", 10.0, 50.0),
            ],
            0.0,
        );
        assert_eq!(analyze(&menu), "首页\n文档\n下载");
    }

    #[test]
    fn split_fragments_of_one_line_rejoin() {
        let lines = [
            line("识别结果", 10.0, 0.0, 90.0, 20.0),
            line("被拆成两段", 94.0, 0.0, 190.0, 20.0),
            line("Hello", 10.0, 40.0, 60.0, 60.0),
            line("world", 66.0, 40.0, 120.0, 60.0),
        ];
        assert_eq!(analyze(&lines), "识别结果被拆成两段\nHello world");
    }

    #[test]
    fn list_markers_are_recognized_without_catching_numbers() {
        for marker in [
            "1. 第一项",
            "2) item",
            "(3) 第三",
            "（四）条款",
            "一、总则",
            "• 要点",
            "- dash",
            "③ 圆圈",
            "a) option",
            "第十二条 规定",
        ] {
            assert!(starts_list_item(marker), "{marker}");
        }
        for text in ["1.5 倍行距", "2024年", "e.g. this", "-5 度", "第一次见面"] {
            assert!(!starts_list_item(text), "{text}");
        }
    }

    #[test]
    fn text_only_fallback_keeps_paragraph_ends() {
        let text = "版面分析会把同一段落中因宽度换行的多行文字\n合并为一行，直到段落结束为止的最后一行文字。\n短句结束。\n## 标题\n- 列表项一\n- 列表项二\n\n空行之后的新段落也会单独成行文字内容";
        assert_eq!(
            paragraphs_from_text(text),
            "版面分析会把同一段落中因宽度换行的多行文字合并为一行，直到段落结束为止的最后一行文字。短句结束。\n## 标题\n- 列表项一\n- 列表项二\n空行之后的新段落也会单独成行文字内容"
        );
        assert_eq!(
            paragraphs_from_text(
                "English lines that wrap are\njoined back with a space until\nthe paragraph ends.\nA new one follows here."
            ),
            "English lines that wrap are joined back with a space until the paragraph ends.\nA new one follows here."
        );
    }
}
