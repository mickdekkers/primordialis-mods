//! The tooltip of the map icon under the mouse: the game's own tooltip for cell pickups, the one it
//! shows for the pickup under the mouse in the world, pointing at the icon the same way.

use modkit::game::{Frame, Pickup, PickupTooltip, Real2};

/// The game slides the tooltip towards the pickup it's for at this rate (the share of the way left
/// per second, exponentially), and points it this many UI units above the pickup's center. These are
/// constants in `render_game`'s code (not symbols), taken from the current build.
const SLIDE_RATE: f32 = 10.0;
const ANCHOR_ABOVE: f32 = 0.1;

#[derive(Default)]
pub struct Tooltip {
    tooltip: PickupTooltip,
    /// Where the tooltip points, in world units: sliding towards the icon it's for.
    anchor: Real2,
}

impl Tooltip {
    /// Shows the tooltip for `pointed`, a pickup and where its icon is (in world units), or fades it
    /// out when that's `None`. Call it every frame while the map is open.
    pub fn draw(&mut self, frame: &Frame, pointed: Option<(Pickup, Real2)>) {
        if let Some((_, at)) = pointed {
            self.anchor = if self.tooltip.is_visible() {
                let stay = (-SLIDE_RATE * frame.dt()).exp();
                Real2::new(
                    at.x + (self.anchor.x - at.x) * stay,
                    at.y + (self.anchor.y - at.y) * stay,
                )
            } else {
                at
            };
        }
        // From the map, through the world camera to the screen, then back through the UI camera.
        let anchor = frame
            .camera()
            .project(self.anchor)
            .and_then(|ndc| frame.ui_camera().unproject(ndc));
        match anchor {
            Some(anchor) => frame.draw_pickup_tooltip(
                &mut self.tooltip,
                pointed.map(|(pickup, _)| pickup),
                Real2::new(anchor.x, anchor.y + ANCHOR_ABOVE),
            ),
            None => self.hide(),
        }
    }

    /// No tooltip: the next one fades in from nothing.
    pub fn hide(&mut self) {
        self.tooltip.hide();
    }
}
