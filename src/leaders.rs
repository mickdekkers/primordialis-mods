//! The lines that connect spread out map icons to where their pickups are.

use modkit::game::{LineRenderInfo, Real2};

/// Spread out icons are connected to where their pickups are by a dashed line this wide (dashes and
/// the gaps between them this long), ending in a dot this large, in icon radii, and this opaque
/// relative to the icon.
pub(crate) const LEADER_WIDTH: f32 = 0.15;
pub(crate) const LEADER_DOT_RADIUS: f32 = 0.2;
const DASH_LENGTH: f32 = 0.45;
const DASH_GAP: f32 = 0.35;
pub(crate) const LEADER_ALPHA: f32 = 0.6;
/// The spread out icon under the mouse gets a solid line this much wider, a dot this much larger,
/// and this opaque.
pub(crate) const POINTED_LEADER_SCALE: f32 = 1.5;
pub(crate) const POINTED_LEADER_ALPHA: f32 = 1.0;

/// Adds the dashes of a dashed line from `from` to `to` to `lines`, for an icon `radius` large,
/// starting with a dash at `from`.
pub(crate) fn dashed_line(
    lines: &mut Vec<LineRenderInfo>,
    from: Real2,
    to: Real2,
    radius: f32,
    color: [f32; 4],
) {
    let length = from.distance(to);
    let width = LEADER_WIDTH * radius;
    let period = (DASH_LENGTH + DASH_GAP) * radius;
    if !(length > 0.0 && period > 0.0) {
        return;
    }
    // Lines have round caps sticking out half their width at both ends, which count towards the
    // dash length.
    let dash = (DASH_LENGTH * radius - width).max(0.0);
    let dashes = ((length / period).ceil() as usize).min(256);
    for k in 0..dashes {
        let start = k as f32 * period;
        let end = (start + dash).min(length);
        lines.push(LineRenderInfo::new(
            from.lerp(to, start / length),
            from.lerp(to, end / length),
            width,
            color,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashed_lines_start_with_a_dash_and_stay_on_the_line() {
        let (from, to, radius) = (Real2::new(1.0, 2.0), Real2::new(11.0, 2.0), 1.0);
        let mut lines = Vec::new();
        dashed_line(&mut lines, from, to, radius, [1.0; 4]);
        let period = (DASH_LENGTH + DASH_GAP) * radius;
        assert_eq!(lines.len(), (10.0 / period).ceil() as usize);
        assert!(lines[0].start().same_bits(from));
        for (k, line) in lines.iter().enumerate() {
            assert!((line.start().x - (from.x + k as f32 * period)).abs() < 1e-4);
            // Round caps stick out half the width at both ends, within the dash length.
            let drawn = line.end().x - line.start().x + line.width();
            assert!(drawn <= DASH_LENGTH * radius + 1e-4, "{k}: {drawn}");
            assert!(line.end().x <= to.x + 1e-4 && line.start().y == 2.0);
            assert!((line.width() - LEADER_WIDTH * radius).abs() < 1e-6);
        }
    }

    #[test]
    fn dashed_lines_of_no_length_have_no_dashes() {
        let mut lines = Vec::new();
        let at = Real2::new(3.0, 4.0);
        dashed_line(&mut lines, at, at, 1.0, [1.0; 4]);
        dashed_line(&mut lines, at, Real2::new(f32::NAN, 0.0), 1.0, [1.0; 4]);
        assert!(lines.is_empty());
    }
}
