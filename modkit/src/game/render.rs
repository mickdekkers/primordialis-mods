//! Rendering: the frame being rendered, its stages, and drawing with the game's own renderers.

use std::ffi::CStr;

use super::types::{CircleRenderInfo, IconRenderInfo, Real2, Real4x4};
use super::{Game, read};

/// `void draw_cell_icons(icon_render_info*, int, real_4x4, real_2)`. The x64 ABI passes the 64-byte
/// matrix by reference and the 8-byte `real_2` by value in a register, which is what these Rust types
/// produce as well.
type DrawCellIcons = unsafe extern "C" fn(*const IconRenderInfo, i32, *const Real4x4, Real2);
/// `void draw_circles(circle_render_info*, int, real_4x4)`, with the matrix passed by reference.
type DrawCircles = unsafe extern "C" fn(*const CircleRenderInfo, i32, *const Real4x4);

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
}

impl<'a> Stage<'a> {
    pub(crate) fn new(name: &'a CStr) -> Self {
        Stage(name)
    }

    pub fn name(&self) -> &'a CStr {
        self.0
    }
}

/// The frame `render_game` is rendering.
pub struct Frame<'a> {
    game: Game<'a>,
    /// The world `render_context*`.
    render_context: usize,
}

impl<'a> Frame<'a> {
    /// # Safety
    ///
    /// As `Game::new`, on the render thread inside `render_game`, whose world `render_context*` is
    /// `render_context`.
    pub(crate) unsafe fn new(game: Game<'a>, render_context: usize) -> Self {
        Frame {
            game,
            render_context,
        }
    }

    pub fn game(&self) -> &Game<'a> {
        &self.game
    }

    /// The world camera this frame is rendered with.
    pub fn camera(&self) -> Camera {
        let bindings = self.game.bindings;
        // SAFETY: `render_context.camera` (a `real_4x4`) and `render_context.camera_pos` (a `real_3`).
        unsafe {
            Camera {
                matrix: read(self.render_context + bindings.rc_camera),
                position: read(self.render_context + bindings.rc_camera_pos),
            }
        }
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
    fn stages_compare_by_name() {
        let name = std::ffi::CString::new("menus").unwrap();
        assert_eq!(Stage::new(&name), Stage::MENUS);
        assert_ne!(Stage::new(&name), Stage::RACING_OVERLAY);
    }
}
