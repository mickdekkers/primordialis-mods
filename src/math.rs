//! Easing and timing shared by the features.

/// The value `t` of the way from `a` to `b`: `a` at 0, `b` at 1.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Eases from 0 at `edge0` to 1 at `edge1`, smoothly at both ends, and stays there beyond them.
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Counts the `frame_number` ticks (120 per second, whatever the frame rate) between calls, for
/// animations.
#[derive(Default)]
pub struct TickClock {
    last: Option<i32>,
}

impl TickClock {
    /// The ticks since the last call: none on the first, or if the frame number went back, and at
    /// most `max`, so an animation doesn't jump after the game was paused or the map closed.
    pub fn advance(&mut self, frame_number: i32, max: i32) -> f32 {
        match self.last.replace(frame_number) {
            Some(last) => frame_number.wrapping_sub(last).clamp(0, max) as f32,
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothstep_eases_between_its_edges() {
        assert_eq!(smoothstep(2.0, 4.0, 1.0), 0.0);
        assert_eq!(smoothstep(2.0, 4.0, 3.0), 0.5);
        assert_eq!(smoothstep(2.0, 4.0, 9.0), 1.0);
        assert!(smoothstep(2.0, 4.0, 2.2) < 0.1, "slow at the start");
    }

    #[test]
    fn the_clock_counts_ticks_up_to_a_maximum() {
        let mut clock = TickClock::default();
        assert_eq!(clock.advance(100, 30), 0.0);
        assert_eq!(clock.advance(104, 30), 4.0);
        assert_eq!(clock.advance(1_000, 30), 30.0);
        assert_eq!(clock.advance(900, 30), 0.0);
        clock.advance(i32::MAX, 30);
        assert_eq!(clock.advance(i32::MIN + 2, 30), 3.0, "wraps around");
    }
}
