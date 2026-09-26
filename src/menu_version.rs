//! Shows the mod's version in the main and pause menus, where the game shows its own: the game's
//! version moves up a line to make room, so the corner reads "Primordialis v0.2 beta" over
//! "QoL mod v0.1.0", both starting at the same place.

use std::ffi::CStr;

use modkit::Feature;
use modkit::game::{MenuButton, Real2};

const LABEL: &CStr = match CStr::from_bytes_with_nul(
    concat!("QoL mod v", env!("CARGO_PKG_VERSION"), "\0").as_bytes(),
) {
    Ok(label) => label,
    Err(_) => panic!("the version contains a NUL"),
};

/// How far apart the two lines are, as a multiple of the text's height. Our choice, tuned by eye.
const LINE_SPACING: f32 = 1.2;

#[derive(Default)]
pub struct MenuVersion;

impl Feature for MenuVersion {
    fn name(&self) -> &'static str {
        "menu version"
    }

    fn menu_button(&mut self, button: &mut MenuButton) {
        if !button.is_game_version() {
            return;
        }
        let (at, size) = (button.center(), button.text_size());
        button.move_to(Real2::new(at.x, at.y + size.y * LINE_SPACING));
        // The game centers the button's text: start where it starts.
        button.add_label(Real2::new(at.x - size.x / 2.0, at.y), LABEL);
    }
}
