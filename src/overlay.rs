//! Draws the icon of every cell pickup in explored areas while the map is open, using the game's own
//! icon renderer (`draw_cell_icons`), so the icons look exactly like the ones on pickups in the world.
//! Combo pickups also get a ring of rainbow dots (`draw_circles`), standing in for the particle ring
//! the game shows around them in the world, which only exists near the player.

use std::cell::RefCell;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::game::Layout;
use crate::log;

/// Icon radius as a fraction of half the screen height, so icons keep the same on-screen size at any
/// map zoom level.
const ICON_SCREEN_RADIUS: f32 = 0.022;
/// A map hex's `explored` value rises from 0 to 1 as you see it. Pickups in hexes below the minimum
/// are hidden; between minimum and full, their icons fade in, matching how the map itself fades in.
const EXPLORED_MIN: f32 = 0.3;
const EXPLORED_FULL: f32 = 0.6;
/// Below this map fade value the map is effectively closed.
const MIN_MAP_ALPHA: f32 = 0.01;

/// Combo pickups don't use their material's color: the game cycles them through a dim rainbow,
/// `COMBO_BASE + COMBO_AMPLITUDE * cos(frame_number * COMBO_SPEED + phase)` for red, green and blue,
/// with green and blue phase-shifted by 2/3 and 1/3 of a cycle. These are constants in
/// `render_game`'s code (not symbols), taken from the current build.
const COMBO_BASE: f32 = 0.35;
const COMBO_AMPLITUDE: f32 = 0.05;
const COMBO_SPEED: f32 = 0.02;

/// The ring of dots around combo pickups, in multiples of the icon radius.
const HALO_RADIUS: f32 = 1.7;
const HALO_DOT_RADIUS: f32 = 0.14;
const HALO_DOTS: usize = 16;
/// Clockwise ring rotation, in radians per `frame_number` tick. Despite the name, `frame_number`
/// counts simulation steps, which run at a fixed 120 per second regardless of frame rate: ~20 s per
/// turn.
const HALO_SPIN: f32 = 0.00265;
/// Dot opacity relative to the icon's.
const HALO_ALPHA: f32 = 0.9;

/// Map hexes are 200 units apart: hex (q, r) is centered at (200q + 100r, 173.205r).
const HEX_SPACING: f32 = 200.0;
const HEX_ROW_HEIGHT: f32 = 173.205_08;

/// The game's `real_2`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Real2 {
    pub x: f32,
    pub y: f32,
}

/// The game's `real_4x4`. The game uploads these with `transpose = GL_TRUE`, so they are stored
/// row-major: element (row, column) is `data[row * 4 + column]`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Real4x4 {
    pub data: [f32; 16],
}

/// The game's `icon_render_info`: one icon instance for `draw_cell_icons`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IconRenderInfo {
    pub x: [f32; 3],
    pub r: f32,
    pub color: [f32; 4],
    pub uv: [f32; 2],
}

/// The game's `circle_render_info`: one filled circle for `draw_circles`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CircleRenderInfo {
    pub x: [f32; 3],
    pub r: f32,
    pub color: [f32; 4],
}

/// `void draw_cell_icons(icon_render_info*, int, real_4x4, real_2)`. The x64 ABI passes the 64-byte
/// matrix by reference and the 8-byte `real_2` by value in a register, which is what these Rust types
/// produce as well.
type DrawCellIcons = unsafe extern "C" fn(*const IconRenderInfo, i32, *const Real4x4, Real2);
/// `void draw_circles(circle_render_info*, int, real_4x4)`, with the matrix passed by reference.
type DrawCircles = unsafe extern "C" fn(*const CircleRenderInfo, i32, *const Real4x4);

/// Per-frame buffers, reused to avoid allocating every frame.
#[derive(Default)]
struct Buffers {
    icons: Vec<IconRenderInfo>,
    halo_dots: Vec<CircleRenderInfo>,
}

thread_local! {
    static BUFFERS: RefCell<Buffers> = RefCell::new(Buffers::default());
}

static LOGGED_FIRST_DRAW: AtomicBool = AtomicBool::new(false);

/// Reads a value of type `T` from game memory.
///
/// # Safety
///
/// `address` must be readable for `size_of::<T>()` bytes and hold a valid `T`.
unsafe fn read<T: Copy>(address: usize) -> T {
    unsafe { ptr::read_unaligned(address as *const T) }
}

/// Draws the overlay into the currently bound framebuffer.
///
/// # Safety
///
/// Must be called on the render thread during `render_game`, with `world_rc` being its world
/// `render_context*`, while the UI framebuffer is bound.
pub unsafe fn draw(layout: &Layout, world_rc: usize) {
    unsafe {
        let fade = read::<f32>(layout.map_icon_alpha);
        if fade.is_nan() || fade <= MIN_MAP_ALPHA {
            return;
        }

        let world = layout.world;
        let count = read::<i32>(world + layout.n_cell_pickups);
        let pickups = read::<usize>(world + layout.cell_pickups);
        let materials = read::<usize>(layout.materials_list);
        let n_materials = read::<i32>(layout.n_materials);
        let explored = read::<usize>(world + layout.explored);
        if count <= 0 || pickups == 0 || materials == 0 || explored == 0 {
            return;
        }
        let grid = HexGrid {
            lower_q: read(world + layout.map_range.lower_x),
            lower_r: read(world + layout.map_range.lower_y),
            upper_q: read(world + layout.map_range.upper_x),
            upper_r: read(world + layout.map_range.upper_y),
            explored: explored as *const f32,
        };

        let camera = read::<Real4x4>(world_rc + layout.rc_camera);
        let camera_pos = read::<[f32; 3]>(world_rc + layout.rc_camera_pos);
        let Some(units_per_ndc) = world_units_per_ndc(&camera, camera_pos[0], camera_pos[1]) else {
            return;
        };
        let radius = ICON_SCREEN_RADIUS * units_per_ndc;
        let frame_number = read::<i32>(world + layout.frame_number);
        let combo_rgb = combo_color(frame_number);
        let (combo_flags, combo_bit) = layout.pickup_is_combo;
        let halo = Halo::new(frame_number, radius);

        BUFFERS.with_borrow_mut(|Buffers { icons, halo_dots }| {
            icons.clear();
            halo_dots.clear();
            for i in 0..count as usize {
                let pickup = pickups + i * layout.pickup_size;
                let material_index = read::<i32>(pickup + layout.pickup_material_index);
                if !(0..n_materials).contains(&material_index) {
                    continue;
                }
                let pos = read::<Real2>(pickup + layout.pickup_x);
                let explored = grid.explored_at(pos);
                if explored < EXPLORED_MIN {
                    continue;
                }
                let material = materials + material_index as usize * layout.material_size;
                let mut color = read::<[f32; 4]>(material + layout.material_base_color);
                let is_combo = read::<u32>(pickup + combo_flags) & (1 << combo_bit) != 0;
                if is_combo {
                    color[..3].copy_from_slice(&combo_rgb);
                }
                color[3] = color[3].clamp(0.0, 1.0) * fade * smoothstep(EXPLORED_MIN, EXPLORED_FULL, explored);
                icons.push(IconRenderInfo {
                    x: [pos.x, pos.y, 0.0],
                    r: radius,
                    color,
                    uv: read(material + layout.material_uv),
                });
                if is_combo {
                    halo.add(halo_dots, pos, color[3] * HALO_ALPHA);
                }
            }

            if !LOGGED_FIRST_DRAW.swap(true, Ordering::Relaxed) {
                log::info(&format!(
                    "first map draw: {} of {count} pickups in explored areas ({} combo), icon radius {radius:.1} \
                     world units",
                    icons.len(),
                    halo_dots.len() / HALO_DOTS
                ));
            }
            if !halo_dots.is_empty() {
                let draw_circles: DrawCircles = std::mem::transmute(layout.draw_circles);
                draw_circles(halo_dots.as_ptr(), halo_dots.len() as i32, &camera);
            }
            if icons.is_empty() {
                return;
            }
            // The icon shader shades icons as if lit from this point: the top of the screen, like the
            // game's own pickups.
            let light = Real2 { x: camera_pos[0], y: camera_pos[1] + units_per_ndc };
            let draw_cell_icons: DrawCellIcons = std::mem::transmute(layout.draw_cell_icons);
            draw_cell_icons(icons.as_ptr(), icons.len() as i32, &camera, light);
        });
    }
}

/// How many world units one unit of normalized device coordinates spans vertically, at the given
/// point on the ground plane. `None` if the camera matrix is degenerate.
fn world_units_per_ndc(camera: &Real4x4, x: f32, y: f32) -> Option<f32> {
    let here = project(camera, x, y)?;
    let above = project(camera, x, y + 1.0)?;
    let units = 1.0 / (above.y - here.y).abs();
    (units.is_finite() && units > 0.0).then_some(units)
}

/// Projects a point on the ground plane (z = 0) to normalized device coordinates.
fn project(camera: &Real4x4, x: f32, y: f32) -> Option<Real2> {
    let m = &camera.data;
    let clip_x = m[0] * x + m[1] * y + m[3];
    let clip_y = m[4] * x + m[5] * y + m[7];
    let clip_w = m[12] * x + m[13] * y + m[15];
    (clip_w > 1e-6).then(|| Real2 { x: clip_x / clip_w, y: clip_y / clip_w })
}

/// The map's grid of `explored` values, one per hex within the map bounds.
struct HexGrid {
    lower_q: i32,
    lower_r: i32,
    upper_q: i32,
    upper_r: i32,
    explored: *const f32,
}

impl HexGrid {
    /// # Safety
    ///
    /// `explored` must point to the game's `(upper_q - lower_q) * (upper_r - lower_r)` values.
    unsafe fn explored_at(&self, pos: Real2) -> f32 {
        let (q, r) = hex_at(pos);
        if q < self.lower_q || q >= self.upper_q || r < self.lower_r || r >= self.upper_r {
            return 0.0;
        }
        let width = (self.upper_q - self.lower_q) as usize;
        let index = (r - self.lower_r) as usize * width + (q - self.lower_q) as usize;
        // SAFETY: The index is within the bounds checked above.
        unsafe { *self.explored.add(index) }
    }
}

/// The map hex containing a world position, in axial coordinates.
fn hex_at(pos: Real2) -> (i32, i32) {
    let r = pos.y / HEX_ROW_HEIGHT;
    let q = (pos.x - 0.5 * HEX_SPACING * r) / HEX_SPACING;
    // Round in cube coordinates (q + r + s = 0), fixing up whichever component rounded the most.
    let s = -q - r;
    let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
    let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
    if dq > dr && dq > ds {
        rq = -rr - rs;
    } else if dr > ds {
        rr = -rq - rs;
    }
    (rq as i32, rr as i32)
}

/// The ring of rainbow dots drawn around combo pickups for the current frame.
struct Halo {
    /// Dot offsets from the pickup's center, and their colors.
    dots: [(Real2, [f32; 3]); HALO_DOTS],
    dot_radius: f32,
}

impl Halo {
    fn new(frame_number: i32, icon_radius: f32) -> Self {
        use std::f32::consts::TAU;
        let spin = (frame_number as f32 * HALO_SPIN) % TAU;
        let dots = std::array::from_fn(|i| {
            let along = i as f32 / HALO_DOTS as f32;
            // World y points up on screen, so decreasing angles turn clockwise.
            let angle = along * TAU - spin;
            let distance = HALO_RADIUS * icon_radius;
            let offset = Real2 { x: angle.cos() * distance, y: angle.sin() * distance };
            (offset, rainbow(along))
        });
        Halo { dots, dot_radius: HALO_DOT_RADIUS * icon_radius }
    }

    fn add(&self, out: &mut Vec<CircleRenderInfo>, center: Real2, alpha: f32) {
        out.extend(self.dots.iter().map(|&(offset, [r, g, b])| CircleRenderInfo {
            x: [center.x + offset.x, center.y + offset.y, 0.0],
            r: self.dot_radius,
            color: [r, g, b, alpha],
        }));
    }
}

/// A light, saturated rainbow color, `t` going once around the hues from 0 to 1.
fn rainbow(t: f32) -> [f32; 3] {
    use std::f32::consts::TAU;
    [0.0, 2.0 / 3.0, 1.0 / 3.0].map(|phase| 0.65 + 0.35 * ((t + phase) * TAU).cos())
}

/// The color the game gives combo pickups on a given frame.
fn combo_color(frame_number: i32) -> [f32; 3] {
    use std::f32::consts::TAU;
    let t = frame_number as f32 * COMBO_SPEED;
    [0.0, 2.0 / 3.0, 1.0 / 3.0].map(|phase| COMBO_BASE + COMBO_AMPLITUDE * (t + phase * TAU).cos())
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn center(q: i32, r: i32) -> Real2 {
        Real2 { x: HEX_SPACING * q as f32 + 0.5 * HEX_SPACING * r as f32, y: HEX_ROW_HEIGHT * r as f32 }
    }

    #[test]
    fn hex_centers_round_trip() {
        for q in -20..20 {
            for r in -20..20 {
                assert_eq!(hex_at(center(q, r)), (q, r));
            }
        }
    }

    #[test]
    fn points_near_a_center_belong_to_its_hex() {
        let c = center(3, -7);
        for (dx, dy) in [(90.0, 0.0), (-90.0, 0.0), (0.0, 90.0), (0.0, -90.0), (60.0, 60.0), (-60.0, -60.0)] {
            assert_eq!(hex_at(Real2 { x: c.x + dx, y: c.y + dy }), (3, -7));
        }
    }

    #[test]
    fn projection_scale() {
        // Orthographic-like camera: 1 world unit = 0.01 NDC vertically.
        let mut m = [0.0; 16];
        m[0] = 0.02;
        m[5] = 0.01;
        m[15] = 1.0;
        let units = world_units_per_ndc(&Real4x4 { data: m }, 5.0, 5.0).unwrap();
        assert!((units - 100.0).abs() < 1e-3);
    }
}
