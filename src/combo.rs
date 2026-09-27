//! The ring of rainbow dots drawn around combo pickups on the map, standing in for the particle ring
//! the game shows around them in the world, which only exists near the player. (Their shifting color
//! is the game's: `Game::combo_color`.)

use modkit::game::{CircleRenderInfo, Real2};

/// The ring of dots around combo pickups, in multiples of the icon radius.
const HALO_RADIUS: f32 = 1.7;
const HALO_DOT_RADIUS: f32 = 0.14;
const HALO_DOTS: usize = 16;
/// Clockwise ring rotation, in radians per `frame_number` tick (120 per second): ~20 s per turn.
/// Angles from the frame number are worked out in `f64`: an `f32` only holds tick counts exactly up
/// to 2^24 (39 hours of play), after which they'd move in steps.
const HALO_SPIN: f64 = 0.00265;
/// Dot opacity relative to the icon's.
pub(crate) const HALO_ALPHA: f32 = 0.9;
/// The dots' colors cycle around this gray, this far towards each hue: light and saturated, to stand
/// out on the dark map. Our choice, tuned by eye.
const RAINBOW_BASE: f32 = 0.65;
const RAINBOW_AMPLITUDE: f32 = 0.35;

/// The ring of rainbow dots drawn around combo pickups for the current frame.
pub(crate) struct Halo {
    /// Dot offsets from the pickup's center, and their colors.
    dots: [(Real2, [f32; 3]); HALO_DOTS],
    dot_radius: f32,
}

impl Halo {
    pub(crate) fn new(frame_number: i32, icon_radius: f32) -> Self {
        use std::f32::consts::TAU;
        let spin = (f64::from(frame_number) * HALO_SPIN % std::f64::consts::TAU) as f32;
        let dots = std::array::from_fn(|i| {
            let along = i as f32 / HALO_DOTS as f32;
            // World y points up on screen, so decreasing angles turn clockwise.
            let angle = along * TAU - spin;
            let distance = HALO_RADIUS * icon_radius;
            (
                Real2::new(angle.cos() * distance, angle.sin() * distance),
                rainbow(along),
            )
        });
        Halo {
            dots,
            dot_radius: HALO_DOT_RADIUS * icon_radius,
        }
    }

    pub(crate) fn add(&self, out: &mut Vec<CircleRenderInfo>, center: Real2, alpha: f32) {
        out.extend(
            self.dots
                .iter()
                .map(|&(offset, [r, g, b])| CircleRenderInfo {
                    x: [center.x + offset.x, center.y + offset.y, 0.0],
                    r: self.dot_radius,
                    color: [r, g, b, alpha],
                }),
        );
    }
}

/// A light, saturated rainbow color, `t` going once around the hues from 0 to 1.
fn rainbow(t: f32) -> [f32; 3] {
    use std::f32::consts::TAU;
    [0.0, 2.0 / 3.0, 1.0 / 3.0]
        .map(|phase| RAINBOW_BASE + RAINBOW_AMPLITUDE * ((t + phase) * TAU).cos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn the_halo_rings_the_icon_and_spins_clockwise() {
        let center = Real2::new(10.0, -5.0);
        let mut circles = Vec::new();
        Halo::new(0, 2.0).add(&mut circles, center, 0.5);
        assert_eq!(circles.len(), HALO_DOTS);
        for circle in &circles {
            let offset = Real2::new(circle.x[0] - center.x, circle.x[1] - center.y);
            assert!((offset.x.hypot(offset.y) - HALO_RADIUS * 2.0).abs() < 1e-4);
            assert!((circle.r - HALO_DOT_RADIUS * 2.0).abs() < 1e-6);
            assert_eq!(circle.color[3], 0.5);
        }
        assert!((circles[0].x[0] - center.x - HALO_RADIUS * 2.0).abs() < 1e-4);

        circles.clear();
        Halo::new(10, 2.0).add(&mut circles, center, 0.5);
        assert!(circles[0].x[1] < center.y, "the first dot turned clockwise");
    }

    #[test]
    fn the_halo_keeps_turning_every_tick_late_in_the_game() {
        let halo_angle = |frame_number| {
            let mut circles = Vec::new();
            Halo::new(frame_number, 1.0).add(&mut circles, Real2::default(), 1.0);
            circles[0].x[1].atan2(circles[0].x[0])
        };
        for frame_number in [1_000, 100_000_000, i32::MAX - 1] {
            let turned = halo_angle(frame_number) - halo_angle(frame_number + 1);
            let turned = turned.rem_euclid(TAU);
            assert!(
                (turned - HALO_SPIN as f32).abs() < 1e-4,
                "{frame_number}: {turned}"
            );
        }
    }
}
