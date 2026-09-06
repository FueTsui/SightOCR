//! Repaint a reusable BGRA preview without changing the frozen capture pixels.
use windows_sys::Win32::Foundation::RECT;

// Match the original Tk selector: a solid, one-physical-pixel #FFFFFF outline.
const BORDER_BGRA: [u8; 4] = [255; 4];

fn intersect(first: RECT, second: RECT) -> Option<RECT> {
    let clipped = RECT {
        left: first.left.max(second.left),
        top: first.top.max(second.top),
        right: first.right.min(second.right),
        bottom: first.bottom.min(second.bottom),
    };
    (clipped.left < clipped.right && clipped.top < clipped.bottom).then_some(clipped)
}

/// Include the old outline when the selection shrinks or crosses its origin.
pub(super) fn damage(previous: RECT, current: RECT) -> RECT {
    RECT {
        left: previous.left.min(current.left).saturating_sub(1),
        top: previous.top.min(current.top).saturating_sub(1),
        right: previous.right.max(current.right).saturating_add(1),
        bottom: previous.bottom.max(current.bottom).saturating_add(1),
    }
}

/// Compose only the damaged rectangle into an existing top-down, 32-bpp DIB.
/// Both buffers have the checked dimensions of their owning BitmapSurface.
/// RECT right/bottom are exclusive, just as for the returned screenshot crop.
pub(super) fn repaint(
    original: &[u8],
    frame: &mut [u8],
    width: i32,
    height: i32,
    selection: Option<RECT>,
    dirty: RECT,
) -> Option<RECT> {
    let canvas = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let dirty = intersect(dirty, canvas)?;
    let stride = width as usize * 4;
    debug_assert_eq!(original.len(), stride * height as usize);
    debug_assert_eq!(frame.len(), original.len());

    // Restore the dark background first, including pixels of any former outline.
    // This happens entirely off-screen; no partially composed layer is presented.
    for y in dirty.top..dirty.bottom {
        let start = y as usize * stride + dirty.left as usize * 4;
        let end = y as usize * stride + dirty.right as usize * 4;
        for (destination, source) in frame[start..end].iter_mut().zip(&original[start..end]) {
            *destination = source / 2;
        }
    }

    if let Some(bounds) = selection.and_then(|bounds| intersect(bounds, canvas)) {
        if let Some(selected_dirty) = intersect(bounds, dirty) {
            for y in selected_dirty.top..selected_dirty.bottom {
                let row = y as usize * stride;
                let start = row + selected_dirty.left as usize * 4;
                let end = row + selected_dirty.right as usize * 4;
                if y == bounds.top || y == bounds.bottom - 1 {
                    for pixel in frame[start..end].as_chunks_mut::<4>().0 {
                        pixel.copy_from_slice(&BORDER_BGRA);
                    }
                } else {
                    frame[start..end].copy_from_slice(&original[start..end]);
                    for x in [bounds.left, bounds.right - 1] {
                        if x >= selected_dirty.left && x < selected_dirty.right {
                            let offset = row + x as usize * 4;
                            frame[offset..offset + 4].copy_from_slice(&BORDER_BGRA);
                        }
                    }
                }
            }
        }
    }
    Some(dirty)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: i32 = 12;
    const HEIGHT: i32 = 9;
    const CANVAS: RECT = rect(0, 0, WIDTH, HEIGHT);

    const fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    fn source() -> Vec<u8> {
        // Synthetic values distinguish every location, channel, and half-bright pixel.
        (0..HEIGHT)
            .flat_map(|y| {
                (0..WIDTH).flat_map(move |x| [21 + x as u8 * 7, 51 + y as u8 * 5, 173, 255])
            })
            .collect()
    }

    fn assert_preview(original: &[u8], frame: &[u8], selection: Option<RECT>) {
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let offset = ((y * WIDTH + x) * 4) as usize;
                let source = &original[offset..offset + 4];
                let actual = &frame[offset..offset + 4];
                let inside = selection.filter(|bounds| {
                    x >= bounds.left && x < bounds.right && y >= bounds.top && y < bounds.bottom
                });
                let expected = match inside {
                    Some(bounds)
                        if x == bounds.left
                            || x == bounds.right - 1
                            || y == bounds.top
                            || y == bounds.bottom - 1 =>
                    {
                        [255; 4]
                    }
                    Some(_) => [source[0], source[1], source[2], source[3]],
                    None => [source[0] / 2, source[1] / 2, source[2] / 2, source[3] / 2],
                };
                assert_eq!(actual, expected, "incorrect preview pixel at ({x}, {y})");
            }
        }
    }

    #[test]
    fn capture_preview_preserves_original_and_matches_legacy_selection() {
        let original = source();
        let frozen = original.clone();
        let mut frame = vec![0; original.len()];
        let selection = Some(rect(2, 1, 10, 7));
        repaint(&original, &mut frame, WIDTH, HEIGHT, selection, CANVAS);
        // Verify every boundary pixel, the original interior, and the dim surroundings.
        assert_preview(&original, &frame, selection);
        assert_eq!(original, frozen, "the screenshot source must stay pristine");
    }

    #[test]
    fn capture_preview_incremental_damage_erases_old_selections() {
        let original = source();
        let mut frame = vec![0; original.len()];
        repaint(&original, &mut frame, WIDTH, HEIGHT, None, CANVAS);
        let allocation = frame.as_ptr();
        let mut previous = rect(5, 4, 5, 4);
        for current in [
            rect(5, 4, 11, 8), // expand toward bottom right
            rect(5, 4, 8, 6),  // shrink, restoring the old interior and border
            rect(1, 0, 5, 4),  // cross the drag origin
            rect(0, 0, 12, 9), // touch every canvas edge
            rect(5, 2, 6, 8),  // one-pixel-wide outline
            rect(2, 4, 10, 5), // one-pixel-high outline
            rect(5, 4, 5, 8),  // zero width: erase the previous selection
            rect(5, 4, 5, 4),  // return to the drag origin
        ] {
            repaint(
                &original,
                &mut frame,
                WIDTH,
                HEIGHT,
                Some(current),
                damage(previous, current),
            );
            assert_preview(&original, &frame, Some(current));
            assert_eq!(frame.as_ptr(), allocation, "dragging must reuse its frame");
            previous = current;
        }
    }

    #[test]
    fn capture_preview_clips_partial_damage_and_coalesced_moves() {
        let original = source();
        let mut frame = vec![0; original.len()];
        repaint(&original, &mut frame, WIDTH, HEIGHT, None, CANVAS);
        // A repaint can cover only part of a border, e.g. after other invalidations.
        let unchanged = frame.clone();
        let first = rect(2, 1, 10, 7);
        let selection = Some(first);
        let dirty = repaint(
            &original,
            &mut frame,
            WIDTH,
            HEIGHT,
            selection,
            rect(-100, 2, 4, 100),
        )
        .unwrap();
        assert_eq!(
            (dirty.left, dirty.top, dirty.right, dirty.bottom),
            (0, 2, 4, HEIGHT)
        );
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if x >= 4 || y < 2 {
                    let offset = ((y * WIDTH + x) * 4) as usize;
                    assert_eq!(&frame[offset..offset + 4], &unchanged[offset..offset + 4]);
                }
            }
        }
        // The rest of the dirty region can arrive in a later paint message.
        repaint(
            &original,
            &mut frame,
            WIDTH,
            HEIGHT,
            selection,
            rect(4, 0, WIDTH, HEIGHT),
        );
        repaint(
            &original,
            &mut frame,
            WIDTH,
            HEIGHT,
            selection,
            rect(0, 0, 4, 2),
        );
        assert_preview(&original, &frame, selection);

        // Several mouse moves can be coalesced before a single WM_PAINT. Painting
        // their combined damage with the final selection must remove every old edge.
        let intermediate = rect(2, 1, 12, 9);
        let final_bounds = rect(2, 1, 4, 3);
        let first_damage = damage(first, intermediate);
        let last_damage = damage(intermediate, final_bounds);
        repaint(
            &original,
            &mut frame,
            WIDTH,
            HEIGHT,
            Some(final_bounds),
            damage(first_damage, last_damage),
        );
        assert_preview(&original, &frame, Some(final_bounds));
        let finished = frame.clone();
        assert!(repaint(
            &original,
            &mut frame,
            WIDTH,
            HEIGHT,
            None,
            rect(-9, -8, -1, -1)
        )
        .is_none());
        assert!(repaint(&original, &mut frame, WIDTH, HEIGHT, None, rect(3, 3, 3, 8)).is_none());
        assert_eq!(
            frame, finished,
            "empty damage must leave the frame unchanged"
        );
    }
}
