//! A tooltip next to the mouse cursor, drawn like the game's own tooltips (those of the cells in the
//! body editor): a dark rounded panel with a white outline, and white text.

use std::ffi::CStr;

use modkit::game::{Font, Frame, Real2, TextParams};

/// The game's tooltip style, in UI units (the screen is 2 high). These are constants in `do_tooltip`'s
/// code (not symbols), taken from the current build.
const PADDING: f32 = 0.03;
const CORNER_RADIUS: f32 = 0.03;
const OUTLINE_WIDTH: f32 = 0.01;
const FILL: [f32; 3] = [0.001, 0.001, 0.001];
const FILL_ALPHA: f32 = 0.9;
const TEXT_SHADOW: f32 = 0.002;

/// From the mouse cursor to the tooltip's nearest corner, in UI units.
const CURSOR_GAP: f32 = 0.04;
/// How long the tooltip takes to fade in, in `frame_number` ticks (120 per second).
const FADE_IN_TICKS: f32 = 10.0;

/// A line of text in the tooltip.
pub struct Line<'a> {
    pub text: &'a CStr,
    /// RGB.
    pub color: [f32; 3],
}

#[derive(Default)]
pub struct Tooltip {
    /// Fades in from 0 to 1 while a tooltip is shown.
    alpha: f32,
    last_frame: Option<i32>,
}

impl Tooltip {
    /// Draws the tooltip with `lines`, next to the mouse cursor at `mouse` (in UI units), faded by
    /// `fade`. Call every frame it's shown; it fades in when it wasn't shown the frame before.
    pub fn draw(
        &mut self,
        frame: &Frame,
        lines: &[Line],
        mouse: Real2,
        fade: f32,
        frame_number: i32,
    ) {
        let ticks = match self.last_frame.replace(frame_number) {
            Some(last) => frame_number.wrapping_sub(last).clamp(0, 30) as f32,
            None => 0.0,
        };
        self.alpha = (self.alpha + ticks / FADE_IN_TICKS).min(1.0);
        let alpha = self.alpha * fade;
        if alpha <= 0.0 || lines.is_empty() {
            return;
        }

        let params = TextParams {
            shadow: TEXT_SHADOW,
            shadow_color: [0.0, 0.0, 0.0, 1.0],
            ..TextParams::default()
        };
        let sizes: Vec<Real2> = lines
            .iter()
            .map(|line| frame.text_size(line.text, Font::Default, &params))
            .collect();
        let width = sizes.iter().map(|size| size.x).fold(0.0, f32::max) + 2.0 * PADDING;
        let height = sizes.iter().map(|size| size.y).sum::<f32>() + 2.0 * PADDING;

        // Below and to the right of the cursor, unless that runs off the screen.
        let mut left = mouse.x + CURSOR_GAP;
        let mut top = mouse.y - CURSOR_GAP;
        if let Some((screen_min, screen_max)) = screen_bounds(frame) {
            if left + width > screen_max.x {
                left = mouse.x - CURSOR_GAP - width;
            }
            if top - height < screen_min.y {
                top = mouse.y + CURSOR_GAP + height;
            }
        }

        let half_size = Real2::new(width / 2.0, height / 2.0);
        let [r, g, b] = FILL;
        frame.draw_ui_panel(
            Real2::new(left + half_size.x, top - half_size.y),
            half_size,
            CORNER_RADIUS,
            OUTLINE_WIDTH,
            [r, g, b, FILL_ALPHA * alpha],
            [1.0, 1.0, 1.0, alpha],
        );
        let mut y = top - PADDING;
        for (line, size) in lines.iter().zip(&sizes) {
            let [r, g, b] = line.color;
            frame.draw_text(
                line.text,
                Real2::new(left + PADDING, y),
                Real2::new(-1.0, 1.0),
                [r, g, b, alpha],
                Font::Default,
                &params,
            );
            y -= size.y;
        }
    }

    /// No tooltip this frame: the next one fades in.
    pub fn hide(&mut self) {
        self.alpha = 0.0;
        self.last_frame = None;
    }
}

/// The screen's bottom left and top right corners, in UI units.
fn screen_bounds(frame: &Frame) -> Option<(Real2, Real2)> {
    let camera = frame.ui_camera();
    let a = camera.unproject(Real2::new(-1.0, -1.0))?;
    let b = camera.unproject(Real2::new(1.0, 1.0))?;
    Some((
        Real2::new(a.x.min(b.x), a.y.min(b.y)),
        Real2::new(a.x.max(b.x), a.y.max(b.y)),
    ))
}
