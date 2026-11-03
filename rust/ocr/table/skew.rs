//! Straighten a slightly rotated table before reconstruction.
//!
//! A photographed or scanned table is rarely level. Even 0.4° moves a rule by
//! several pixels across a page, which splits borders into pieces and lets row
//! text drift into neighbouring rows. The page angle is measured from the
//! image, then the image and OneOCR's four-point boxes are rotated level
//! together; OCR does not run again.

use super::Rect;
use image::{Rgba, RgbaImage};

/// Corners in reading order: top-left, top-right, bottom-right, bottom-left.
pub(in crate::ocr) type Quad = [(f32, f32); 4];

/// Below this the drift across a page stays within rule tolerance, and
/// resampling would only soften crisp screenshot rules.
const MIN_ANGLE: f32 = 0.12 * std::f32::consts::PI / 180.0;
/// Tilt searched in either direction; more is a rotated page, not a tilted table.
const SEARCH: f32 = 4.0 * std::f32::consts::PI / 180.0;

/// The level image and each quad's upright bounding box, or `None` when the
/// table is already level or its angle cannot be measured reliably.
pub(in crate::ocr) fn straighten(
    image: &RgbaImage,
    quads: &[Quad],
) -> Option<(RgbaImage, Vec<Rect>)> {
    let angle = estimate(image)?;
    if angle.abs() < MIN_ANGLE {
        return None;
    }
    let rotation = Rotation::new(image.width(), image.height(), angle);
    let rects = rotation.boxes(quads);
    Some((rotation.apply(image), rects))
}

/// Only the quads' upright bounding boxes in the levelled frame, for text
/// layout, which needs no levelled pixels.
pub(in crate::ocr) fn level_boxes(image: &RgbaImage, quads: &[Quad]) -> Option<Vec<Rect>> {
    let angle = estimate(image)?;
    if angle.abs() < MIN_ANGLE {
        return None;
    }
    Some(Rotation::new(image.width(), image.height(), angle).boxes(quads))
}

/// Page angle (radians, positive when rows descend to the right) at which the
/// dark pixels' horizontal projection is sharpest. Rules and text rows of a
/// level table each collapse onto a few pixel rows; any tilt smears them. The
/// image itself is measured because OneOCR reports small tilts inconsistently,
/// some lines as rotated boxes and others as level ones.
fn estimate(image: &RgbaImage) -> Option<f32> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let luma: Vec<u8> = image
        .pixels()
        .map(|p| {
            ((u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) / 256) as u8
        })
        .collect();
    let mut histogram = [0usize; 256];
    luma.iter().for_each(|&v| histogram[v as usize] += 1);
    let mut seen = 0;
    let median = histogram
        .iter()
        .position(|&count| {
            seen += count;
            seen * 2 >= luma.len()
        })
        .unwrap_or(255) as i32;
    // Dark themes: ink is lighter than the background.
    let light_ink = median < 110;
    let stride = (luma.len() / 400_000).max(1);
    let ink: Vec<(f32, f32)> = luma
        .iter()
        .enumerate()
        .step_by(stride)
        .filter(|&(_, &v)| {
            let contrast = if light_ink {
                i32::from(v) - median
            } else {
                median - i32::from(v)
            };
            contrast > 60
        })
        .map(|(index, _)| ((index % width) as f32, (index / width) as f32))
        .collect();
    if ink.len() < 200 {
        return None;
    }
    let margin = (width as f32 * SEARCH.sin()).ceil() as usize + 2;
    let mut bins = vec![0f32; height + margin * 2 + 2];
    let mut sharpness = |angle: f32| -> f64 {
        let (sin, cos) = angle.sin_cos();
        bins.iter_mut().for_each(|bin| *bin = 0.0);
        for &(x, y) in &ink {
            // Split each pixel between its two nearest rows so the score varies
            // smoothly with the angle instead of in whole-pixel steps.
            let position = (y * cos - x * sin + margin as f32).max(0.0);
            let row = position.floor() as usize;
            let fraction = position - row as f32;
            if row + 1 < bins.len() {
                bins[row] += 1.0 - fraction;
                bins[row + 1] += fraction;
            }
        }
        bins.iter().map(|&c| f64::from(c) * f64::from(c)).sum()
    };
    let degree = std::f32::consts::PI / 180.0;
    let level = sharpness(0.0);
    let mut best = (0.0f32, level);
    for step in -40..=40 {
        let angle = step as f32 * 0.1 * degree;
        let score = sharpness(angle);
        if score > best.1 {
            best = (angle, score);
        }
    }
    let coarse = best.0;
    for step in -10..=10 {
        let angle = coarse + step as f32 * 0.01 * degree;
        let score = sharpness(angle);
        if score > best.1 {
            best = (angle, score);
        }
    }
    // A marginal gain is noise in an already level image.
    (best.1 > level * 1.03).then_some(best.0)
}

/// Rotation by `-angle` about the image centre into a canvas that holds every
/// source pixel.
struct Rotation {
    cos: f32,
    sin: f32,
    source_center: (f32, f32),
    target_center: (f32, f32),
    size: (u32, u32),
}

impl Rotation {
    fn new(width: u32, height: u32, angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        let (w, h) = (width as f32, height as f32);
        let size = (
            (w * cos + h * sin.abs()).ceil() as u32,
            (w * sin.abs() + h * cos).ceil() as u32,
        );
        Self {
            cos,
            sin,
            source_center: (w / 2.0, h / 2.0),
            target_center: (size.0 as f32 / 2.0, size.1 as f32 / 2.0),
            size,
        }
    }

    fn boxes(&self, quads: &[Quad]) -> Vec<Rect> {
        quads
            .iter()
            .map(|quad| {
                let points = quad.map(|(x, y)| self.forward(x, y));
                Rect {
                    left: points.iter().map(|p| p.0).fold(f32::INFINITY, f32::min),
                    top: points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min),
                    right: points.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max),
                    bottom: points.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max),
                }
            })
            .collect()
    }

    fn forward(&self, x: f32, y: f32) -> (f32, f32) {
        let (u, v) = (x - self.source_center.0, y - self.source_center.1);
        (
            self.cos * u + self.sin * v + self.target_center.0,
            -self.sin * u + self.cos * v + self.target_center.1,
        )
    }

    fn backward(&self, x: f32, y: f32) -> (f32, f32) {
        let (u, v) = (x - self.target_center.0, y - self.target_center.1);
        (
            self.cos * u - self.sin * v + self.source_center.0,
            self.sin * u + self.cos * v + self.source_center.1,
        )
    }

    fn apply(&self, image: &RgbaImage) -> RgbaImage {
        let (width, height) = (image.width() as i64, image.height() as i64);
        // Corners uncovered by the rotation repeat the nearest edge pixel. A flat
        // fill colour would draw a straight artificial edge against a photo's
        // darker border, which reads as a table rule.
        let pixel = |x: i64, y: i64| -> [f32; 4] {
            image
                .get_pixel(x.clamp(0, width - 1) as u32, y.clamp(0, height - 1) as u32)
                .0
                .map(f32::from)
        };
        RgbaImage::from_fn(self.size.0, self.size.1, |x, y| {
            // Sample at pixel centres so a level image maps onto itself.
            let (sx, sy) = self.backward(x as f32 + 0.5, y as f32 + 0.5);
            let (sx, sy) = (sx - 0.5, sy - 0.5);
            let (x0, y0) = (sx.floor(), sy.floor());
            let (fx, fy) = (sx - x0, sy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let [a, b, c, d] = [
                pixel(x0, y0),
                pixel(x0 + 1, y0),
                pixel(x0, y0 + 1),
                pixel(x0 + 1, y0 + 1),
            ];
            Rgba(std::array::from_fn(|i| {
                let top = a[i] + (b[i] - a[i]) * fx;
                let bottom = c[i] + (d[i] - c[i]) * fx;
                (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x: f32, y: f32, length: f32, angle_deg: f32) -> Quad {
        let (s, c) = angle_deg.to_radians().sin_cos();
        let h = 12.0;
        [
            (x, y),
            (x + length * c, y + length * s),
            (x + length * c - h * s, y + length * s + h * c),
            (x - h * s, y + h * c),
        ]
    }

    /// Horizontal rules at the given rows, tilted by `angle_deg` about x=200.
    fn ruled(rows: &[f32], angle_deg: f32) -> RgbaImage {
        let tan = angle_deg.to_radians().tan();
        let mut image = RgbaImage::from_pixel(400, 240, Rgba([255; 4]));
        for &row in rows {
            for x in 20..380 {
                let y = row + (x as f32 - 200.0) * tan;
                image.put_pixel(x, y.round() as u32, Rgba([0, 0, 0, 255]));
            }
        }
        image
    }

    #[test]
    fn level_tables_are_left_untouched() {
        let image = ruled(&[40.0, 80.0, 120.0, 160.0], 0.0);
        let quads = [line(30.0, 50.0, 300.0, 0.0), line(30.0, 90.0, 300.0, 0.3)];
        assert!(straighten(&image, &quads).is_none());
        let blank = RgbaImage::from_pixel(200, 100, Rgba([255; 4]));
        assert!(straighten(&blank, &quads).is_none());
    }

    #[test]
    fn angle_is_measured_in_both_directions() {
        for tilt in [0.4f32, -0.4, 1.5, -2.5] {
            let image = ruled(&[40.0, 80.0, 120.0, 160.0, 200.0], tilt);
            let angle = estimate(&image).unwrap().to_degrees();
            assert!((angle - tilt).abs() < 0.1, "{tilt} measured as {angle}");
        }
    }

    #[test]
    fn tilted_rule_becomes_level_with_its_text() {
        let image = ruled(&[60.0, 120.0, 180.0], 0.8);
        let quads = [line(30.0, 70.0, 300.0, 0.8), line(30.0, 130.0, 300.0, 0.8)];
        let (level, rects) = straighten(&image, &quads).unwrap();
        // Each rule now lies on one or two pixel rows.
        let dark_rows: Vec<u32> = (0..level.height())
            .filter(|&y| {
                (0..level.width())
                    .filter(|&x| level.get_pixel(x, y)[0] < 128)
                    .count()
                    > 250
            })
            .collect();
        assert!((3..=6).contains(&dark_rows.len()), "{dark_rows:?}");
        // Each text box shrinks to about its own height.
        assert!(rects.iter().all(|r| r.bottom - r.top < 14.0), "{rects:?}");
        // The white corners stay white.
        assert_eq!(level.get_pixel(0, 0)[0], 255);
    }
}
