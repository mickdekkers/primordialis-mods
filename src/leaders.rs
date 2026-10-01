//! The lines that connect spread out map icons to where their pickups are.

use modkit::game::{CircleRenderInfo, LineRenderInfo, Real2};

use crate::math;

/// Spread out icons are connected to where their pickups are by a dashed line this wide (dashes and
/// the gaps between them this long), ending in a dot this large, in icon radii, and this opaque
/// relative to the icon.
const LEADER_WIDTH: f32 = 0.15;
const LEADER_DOT_RADIUS: f32 = 0.2;
const DASH_LENGTH: f32 = 0.45;
const DASH_GAP: f32 = 0.35;
const LEADER_ALPHA: f32 = 0.6;
/// The spread out icon under the mouse gets a solid line this much wider, a dot this much larger,
/// and this opaque.
const POINTED_LEADER_SCALE: f32 = 1.5;
const POINTED_LEADER_ALPHA: f32 = 1.0;

/// The leader of a spread out icon: a line from where its pickup is to where the icon is drawn, and a
/// dot at its pickup.
pub(crate) struct Leader {
    pub from: Real2,
    pub to: Real2,
    /// Whether the mouse points at the icon: its leader is then solid and bolder, instead of dashed.
    pub pointed: bool,
    /// The icon's opacity from fading around the mouse.
    pub fade: f32,
    /// How far the icon is spread out, times its opacity.
    pub visibility: f32,
}

impl Leader {
    /// Adds the line to `lines` and the dot to `dots`, for icons `radius` large.
    pub(crate) fn add(
        &self,
        radius: f32,
        lines: &mut Vec<LineRenderInfo>,
        dots: &mut Vec<CircleRenderInfo>,
    ) {
        let (from, to) = (self.from, self.to);
        let dot = if self.pointed {
            let alpha = math::lerp(LEADER_ALPHA, POINTED_LEADER_ALPHA, self.fade) * self.visibility;
            let color = [1.0, 1.0, 1.0, alpha];
            let width = LEADER_WIDTH * POINTED_LEADER_SCALE * radius;
            lines.push(LineRenderInfo::new(from, to, width, color));
            (LEADER_DOT_RADIUS * POINTED_LEADER_SCALE, color)
        } else {
            let color = [1.0, 1.0, 1.0, LEADER_ALPHA * self.fade * self.visibility];
            dashed_line(lines, from, to, radius, color);
            (LEADER_DOT_RADIUS, color)
        };
        dots.push(CircleRenderInfo {
            x: [from.x, from.y, 0.0],
            r: dot.0 * radius,
            color: dot.1,
        });
    }
}

/// Adds the dashes of a dashed line from `from` to `to` to `lines`, for an icon `radius` large,
/// starting with a dash at `from`.
fn dashed_line(
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
        let (from, to, radius) = (Real2::new(1.0, 2.0), Real2::new(11.0, 2.0), 2.0);
        let mut lines = Vec::new();
        dashed_line(&mut lines, from, to, radius, [1.0; 4]);
        let period = (DASH_LENGTH + DASH_GAP) * radius;
        // The last dash is cut short at the end.
        assert_eq!(lines.len(), (10.0 / period).ceil() as usize);
        assert!(lines[0].start().same_bits(from));
        let last = lines.len() - 1;
        for (k, line) in lines.iter().enumerate() {
            assert!((line.start().x - (from.x + k as f32 * period)).abs() < 1e-4);
            // Round caps stick out half the width at both ends, within the dash length.
            let drawn = line.end().x - line.start().x + line.width();
            if k < last {
                assert!((drawn - DASH_LENGTH * radius).abs() < 1e-4, "{k}: {drawn}");
            } else {
                assert!(drawn <= DASH_LENGTH * radius + 1e-4, "{k}: {drawn}");
                assert!((line.end().x - to.x).abs() < 1e-4, "cut off at the end");
            }
            assert!(line.start().y == 2.0 && line.end().y == 2.0);
            assert!((line.width() - LEADER_WIDTH * radius).abs() < 1e-6);
        }
    }

    #[test]
    fn leaders_are_solid_and_bolder_when_pointed_at() {
        let mut leader = Leader {
            from: Real2::new(0.0, 0.0),
            to: Real2::new(10.0, 0.0),
            pointed: true,
            fade: 1.0,
            visibility: 0.5,
        };
        let (mut lines, mut dots) = (Vec::new(), Vec::new());
        leader.add(2.0, &mut lines, &mut dots);
        assert_eq!(lines.len(), 1);
        assert!((lines[0].width() - LEADER_WIDTH * POINTED_LEADER_SCALE * 2.0).abs() < 1e-6);
        assert_eq!(lines[0].color()[3], POINTED_LEADER_ALPHA * 0.5);
        assert_eq!(dots.len(), 1);
        assert!((dots[0].r - LEADER_DOT_RADIUS * POINTED_LEADER_SCALE * 2.0).abs() < 1e-6);
        assert_eq!(dots[0].x, [0.0, 0.0, 0.0], "the dot is at the pickup");

        leader.pointed = false;
        leader.fade = 0.5;
        let (mut lines, mut dots) = (Vec::new(), Vec::new());
        leader.add(2.0, &mut lines, &mut dots);
        assert!(lines.len() > 1, "dashed");
        assert_eq!(lines[0].color()[3], LEADER_ALPHA * 0.5 * 0.5);
        assert!((dots[0].r - LEADER_DOT_RADIUS * 2.0).abs() < 1e-6);
    }

    #[test]
    fn dashed_lines_of_no_length_have_no_dashes() {
        let mut lines = Vec::new();
        let at = Real2::new(3.0, 4.0);
        dashed_line(&mut lines, at, at, 1.0, [1.0; 4]);
        dashed_line(&mut lines, at, Real2::new(f32::NAN, 0.0), 1.0, [1.0; 4]);
        dashed_line(&mut lines, at, Real2::new(10.0, 4.0), 0.0, [1.0; 4]);
        assert!(lines.is_empty());
    }
}
