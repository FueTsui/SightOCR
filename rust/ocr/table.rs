//! Table reconstruction combining local image contrast and text geometry.
//! This deliberately has no OpenCV, NumPy or clustering runtime dependency.

use super::BoundingBox;
use anyhow::{ensure, Result};
use image::RgbaImage;

mod rules;
use rules::{Rule, RuleSet};

const MAX_TABLE_CELLS: usize = 262_144;

#[derive(Clone, Copy, Debug)]
pub(super) struct Rect {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
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
    let mut ordered: Vec<_> = lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .collect();
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
    if let Some(frame) = enclosing_frame(&rules, bounds) {
        // A page number outside a closed table is not an extra data row.
        ordered.retain(|line| {
            let x = (line.rect.left + line.rect.right) / 2.0;
            x >= frame.left
                && x <= frame.right
                && line.rect.center_y() >= frame.top
                && line.rect.center_y() <= frame.bottom
        });
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
    }
    let text_rows = group_text_rows(&ordered, height);
    let physical_columns = positions(&rules.vertical, bounds.left, bounds.right);
    // Infer missing separators even when some physical borders were found.
    // Use text baselines here; sorting a multiline cell by x creates false gaps.
    let mut columns = infer_columns(&text_rows, height, &physical_columns);
    let mut cuts = row_boundaries(&ordered, &columns, &rules, bounds, height);
    if (cuts.len() + 1).saturating_mul(columns.len() + 1) > MAX_TABLE_CELLS {
        // A textured image can generate thousands of plausible rules. Fall
        // back to the at-most-1000 OCR lines before allocating a dense grid.
        columns = infer_columns(&text_rows, height, &[]);
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
        let cell = &mut grid[row][col];
        if !cell.is_empty() {
            cell.push(' ');
        }
        // Embedded tabs/newlines must not manufacture extra TSV cells.
        cell.push_str(&line.text.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    // Preserve empty interior columns indicated by physical borders. Removing
    // them shifts every following value left when pasted into a spreadsheet.
    let populated: Vec<_> = (0..columns.len() + 1)
        .filter(|&c| grid.iter().any(|r| !r[c].is_empty()))
        .collect();
    grid.iter()
        .filter(|row| row.iter().any(|cell| !cell.is_empty()))
        .map(|row| {
            (*populated.first().unwrap()..=*populated.last().unwrap())
                .map(|c| row[c].as_str())
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy)]
struct RowBoundary {
    position: f32,
    inferred: bool,
}

fn enclosing_frame(rules: &RuleSet, bounds: Rect) -> Option<Rect> {
    let wide: Vec<_> = rules
        .horizontal
        .iter()
        .filter(|rule| rule.end - rule.start >= (bounds.right - bounds.left) * 0.8)
        .collect();
    let first = wide.first()?;
    let last = wide.last()?;
    if last.position - first.position < bounds.height() * 0.7 {
        return None;
    }
    let left = first.start.max(last.start);
    let right = first.end.min(last.end);
    for x in [left, right] {
        if !rules.vertical.iter().any(|rule| {
            (rule.position - x).abs() <= 4.0
                && rule.start <= first.position + 4.0
                && rule.end >= last.position - 4.0
        }) {
            return None;
        }
    }
    Some(Rect {
        left: left - 3.0,
        right: right + 3.0,
        top: first.position - 3.0,
        bottom: last.position + 3.0,
    })
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
                .any(|line| position > line.rect.top && position < line.rect.bottom)
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
    let typical_band = horizontal
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|&gap| gap >= height * 1.5)
        .fold(f32::INFINITY, f32::min);
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
    gaps.retain(|gap| !physical.iter().any(|&x| gap.left <= x && x <= gap.right));
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
