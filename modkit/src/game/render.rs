//! Rendering: the frame being rendered, its stages, and drawing with the game's own renderers.

use std::ffi::{CStr, c_char, c_void};

use super::types::{
    CircleRenderInfo, FontInfo, IconRenderInfo, LineRenderInfo, Real2, Real4x4, TextParams,
};
use super::{Game, read};

/// `void draw_cell_icons(icon_render_info*, int, real_4x4, real_2)`. The x64 ABI passes the 64-byte
/// matrix by reference and the 8-byte `real_2` by value in a register, which is what these Rust types
/// produce as well.
type DrawCellIcons = unsafe extern "C" fn(*const IconRenderInfo, i32, *const Real4x4, Real2);
/// `void draw_circles(circle_render_info*, int, real_4x4)`, with the matrix passed by reference.
type DrawCircles = unsafe extern "C" fn(*const CircleRenderInfo, i32, *const Real4x4);
/// `void draw_lines(render_context*, line_render_info*, int)`.
type DrawLines = unsafe extern "C" fn(*const c_void, *const LineRenderInfo, i32);
/// `draw_lines` uploads the lines into the game's general-purpose vertex buffer, after 0x30 bytes of
/// quad vertices, without checking that they fit. The buffer is 16 MiB: a constant in
/// `gl_init_buffers`' code (not a symbol), taken from the current build. More lines are drawn in
/// batches of this many.
const MAX_LINES_PER_DRAW: usize = (0x100_0000 - 0x30) / size_of::<LineRenderInfo>();
/// `void draw_line(render_context*, real_2 from, real_2 delta, float radius, real_4* color)`: the
/// second point is relative to the first (`s` in `line.glsl`), and the width is given as half of it,
/// the distance from the line's middle to its edges (which is also how far its round caps stick out).
type DrawLine = unsafe extern "C" fn(*const c_void, Real2, Real2, f32, *const [f32; 4]);
/// `void draw_rounded_rectangle_outlined(render_context*, real_3 center, real_2 half_size,
/// float corner_radius, float outline_width, real_4* fill, real_4* outline)`, with the `real_3`
/// passed by reference.
type DrawRoundedRectangleOutlined = unsafe extern "C" fn(
    *const c_void,
    *const [f32; 3],
    Real2,
    f32,
    f32,
    *const [f32; 4],
    *const [f32; 4],
);
/// `void draw_text(char*, float x, float y, real_4 color, real_2 align, font_info*, text_params*)`,
/// with the `real_4` passed by reference.
type DrawText = unsafe extern "C" fn(
    *const c_char,
    f32,
    f32,
    *const [f32; 4],
    Real2,
    *const FontInfo,
    *const TextParams,
);
/// `real_2 get_text_size(char*, font_info, text_params)`, with both structs passed by reference to
/// copies the callee may change.
type GetTextSize = unsafe extern "C" fn(*const c_char, *mut FontInfo, *mut TextParams) -> Real2;

/// A rendering stage of `render_game`, named by the game's profiler markers (`begin_trace_stage`).
/// Stages run one after another; a feature is told when each begins and ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage<'a>(&'a CStr);

impl Stage<'static> {
    /// Draws the Echolocation mutation's markers on the map.
    pub const RACING_OVERLAY: Stage<'static> = Stage(c"racing_overlay");
    /// Draws menus. When it begins, the UI framebuffer is bound, and the game has just drawn its map
    /// markers above the fog of war: the place to draw more of them.
    pub const MENUS: Stage<'static> = Stage(c"menus");
    /// Queues the cell pickups near the camera to be drawn in the world.
    pub const CELL_PICKUPS: Stage<'static> = Stage(c"cell pickups");
}

impl<'a> Stage<'a> {
    pub(crate) fn new(name: &'a CStr) -> Self {
        Stage(name)
    }

    pub fn name(&self) -> &'a CStr {
        self.0
    }
}

/// A font of the UI render context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    Small,
    /// The one menus and tooltips use.
    Default,
    Medium,
    Big,
}

/// The frame `render_game` is rendering.
pub struct Frame<'a> {
    game: Game<'a>,
    /// The world and UI `render_context*`.
    render_context: usize,
    ui_render_context: usize,
    /// The frame's `user_input*`, or 0.
    input: usize,
}

impl<'a> Frame<'a> {
    /// # Safety
    ///
    /// As `Game::new`, on the render thread inside `render_game`, called with these world and UI
    /// `render_context*`s and `user_input*` (which may be null).
    pub(crate) unsafe fn new(
        game: Game<'a>,
        render_context: usize,
        ui_render_context: usize,
        input: usize,
    ) -> Self {
        Frame {
            game,
            render_context,
            ui_render_context,
            input,
        }
    }

    pub fn game(&self) -> &Game<'a> {
        &self.game
    }

    /// The world camera this frame is rendered with.
    pub fn camera(&self) -> Camera {
        self.camera_of(self.render_context)
    }

    /// The camera the UI is drawn with, from UI units (see `mouse`) to the screen.
    pub fn ui_camera(&self) -> Camera {
        self.camera_of(self.ui_render_context)
    }

    fn camera_of(&self, render_context: usize) -> Camera {
        let bindings = self.game.bindings;
        // SAFETY: `render_context.camera` (a `real_4x4`) and `render_context.camera_pos` (a `real_3`)
        // of one of the frame's render contexts.
        unsafe {
            Camera {
                matrix: read(render_context + bindings.rc_camera),
                position: read(render_context + bindings.rc_camera_pos),
            }
        }
    }

    /// Where the mouse cursor is, in UI units: the screen's height spans -1 (bottom) to 1 (top) around
    /// its center, with the same scale horizontally. `None` if the game has no input for this frame.
    pub fn mouse(&self) -> Option<Real2> {
        if self.input == 0 {
            return None;
        }
        // SAFETY: `user_input.mouse`, a `real_2`, of the frame's input.
        let mouse: Real2 = unsafe { read(self.input + self.game.bindings.input_mouse) };
        mouse.is_finite().then_some(mouse)
    }

    /// Draws cell icons with the game's icon renderer, into the framebuffer bound at the current
    /// stage, with the frame's camera. Icons are shaded as if lit from `light`, a world position.
    pub fn draw_cell_icons(&self, icons: &[IconRenderInfo], light: Real2) {
        let Some(count) = draw_count(icons) else {
            return;
        };
        let camera = self.camera().matrix;
        // SAFETY: The game's `draw_cell_icons`, called on the render thread with `count` icons.
        unsafe {
            let draw: DrawCellIcons = std::mem::transmute(self.game.bindings.draw_cell_icons);
            draw(icons.as_ptr(), count, &camera, light);
        }
    }

    /// Draws filled circles with the game's circle renderer, into the framebuffer bound at the
    /// current stage, with the frame's camera.
    pub fn draw_circles(&self, circles: &[CircleRenderInfo]) {
        let Some(count) = draw_count(circles) else {
            return;
        };
        let camera = self.camera().matrix;
        // SAFETY: The game's `draw_circles`, called on the render thread with `count` circles.
        unsafe {
            let draw: DrawCircles = std::mem::transmute(self.game.bindings.draw_circles);
            draw(circles.as_ptr(), count, &camera);
        }
    }
}

/// Drawing lines, and the UI.
impl Frame<'_> {
    /// Draws lines with the game's line renderer, into the framebuffer bound at the current stage,
    /// with the frame's camera.
    pub fn draw_lines(&self, lines: &[LineRenderInfo]) {
        for batch in lines.chunks(MAX_LINES_PER_DRAW) {
            let Some(count) = draw_count(batch) else {
                return;
            };
            // SAFETY: The game's `draw_lines`, called on the render thread with the world render
            // context, of which it only reads the camera, and `count` lines, few enough to fit its
            // vertex buffer.
            unsafe {
                let draw: DrawLines = std::mem::transmute(self.game.bindings.draw_lines);
                draw(self.render_context as *const c_void, batch.as_ptr(), count);
            }
        }
    }

    /// Draws a line between two world positions, `width` world units wide with round caps, with the
    /// frame's camera.
    pub fn draw_line(&self, from: Real2, to: Real2, width: f32, color: [f32; 4]) {
        let delta = Real2::new(to.x - from.x, to.y - from.y);
        // SAFETY: The game's `draw_line`, called on the render thread with the world render context,
        // of which it only reads the camera and resolution.
        unsafe {
            let draw: DrawLine = std::mem::transmute(self.game.bindings.draw_line);
            draw(
                self.render_context as *const c_void,
                from,
                delta,
                width / 2.0,
                &color,
            );
        }
    }

    /// Draws a filled rectangle with rounded corners and an outline, like the game's tooltips, in UI
    /// units.
    pub fn draw_ui_panel(
        &self,
        center: Real2,
        half_size: Real2,
        corner_radius: f32,
        outline_width: f32,
        fill: [f32; 4],
        outline: [f32; 4],
    ) {
        // SAFETY: The game's `draw_rounded_rectangle_outlined`, called on the render thread with the
        // UI render context, of which it only reads the camera and resolution.
        unsafe {
            let draw: DrawRoundedRectangleOutlined =
                std::mem::transmute(self.game.bindings.draw_rounded_rectangle_outlined);
            draw(
                self.ui_render_context as *const c_void,
                &[center.x, center.y, 0.0],
                half_size,
                corner_radius,
                outline_width,
                &fill,
                &outline,
            );
        }
    }

    /// The width and height `text` takes when drawn with `draw_text`, in UI units.
    pub fn text_size(&self, text: &CStr, font: Font, params: &TextParams) -> Real2 {
        let (mut font, mut params) = (self.font(font), *params);
        // SAFETY: The game's `get_text_size`, which reads the NUL-terminated text, with our copies of
        // the font and parameters.
        let size = unsafe {
            let size: GetTextSize = std::mem::transmute(self.game.bindings.get_text_size);
            size(text.as_ptr(), &mut font, &mut params)
        };
        if size.is_finite() {
            size
        } else {
            Real2::default()
        }
    }

    /// Draws `text` at `position`, in UI units. `align` is which point of the text's box goes at
    /// `position`, from -1 to 1 on each axis: (-1, 1) puts its top left corner there.
    pub fn draw_text(
        &self,
        text: &CStr,
        position: Real2,
        align: Real2,
        color: [f32; 4],
        font: Font,
        params: &TextParams,
    ) {
        let font = self.font(font);
        // SAFETY: The game's `draw_text`, called on the render thread (in a stage where the game
        // draws text itself), with the NUL-terminated text, and our copies of the font and
        // parameters.
        unsafe {
            let draw: DrawText = std::mem::transmute(self.game.bindings.draw_text);
            draw(
                text.as_ptr(),
                position.x,
                position.y,
                &color,
                align,
                &font,
                params,
            );
        }
    }

    fn font(&self, font: Font) -> FontInfo {
        let offset = self.game.bindings.rc_fonts[font as usize];
        // SAFETY: One of the `font_info`s of the UI render context, whose size `bindings` checked.
        unsafe { read(self.ui_render_context + offset) }
    }
}

/// How many items to pass to a draw function: `None` if there are none, or too many.
fn draw_count<T>(items: &[T]) -> Option<i32> {
    i32::try_from(items.len()).ok().filter(|&count| count > 0)
}

/// A camera: where it is, and how it projects the world onto the screen.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// World to clip space.
    pub matrix: Real4x4,
    pub position: [f32; 3],
}

impl Camera {
    /// How many world units half the screen's height spans, around what the camera looks at: for
    /// sizing things so that they look the same at any zoom level. `None` if the camera is degenerate.
    pub fn world_units_per_half_screen(&self) -> Option<f32> {
        let [x, y, _] = self.position;
        let here = self.project(Real2::new(x, y))?;
        let above = self.project(Real2::new(x, y + 1.0))?;
        let units = 1.0 / (above.y - here.y).abs();
        (units.is_finite() && units > 0.0).then_some(units)
    }

    /// Projects a point on the ground plane (z = 0) to normalized device coordinates (-1 to 1 across
    /// the screen). `None` if it's behind the camera.
    pub fn project(&self, point: Real2) -> Option<Real2> {
        let m = &self.matrix.data;
        let clip_x = m[0] * point.x + m[1] * point.y + m[3];
        let clip_y = m[4] * point.x + m[5] * point.y + m[7];
        let clip_w = m[12] * point.x + m[13] * point.y + m[15];
        (clip_w > 1e-6).then(|| Real2::new(clip_x / clip_w, clip_y / clip_w))
    }

    /// The point on the ground plane (z = 0) that appears at `ndc` in normalized device coordinates:
    /// the inverse of `project`. `None` if no point of the plane appears there.
    pub fn unproject(&self, ndc: Real2) -> Option<Real2> {
        // `project` is the homography H = [m0 m1 m3; m4 m5 m7; m12 m13 m15] from (x, y, 1) to clip
        // space, then the perspective divide. H's adjugate inverts it, up to a scale the divide
        // cancels.
        let m = &self.matrix.data;
        let (a, b, c) = (m[0], m[1], m[3]);
        let (d, e, f) = (m[4], m[5], m[7]);
        let (g, h, i) = (m[12], m[13], m[15]);
        let (u, v) = (ndc.x, ndc.y);
        let x = (e * i - f * h) * u + (c * h - b * i) * v + (b * f - c * e);
        let y = (f * g - d * i) * u + (a * i - c * g) * v + (c * d - a * f);
        let w = (d * h - e * g) * u + (b * g - a * h) * v + (a * e - b * d);
        let point = Real2::new(x / w, y / w);
        // Not finite for a degenerate camera; behind the camera for a direction above the horizon.
        (point.is_finite() && self.project(point).is_some()).then_some(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_scale() {
        // Orthographic-like camera: 1 world unit = 0.01 NDC vertically.
        let mut data = [0.0; 16];
        data[0] = 0.02;
        data[5] = 0.01;
        data[15] = 1.0;
        let camera = Camera {
            matrix: Real4x4 { data },
            position: [5.0, 5.0, 0.0],
        };
        let units = camera.world_units_per_half_screen().unwrap();
        assert!((units - 100.0).abs() < 1e-3);
    }

    #[test]
    fn unproject_inverts_project() {
        // A tilted perspective camera.
        let data = [
            1.2, 0.1, 0.0, -30.0, //
            -0.05, 0.9, 0.3, 12.0, //
            0.0, 0.0, 0.0, 0.0, //
            0.0005, 0.002, 0.0, 1.5,
        ];
        let camera = Camera {
            matrix: Real4x4 { data },
            position: [0.0; 3],
        };
        for point in [
            Real2::new(0.0, 0.0),
            Real2::new(120.0, -40.0),
            Real2::new(-300.0, 250.0),
        ] {
            let back = camera.unproject(camera.project(point).unwrap()).unwrap();
            assert!((back.x - point.x).abs() < 1e-2 && (back.y - point.y).abs() < 1e-2);
        }
    }

    #[test]
    fn stages_compare_by_name() {
        let name = std::ffi::CString::new("menus").unwrap();
        assert_eq!(Stage::new(&name), Stage::MENUS);
        assert_ne!(Stage::new(&name), Stage::RACING_OVERLAY);
    }
}
