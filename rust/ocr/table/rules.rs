//! Local rule segments, with recognized glyphs removed before line detection.
//! Keeping their extent distinguishes partial row borders from merged cells.

use super::{Rect, TextLine};
use image::RgbaImage;

const MAX_RULES: usize = 2048;

#[derive(Clone, Copy, Debug)]
pub(super) struct Rule {
    pub(super) position: f32,
    pub(super) start: f32,
    pub(super) end: f32,
}

#[derive(Debug, Default)]
pub(super) struct RuleSet {
    pub(super) horizontal: Vec<Rule>,
    pub(super) vertical: Vec<Rule>,
}

pub(super) fn detect(image: &RgbaImage, lines: &[&TextLine], bounds: Rect, height: f32) -> RuleSet {
    let margin = height * 2.0;
    let x0 = (bounds.left - margin).clamp(0.0, image.width() as f32) as usize;
    let y0 = (bounds.top - margin).clamp(0.0, image.height() as f32) as usize;
    let x1 = (bounds.right + margin + 1.0).clamp(0.0, image.width() as f32) as usize;
    let y1 = (bounds.bottom + margin + 1.0).clamp(0.0, image.height() as f32) as usize;
    let width = x1.saturating_sub(x0);
    let rows = y1.saturating_sub(y0);
    if width == 0 || rows == 0 {
        return RuleSet::default();
    }
    let mut gray = Vec::with_capacity(width * rows);
    let mut histogram = [0u64; 256];
    for y in y0..y1 {
        for x in x0..x1 {
            let p = image.get_pixel(x as u32, y as u32);
            let luma = (u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) / 256;
            let value = ((luma * u32::from(p[3]) + 255 * (255 - u32::from(p[3]))) / 255) as u8;
            histogram[value as usize] += 1;
            gray.push(value);
        }
    }
    let mut binary = adaptive_mask(&gray, width, rows, height, otsu(&histogram).clamp(20, 200));
    // Erase only the local rectangle. A label in a vertically merged cell may
    // cross a row border's y coordinate while the actual border starts farther
    // right; rejecting that complete y coordinate would merge unrelated rows.
    for line in lines {
        // Subpixel boxes come from rescaling OCR coordinates. Mask only their
        // interior: rounding a top edge of 342.5 down erases the neighboring
        // table border at y=342 and merges the entire row above it.
        let left = line.rect.left.ceil().clamp(x0 as f32, x1 as f32) as usize - x0;
        let right = line.rect.right.floor().clamp(x0 as f32, x1 as f32) as usize - x0;
        let top = line.rect.top.ceil().clamp(y0 as f32, y1 as f32) as usize - y0;
        let bottom = line.rect.bottom.floor().clamp(y0 as f32, y1 as f32) as usize - y0;
        if left >= right || top >= bottom {
            continue;
        }
        for y in top..bottom {
            binary[y * width + left..y * width + right].fill(false);
        }
    }
    let max_gap = (height * 0.5).round().clamp(2.0, 5.0) as usize;
    let max_thickness = (height * 0.8 + 2.0).max(5.0) as usize;
    let minimum_h = ((width as f32 / 8.0).max(height * 3.0).max(20.0)) as usize;
    let minimum_v = ((rows as f32 / 8.0).max(height * 2.5).max(20.0)) as usize;
    let mut detected = RuleSet {
        horizontal: axis_rules(
            &binary,
            width,
            rows,
            false,
            minimum_h,
            max_gap,
            max_thickness,
            x0,
            y0,
        ),
        vertical: axis_rules(
            &binary,
            width,
            rows,
            true,
            minimum_v,
            max_gap,
            max_thickness,
            y0,
            x0,
        ),
    };
    // Colored blocks create one-sided contrast halos. A second, stricter
    // channel finds narrow strokes darker than the background on BOTH sides,
    // including short cell borders previously hidden in those broad halos.
    let distance = (height * 0.4).round().clamp(3.0, 12.0) as usize;
    let short_minimum = (height * 1.2).max(20.0) as usize;
    let strict_h = directional_mask(&gray, &binary, width, rows, false, distance);
    let strict_v = directional_mask(&gray, &binary, width, rows, true, distance);
    let short_h = axis_rules(
        &strict_h,
        width,
        rows,
        false,
        short_minimum,
        max_gap,
        max_thickness,
        x0,
        y0,
    );
    let short_v = axis_rules(
        &strict_v,
        width,
        rows,
        true,
        short_minimum,
        max_gap,
        max_thickness,
        y0,
        x0,
    );
    let supported = |rule: &Rule, strict: &[Rule], length: usize| {
        rule.end - rule.start >= length as f32 * 2.5
            || strict.iter().any(|s| {
                (s.position - rule.position).abs() <= 2.5
                    && (s.end.min(rule.end) - s.start.max(rule.start)).max(0.0)
                        >= (rule.end - rule.start) * 0.72
            })
    };
    detected
        .horizontal
        .retain(|r| supported(r, &short_h, minimum_h));
    detected
        .vertical
        .retain(|r| supported(r, &short_v, minimum_v));
    for &rule in &short_h {
        if rule.end - rule.start >= minimum_h as f32 {
            insert_rule(&mut detected.horizontal, rule, height, max_gap);
        }
    }
    for &rule in &short_v {
        if rule.end - rule.start >= minimum_v as f32 {
            insert_rule(&mut detected.vertical, rule, height, max_gap);
        }
    }
    // Short strokes need both ends attached to an established perpendicular
    // border. This admits small header/footer cells without treating isolated
    // graphic details as an additional global table column.
    for &rule in &short_h {
        if joins_at_ends(rule, &detected.vertical, height) {
            insert_rule(&mut detected.horizontal, rule, height, max_gap);
        }
    }
    for &rule in &short_v {
        if joins_at_ends(rule, &detected.horizontal, height) {
            insert_rule(&mut detected.vertical, rule, height, max_gap);
        }
    }
    detected.horizontal.sort_by(|a, b| {
        a.position
            .total_cmp(&b.position)
            .then(a.start.total_cmp(&b.start))
    });
    detected.vertical.sort_by(|a, b| {
        a.position
            .total_cmp(&b.position)
            .then(a.start.total_cmp(&b.start))
    });
    detected
}

fn directional_mask(
    gray: &[u8],
    base: &[bool],
    width: usize,
    rows: usize,
    vertical: bool,
    distance: usize,
) -> Vec<bool> {
    (0..gray.len())
        .map(|index| {
            if !base[index] {
                return false;
            }
            let (x, y) = (index % width, index / width);
            let (before, after) = if vertical {
                (
                    y * width + x.saturating_sub(distance),
                    y * width + (x + distance).min(width - 1),
                )
            } else {
                (
                    y.saturating_sub(distance) * width + x,
                    (y + distance).min(rows - 1) * width + x,
                )
            };
            u16::from(gray[index]) + 4 < u16::from(gray[before].min(gray[after]))
        })
        .collect()
}

fn joins_at_ends(rule: Rule, perpendicular: &[Rule], height: f32) -> bool {
    let tolerance = (height * 0.35).max(3.0);
    [rule.start, rule.end].into_iter().all(|end| {
        perpendicular.iter().any(|p| {
            (end - p.position).abs() <= tolerance
                && p.start - tolerance <= rule.position
                && rule.position <= p.end + tolerance
        })
    })
}

fn insert_rule(rules: &mut Vec<Rule>, rule: Rule, height: f32, max_gap: usize) {
    let mut merged = rule;
    let mut index = 0;
    while index < rules.len() {
        let existing = rules[index];
        if (existing.position - merged.position).abs() <= (height * 0.2).max(2.0)
            && existing.start.max(merged.start) <= existing.end.min(merged.end) + max_gap as f32
        {
            if existing.end - existing.start > merged.end - merged.start {
                merged.position = existing.position;
            }
            merged.start = merged.start.min(existing.start);
            merged.end = merged.end.max(existing.end);
            rules.swap_remove(index);
        } else {
            index += 1;
        }
    }
    if rules.len() < MAX_RULES {
        rules.push(merged);
    }
}

#[derive(Clone, Copy)]
struct Band {
    first: usize,
    last: usize,
    start: usize,
    end: usize,
    peak_length: usize,
    peak_first: usize,
    peak_last: usize,
}

#[allow(clippy::too_many_arguments)]
fn axis_rules(
    mask: &[bool],
    width: usize,
    rows: usize,
    vertical: bool,
    minimum: usize,
    max_gap: usize,
    max_thickness: usize,
    along_offset: usize,
    across_offset: usize,
) -> Vec<Rule> {
    let (along, across) = if vertical {
        (rows, width)
    } else {
        (width, rows)
    };
    let mut active: Vec<Band> = Vec::new();
    let mut rules = Vec::new();
    for position in 0..=across {
        let mut segments = Vec::new();
        if position < across {
            let values = (0..along).map(|offset| {
                (position.saturating_sub(1)..=(position + 1).min(across - 1)).any(|near| {
                    if vertical {
                        mask[offset * width + near]
                    } else {
                        mask[near * width + offset]
                    }
                })
            });
            let (mut start, mut last, mut gap) = (None, 0, 0);
            for (offset, dark) in values.enumerate() {
                if dark {
                    start.get_or_insert(offset);
                    last = offset;
                    gap = 0;
                } else if start.is_some() {
                    gap += 1;
                    if gap > max_gap {
                        if let Some(begin) = start.take() {
                            if last + 1 - begin >= minimum {
                                segments.push((begin, last + 1));
                            }
                        }
                    }
                }
            }
            if let Some(begin) = start {
                if last + 1 - begin >= minimum {
                    segments.push((begin, last + 1));
                }
            }
        }
        for (start, end) in segments {
            // Neighboring scan lines observe the same stroke. Match by its
            // extent, so two disjoint rules at one position remain separate.
            let matching = active
                .iter()
                .enumerate()
                .filter_map(|(index, band)| {
                    let overlap = end.min(band.end).saturating_sub(start.max(band.start));
                    let shortest = (end - start).min(band.end - band.start);
                    (overlap * 2 >= shortest).then_some(index)
                })
                .collect::<Vec<_>>();
            let mut merged = Band {
                first: position,
                last: position,
                start,
                end,
                peak_length: end - start,
                peak_first: position,
                peak_last: position,
            };
            // A thick stroke may split into shorter runs where text masks
            // intersect it. Keep the entire connected band, including its
            // thickness, instead of accepting its thin peripheral fragments.
            for index in matching.into_iter().rev() {
                let band = active.remove(index);
                merged.first = merged.first.min(band.first);
                merged.start = merged.start.min(band.start);
                merged.end = merged.end.max(band.end);
                if band.peak_length > merged.peak_length {
                    merged.peak_length = band.peak_length;
                    merged.peak_first = band.peak_first;
                    merged.peak_last = band.peak_last;
                } else if band.peak_length == merged.peak_length {
                    merged.peak_first = merged.peak_first.min(band.peak_first);
                    merged.peak_last = merged.peak_last.max(band.peak_last);
                }
            }
            active.push(merged);
        }
        let mut next = Vec::new();
        for band in active {
            if band.last == position {
                next.push(band);
            } else if band.last + 1 - band.first <= max_thickness {
                rules.push(Rule {
                    // A colored header can create a wide contrast halo on one
                    // side of a border. Its longest run locates the stroke,
                    // while the full band thickness still rejects filled bars.
                    position: across_offset as f32
                        + (band.peak_first + band.peak_last) as f32 / 2.0,
                    start: (along_offset + band.start) as f32,
                    end: (along_offset + band.end) as f32,
                });
            }
        }
        if rules.len() + next.len() > MAX_RULES {
            return Vec::new();
        }
        active = next;
    }
    rules.sort_by(|a, b| {
        a.position
            .total_cmp(&b.position)
            .then(a.start.total_cmp(&b.start))
    });
    rules
}

fn adaptive_mask(gray: &[u8], width: usize, rows: usize, height: f32, global: u8) -> Vec<bool> {
    let radius = (height * 0.75).round().clamp(6.0, 24.0) as usize;
    let mut columns = vec![0u32; width];
    for y in 0..(radius + 1).min(rows) {
        for x in 0..width {
            columns[x] += u32::from(gray[y * width + x]);
        }
    }
    let mut prefix = vec![0u64; width + 1];
    let mut binary = vec![false; gray.len()];
    for y in 0..rows {
        if y > 0 {
            for x in 0..width {
                if y + radius < rows {
                    columns[x] += u32::from(gray[(y + radius) * width + x]);
                }
                if y > radius {
                    columns[x] -= u32::from(gray[(y - radius - 1) * width + x]);
                }
            }
        }
        for x in 0..width {
            prefix[x + 1] = prefix[x] + u64::from(columns[x]);
        }
        let window_rows = (y + radius + 1).min(rows) - y.saturating_sub(radius);
        for x in 0..width {
            let left = x.saturating_sub(radius);
            let right = (x + radius + 1).min(width);
            let count = ((right - left) * window_rows) as u64;
            let value = gray[y * width + x];
            let contrast = if value <= global { 2 } else { 5 };
            binary[y * width + x] =
                (u64::from(value) + contrast) * count < prefix[right] - prefix[left];
        }
    }
    binary
}

fn otsu(histogram: &[u64; 256]) -> u8 {
    let total: u64 = histogram.iter().sum();
    let sum: f64 = histogram
        .iter()
        .enumerate()
        .map(|(i, &count)| i as f64 * count as f64)
        .sum();
    let (mut background, mut background_sum, mut best, mut threshold) = (0u64, 0.0, 0.0, 128);
    for (i, &count) in histogram.iter().enumerate() {
        background += count;
        background_sum += i as f64 * count as f64;
        let foreground = total - background;
        if background == 0 || foreground == 0 {
            continue;
        }
        let difference =
            background_sum / background as f64 - (sum - background_sum) / foreground as f64;
        let variance = background as f64 * foreground as f64 * difference * difference;
        if variance > best {
            best = variance;
            threshold = i as u8;
        }
    }
    threshold
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn bounds() -> Rect {
        Rect {
            left: 0.0,
            top: 0.0,
            right: 350.0,
            bottom: 160.0,
        }
    }

    #[test]
    fn local_row_border_survives_label_in_a_merged_neighbor() {
        let mut image = RgbaImage::from_pixel(350, 160, Rgba([255; 4]));
        for x in 100..340 {
            image.put_pixel(x, 80, Rgba([0, 0, 0, 255]));
        }
        let label = TextLine {
            text: "merged".into(),
            rect: Rect {
                left: 10.0,
                top: 72.0,
                right: 80.0,
                bottom: 89.0,
            },
        };
        let rules = detect(&image, &[&label], bounds(), 17.0);
        assert_eq!(rules.horizontal.len(), 1, "{rules:?}");
        assert!((rules.horizontal[0].position - 80.0).abs() < 1.0);
        assert_eq!(rules.horizontal[0].start, 100.0);
    }

    #[test]
    fn repeated_glyph_strokes_are_not_vertical_cell_borders() {
        let mut image = RgbaImage::from_pixel(350, 160, Rgba([255; 4]));
        let mut lines = Vec::new();
        for top in (10..145).step_by(15) {
            for y in top..top + 11 {
                image.put_pixel(80, y, Rgba([0, 0, 0, 255]));
            }
            lines.push(TextLine {
                text: "I".into(),
                rect: Rect {
                    left: 78.0,
                    top: top as f32,
                    right: 84.0,
                    bottom: (top + 11) as f32,
                },
            });
        }
        for y in 5..155 {
            image.put_pixel(170, y, Rgba([232, 232, 232, 255]));
        }
        let refs = lines.iter().collect::<Vec<_>>();
        let rules = detect(&image, &refs, bounds(), 11.0);
        assert_eq!(rules.vertical.len(), 1, "{rules:?}");
        assert!((rules.vertical[0].position - 170.0).abs() < 1.0);
    }

    #[test]
    fn separate_segments_at_the_same_height_keep_their_extent() {
        let mut image = RgbaImage::from_pixel(350, 160, Rgba([255; 4]));
        for x in (10..120).chain(220..340) {
            image.put_pixel(x, 80, Rgba([0, 0, 0, 255]));
        }
        let rules = detect(&image, &[], bounds(), 10.0);
        assert_eq!(rules.horizontal.len(), 2, "{rules:?}");
        assert_eq!(
            (rules.horizontal[0].start, rules.horizontal[0].end),
            (10.0, 120.0)
        );
        assert_eq!(
            (rules.horizontal[1].start, rules.horizontal[1].end),
            (220.0, 340.0)
        );
    }

    #[test]
    fn pale_broken_stepped_rule_is_one_segment() {
        let mut image = RgbaImage::from_pixel(350, 160, Rgba([255; 4]));
        for x in 10..340 {
            if x % 30 < 26 {
                image.put_pixel(x, 80 + x / 120, Rgba([232, 232, 232, 255]));
            }
        }
        let rules = detect(&image, &[], bounds(), 10.0);
        assert_eq!(rules.horizontal.len(), 1, "{rules:?}");
        assert!((rules.horizontal[0].position - 81.0).abs() <= 1.0);
        assert!(rules.horizontal[0].start <= 10.0);
        assert!(rules.horizontal[0].end >= 335.0);
    }

    #[test]
    fn excessive_rule_noise_is_bounded() {
        let width = 80;
        let rows = (MAX_RULES + 1) * 4;
        let mask = (0..width * rows)
            .map(|i| (i / width) % 4 == 0)
            .collect::<Vec<_>>();
        assert!(axis_rules(&mask, width, rows, false, 20, 2, 5, 0, 0).is_empty());
    }

    #[test]
    fn short_cell_borders_are_recovered_without_bridging_merged_rows() {
        let mut image = RgbaImage::from_pixel(350, 600, Rgba([255; 4]));
        for y in [10, 50, 110, 150] {
            for x in 10..340 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        for y in (10..51).chain(110..151) {
            image.put_pixel(180, y, Rgba([0, 0, 0, 255]));
        }
        // An isolated short stroke has no enclosing cell boundaries.
        for y in 250..280 {
            image.put_pixel(80, y, Rgba([0, 0, 0, 255]));
        }
        let rules = detect(
            &image,
            &[],
            Rect {
                left: 0.0,
                top: 0.0,
                right: 350.0,
                bottom: 600.0,
            },
            10.0,
        );
        assert_eq!(rules.vertical.len(), 2, "{rules:?}");
        assert!(rules
            .vertical
            .iter()
            .all(|r| (r.position - 180.0).abs() < 1.0));
        assert!(rules.vertical[0].end < 55.0);
        assert!(rules.vertical[1].start > 105.0);
    }

    #[test]
    fn fractional_text_boxes_do_not_erase_neighboring_rules() {
        let mut image = RgbaImage::from_pixel(350, 160, Rgba([255; 4]));
        for x in 10..340 {
            image.put_pixel(x, 80, Rgba([0, 0, 0, 255]));
        }
        let label = TextLine {
            text: "near border".into(),
            rect: Rect {
                left: 90.5,
                top: 80.5,
                right: 290.5,
                bottom: 94.5,
            },
        };
        let tiny = TextLine {
            text: "tiny".into(),
            rect: Rect {
                left: 10.2,
                top: 20.2,
                right: 10.6,
                bottom: 20.6,
            },
        };
        let detected = detect(&image, &[&label, &tiny], bounds(), 14.0);
        assert!(
            detected
                .horizontal
                .iter()
                .any(|rule| (rule.position - 80.0).abs() <= 1.0
                    && rule.start <= 10.0
                    && rule.end >= 340.0),
            "{detected:?}"
        );

        let mut image = RgbaImage::from_pixel(350, 160, Rgba([255; 4]));
        for y in 10..150 {
            image.put_pixel(180, y, Rgba([0, 0, 0, 255]));
        }
        let label = TextLine {
            text: "near border".into(),
            rect: Rect {
                left: 180.5,
                top: 30.5,
                right: 194.5,
                bottom: 140.5,
            },
        };
        let detected = detect(&image, &[&label], bounds(), 14.0);
        assert!(
            detected
                .vertical
                .iter()
                .any(|rule| (rule.position - 180.0).abs() <= 1.0
                    && rule.start <= 10.0
                    && rule.end >= 150.0),
            "{detected:?}"
        );
    }
}
