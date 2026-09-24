//! Draws the icon of every cell pickup in explored areas while the map is open, using the game's own
//! icon renderer, so the icons look exactly like the ones on pickups in the world. Combo pickups also
//! get a ring of rainbow dots, standing in for the particle ring the game shows around them in the
//! world, which only exists near the player.

use modkit::Feature;
use modkit::game::{CircleRenderInfo, Frame, IconRenderInfo, Real2, Stage};
use modkit::log;
use modkit::settings::{Setting, Toggle};

use crate::settle::SettledPositions;

static FIX_ICON_POSITIONS: Toggle = Toggle::new(
    "fix_icon_positions",
    true,
    "\
The game only simulates cell pickups near you. Far away, a pickup can sit inside rock where it
spawned, until the game pushes it out as you get close. Show map icons where pickups will end up.",
);

/// Icon radius as a fraction of half the screen height, so icons keep the same on-screen size at any
/// map zoom level.
const ICON_SCREEN_RADIUS: f32 = 0.022;
/// A map hex's `explored` value rises from 0 to 1 as you see it. Pickups in hexes below the minimum
/// are hidden; between minimum and full, their icons fade in, matching how the map itself fades in.
const EXPLORED_MIN: f32 = 0.3;
const EXPLORED_FULL: f32 = 0.6;

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
/// Clockwise ring rotation, in radians per `frame_number` tick (120 per second): ~20 s per turn.
const HALO_SPIN: f32 = 0.00265;
/// Dot opacity relative to the icon's.
const HALO_ALPHA: f32 = 0.9;

#[derive(Default)]
pub struct MapIcons {
    /// Reused every frame, to avoid allocating.
    icons: Vec<IconRenderInfo>,
    halo_dots: Vec<CircleRenderInfo>,
    settled: SettledPositions,
    logged_first_draw: bool,
}

impl Feature for MapIcons {
    fn name(&self) -> &'static str {
        "map icons"
    }

    fn settings(&self) -> Vec<&'static dyn Setting> {
        vec![&FIX_ICON_POSITIONS]
    }

    fn stage_begin(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::MENUS {
            self.draw(frame);
        }
    }
}

impl MapIcons {
    fn draw(&mut self, frame: &Frame) {
        let game = frame.game();
        self.settled.refresh(game);
        if !game.map_open() {
            return;
        }
        let camera = frame.camera();
        let Some(units_per_half_screen) = camera.world_units_per_half_screen() else { return };
        let radius = ICON_SCREEN_RADIUS * units_per_half_screen;
        let fade = game.map_fade();
        let frame_number = game.frame_number();
        let combo_rgb = combo_color(frame_number);
        let halo = Halo::new(frame_number, radius);
        let (pickups, map) = (game.pickups(), game.map());
        let fix_positions = FIX_ICON_POSITIONS.get();

        self.icons.clear();
        self.halo_dots.clear();
        let mut moved = 0;
        for pickup in pickups.iter() {
            let Some(material) = pickup.material() else { continue };
            let spawned_at = pickup.position();
            let pos = if fix_positions { self.settled.get(&map, &pickup) } else { spawned_at };
            if !pos.same_bits(spawned_at) {
                moved += 1;
            }
            let explored = map.explored_at(pos);
            if explored < EXPLORED_MIN {
                continue;
            }
            let mut color = material.base_color();
            let is_combo = pickup.is_combo();
            if is_combo {
                color[..3].copy_from_slice(&combo_rgb);
            }
            color[3] = color[3].clamp(0.0, 1.0) * fade * smoothstep(EXPLORED_MIN, EXPLORED_FULL, explored);
            self.icons.push(IconRenderInfo { x: [pos.x, pos.y, 0.0], r: radius, color, uv: material.icon_uv() });
            if is_combo {
                halo.add(&mut self.halo_dots, pos, color[3] * HALO_ALPHA);
            }
        }

        if !self.logged_first_draw {
            self.logged_first_draw = true;
            log::info(&format!(
                "first map draw: {} of {} pickups in explored areas ({} combo, {moved} moved out of walls), icon \
                 radius {radius:.1} world units",
                self.icons.len(),
                pickups.len(),
                self.halo_dots.len() / HALO_DOTS
            ));
        }
        frame.draw_circles(&self.halo_dots);
        // The icon shader shades icons as if lit from this point: the top of the screen, like the
        // game's own pickups.
        let light = Real2::new(camera.position[0], camera.position[1] + units_per_half_screen);
        frame.draw_cell_icons(&self.icons, light);
    }
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
            (Real2::new(angle.cos() * distance, angle.sin() * distance), rainbow(along))
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
