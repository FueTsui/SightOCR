//! Keystone correction for photographed tables.
//!
//! A phone held at a slight angle images a rectangular table as a trapezoid:
//! after levelling the rows, the left and right borders still lean in opposite
//! directions (or both one way, a shear). A vertical rule leaning a few degrees
//! is invisible to the axis-aligned rule search, so its columns are lost.
//!
//! The full-width horizontal rules start on the left border and end on the
//! right one; fitting a line through each side measures the lean. Every pixel
//! row is then stretched horizontally so both sides become vertical, and the
//! text boxes move with it.

use super::rules::RuleSet;
use super::{Rect, TextLine};
use image::{Rgba, RgbaImage};

/// Lean (horizontal pixels per pixel of height) below which no correction is
/// worth a resample: under 0.6° a border stays within the rule search.
const MIN_LEAN: f32 = 0.010;
/// Larger leans are not a table photographed at a slight angle.
const MAX_LEAN: f32 = 0.20;

/// The two side borders as `x = intercept + slope * y`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Keystone {
    left: (f32, f32),
    right: (f32, f32),
    /// Where both sides are moved to.
    target: (f32, f32),
}

impl Keystone {
    /// The sides' lean measured from the full-width rules' ends, when it is
    /// clear and consistent.
    pub(super) fn measure(rules: &RuleSet, height: f32) -> Option<Self> {
        let longest = rules
            .horizontal
            .iter()
            .map(|rule| rule.end - rule.start)
            .fold(0.0f32, f32::max);
        let wide: Vec<_> = rules
            .horizontal
            .iter()
            .filter(|rule| rule.end - rule.start >= longest * 0.6)
            .collect();
        if wide.len() < 3 {
            return None;
        }
        let span = wide.last()?.position - wide.first()?.position;
        if span < height * 6.0 {
            return None;
        }
        // Starts may also lie on an inner rule (rows of a merged left column),
        // and a stray line may end anywhere: each side is the outermost
        // straight line through several rule ends.
        let starts: Vec<(f32, f32)> = wide
            .iter()
            .map(|rule| (rule.position, rule.start))
            .collect();
        let ends: Vec<(f32, f32)> = wide.iter().map(|rule| (rule.position, rule.end)).collect();
        let left = fit(&starts, span * 0.5, height, -1.0)?;
        let right = fit(&ends, span * 0.5, height, 1.0)?;
        if left.1.abs().max(right.1.abs()) < MIN_LEAN || left.1.abs().max(right.1.abs()) > MAX_LEAN
        {
            return None;
        }
        // Keep the table's size: its sides move to their positions at mid-height.
        let middle = (wide.first()?.position + wide.last()?.position) / 2.0;
        let target = (left.0 + left.1 * middle, right.0 + right.1 * middle);
        (target.1 - target.0 > height * 4.0).then_some(Self {
            left,
            right,
            target,
        })
    }

    fn sides(&self, y: f32) -> (f32, f32) {
        (
            self.left.0 + self.left.1 * y,
            self.right.0 + self.right.1 * y,
        )
    }

    /// Where a point of the photograph lands in the corrected image.
    fn forward(&self, x: f32, y: f32) -> f32 {
        let (left, right) = self.sides(y);
        self.target.0 + (x - left) * (self.target.1 - self.target.0) / (right - left).max(1.0)
    }

    pub(super) fn line(&self, line: &TextLine) -> TextLine {
        let y = line.rect.center_y();
        TextLine {
            text: line.text.clone(),
            rect: Rect {
                left: self.forward(line.rect.left, y),
                top: line.rect.top,
                right: self.forward(line.rect.right, y),
                bottom: line.rect.bottom,
            },
        }
    }

    /// The corrected image: each row resampled horizontally.
    pub(super) fn apply(&self, image: &RgbaImage) -> RgbaImage {
        let width = image.width() as i64;
        RgbaImage::from_fn(image.width(), image.height(), |x, y| {
            let row = y as f32 + 0.5;
            let (left, right) = self.sides(row);
            let scale = (right - left) / (self.target.1 - self.target.0).max(1.0);
            let source = left + (x as f32 + 0.5 - self.target.0) * scale - 0.5;
            let base = source.floor();
            let fraction = source - base;
            let pixel = |at: i64| image.get_pixel(at.clamp(0, width - 1) as u32, y).0;
            let (a, b) = (pixel(base as i64), pixel(base as i64 + 1));
            Rgba(std::array::from_fn(|i| {
                (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * fraction).round() as u8
            }))
        })
    }
}

/// The outermost line `x = a + b * y` (leftmost for `side` -1, rightmost for
/// +1) through at least three (y, x) points within a few pixels of it that
/// cover at least `extent` vertically, refined by least squares.
fn fit(points: &[(f32, f32)], extent: f32, height: f32, side: f32) -> Option<(f32, f32)> {
    let tolerance = (height * 0.25).max(4.0);
    let middle = points.iter().map(|p| p.0).sum::<f32>() / points.len().max(1) as f32;
    // (line, how far out it lies at mid-height, supporting points)
    let mut candidates: Vec<((f32, f32), f32, usize)> = Vec::new();
    for (i, a) in points.iter().enumerate() {
        for b in &points[i + 1..] {
            if (b.0 - a.0).abs() < extent * 0.3 {
                continue;
            }
            let slope = (b.1 - a.1) / (b.0 - a.0);
            if slope.abs() > MAX_LEAN {
                continue;
            }
            let intercept = a.1 - slope * a.0;
            let inliers: Vec<(f32, f32)> = points
                .iter()
                .copied()
                .filter(|p| (intercept + slope * p.0 - p.1).abs() <= tolerance)
                .collect();
            let Some(line) = least_squares(&inliers, extent) else {
                continue;
            };
            if inliers
                .iter()
                .any(|p| (line.0 + line.1 * p.0 - p.1).abs() > tolerance)
            {
                continue;
            }
            candidates.push((line, side * (line.0 + line.1 * middle), inliers.len()));
        }
    }
    // The border is the outermost line; among lines at about the same place
    // (a vertical border and a slightly tilted line through the same ends),
    // the one through the most ends, then the least tilted.
    let outermost = candidates
        .iter()
        .map(|c| c.1)
        .fold(f32::NEG_INFINITY, f32::max);
    candidates
        .into_iter()
        .filter(|c| c.1 >= outermost - tolerance)
        .max_by(|a, b| a.2.cmp(&b.2).then(b.0 .1.abs().total_cmp(&a.0 .1.abs())))
        .map(|c| c.0)
}

fn least_squares(points: &[(f32, f32)], extent: f32) -> Option<(f32, f32)> {
    if points.len() < 3 {
        return None;
    }
    let (low, high) = points
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.0), hi.max(p.0))
        });
    if high - low < extent {
        return None;
    }
    let n = points.len() as f32;
    let mean_y = points.iter().map(|p| p.0).sum::<f32>() / n;
    let mean_x = points.iter().map(|p| p.1).sum::<f32>() / n;
    let covariance: f32 = points.iter().map(|p| (p.0 - mean_y) * (p.1 - mean_x)).sum();
    let variance: f32 = points.iter().map(|p| (p.0 - mean_y).powi(2)).sum();
    (variance > 0.0).then(|| {
        let slope = covariance / variance;
        (mean_x - slope * mean_y, slope)
    })
}

#[cfg(test)]
mod tests {
    use super::super::rules::Rule;
    use super::*;

    fn trapezoid() -> RuleSet {
        // Left border leans left going down, right border leans right.
        RuleSet {
            horizontal: (0..8)
                .map(|i| {
                    let y = 50.0 + i as f32 * 60.0;
                    Rule {
                        position: y,
                        start: 100.0 - 0.05 * (y - 50.0),
                        end: 700.0 + 0.05 * (y - 50.0),
                    }
                })
                .collect(),
            vertical: Vec::new(),
        }
    }

    #[test]
    fn leaning_sides_become_vertical_and_text_moves_with_them() {
        let keystone = Keystone::measure(&trapezoid(), 20.0).unwrap();
        let top = keystone.line(&TextLine {
            text: "a".into(),
            rect: Rect {
                left: 100.0,
                top: 40.0,
                right: 120.0,
                bottom: 60.0,
            },
        });
        let bottom = keystone.line(&TextLine {
            text: "b".into(),
            rect: Rect {
                left: 79.0,
                top: 460.0,
                right: 99.0,
                bottom: 480.0,
            },
        });
        // Both sit at the left border; they now share one x.
        assert!(
            (top.rect.left - bottom.rect.left).abs() < 1.0,
            "{:?} {:?}",
            top.rect,
            bottom.rect
        );
    }

    #[test]
    fn a_stray_line_and_inner_rules_do_not_hide_the_borders() {
        let mut rules = trapezoid();
        // A photo edge above the table, and rules that start on an inner column.
        rules.horizontal.insert(
            0,
            Rule {
                position: 5.0,
                start: 0.0,
                end: 420.0,
            },
        );
        for i in 0..5 {
            let y = 80.0 + i as f32 * 60.0;
            rules.horizontal.push(Rule {
                position: y,
                start: 300.0 - 0.02 * y,
                end: 700.0 + 0.05 * (y - 50.0),
            });
        }
        rules
            .horizontal
            .sort_by(|a, b| a.position.total_cmp(&b.position));
        let keystone = Keystone::measure(&rules, 20.0).unwrap();
        assert!((keystone.left.1 + 0.05).abs() < 0.005, "{keystone:?}");
        assert!((keystone.right.1 - 0.05).abs() < 0.005, "{keystone:?}");
    }

    #[test]
    fn straight_or_rotated_tables_are_left_alone() {
        let straight = RuleSet {
            horizontal: (0..6)
                .map(|i| Rule {
                    position: 50.0 + i as f32 * 60.0,
                    start: 100.0,
                    end: 700.0,
                })
                .collect(),
            vertical: Vec::new(),
        };
        assert!(Keystone::measure(&straight, 20.0).is_none());
        // Inconsistent ends (different tables, ragged rules) are not a border.
        let ragged = RuleSet {
            horizontal: [(50.0, 100.0), (110.0, 160.0), (170.0, 90.0), (230.0, 150.0)]
                .iter()
                .map(|&(y, start)| Rule {
                    position: y,
                    start,
                    end: 700.0,
                })
                .collect(),
            vertical: Vec::new(),
        };
        assert!(Keystone::measure(&ragged, 20.0).is_none());
    }

    #[test]
    fn corrected_image_straightens_a_leaning_vertical_line() {
        let mut image = RgbaImage::from_pixel(800, 500, Rgba([255; 4]));
        for y in 50..470u32 {
            let x = 100.0 - 0.05 * (y as f32 - 50.0);
            image.put_pixel(x.round() as u32, y, Rgba([0, 0, 0, 255]));
        }
        let keystone = Keystone::measure(&trapezoid(), 20.0).unwrap();
        let corrected = keystone.apply(&image);
        let columns: Vec<u32> = (50..470)
            .map(|y| {
                (0..800)
                    .min_by_key(|&x| corrected.get_pixel(x, y)[0])
                    .unwrap()
            })
            .collect();
        let (min, max) = (columns.iter().min().unwrap(), columns.iter().max().unwrap());
        assert!(max - min <= 2, "{min}..{max}");
    }
}
