//! Table reconstruction combining local image contrast and text geometry.
//! This deliberately has no OpenCV, NumPy or clustering runtime dependency.

use super::BoundingBox;
use anyhow::{ensure, Result};
use image::RgbaImage;

mod keystone;
mod rules;
#[cfg(all(test, feature = "private-table-samples"))]
mod samples;
mod skew;
use rules::{Rule, RuleSet};
#[cfg(test)]
pub(super) use skew::Quad;
pub(super) use skew::{level_boxes, straighten};

const MAX_TABLE_CELLS: usize = 262_144;

#[derive(Clone, Copy, Debug)]
pub(super) struct Rect {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) right: f32,
    pub(super) bottom: f32,
}

impl Rect {
    pub(super) fn from_box(bounds: BoundingBox) -> Result<Self> {
        let points = [
            bounds.top_left,
            bounds.top_right,
            bounds.bottom_right,
            bounds.bottom_left,
        ];
        ensure!(
            points.iter().all(|p| p.x.is_finite() && p.y.is_finite()),
            "OCR 文本边界框包含无效坐标"
        );
        Ok(Self {
            left: points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min),
            top: points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min),
            right: points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max),
            bottom: points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max),
        })
    }

    fn height(self) -> f32 {
        (self.bottom - self.top).max(1.0)
    }
    fn center_y(self) -> f32 {
        (self.top + self.bottom) / 2.0
    }
}

pub(super) struct TextLine {
    pub text: String,
    pub rect: Rect,
}

pub(super) fn reconstruct(lines: &[TextLine], image: &RgbaImage) -> String {
    reconstruct_pass(lines, image, true)
}

fn reconstruct_pass(lines: &[TextLine], image: &RgbaImage, correct_keystone: bool) -> String {
    let mut ordered: Vec<_> = lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .collect();
    drop_duplicate_lines(&mut ordered);
    if ordered.is_empty() {
        return String::new();
    }
    ordered.sort_by(|a, b| {
        a.rect
            .center_y()
            .total_cmp(&b.rect.center_y())
            .then(a.rect.left.total_cmp(&b.rect.left))
    });
    let mut heights: Vec<_> = ordered.iter().map(|line| line.rect.height()).collect();
    heights.sort_by(f32::total_cmp);
    let height = heights[heights.len() / 2];
    let mut bounds = Rect {
        left: ordered
            .iter()
            .map(|l| l.rect.left)
            .fold(f32::INFINITY, f32::min),
        top: ordered
            .iter()
            .map(|l| l.rect.top)
            .fold(f32::INFINITY, f32::min),
        right: ordered
            .iter()
            .map(|l| l.rect.right)
            .fold(f32::NEG_INFINITY, f32::max),
        bottom: ordered
            .iter()
            .map(|l| l.rect.bottom)
            .fold(f32::NEG_INFINITY, f32::max),
    };
    let mut rules = rules::detect(image, &ordered, bounds, height);
    // A table photographed at an angle has leaning side borders; make them
    // vertical first, then measure everything again.
    if correct_keystone {
        if let Some(keystone) = keystone::Keystone::measure(&rules, height) {
            let corrected: Vec<TextLine> = lines.iter().map(|line| keystone.line(line)).collect();
            return reconstruct_pass(&corrected, &keystone.apply(image), false);
        }
    }
    let mut above = Vec::new();
    let mut below = Vec::new();
    let diagonal_header;
    if let Some(frame) = enclosing_frame(&rules, bounds, &ordered, height) {
        // A diagonally split corner cell (斜线表头) holds scattered labels whose
        // gaps must not become columns: read them as one cell.
        if let Some(header) = diagonal_header_cell(image, &rules, frame, &ordered, height) {
            ordered.retain(|line| {
                !header
                    .pieces
                    .iter()
                    .any(|piece| std::ptr::eq(*piece, *line))
            });
            diagonal_header = header.line;
            ordered.push(&diagonal_header);
            ordered.sort_by(|a, b| {
                a.rect
                    .center_y()
                    .total_cmp(&b.rect.center_y())
                    .then(a.rect.left.total_cmp(&b.rect.left))
            });
        }
        // Titles above and notes below a closed table become their own lines
        // instead of joining its first or last row; a page number is dropped.
        for &line in &ordered {
            if inside(frame, line) || is_page_number(&line.text) {
                continue;
            }
            if line.rect.center_y() < frame.top {
                above.push(line);
            } else if line.rect.center_y() > frame.bottom {
                below.push(line);
            }
        }
        ordered.retain(|line| inside(frame, line));
        if ordered.is_empty() {
            return String::new();
        }
        bounds.top = ordered
            .iter()
            .map(|line| line.rect.top)
            .fold(f32::INFINITY, f32::min);
        bounds.bottom = ordered
            .iter()
            .map(|line| line.rect.bottom)
            .fold(f32::NEG_INFINITY, f32::max);
        // The frame's own sides are borders, not column separators, even when
        // text in a skewed photo pokes past them.
        let margin = (height * 0.3).max(3.0) + 3.0;
        bounds.left = bounds.left.max(frame.left + margin);
        bounds.right = bounds.right.min(frame.right - margin);
    }
    let text_rows = group_text_rows(&ordered, height);
    let column_rows = with_vertical_labels(&text_rows, height);
    let physical_columns = positions(&rules.vertical, bounds.left, bounds.right);
    // Infer missing separators even when some physical borders were found.
    // Use text baselines here; sorting a multiline cell by x creates false gaps.
    let mut columns = infer_columns(&column_rows, height, &physical_columns);
    vertical_label_columns(&ordered, &mut columns, height);
    let mut cuts = row_boundaries(&ordered, &columns, &rules, bounds, height);
    if (cuts.len() + 1).saturating_mul(columns.len() + 1) > MAX_TABLE_CELLS {
        // A textured image can generate thousands of plausible rules. Fall
        // back to the at-most-1000 OCR lines before allocating a dense grid.
        columns = infer_columns(&column_rows, height, &[]);
        cuts = text_boundaries(&text_rows);
        rules = RuleSet::default();
    }
    let mut grid = vec![vec![String::new(); columns.len() + 1]; cuts.len() + 1];
    let active_columns = column_activity(&ordered, &columns, &cuts, &rules, bounds, height);
    ordered.sort_by(|a, b| {
        a.rect
            .top
            .total_cmp(&b.rect.top)
            .then(a.rect.left.total_cmp(&b.rect.left))
    });
    for line in ordered {
        let mut row = cuts.partition_point(|cut| cut.position < line.rect.center_y());
        let x = (line.rect.left + line.rect.right) / 2.0;
        // A partial rule separates only the cells it actually crosses. Walk
        // back through absent boundaries to anchor vertically merged content.
        while row > 0
            && !cuts[row - 1].inferred
            && !covers(&rules.horizontal, cuts[row - 1].position, x)
        {
            row -= 1;
        }
        let mut col = columns.partition_point(|&x| x <= line.rect.left);
        while col > 0 && !active_columns[row][col - 1] {
            col -= 1;
        }
        // Embedded tabs/newlines must not manufacture extra TSV cells, and wrapped
        // Chinese/Japanese lines must not gain a space at the wrap.
        super::text::push_wrapped(&mut grid[row][col], &super::text::tidy_line(&line.text));
    }
    // Preserve empty interior columns indicated by physical borders. Removing
    // them shifts every following value left when pasted into a spreadsheet.
    let populated: Vec<_> = (0..columns.len() + 1)
        .filter(|&c| grid.iter().any(|r| !r[c].is_empty()))
        .collect();
    let table = grid
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.is_empty()))
        .map(|row| {
            (*populated.first().unwrap()..=*populated.last().unwrap())
                .map(|c| row[c].as_str())
                .collect::<Vec<_>>()
                .join("\t")
        });
    outside_rows(&above, height)
        .into_iter()
        .chain(table)
        .chain(outside_rows(&below, height))
        .collect::<Vec<_>>()
        .join("\n")
}

struct DiagonalHeader<'a> {
    pieces: Vec<&'a TextLine>,
    line: TextLine,
}

/// The table's top-left cell, when a diagonal stroke starts at its top-left
/// corner and it holds several labels. Labels are grouped by their side of the
/// diagonal(s) and read top-right to bottom-left: `星期/科目/时间`.
fn diagonal_header_cell<'a>(
    image: &RgbaImage,
    rules: &RuleSet,
    frame: Rect,
    lines: &[&'a TextLine],
    height: f32,
) -> Option<DiagonalHeader<'a>> {
    let tolerance = (height * 0.5).max(4.0);
    let (x0, y0) = (frame.left + 3.0, frame.top + 3.0);
    let y1 = rules
        .horizontal
        .iter()
        .filter(|rule| rule.position > y0 + height * 1.5 && rule.start <= x0 + tolerance)
        .map(|rule| rule.position)
        .fold(f32::INFINITY, f32::min);
    let x1 = rules
        .vertical
        .iter()
        .filter(|rule| {
            rule.position > x0 + height * 1.5
                && rule.start <= y0 + tolerance
                && rule.end >= y1 - tolerance
        })
        .map(|rule| rule.position)
        .fold(f32::INFINITY, f32::min);
    if !x1.is_finite() || !y1.is_finite() || x1 - x0 > (frame.right - frame.left) * 0.5 {
        return None;
    }
    let cell = Rect {
        left: x0,
        top: y0,
        right: x1,
        bottom: y1,
    };
    let pieces: Vec<&TextLine> = lines
        .iter()
        .copied()
        .filter(|line| {
            let (x, y) = (
                (line.rect.left + line.rect.right) / 2.0,
                line.rect.center_y(),
            );
            x > cell.left && x < cell.right && y > cell.top && y < cell.bottom
        })
        .collect();
    if pieces.is_empty()
        || !(has_corner_diagonal(image, cell) || diagonal_layout(&pieces, cell, height))
    {
        return None;
    }
    let (width, depth) = (cell.right - cell.left, cell.bottom - cell.top);
    // u/v: position in the cell; side: offset across the diagonal; along: progress.
    let mut placed: Vec<(f32, f32, &TextLine)> = pieces
        .iter()
        .map(|line| {
            let u = ((line.rect.left + line.rect.right) / 2.0 - cell.left) / width;
            let v = (line.rect.center_y() - cell.top) / depth;
            (u - v, u + v, *line)
        })
        .collect();
    placed.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut groups: Vec<Vec<(f32, f32, &TextLine)>> = Vec::new();
    for item in placed {
        match groups.last_mut() {
            Some(group) if group.last().unwrap().0 - item.0 < 0.3 => group.push(item),
            _ => groups.push(vec![item]),
        }
    }
    let text = groups
        .into_iter()
        .map(|mut group| {
            group.sort_by(|a, b| a.1.total_cmp(&b.1));
            group
                .iter()
                .map(|(_, _, line)| super::text::tidy_line(&line.text))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/");
    // Anchor the merged label at the cell's left edge: when only a right-hand
    // label was recognized it still belongs to the corner cell's column.
    let union = pieces.iter().fold(pieces[0].rect, |acc, line| Rect {
        left: acc.left.min(line.rect.left).min(cell.left + 1.0),
        top: acc.top.min(line.rect.top),
        right: acc.right.max(line.rect.right),
        bottom: acc.bottom.max(line.rect.bottom),
    });
    Some(DiagonalHeader {
        pieces,
        line: TextLine { text, rect: union },
    })
}

/// Labels laid out like a diagonal header even when its faint strokes are lost
/// (blur, low contrast): several labels in at least two lines, the upper line
/// reaching the right part of the cell and the lower line the left part. A
/// wrapped, centred label stacks its lines and does not qualify.
fn diagonal_layout(pieces: &[&TextLine], cell: Rect, height: f32) -> bool {
    if pieces.len() < 3 {
        return false;
    }
    let mut ordered = pieces.to_vec();
    ordered.sort_by(|a, b| a.rect.center_y().total_cmp(&b.rect.center_y()));
    let rows = group_text_rows(&ordered, height);
    let center = |line: &&TextLine| (line.rect.left + line.rect.right) / 2.0;
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return false;
    };
    let upper = first.iter().map(center).fold(f32::NEG_INFINITY, f32::max);
    let lower = last.iter().map(center).fold(f32::INFINITY, f32::min);
    let width = cell.right - cell.left;
    rows.len() >= 2
        && upper - lower > width * 0.25
        && upper > cell.left + width * 0.5
        && lower < cell.left + width * 0.5
}

/// A dark stroke into the cell's top-left or bottom-right corner from one of the
/// opposite edges. Printed and photographed tables use both forms.
fn has_corner_diagonal(image: &RgbaImage, cell: Rect) -> bool {
    let luma = |x: f32, y: f32| -> Option<u8> {
        let (x, y) = (x.round(), y.round());
        if x < 0.0 || y < 0.0 || x >= image.width() as f32 || y >= image.height() as f32 {
            return None;
        }
        let p = image.get_pixel(x as u32, y as u32);
        Some(((u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) / 256) as u8)
    };
    let mut background = Vec::new();
    for i in 1..20 {
        for j in 1..20 {
            let x = cell.left + (cell.right - cell.left) * i as f32 / 20.0;
            let y = cell.top + (cell.bottom - cell.top) * j as f32 / 20.0;
            background.extend(luma(x, y));
        }
    }
    if background.is_empty() {
        return false;
    }
    background.sort_unstable();
    let paper = background[background.len() / 2];
    // A stroke pixel is clearly darker than the paper, or (faint photographed
    // pencil-like lines) darker than the paper a few pixels above and below it.
    let dark = |x: f32, y: f32| {
        let core = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (dx, dy)))
            .filter_map(|(dx, dy)| luma(x + dx as f32, y + dy as f32))
            .min();
        let sides = [luma(x, y - 6.0), luma(x, y + 6.0)]
            .into_iter()
            .flatten()
            .min();
        core.is_some_and(|core| {
            u16::from(core) + 30 < u16::from(paper)
                || sides.is_some_and(|sides| u16::from(core) + 10 < u16::from(sides))
        })
    };
    let (width, depth) = (cell.right - cell.left, cell.bottom - cell.top);
    let stroke = |(sx, sy): (f32, f32), (ex, ey): (f32, f32)| {
        (2..=18)
            .filter(|&i| {
                let t = i as f32 / 20.0;
                dark(sx + (ex - sx) * t, sy + (ey - sy) * t)
            })
            .count()
            >= 13
    };
    // Photographed lines may meet slightly inside the corner.
    let near = |(x, y): (f32, f32), sx: f32, sy: f32| {
        [(0.0, 0.0), (0.0, 0.12), (0.06, 0.0), (0.06, 0.12)]
            .map(|(fx, fy)| (x + sx * width * fx, y + sy * depth * fy))
    };
    (2..=8).map(|t| t as f32 / 10.0).any(|t| {
        near((cell.left, cell.top), 1.0, 1.0)
            .into_iter()
            .any(|corner| {
                stroke(corner, (cell.right, cell.top + depth * t))
                    || stroke(corner, (cell.left + width * t, cell.bottom))
            })
            || near((cell.right, cell.bottom), -1.0, -1.0)
                .into_iter()
                .any(|corner| {
                    stroke((cell.left, cell.top + depth * t), corner)
                        || stroke((cell.left + width * t, cell.top), corner)
                })
    })
}

fn inside(frame: Rect, line: &TextLine) -> bool {
    let x = (line.rect.left + line.rect.right) / 2.0;
    x >= frame.left
        && x <= frame.right
        && line.rect.center_y() >= frame.top
        && line.rect.center_y() <= frame.bottom
}

/// `1`, `- 2 -`, `第 3 页`, `Page 4` and similar page labels.
fn is_page_number(text: &str) -> bool {
    let mut rest: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    for noise in ["Page", "page", "第", "页", "頁", "-", "—", "–"] {
        rest = rest.replace(noise, "");
    }
    !rest.is_empty() && rest.len() <= 4 && rest.bytes().all(|b| b.is_ascii_digit())
}

/// Text outside a framed table: one line per visual row, in reading order.
fn outside_rows(lines: &[&TextLine], height: f32) -> Vec<String> {
    let mut ordered = lines.to_vec();
    ordered.sort_by(|a, b| a.rect.center_y().total_cmp(&b.rect.center_y()));
    group_text_rows(&ordered, height)
        .into_iter()
        .map(|row| {
            let mut text = String::new();
            let mut previous: Option<&TextLine> = None;
            for line in row {
                let fragment = super::text::tidy_line(&line.text);
                // Letter-spaced titles ("课  程  表") are one word; other wide
                // gaps separate distinct items such as a school and a class.
                let spaced_title = previous.is_some_and(|before| {
                    before.text.chars().count() <= 2 && fragment.chars().count() <= 2
                });
                let far = previous
                    .is_some_and(|before| line.rect.left - before.rect.right > height * 1.5);
                if far && !spaced_title && !text.is_empty() {
                    text.push(' ');
                    text.push_str(&fragment);
                } else {
                    super::text::push_wrapped(&mut text, &fragment);
                }
                previous = Some(line);
            }
            text
        })
        .filter(|text| !text.is_empty())
        .collect()
}

/// OneOCR occasionally reports a fragment again inside a longer line (`下` within
/// `下午`). Keep only the longer reading.
fn drop_duplicate_lines(lines: &mut Vec<&TextLine>) {
    let area = |r: Rect| (r.right - r.left).max(0.0) * (r.bottom - r.top).max(0.0);
    let compact = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let snapshot = lines.clone();
    lines.retain(|line| {
        let own = area(line.rect);
        let text = compact(&line.text);
        !snapshot.iter().any(|other| {
            if std::ptr::eq(*other, *line) || area(other.rect) <= own {
                return false;
            }
            let overlap = (line.rect.right.min(other.rect.right)
                - line.rect.left.max(other.rect.left))
            .max(0.0)
                * (line.rect.bottom.min(other.rect.bottom) - line.rect.top.max(other.rect.top))
                    .max(0.0);
            own > 0.0 && overlap >= own * 0.8 && compact(&other.text).contains(&text)
        })
    });
}

#[derive(Clone, Copy)]
struct RowBoundary {
    position: f32,
    inferred: bool,
}

/// A closed outer border: the outermost wide rules joined by full-height sides.
/// Text outside it (titles, notes) may be taller than the table itself, so the
/// frame is judged by how much text it encloses, not by its share of the height.
fn enclosing_frame(
    rules: &RuleSet,
    bounds: Rect,
    lines: &[&TextLine],
    height: f32,
) -> Option<Rect> {
    let wide: Vec<_> = rules
        .horizontal
        .iter()
        .filter(|rule| rule.end - rule.start >= (bounds.right - bounds.left) * 0.8)
        .collect();
    let first = wide.first()?;
    let last = wide.last()?;
    if last.position - first.position < height * 3.0 {
        return None;
    }
    let left = first.start.max(last.start);
    let right = first.end.min(last.end);
    let tolerance = (height * 0.5).max(4.0);
    let sides = [left, right]
        .into_iter()
        .filter(|&x| {
            rules.vertical.iter().any(|rule| {
                (rule.position - x).abs() <= tolerance
                    && rule.start <= first.position + tolerance
                    && rule.end >= last.position - tolerance
            })
        })
        .count();
    // A blurred photo can lose both faint outer sides. Interior column rules
    // that run exactly from the top border to the bottom one, and none that
    // continue past them, still close the table.
    let spanning = rules
        .vertical
        .iter()
        .filter(|rule| {
            (rule.start - first.position).abs() <= tolerance
                && (rule.end - last.position).abs() <= tolerance
        })
        .count();
    let escapes = rules.vertical.iter().any(|rule| {
        rule.start < first.position - tolerance * 2.0 || rule.end > last.position + tolerance * 2.0
    });
    if sides == 0 && (spanning < 2 || escapes) {
        return None;
    }
    let frame = Rect {
        left: left - 3.0,
        right: right + 3.0,
        top: first.position - 3.0,
        bottom: last.position + 3.0,
    };
    // Text beside the frame, within its height, means the measured sides are
    // not the outer border (a logo block can cut a border short at small sizes).
    if lines.iter().any(|line| {
        let x = (line.rect.left + line.rect.right) / 2.0;
        let y = line.rect.center_y();
        y > frame.top && y < frame.bottom && (x < frame.left || x > frame.right)
    }) {
        return None;
    }
    // A photo can lose one tilted side; then nearly all text must lie inside.
    let enclosed = lines.iter().filter(|line| inside(frame, line)).count() as f32;
    let needed = if sides == 2 { 0.5 } else { 0.75 };
    (enclosed >= lines.len() as f32 * needed).then_some(frame)
}

fn positions(rules: &[Rule], start: f32, end: f32) -> Vec<f32> {
    let mut values: Vec<_> = rules
        .iter()
        .map(|rule| rule.position)
        .filter(|&p| p > start && p < end)
        .collect();
    values.sort_by(f32::total_cmp);
    values.dedup_by(|a, b| (*a - *b).abs() <= 3.0);
    values
}

fn covers(rules: &[Rule], position: f32, along: f32) -> bool {
    rules.iter().any(|rule| {
        (rule.position - position).abs() <= 3.0
            && along >= rule.start - 3.0
            && along <= rule.end + 3.0
    })
}

fn text_boundaries(rows: &[Vec<&TextLine>]) -> Vec<RowBoundary> {
    rows.windows(2)
        .map(|pair| {
            let bottom = pair[0]
                .iter()
                .map(|line| line.rect.bottom)
                .fold(f32::NEG_INFINITY, f32::max);
            let top = pair[1]
                .iter()
                .map(|line| line.rect.top)
                .fold(f32::INFINITY, f32::min);
            RowBoundary {
                position: (bottom + top) / 2.0,
                inferred: true,
            }
        })
        .collect()
}

fn row_boundaries(
    lines: &[&TextLine],
    columns: &[f32],
    rules: &RuleSet,
    bounds: Rect,
    height: f32,
) -> Vec<RowBoundary> {
    let horizontal = positions(&rules.horizontal, f32::NEG_INFINITY, f32::INFINITY);
    let mut cuts: Vec<_> = horizontal
        .iter()
        .copied()
        .filter(|&y| y > bounds.top && y < bounds.bottom)
        .map(|position| RowBoundary {
            position,
            inferred: false,
        })
        .collect();
    // Several vertical separators beginning together identify the bottom of
    // a grouped heading even when no horizontal stroke was drawn there.
    let mut endpoints: Vec<_> = rules
        .vertical
        .iter()
        .flat_map(|rule| [rule.start, rule.end])
        .collect();
    endpoints.sort_by(f32::total_cmp);
    // A box that only grazes the boundary (loose OCR padding) does not cross it.
    let graze = height * 0.25;
    let mut index = 0;
    while index < endpoints.len() {
        let start = index;
        while index < endpoints.len() && endpoints[index] - endpoints[start] <= 3.0 {
            index += 1;
        }
        let position = endpoints[start..index].iter().sum::<f32>() / (index - start) as f32;
        if index - start >= 2
            && position > bounds.top
            && position < bounds.bottom
            && !cuts
                .iter()
                .any(|cut| (cut.position - position).abs() <= 3.0)
            && !lines
                .iter()
                .any(|line| position > line.rect.top + graze && position < line.rect.bottom - graze)
        {
            cuts.push(RowBoundary {
                position,
                inferred: true,
            });
        }
    }
    cuts.sort_by(|a, b| a.position.total_cmp(&b.position));
    if cuts.is_empty() {
        return text_boundaries(&group_text_rows(lines, height));
    }
    let mut bands = vec![Vec::new(); cuts.len() + 1];
    for &line in lines {
        bands[cuts.partition_point(|cut| cut.position < line.rect.center_y())].push(line);
    }
    // Blank horizontal rules inside a large body are reconstructed from
    // repeated text columns. Wrapped lines in one cell remain in that row.
    for band in bands.into_iter().filter(|band| !band.is_empty()) {
        let rows = split_incomplete_row_bands(vec![band], columns, &horizontal, height);
        cuts.extend(text_boundaries(&rows));
    }
    cuts.sort_by(|a, b| a.position.total_cmp(&b.position));
    cuts.dedup_by(|a, b| (a.position - b.position).abs() <= 3.0);
    cuts
}

fn column_activity(
    lines: &[&TextLine],
    columns: &[f32],
    cuts: &[RowBoundary],
    rules: &RuleSet,
    bounds: Rect,
    height: f32,
) -> Vec<Vec<bool>> {
    let physical = positions(&rules.vertical, bounds.left, bounds.right);
    (0..=cuts.len())
        .map(|row| {
            let top = row.checked_sub(1).map_or(bounds.top, |i| cuts[i].position);
            let bottom = cuts.get(row).map_or(bounds.bottom, |cut| cut.position);
            let middle = (top + bottom) / 2.0;
            let actual: Vec<_> = physical
                .iter()
                .copied()
                .filter(|&x| covers(&rules.vertical, x, middle))
                .collect();
            columns
                .iter()
                .map(|&x| {
                    if physical.iter().any(|&p| (p - x).abs() <= 3.0) {
                        if covers(&rules.vertical, x, middle) {
                            return true;
                        }
                        // Independent headings within an otherwise merged parent
                        // cell may borrow their children's physical separators.
                        // A single centered title still spans the complete parent.
                        let begins_below = rules.vertical.iter().any(|rule| {
                            (rule.position - x).abs() <= 3.0 && (rule.start - bottom).abs() <= 3.0
                        });
                        let parent = actual.partition_point(|&p| p < x);
                        begins_below
                            && lines
                                .iter()
                                .filter(|line| {
                                    let center = (line.rect.left + line.rect.right) / 2.0;
                                    line.rect.center_y() > top
                                        && line.rect.center_y() < bottom
                                        && actual.partition_point(|&p| p < center) == parent
                                })
                                .any(|first| {
                                    lines.iter().any(|second| {
                                        let center = (second.rect.left + second.rect.right) / 2.0;
                                        second.rect.center_y() > top
                                            && second.rect.center_y() < bottom
                                            && actual.partition_point(|&p| p < center) == parent
                                            && (first.rect.center_y() - second.rect.center_y())
                                                .abs()
                                                < height * 0.5
                                            && second.rect.left - first.rect.right > height * 0.7
                                    })
                                })
                    } else {
                        // Whitespace columns belong to the physical section where
                        // they were observed; they must not split its merged title.
                        let right = physical.partition_point(|&p| p < x);
                        right
                            .checked_sub(1)
                            .is_none_or(|i| covers(&rules.vertical, physical[i], middle))
                            && physical
                                .get(right)
                                .is_none_or(|&p| covers(&rules.vertical, p, middle))
                    }
                })
                .collect()
        })
        .collect()
}

/// A vertical label (`上午` spanning several rows) beside a column of short
/// entries (period numbers) shares no text row with most of them, so row gaps
/// barely support the boundary between them, and in a blurred photo its faint
/// rule may be lost. A clear vertical strip from the label to the entries, which
/// no text over the same rows crosses, is that boundary.
fn vertical_label_columns(lines: &[&TextLine], columns: &mut Vec<f32>, height: f32) {
    let labels: Vec<&TextLine> = lines
        .iter()
        .copied()
        .filter(|line| {
            line.rect.height() > height * 2.5
                && line.rect.height() > (line.rect.right - line.rect.left) * 1.5
        })
        .collect();
    for label in labels {
        let edge = label.rect.right;
        let section_end = columns
            .iter()
            .copied()
            .find(|&x| x > edge - height * 0.3)
            .unwrap_or(f32::INFINITY);
        // Already separated by a rule or an inferred boundary near the label.
        let entries: Vec<&TextLine> = lines
            .iter()
            .copied()
            .filter(|line| {
                line.rect.left >= edge + height * 0.3
                    && line.rect.right <= section_end
                    && line.rect.height() <= height * 2.5
            })
            .collect();
        if entries.len() < 2 {
            continue;
        }
        let next = entries
            .iter()
            .map(|line| line.rect.left)
            .fold(f32::INFINITY, f32::min);
        if next - edge < height * 0.7 || section_end < next {
            continue;
        }
        let top = entries
            .iter()
            .map(|line| line.rect.top)
            .fold(label.rect.top, f32::min);
        let bottom = entries
            .iter()
            .map(|line| line.rect.bottom)
            .fold(label.rect.bottom, f32::max);
        let strip = (edge + height * 0.1, next - height * 0.1);
        let crossed = lines.iter().any(|line| {
            let y = line.rect.center_y();
            y > top && y < bottom && line.rect.left < strip.1 && line.rect.right > strip.0
        });
        if !crossed {
            columns.push((edge + next) / 2.0);
            columns.sort_by(f32::total_cmp);
        }
    }
}

/// Column evidence only: a vertical label (`上午` written top to bottom) sits in a
/// single text row, yet it separates every row it spans from the next column.
fn with_vertical_labels<'a>(rows: &[Vec<&'a TextLine>], height: f32) -> Vec<Vec<&'a TextLine>> {
    let tall: Vec<&TextLine> = rows
        .iter()
        .flatten()
        .copied()
        .filter(|line| {
            line.rect.height() > height * 2.5
                && line.rect.height() > (line.rect.right - line.rect.left) * 1.5
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut row = row.clone();
            let middle =
                row.iter().map(|line| line.rect.center_y()).sum::<f32>() / row.len() as f32;
            for &label in &tall {
                let beside = !row.iter().any(|line| {
                    std::ptr::eq(*line, label)
                        || (line.rect.left < label.rect.right && label.rect.left < line.rect.right)
                });
                if beside && middle > label.rect.top && middle < label.rect.bottom {
                    row.push(label);
                }
            }
            row.sort_by(|a, b| a.rect.left.total_cmp(&b.rect.left));
            row
        })
        .collect()
}

fn group_text_rows<'a>(ordered: &[&'a TextLine], height: f32) -> Vec<Vec<&'a TextLine>> {
    let mut rows: Vec<Vec<&TextLine>> = Vec::new();
    for &line in ordered {
        if let Some(row) = rows.last_mut() {
            // Compare with the anchor rather than allowing offset chains to
            // merge two distinct rows.
            let anchor = row[0].rect;
            let overlap =
                (anchor.bottom.min(line.rect.bottom) - anchor.top.max(line.rect.top)).max(0.0);
            if overlap / anchor.height().min(line.rect.height()) > 0.45
                || (anchor.center_y() - line.rect.center_y()).abs() < height * 0.3
            {
                row.push(line);
                continue;
            }
        }
        rows.push(vec![line]);
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.rect.left.total_cmp(&b.rect.left));
    }
    rows
}

fn split_incomplete_row_bands<'a>(
    rows: Vec<Vec<&'a TextLine>>,
    columns: &[f32],
    horizontal: &[f32],
    height: f32,
) -> Vec<Vec<&'a TextLine>> {
    if columns.is_empty() {
        return rows;
    }
    // The median ruled row, not the smallest: one short heading row must not
    // make every ordinary two-line row look like several missing rows.
    let mut bands: Vec<f32> = horizontal
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|&gap| gap >= height * 1.5)
        .collect();
    bands.sort_by(f32::total_cmp);
    let typical_band = bands
        .get(bands.len().saturating_sub(1) / 2)
        .copied()
        .unwrap_or(f32::INFINITY);
    let mut result = Vec::new();
    for row in rows {
        let groups = group_text_rows(&row, height);
        let top = row
            .iter()
            .map(|line| line.rect.top)
            .fold(f32::INFINITY, f32::min);
        let bottom = row
            .iter()
            .map(|line| line.rect.bottom)
            .fold(f32::NEG_INFINITY, f32::max);
        let repeated = groups
            .iter()
            .filter(|group| {
                let first = columns.partition_point(|&x| x <= group[0].rect.left);
                group
                    .iter()
                    .any(|line| columns.partition_point(|&x| x <= line.rect.left) != first)
            })
            .count();
        // A band substantially taller than other ruled rows can contain missing
        // separators. Require repeated populated columns; a wrapped label in
        // one cell is not evidence for another table row.
        if (horizontal.len() < 2 || bottom - top > (typical_band * 1.25).max(height * 6.0))
            && repeated >= 2
        {
            let band_start = result.len();
            for group in groups {
                let first = columns.partition_point(|&x| x <= group[0].rect.left);
                let multiple_columns = group
                    .iter()
                    .any(|line| columns.partition_point(|&x| x <= line.rect.left) != first);
                if multiple_columns || result.len() == band_start {
                    result.push(group);
                } else {
                    result.last_mut().unwrap().extend(group);
                }
            }
        } else {
            result.push(row);
        }
    }
    result
}

fn infer_columns(rows: &[Vec<&TextLine>], height: f32, physical: &[f32]) -> Vec<f32> {
    #[derive(Clone, Copy)]
    struct Gap {
        left: f32,
        right: f32,
    }
    let mut gaps = Vec::new();
    for row in rows {
        for pair in row.windows(2) {
            let left = pair[0].rect.right;
            let right = pair[1].rect.left;
            if right > left {
                gaps.push(Gap {
                    left: left + height * 0.1,
                    right: right - height * 0.1,
                });
            }
        }
    }
    // Wide spaces between group headings already separated by a real rule
    // cannot establish a threshold for the missing columns inside each group.
    // Text can touch a ruled border; a gap starting right beside a rule is still
    // that rule's gap, not evidence for another column a few pixels away.
    let touching = height * 0.3;
    gaps.retain(|gap| {
        !physical
            .iter()
            .any(|&x| gap.left - touching <= x && x <= gap.right + touching)
    });
    let sizes = gaps.iter().map(|gap| gap.right - gap.left).collect();
    let threshold = gap_threshold(sizes, height);
    gaps.retain(|gap| gap.right - gap.left >= threshold);
    let mut boundaries = physical.to_vec();
    // Intersect whitespace intervals across rows instead of clustering their
    // midpoints. Variable-length labels and absent cells then share one cut.
    while !gaps.is_empty() {
        let mut best = (0i32, 0.0f32, 0.0f32);
        let mut events = gaps
            .iter()
            .flat_map(|gap| [(gap.left, 1i32), (gap.right, -1)])
            .collect::<Vec<_>>();
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        // A row's whitespace intervals cannot overlap, so event counts are
        // also counts of supporting rows. Sweep avoids a cubic gap comparison.
        let mut support = 0;
        for pair in events.windows(2) {
            support += pair[0].1;
            let width = pair[1].0 - pair[0].0;
            if width > 0.0 && (support > best.0 || (support == best.0 && width > best.2)) {
                best = (support, (pair[0].0 + pair[1].0) / 2.0, width);
            }
        }
        if best.0 == 0 || (best.0 < 2 && rows.len() >= 3 && !boundaries.is_empty()) {
            break;
        }
        boundaries.push(best.1);
        gaps.retain(|gap| !(gap.left <= best.1 && best.1 <= gap.right));
    }
    boundaries.sort_by(f32::total_cmp);
    boundaries
}

// The Python implementation separated word gaps from inter-column gaps using
// a large jump in their distribution. Keep that behavior without clustering
// dependencies: a fixed fraction of text height alone splits multiword cells.
fn gap_threshold(mut sizes: Vec<f32>, height: f32) -> f32 {
    let fallback = height * 0.7;
    sizes.sort_by(f32::total_cmp);
    let mut sum = 0.0;
    for (index, pair) in sizes.windows(2).enumerate() {
        sum += pair[0];
        let mean = sum / (index + 1) as f32;
        if index >= 1 && mean > 0.0 && mean <= height && pair[1] - pair[0] > mean * 2.0 {
            return ((pair[0] + pair[1]) / 2.0).max(fallback);
        }
    }
    fallback
}

#[cfg(test)]
pub(super) fn debug_rules(lines: &[TextLine], image: &RgbaImage) {
    let ordered: Vec<_> = lines.iter().collect();
    if ordered.is_empty() {
        return;
    }
    let mut heights: Vec<_> = ordered.iter().map(|line| line.rect.height()).collect();
    heights.sort_by(f32::total_cmp);
    let height = heights[heights.len() / 2];
    let bounds = Rect {
        left: ordered
            .iter()
            .map(|l| l.rect.left)
            .fold(f32::INFINITY, f32::min),
        top: ordered
            .iter()
            .map(|l| l.rect.top)
            .fold(f32::INFINITY, f32::min),
        right: ordered
            .iter()
            .map(|l| l.rect.right)
            .fold(f32::NEG_INFINITY, f32::max),
        bottom: ordered
            .iter()
            .map(|l| l.rect.bottom)
            .fold(f32::NEG_INFINITY, f32::max),
    };
    let rules = rules::detect(image, &ordered, bounds, height);
    println!("  height={height:.1} horizontal={:?}", rules.horizontal);
    println!("  vertical={:?}", rules.vertical);
    let rows = group_text_rows(&ordered, height);
    let physical = positions(&rules.vertical, bounds.left, bounds.right);
    println!(
        "  physical={physical:?} columns={:?}",
        infer_columns(&rows, height, &physical)
    );
}

#[cfg(test)]
fn detect_grid(image: &RgbaImage, bounds: Rect, height: f32) -> (Vec<f32>, Vec<f32>) {
    let rules = rules::detect(image, &[], bounds, height);
    (
        positions(&rules.horizontal, f32::NEG_INFINITY, f32::INFINITY),
        positions(&rules.vertical, f32::NEG_INFINITY, f32::INFINITY),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn line(text: &str, x: f32, y: f32, width: f32) -> TextLine {
        TextLine {
            text: text.into(),
            rect: Rect {
                left: x,
                top: y,
                right: x + width,
                bottom: y + 10.0,
            },
        }
    }

    fn white() -> RgbaImage {
        RgbaImage::from_pixel(350, 160, Rgba([255; 4]))
    }

    /// OneOCR boxes of a 5×5 screenshot table (cells 190/60/80/90/90 × 40 px).
    fn screenshot_lines() -> Vec<TextLine> {
        let rows = [
            ["项目", "Qty", "单价", "金额", "备注"],
            ["笔记本 Notebook", "2", "24.00", "48.00", "办公"],
            ["签字笔", "3", "6.00", "18.00", ""],
            ["Stapler", "1", "35.50", "35.50", "会议室"],
            ["合计 Total", "6", "", "101.50", "已报销"],
        ];
        let lefts = [31.0, 222.0, 282.0, 362.0, 452.0];
        let mut lines = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            for (c, text) in row.iter().enumerate().filter(|(_, text)| !text.is_empty()) {
                let width = text.chars().count() as f32 * 11.0 + 8.0;
                lines.push(TextLine {
                    text: (*text).into(),
                    rect: Rect {
                        left: lefts[c],
                        top: 31.5 + r as f32 * 40.0,
                        right: lefts[c] + width,
                        bottom: 50.0 + r as f32 * 40.0,
                    },
                });
            }
        }
        lines
    }

    const SCREENSHOT_TSV: &str = "项目\tQty\t单价\t金额\t备注\n笔记本 Notebook\t2\t24.00\t48.00\t办公\n签字笔\t3\t6.00\t18.00\t\nStapler\t1\t35.50\t35.50\t会议室\n合计 Total\t6\t\t101.50\t已报销";

    #[test]
    fn dark_theme_grid_uses_its_light_rules() {
        let mut image = RgbaImage::from_pixel(550, 240, Rgba([30, 30, 30, 255]));
        let grey = Rgba([90, 90, 90, 255]);
        for x in [20, 210, 270, 350, 440, 530] {
            for y in 20..=220 {
                image.put_pixel(x, y, grey);
            }
        }
        for y in [20, 60, 100, 140, 180, 220] {
            for x in 20..=530 {
                image.put_pixel(x, y, grey);
            }
        }
        assert_eq!(reconstruct(&screenshot_lines(), &image), SCREENSHOT_TSV);
    }

    #[test]
    fn zebra_stripe_edges_are_not_partial_cell_borders() {
        let mut image = RgbaImage::from_pixel(550, 240, Rgba([255; 4]));
        for top in [60, 140] {
            for y in top..top + 40 {
                for x in 20..530 {
                    image.put_pixel(x, y, Rgba([236, 241, 248, 255]));
                }
            }
        }
        assert_eq!(reconstruct(&screenshot_lines(), &image), SCREENSHOT_TSV);
    }

    #[test]
    fn wrapped_chinese_cell_lines_join_without_spaces() {
        let mut image = RgbaImage::from_pixel(350, 160, Rgba([255; 4]));
        for y in [10, 40, 100] {
            for x in 10..=340 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        for x in [10, 170, 340] {
            for y in 10..=100 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        // One ruled body row: the first cell wraps over three lines, the second
        // over two. Neither wrap is a separate table row.
        let lines = [
            line("标准", 20.0, 20.0, 30.0),
            line("Notes", 180.0, 20.0, 40.0),
            line("现行国家标", 20.0, 50.0, 60.0),
            line("Total amount", 180.0, 50.0, 90.0),
            line("准《公共建 筑》", 20.0, 64.0, 60.0),
            line("is due", 180.0, 64.0, 50.0),
            line("说明", 20.0, 78.0, 30.0),
        ];
        assert_eq!(
            reconstruct(&lines, &image),
            "标准\tNotes\n现行国家标准《公共建筑》说明\tTotal amount is due"
        );
    }

    #[test]
    fn borderless_table_preserves_missing_cells_and_input_order_does_not_matter() {
        let lines = vec![
            line("6", 250.0, 80.0, 10.0),
            line("名称", 10.0, 10.0, 40.0),
            line("金额", 150.0, 10.0, 20.0),
            line("数目", 250.0, 10.0, 20.0),
            line("长名称", 10.0, 45.0, 80.0),
            line("20", 150.0, 45.0, 20.0),
            line("3", 250.0, 45.0, 10.0),
            line("缺金额", 10.0, 80.0, 60.0),
        ];
        assert_eq!(
            reconstruct(&lines, &white()),
            "名称\t金额\t数目\n长名称\t20\t3\n缺金额\t\t6"
        );
    }

    #[test]
    fn physical_borders_keep_multiline_cells_together() {
        let mut image = white();
        for y in [0, 50, 100] {
            for x in 0..=300 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        for x in [0, 140, 300] {
            for y in 0..=100 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let lines = vec![
            line("first", 10.0, 10.0, 40.0),
            line("second", 10.0, 30.0, 50.0),
            line("value", 160.0, 10.0, 40.0),
            line("next", 10.0, 60.0, 40.0),
            line("42", 160.0, 60.0, 20.0),
        ];
        assert_eq!(reconstruct(&lines, &image), "first second\tvalue\nnext\t42");
    }

    #[test]
    fn a_single_row_can_be_a_table() {
        let lines = [line("A", 10.0, 10.0, 10.0), line("B", 100.0, 10.0, 10.0)];
        assert_eq!(reconstruct(&lines, &white()), "A\tB");
    }

    #[test]
    fn embedded_whitespace_does_not_change_tsv_shape() {
        let lines = [line("A\tB\nC", 10.0, 10.0, 70.0)];
        assert_eq!(reconstruct(&lines, &white()), "A B C");
        assert_eq!(reconstruct(&[], &white()), "");
    }

    #[test]
    fn pale_grid_preserves_multiline_cells() {
        let mut image = white();
        for y in [0, 50, 100] {
            for x in 0..=300 {
                image.put_pixel(x, y, Rgba([232, 232, 232, 255]));
            }
        }
        for x in [0, 140, 300] {
            for y in 0..=100 {
                image.put_pixel(x, y, Rgba([232, 232, 232, 255]));
            }
        }
        let lines = vec![
            line("first", 10.0, 10.0, 40.0),
            line("second", 10.0, 30.0, 50.0),
            line("value", 160.0, 10.0, 40.0),
            line("next", 10.0, 60.0, 40.0),
            line("42", 160.0, 60.0, 20.0),
        ];
        assert_eq!(reconstruct(&lines, &image), "first second\tvalue\nnext\t42");
    }

    #[test]
    fn short_internal_separator_is_detected() {
        let mut image = white();
        for y in 45..=90 {
            image.put_pixel(140, y, Rgba([0, 0, 0, 255]));
        }
        let bounds = Rect {
            left: 10.0,
            top: 10.0,
            right: 300.0,
            bottom: 140.0,
        };
        let (_, vertical) = detect_grid(&image, bounds, 10.0);
        assert!(
            vertical.iter().any(|&x| (x - 140.0).abs() < 2.0),
            "{vertical:?}"
        );
    }

    #[test]
    fn incomplete_vertical_borders_are_supplemented_by_text_columns() {
        let mut image = white();
        for x in [0, 140] {
            for y in 0..140 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let mut lines = Vec::new();
        for y in [10.0, 50.0, 90.0] {
            lines.extend([
                line("A", 10.0, y, 20.0),
                line("B", 160.0, y, 20.0),
                line("C", 250.0, y, 20.0),
            ]);
        }
        assert_eq!(reconstruct(&lines, &image), "A\tB\tC\nA\tB\tC\nA\tB\tC");
    }

    #[test]
    fn outer_horizontal_borders_do_not_merge_all_data_rows() {
        let mut image = white();
        for y in [5, 100] {
            for x in 0..=300 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let mut lines = Vec::new();
        for y in [15.0, 45.0, 85.0] {
            lines.extend([line("A", 10.0, y, 20.0), line("B", 160.0, y, 20.0)]);
        }
        assert_eq!(reconstruct(&lines, &image), "A\tB\nA\tB\nA\tB");
    }

    #[test]
    fn word_spaces_are_not_promoted_to_table_columns() {
        let mut lines = Vec::new();
        for y in [10.0, 50.0, 90.0] {
            lines.extend([
                line("New", 10.0, y, 20.0),
                line("York", 40.0, y, 20.0),
                line("42", 160.0, y, 20.0),
            ]);
        }
        assert_eq!(
            reconstruct(&lines, &white()),
            "New York\t42\nNew York\t42\nNew York\t42"
        );
    }

    #[test]
    fn one_header_rule_still_allows_unruled_body_rows() {
        let mut image = white();
        for x in 0..=300 {
            image.put_pixel(x, 40, Rgba([0, 0, 0, 255]));
        }
        let mut lines = Vec::new();
        for y in [10.0, 50.0, 90.0, 130.0] {
            lines.extend([line("A", 10.0, y, 20.0), line("B", 160.0, y, 20.0)]);
        }
        assert_eq!(reconstruct(&lines, &image), "A\tB\nA\tB\nA\tB\nA\tB");
    }

    #[test]
    fn multiline_merged_title_does_not_borrow_child_columns() {
        let mut image = white();
        for y in [0, 50, 100] {
            for x in 0..=300 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        for y in 50..=100 {
            image.put_pixel(140, y, Rgba([0, 0, 0, 255]));
        }
        let lines = [
            line("title", 120.0, 10.0, 100.0),
            line("continued", 160.0, 30.0, 80.0),
            line("A", 10.0, 60.0, 20.0),
            line("B", 160.0, 60.0, 20.0),
        ];
        assert_eq!(reconstruct(&lines, &image), "title continued\t\nA\tB");
    }

    #[test]
    fn page_number_below_closed_table_is_excluded() {
        let mut image = white();
        for y in [0, 50, 100] {
            for x in 0..=300 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        for x in [0, 140, 300] {
            for y in 0..=100 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let lines = [
            line("A", 10.0, 10.0, 20.0),
            line("B", 270.0, 10.0, 20.0),
            line("C", 10.0, 60.0, 20.0),
            line("D", 270.0, 60.0, 20.0),
            line("1", 140.0, 120.0, 10.0),
        ];
        assert_eq!(reconstruct(&lines, &image), "A\tB\nC\tD");
    }

    #[test]
    fn broken_and_slightly_stepped_rules_are_detected() {
        let mut image = white();
        for x in 0..=300 {
            if x % 30 < 26 {
                image.put_pixel(x, 40 + x / 100, Rgba([225, 225, 225, 255]));
            }
        }
        let bounds = Rect {
            left: 10.0,
            top: 10.0,
            right: 300.0,
            bottom: 140.0,
        };
        let (horizontal, _) = detect_grid(&image, bounds, 10.0);
        assert!(
            horizontal.iter().any(|&y| (y - 41.0).abs() <= 3.0),
            "{horizontal:?}"
        );
    }

    #[test]
    fn missing_horizontal_rules_split_repeated_data_rows_but_keep_wrapped_labels() {
        let mut image = white();
        for y in [0, 40, 150] {
            for x in 0..=300 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let lines = vec![
            line("Name", 10.0, 10.0, 40.0),
            line("Value", 160.0, 10.0, 40.0),
            line("first", 10.0, 50.0, 40.0),
            line("1", 160.0, 50.0, 10.0),
            line("wrapped", 10.0, 65.0, 60.0),
            line("second", 10.0, 90.0, 50.0),
            line("2", 160.0, 90.0, 10.0),
            line("third", 10.0, 125.0, 40.0),
            line("3", 160.0, 125.0, 10.0),
        ];
        assert_eq!(
            reconstruct(&lines, &image),
            "Name\tValue\nfirst wrapped\t1\nsecond\t2\nthird\t3"
        );
    }

    #[test]
    fn physically_empty_interior_column_keeps_its_position() {
        let mut image = white();
        for x in [0, 100, 200, 300] {
            for y in 0..=100 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let lines = vec![
            line("A", 10.0, 10.0, 20.0),
            line("C", 250.0, 10.0, 20.0),
            line("1", 10.0, 60.0, 20.0),
            line("3", 250.0, 60.0, 20.0),
        ];
        assert_eq!(reconstruct(&lines, &image), "A\t\tC\n1\t\t3");
    }

    #[test]
    fn uniform_dark_background_is_not_a_grid() {
        let image = RgbaImage::from_pixel(350, 160, Rgba([80, 80, 80, 255]));
        let bounds = Rect {
            left: 10.0,
            top: 10.0,
            right: 300.0,
            bottom: 140.0,
        };
        assert_eq!(detect_grid(&image, bounds, 10.0), (vec![], vec![]));
    }

    #[test]
    fn text_baselines_are_not_mistaken_for_row_separators() {
        let mut image = white();
        for y in [0, 50, 100] {
            for x in 0..=300 {
                image.put_pixel(x, y, Rgba([225, 225, 225, 255]));
            }
        }
        // Repeated glyph bottoms can resemble a broken horizontal rule.
        for y in [10, 19, 30, 39, 60, 69] {
            for x in 10..120 {
                if x % 9 < 6 {
                    image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                }
            }
        }
        let lines = [
            line("long first", 10.0, 10.0, 110.0),
            line("wrapped", 10.0, 30.0, 110.0),
            line("value", 160.0, 10.0, 50.0),
            line("next", 10.0, 60.0, 110.0),
            line("42", 160.0, 60.0, 20.0),
        ];
        assert_eq!(
            reconstruct(&lines, &image),
            "long first wrapped\tvalue\nnext\t42"
        );
    }

    #[test]
    fn large_gradient_background_preserves_low_contrast_rules() {
        let mut image = RgbaImage::new(1600, 1000);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let background = 215 + (x * 35 / 1600) as u8;
            let value = if x == 800 || y == 500 {
                background - 15
            } else {
                background
            };
            *pixel = Rgba([value, value, value, 255]);
        }
        let bounds = Rect {
            left: 10.0,
            top: 10.0,
            right: 1590.0,
            bottom: 990.0,
        };
        let (horizontal, vertical) = detect_grid(&image, bounds, 20.0);
        assert!(
            horizontal.iter().any(|&y| (y - 500.0).abs() < 2.0),
            "{horizontal:?}"
        );
        assert!(
            vertical.iter().any(|&x| (x - 800.0).abs() < 2.0),
            "{vertical:?}"
        );
    }
}
