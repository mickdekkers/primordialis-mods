//! Fading the map icons around the icon under the mouse.

use modkit::game::PickupsId;

use crate::math::TickClock;

/// While the mouse points at an icon, the other icons around the mouse (and their lines and dots on
/// the grid) fade to this opacity, and back once it doesn't.
pub(crate) const UNFOCUSED_ALPHA: f32 = 0.5;
/// Icons ease towards the opacity they should have with a time constant, in `frame_number` ticks (120
/// per second), of the first under the mouse (25 ms), rising with the distance from it to the second
/// at the spotlight's edge and beyond (125 ms, so ~95% of the way there in 375 ms): what the mouse
/// points at responds at once, and the icons around it follow smoothly.
pub(crate) const FADE_TICKS_NEAR: f32 = 3.0;
pub(crate) const FADE_TICKS_FAR: f32 = 15.0;
/// The icons faded are those within this many icon radii of the mouse, easing back to full opacity
/// by the second, so icons further away stay clear.
pub(crate) const SPOTLIGHT_RADIUS: f32 = 2.0;
pub(crate) const SPOTLIGHT_EDGE: f32 = 10.0;

/// How faded each icon is. Every icon eases on its own, so the one the mouse leaves doesn't jump,
/// whether to the next icon (it fades out as that one fades in) or off them all (the rest fade back in
/// to it).
#[derive(Default)]
pub(crate) struct Fades {
    /// The pickup array `alphas` and `targets` are indexed like.
    pickups: Option<PickupsId>,
    /// Opacity of each pickup's icon, from `UNFOCUSED_ALPHA` to 1.
    alphas: Vec<f32>,
    /// The opacity each is easing towards this frame, and its time constant in ticks.
    targets: Vec<(f32, f32)>,
    clock: TickClock,
}

impl Fades {
    /// Starts a frame in which every pickup's icon eases slowly towards full opacity, unless
    /// `set_target` says otherwise before `ease`.
    pub(crate) fn begin(&mut self, pickups: PickupsId, len: usize) {
        if self.pickups != Some(pickups) {
            self.pickups = Some(pickups);
            self.alphas.clear();
            self.alphas.resize(len, 1.0);
        }
        self.targets.clear();
        self.targets.resize(len, (1.0, FADE_TICKS_FAR));
    }

    pub(crate) fn set_target(&mut self, pickup: usize, alpha: f32, ticks: f32) {
        if let Some(target) = self.targets.get_mut(pickup) {
            *target = (alpha, ticks);
        }
    }

    /// Moves every icon's opacity towards its target, exponentially: the same share of the way each
    /// tick, so it slows as it arrives, and a target that keeps moving (as the mouse does) is
    /// followed smoothly.
    pub(crate) fn ease(&mut self, frame_number: i32) {
        let ticks = self.clock.advance(frame_number, 60);
        for (alpha, &(target, time_constant)) in self.alphas.iter_mut().zip(&self.targets) {
            *alpha += (target - *alpha) * (1.0 - (-ticks / time_constant).exp());
        }
    }

    pub(crate) fn get(&self, pickup: usize) -> f32 {
        self.alphas.get(pickup).copied().unwrap_or(1.0)
    }

    pub(crate) fn clear(&mut self) {
        *self = Fades::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eased(ticks: f32, time_constant: f32) -> f32 {
        (-ticks / time_constant).exp()
    }

    #[test]
    fn fades_ease_exponentially_with_ticks() {
        let id = PickupsId::for_tests(0x1000, 3);
        let mut fades = Fades::default();
        fades.begin(id, 3);
        fades.set_target(1, 0.0, 10.0);
        fades.ease(100);
        assert_eq!(fades.get(1), 1.0, "no time has passed on the first frame");

        fades.begin(id, 3);
        fades.set_target(1, 0.0, 10.0);
        fades.ease(110);
        assert!((fades.get(1) - eased(10.0, 10.0)).abs() < 1e-6);
        assert_eq!(fades.get(0), 1.0, "the others stay at full opacity");
        assert_eq!(fades.get(7), 1.0, "unknown pickups are opaque");
    }

    #[test]
    fn fades_skip_at_most_half_a_second() {
        let id = PickupsId::for_tests(0x1000, 1);
        let mut fades = Fades::default();
        let frame = |fades: &mut Fades, frame_number: i32| {
            fades.begin(id, 1);
            fades.set_target(0, 0.0, 60.0);
            fades.ease(frame_number);
        };
        frame(&mut fades, 0);
        frame(&mut fades, 100_000);
        assert!((fades.get(0) - eased(60.0, 60.0)).abs() < 1e-6);
        frame(&mut fades, 50_000);
        assert!(
            (fades.get(0) - eased(60.0, 60.0)).abs() < 1e-6,
            "going back in time changes nothing"
        );
        frame(&mut fades, i32::MAX);
        let before = fades.get(0);
        frame(&mut fades, i32::MIN.wrapping_add(59));
        assert!(
            (fades.get(0) - before * eased(60.0, 60.0)).abs() < 1e-6,
            "the frame number wraps around"
        );
    }

    #[test]
    fn fades_start_over_for_another_pickup_array() {
        let (a, b) = (
            PickupsId::for_tests(0x1000, 2),
            PickupsId::for_tests(0x2000, 2),
        );
        let mut fades = Fades::default();
        for frame_number in [0, 60] {
            fades.begin(a, 2);
            fades.set_target(0, 0.5, 1.0);
            fades.ease(frame_number);
        }
        assert!((fades.get(0) - 0.5).abs() < 1e-3);
        fades.begin(b, 2);
        assert_eq!(fades.get(0), 1.0);
    }
}
