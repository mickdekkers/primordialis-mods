//! The game's menu buttons: the main and pause menus draw theirs with `do_text_button`. Features can
//! move one before the game draws it, and add labels next to it in the same font.

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;

use super::bindings::MenuBindings;
use super::read;
use super::types::{Real2, Real3};

/// `void draw_text(char*, float x, float y, real_4 color, real_2 align, font_info*, text_params*)`.
/// The x64 ABI passes the 16-byte `real_4` by reference and the 8-byte `real_2` by value in a
/// register-sized slot, which is what these Rust types produce as well.
type DrawText = unsafe extern "C" fn(
    *const c_char,
    f32,
    f32,
    *const [f32; 4],
    Real2,
    *const c_void,
    *const c_void,
);
/// `real_2 get_text_size(char*, font_info, text_params)`: the width and height of the text as
/// `draw_text` draws it. Both structs are passed by reference to a copy, and the `real_2` comes back
/// in `rax`, as Rust does for these types.
type GetTextSize = unsafe extern "C" fn(*const c_char, *const c_void, *const c_void) -> Real2;

/// `draw_text`'s alignment for text that starts at its position, centered vertically on it.
const LEFT_ALIGNED: Real2 = Real2::new(-1.0, 0.0);

/// A text button the game is about to draw in a menu.
pub struct MenuButton<'a> {
    text: &'a CStr,
    text_size: Real2,
    is_game_version: bool,
    position: Real3,
    labels: Vec<(Real2, CString)>,
}

impl<'a> MenuButton<'a> {
    pub(crate) fn new(
        bindings: &MenuBindings,
        text: &'a CStr,
        text_size: Real2,
        position: Real3,
    ) -> Self {
        // SAFETY: The global `char* version_string`.
        let version: usize = unsafe { read(bindings.version_string) };
        MenuButton {
            text,
            text_size,
            is_game_version: text.as_ptr() as usize == version,
            position,
            labels: Vec::new(),
        }
    }

    pub fn text(&self) -> &CStr {
        self.text
    }

    /// The width and height of its text as the game draws it, centered on the button, in UI units
    /// (see `Frame::mouse`).
    pub fn text_size(&self) -> Real2 {
        self.text_size
    }

    /// Whether this is the game's version, which the main and pause menus show in their bottom-left
    /// corner.
    pub fn is_game_version(&self) -> bool {
        self.is_game_version
    }

    /// Its center, in UI units.
    pub fn center(&self) -> Real2 {
        Real2::new(self.position.x, self.position.y)
    }

    /// Moves the button: where the game draws it, and where it can be clicked.
    pub fn move_to(&mut self, center: Real2) {
        self.position.x = center.x;
        self.position.y = center.y;
    }

    /// Draws `text` after the button, in the font, color and shadow the game draws button text with:
    /// starting at `at.x`, and centered vertically on `at.y`. Unlike a button, it can't be clicked or
    /// selected with a gamepad.
    pub fn add_label(&mut self, at: Real2, text: &CStr) {
        self.labels.push((at, text.to_owned()));
    }

    /// Where the game should draw the button.
    pub(crate) fn position(&self) -> Real3 {
        self.position
    }
}

/// The width and height of `text` in the buttons' font, as `draw_text` draws it.
///
/// # Safety
///
/// On the thread drawing the menu, with the `render_context*` the game draws the button with.
pub(crate) unsafe fn text_size(
    bindings: &MenuBindings,
    render_context: usize,
    text: &CStr,
) -> Real2 {
    // SAFETY: `render_context` has a `font_info` default font at this offset, `default_shadow` is
    // the game's `text_params`, and `get_text_size` gets its own copies of both, as it does from the
    // game, as guaranteed by the caller.
    unsafe {
        let font = copy(
            render_context + bindings.rc_default_font,
            bindings.font_info_size,
        );
        let params = copy(bindings.default_shadow, bindings.text_params_size);
        let get_text_size: GetTextSize = std::mem::transmute(bindings.get_text_size);
        get_text_size(text.as_ptr(), font.as_ptr().cast(), params.as_ptr().cast())
    }
}

/// Draws the labels features added to `button`, once the game has drawn it.
///
/// # Safety
///
/// On the thread drawing the menu, right after `do_text_button` drew the button with
/// `render_context`, so that the menu's framebuffer is still bound.
pub(crate) unsafe fn draw_labels(
    bindings: &MenuBindings,
    render_context: usize,
    button: &MenuButton,
) {
    if button.labels.is_empty() {
        return;
    }
    // SAFETY: `render_context` has a `real_4` foreground color and a `font_info` default font at
    // these offsets, `default_shadow` is the game's `text_params`, and `draw_text` is called the way
    // `do_text_button` calls it, as guaranteed by the caller.
    unsafe {
        let color: [f32; 4] = read(render_context + bindings.rc_foreground_color);
        // Buttons draw their text with a copy of the default font; so do labels.
        let font = copy(
            render_context + bindings.rc_default_font,
            bindings.font_info_size,
        );
        let draw_text: DrawText = std::mem::transmute(bindings.draw_text);
        for (at, text) in &button.labels {
            draw_text(
                text.as_ptr(),
                at.x,
                at.y,
                &color,
                LEFT_ALIGNED,
                font.as_ptr().cast(),
                bindings.default_shadow as *const c_void,
            );
        }
    }
}

/// A copy of `size` bytes of game memory, aligned for the pointers and floats of the game's structs.
///
/// # Safety
///
/// `address` must be readable for `size` bytes.
unsafe fn copy(address: usize, size: usize) -> Vec<u64> {
    let mut buffer = vec![0u64; size.div_ceil(size_of::<u64>())];
    // SAFETY: Guaranteed by the caller, and the buffer holds at least `size` bytes.
    unsafe { ptr::copy_nonoverlapping(address as *const u8, buffer.as_mut_ptr().cast(), size) };
    buffer
}
