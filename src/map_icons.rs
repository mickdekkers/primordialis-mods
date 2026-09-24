//! Draws the icon of every cell pickup in explored areas while the map is open, using the game's own
//! icon renderer, so the icons look exactly like the ones on pickups in the world. Combo pickups also
//! get a ring of rainbow dots, standing in for the particle ring the game shows around them in the
//! world, which only exists near the player.
//!
//! Pointing at icons with the mouse spreads out the ones that overlap onto a grid (other icons in the
//! way make room on it), and shows the game's own tooltip for the cell under the cursor, the one it
//! shows for pickups in the world. Meanwhile, the pickups on the grid aren't drawn in the world (if
//! they're near enough to be), so they only show up once. For the same reason, the game's tooltip for
//! the pickup under the mouse in the world isn't shown while the map is open.

use modkit::Feature;
use modkit::game::{
    CircleRenderInfo, Frame, Game, IconRenderInfo, LineRenderInfo, PickupsId, Real2, Stage,
};
use modkit::log;
use modkit::settings::{Setting, Toggle};

use crate::grid_pickups::GridPickups;
use crate::settle::SettledPositions;
use crate::spread::{self, Spread};
use crate::tooltip::Tooltip;

static FIX_ICON_POSITIONS: Toggle = Toggle::new(
    "fix_icon_positions",
    true,
    "\
The game only simulates cell pickups near you. Far away, a pickup can sit inside rock where it
spawned, until the game pushes it out as you get close. Show map icons where pickups will end up.",
);

static SPREAD_CLUSTERS: Toggle = Toggle::new(
    "spread_clusters",
    true,
    "Spread out map icons that overlap while the mouse is over them, so you can tell them apart.",
);

static SHOW_TOOLTIPS: Toggle = Toggle::new(
    "show_tooltips",
    true,
    "Show the cell of the map icon under the mouse in a tooltip, as the game does for pickups near you.",
);

/// Icon radius as a fraction of half the screen height, so icons keep the same on-screen size at any
/// map zoom level.
const ICON_SCREEN_RADIUS: f32 = 0.0385;
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

/// The icon under the mouse is drawn this much larger.
const HOVER_SCALE: f32 = 1.15;
/// Spread out icons are connected to where their pickups are by a dashed line this wide (dashes and
/// the gaps between them this long), ending in a dot this large, in icon radii, and this opaque
/// relative to the icon.
const LEADER_WIDTH: f32 = 0.15;
const LEADER_DOT_RADIUS: f32 = 0.2;
const DASH_LENGTH: f32 = 0.45;
const DASH_GAP: f32 = 0.35;
const LEADER_ALPHA: f32 = 0.6;
/// The spread out icon under the mouse gets a solid line this much wider, a dot this much larger,
/// and this opaque.
const POINTED_LEADER_SCALE: f32 = 1.5;
const POINTED_LEADER_ALPHA: f32 = 1.0;
/// While the mouse points at an icon, the other icons around the mouse (and their lines and dots on
/// the grid) fade to this opacity, and back once it doesn't.
const UNFOCUSED_ALPHA: f32 = 0.5;
/// Icons ease towards the opacity they should have with a time constant, in `frame_number` ticks (120
/// per second), of the first under the mouse (25 ms), rising with the distance from it to the second
/// at the spotlight's edge and beyond (125 ms, so ~95% of the way there in 375 ms): what the mouse
/// points at responds at once, and the icons around it follow smoothly.
const FADE_TICKS_NEAR: f32 = 3.0;
const FADE_TICKS_FAR: f32 = 15.0;
/// The icons faded are those within this many icon radii of the mouse, easing back to full opacity
/// by the second, so icons further away stay clear.
const SPOTLIGHT_RADIUS: f32 = 2.0;
const SPOTLIGHT_EDGE: f32 = 10.0;

#[derive(Default)]
pub struct MapIcons {
    /// Reused every frame, to avoid allocating.
    visible: Vec<Visible>,
    positions: Vec<Real2>,
    pickup_indices: Vec<usize>,
    /// Where each visible icon is drawn: its position, unless spread out.
    shown: Vec<Real2>,
    /// The order icons are drawn in, each drawn over the ones before, and how much they're faded.
    order: Vec<(Layer, usize, f32)>,
    icons: Vec<IconRenderInfo>,
    circles: Vec<CircleRenderInfo>,
    lines: Vec<LineRenderInfo>,
    /// For each visible icon, its index in `Spread::moved`, if it's on the grid.
    moved_index: Vec<Option<usize>>,
    settled: SettledPositions,
    spread: Spread,
    /// The pickups on the grid, shared with the Echolocation fix, and kept here too: sorted pickup
    /// indices, in the pickup array they index.
    grid: GridPickups,
    on_grid: (Option<PickupsId>, Vec<usize>),
    hidden: Hidden,
    /// Whether the game's tooltip for the pickup under the mouse in the world was active, and its
    /// opacity, while it's hidden.
    hidden_world_tooltip: Option<(bool, f32)>,
    fades: Fades,
    tooltip: Tooltip,
    logged_first_draw: bool,
}

/// Which icons are drawn over which: the spread out grid over the rest, and the icon under the mouse
/// over everything.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Rest,
    Grid,
    Hovered,
}

/// How faded each icon is. Every icon eases on its own, so the one the mouse leaves doesn't jump,
/// whether to the next icon (it fades out as that one fades in) or off them all (the rest fade back in
/// to it).
#[derive(Default)]
struct Fades {
    /// The pickup array `alphas` and `targets` are indexed like.
    pickups: Option<PickupsId>,
    /// Opacity of each pickup's icon, from `UNFOCUSED_ALPHA` to 1.
    alphas: Vec<f32>,
    /// The opacity each is easing towards this frame, and its time constant in ticks.
    targets: Vec<(f32, f32)>,
    last_frame: Option<i32>,
}

impl Fades {
    /// Starts a frame in which every pickup's icon eases slowly towards full opacity, unless
    /// `set_target` says otherwise before `ease`.
    fn begin(&mut self, pickups: PickupsId, len: usize) {
        if self.pickups != Some(pickups) {
            self.pickups = Some(pickups);
            self.alphas.clear();
            self.alphas.resize(len, 1.0);
        }
        self.targets.clear();
        self.targets.resize(len, (1.0, FADE_TICKS_FAR));
    }

    fn set_target(&mut self, pickup: usize, alpha: f32, ticks: f32) {
        if let Some(target) = self.targets.get_mut(pickup) {
            *target = (alpha, ticks);
        }
    }

    /// Moves every icon's opacity towards its target, exponentially: the same share of the way each
    /// tick, so it slows as it arrives, and a target that keeps moving (as the mouse does) is
    /// followed smoothly.
    fn ease(&mut self, frame_number: i32) {
        let ticks = match self.last_frame.replace(frame_number) {
            Some(last) => frame_number.wrapping_sub(last).clamp(0, 60) as f32,
            None => 0.0,
        };
        for (alpha, &(target, time_constant)) in self.alphas.iter_mut().zip(&self.targets) {
            *alpha += (target - *alpha) * (1.0 - (-ticks / time_constant).exp());
        }
    }

    fn get(&self, pickup: usize) -> f32 {
        self.alphas.get(pickup).copied().unwrap_or(1.0)
    }

    fn clear(&mut self) {
        *self = Fades::default();
    }
}

/// Pickups on the grid hidden in the world, to be shown again.
#[derive(Default)]
struct Hidden {
    /// The pickup array they were hidden in.
    pickups: Option<PickupsId>,
    /// Index of each hidden pickup, and its opacity.
    alphas: Vec<(usize, f32)>,
}

/// A pickup whose icon is drawn this frame.
struct Visible {
    pickup: usize,
    color: [f32; 4],
    uv: [f32; 2],
    is_combo: bool,
}

impl Feature for MapIcons {
    fn name(&self) -> &'static str {
        "map icons"
    }

    fn settings(&self) -> Vec<&'static dyn Setting> {
        vec![&FIX_ICON_POSITIONS, &SPREAD_CLUSTERS, &SHOW_TOOLTIPS]
    }

    fn stage_begin(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::CELL_PICKUPS {
            self.hide_in_world(frame.game());
        } else if stage == Stage::RACING_OVERLAY {
            self.hide_world_tooltip(frame.game());
        } else if stage == Stage::MENUS {
            self.draw(frame);
        }
    }

    fn stage_end(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::CELL_PICKUPS {
            self.show_in_world(frame.game());
        } else if stage == Stage::RACING_OVERLAY {
            self.show_world_tooltip(frame.game());
        }
    }

    fn revert(&mut self, game: &Game) {
        self.show_in_world(game);
        self.show_world_tooltip(game);
    }
}

impl MapIcons {
    pub fn new(grid: GridPickups) -> Self {
        MapIcons {
            grid,
            ..MapIcons::default()
        }
    }

    /// Makes the pickups on the grid transparent while the game queues the pickups near the camera
    /// to be drawn in the world, until `show_in_world`: their icons stand in for them. Not once the map
    /// starts closing: its icons fade out then, and the pickups should be back at once.
    fn hide_in_world(&mut self, game: &Game) {
        self.show_in_world(game);
        let pickups = game.pickups();
        if self.on_grid.0 != Some(pickups.id()) || !game.map_mode() {
            return;
        }
        self.hidden.pickups = Some(pickups.id());
        for &index in &self.on_grid.1 {
            if let Some(pickup) = pickups.get(index) {
                self.hidden.alphas.push((index, pickup.alpha()));
                pickup.set_alpha(0.0);
            }
        }
    }

    fn show_in_world(&mut self, game: &Game) {
        if self.hidden.alphas.is_empty() {
            return;
        }
        let pickups = game.pickups();
        // Nothing should change the pickups in between, but if something did, leave them be.
        if self.hidden.pickups == Some(pickups.id()) {
            for &(index, alpha) in &self.hidden.alphas {
                if let Some(pickup) = pickups.get(index)
                    && pickup.alpha().to_bits() == 0f32.to_bits()
                {
                    pickup.set_alpha(alpha);
                }
            }
        }
        self.hidden.alphas.clear();
    }

    /// While the map is open, keeps the game from drawing its tooltip for the pickup under the mouse
    /// in the world (it does in this stage) until `show_world_tooltip`: it would show up on the map,
    /// next to the one for the icon under the mouse.
    fn hide_world_tooltip(&mut self, game: &Game) {
        self.show_world_tooltip(game);
        if !game.map_open() {
            return;
        }
        let tooltip = game.world_tooltip();
        self.hidden_world_tooltip = Some((tooltip.active(), tooltip.alpha()));
        tooltip.set_active(false);
        tooltip.set_alpha(0.0);
    }

    fn show_world_tooltip(&mut self, game: &Game) {
        let Some((active, alpha)) = self.hidden_world_tooltip.take() else {
            return;
        };
        let tooltip = game.world_tooltip();
        // Nothing should change it in between, but if something did, leave it be.
        if !tooltip.active() && tooltip.alpha().to_bits() == 0f32.to_bits() {
            tooltip.set_active(active);
            tooltip.set_alpha(alpha);
        }
    }

    /// Records which pickups are on the grid, and shares it.
    fn set_on_grid(&mut self, pickups: Option<PickupsId>) {
        let (id, indices) = &mut self.on_grid;
        *id = pickups;
        indices.clear();
        indices.extend(
            self.spread
                .moved()
                .iter()
                .map(|moved| self.visible[moved.icon].pickup),
        );
        indices.sort_unstable();
        match pickups {
            Some(pickups) => self.grid.set(pickups, indices.iter().copied()),
            None => self.grid.clear(),
        }
    }
    fn draw(&mut self, frame: &Frame) {
        let game = frame.game();
        self.settled.refresh(game);
        if !game.map_open() {
            self.spread.close_now();
            self.set_on_grid(None);
            self.fades.clear();
            self.tooltip.hide();
            return;
        }
        let camera = frame.camera();
        let Some(units_per_half_screen) = camera.world_units_per_half_screen() else {
            return;
        };
        let radius = ICON_SCREEN_RADIUS * units_per_half_screen;
        let frame_number = game.frame_number();
        let moved_out_of_walls = self.collect(game);

        // Where the mouse points on the map, through the UI camera to the screen, then back through
        // the world camera onto the ground. Nowhere while the map is closing: the grid collapses, and
        // the tooltip fades out.
        let mouse_on_map = frame
            .mouse()
            .filter(|_| game.map_mode())
            .and_then(|mouse| frame.ui_camera().project(mouse))
            .and_then(|ndc| camera.unproject(ndc));

        if SPREAD_CLUSTERS.get() {
            self.spread.update(
                game.pickups().id(),
                &self.pickup_indices,
                &self.positions,
                radius,
                mouse_on_map,
                frame_number,
            );
        } else {
            self.spread.close_now();
        }
        self.set_on_grid(Some(game.pickups().id()));
        self.shown.clone_from(&self.positions);
        for moved in self.spread.moved() {
            self.shown[moved.icon] = lerp(moved.from, moved.to, moved.progress);
        }
        self.moved_index.clear();
        self.moved_index.resize(self.visible.len(), None);
        for (index, moved) in self.spread.moved().iter().enumerate() {
            self.moved_index[moved.icon] = Some(index);
        }
        let (shown, moved, moved_index) = (&self.shown, self.spread.moved(), &self.moved_index);
        // Icons on the grid can be pointed at from further, as they spread out.
        let reach = |icon: usize| {
            let progress = moved_index[icon].map_or(0.0, |index| moved[index].progress);
            lerp_f32(
                spread::HOVER_DISTANCE,
                spread::GRID_HOVER_DISTANCE,
                progress,
            ) * radius
        };
        let hovered = mouse_on_map.and_then(|mouse| {
            (0..shown.len())
                .map(|icon| (icon, distance(shown[icon], mouse)))
                .filter(|&(icon, d)| d <= reach(icon))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(icon, _)| icon)
        });

        // The icon under the mouse stands out, and the other icons around the mouse fade.
        let pickups = game.pickups();
        self.fades.begin(pickups.id(), pickups.len());
        if let Some(mouse) = mouse_on_map {
            let (inner, outer) = (SPOTLIGHT_RADIUS * radius, SPOTLIGHT_EDGE * radius);
            for (icon, visible) in self.visible.iter().enumerate() {
                let d = distance(shown[icon], mouse);
                let alpha = match hovered {
                    Some(hovered) if hovered != icon => {
                        lerp_f32(UNFOCUSED_ALPHA, 1.0, smoothstep(inner, outer, d))
                    }
                    _ => 1.0,
                };
                let ticks = lerp_f32(FADE_TICKS_NEAR, FADE_TICKS_FAR, (d / outer).min(1.0));
                self.fades.set_target(visible.pickup, alpha, ticks);
            }
        }
        self.fades.ease(frame_number);

        self.order.clear();
        self.order.extend(
            (0..self.visible.len())
                .map(|icon| (Layer::Rest, icon, self.fades.get(self.visible[icon].pickup))),
        );
        for moved in self.spread.moved() {
            self.order[moved.icon].0 = Layer::Grid;
        }
        if let Some(icon) = hovered {
            self.order[icon].0 = Layer::Hovered;
        }
        // Stable, so icons within a layer keep the game's order.
        self.order.sort_by_key(|&(layer, _, _)| layer);

        // Leader lines first, so icons cover their ends, in the icons' order, so the hovered icon's
        // line crosses over the rest.
        self.circles.clear();
        self.lines.clear();
        for &(_, icon, fade) in &self.order {
            let Some(moved) = self.moved_index[icon].map(|index| &self.spread.moved()[index])
            else {
                continue;
            };
            let (from, to) = (moved.from, shown[icon]);
            let visibility = moved.progress * self.visible[icon].color[3];
            let dot = if hovered == Some(icon) {
                let alpha = lerp_f32(LEADER_ALPHA, POINTED_LEADER_ALPHA, fade) * visibility;
                let color = [1.0, 1.0, 1.0, alpha];
                let width = LEADER_WIDTH * POINTED_LEADER_SCALE * radius;
                self.lines.push(LineRenderInfo::new(from, to, width, color));
                (LEADER_DOT_RADIUS * POINTED_LEADER_SCALE, color)
            } else {
                let color = [1.0, 1.0, 1.0, LEADER_ALPHA * fade * visibility];
                dashed_line(&mut self.lines, from, to, radius, color);
                (LEADER_DOT_RADIUS, color)
            };
            self.circles.push(CircleRenderInfo {
                x: [from.x, from.y, 0.0],
                r: dot.0 * radius,
                color: dot.1,
            });
        }

        frame.draw_lines(&self.lines);

        let halo = Halo::new(frame_number, radius);
        self.icons.clear();
        for &(layer, icon, fade) in &self.order {
            let visible = &self.visible[icon];
            let at = shown[icon];
            let mut color = visible.color;
            color[3] *= fade;
            let scale = if layer == Layer::Hovered {
                HOVER_SCALE
            } else {
                1.0
            };
            self.icons.push(IconRenderInfo {
                x: [at.x, at.y, 0.0],
                r: radius * scale,
                color,
                uv: visible.uv,
            });
            if visible.is_combo {
                halo.add(&mut self.circles, at, color[3] * HALO_ALPHA);
            }
        }

        if !self.logged_first_draw {
            self.logged_first_draw = true;
            log::info(&format!(
                "first map draw: {} of {} pickups in explored areas ({} combo, \
                 {moved_out_of_walls} moved out of walls), icon radius {radius:.1} world units",
                self.icons.len(),
                game.pickups().len(),
                self.visible.iter().filter(|v| v.is_combo).count(),
            ));
        }
        frame.draw_circles(&self.circles);
        // The icon shader shades icons as if lit from this point: the top of the screen, like the
        // game's own pickups.
        let light = Real2::new(
            camera.position[0],
            camera.position[1] + units_per_half_screen,
        );
        frame.draw_cell_icons(&self.icons, light);

        if SHOW_TOOLTIPS.get() {
            let pointed = hovered
                .and_then(|icon| Some((pickups.get(self.visible[icon].pickup)?, shown[icon])));
            self.tooltip.draw(frame, pointed);
        } else {
            self.tooltip.hide();
        }
    }

    /// Fills `visible`, `positions` and `pickup_indices` with the pickups to draw. Returns how many
    /// were moved out of walls.
    fn collect(&mut self, game: &Game) -> usize {
        let fade = game.map_fade();
        let combo_rgb = combo_color(game.frame_number());
        let (pickups, map) = (game.pickups(), game.map());
        let fix_positions = FIX_ICON_POSITIONS.get();
        self.visible.clear();
        self.positions.clear();
        self.pickup_indices.clear();
        let mut moved = 0;
        for (index, pickup) in pickups.iter().enumerate() {
            let Some(material) = pickup.material() else {
                continue;
            };
            let spawned_at = pickup.position();
            let pos = if fix_positions {
                self.settled.get(&map, &pickup)
            } else {
                spawned_at
            };
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
            color[3] =
                color[3].clamp(0.0, 1.0) * fade * smoothstep(EXPLORED_MIN, EXPLORED_FULL, explored);
            self.visible.push(Visible {
                pickup: index,
                color,
                uv: material.icon_uv(),
                is_combo,
            });
            self.positions.push(pos);
            self.pickup_indices.push(index);
        }
        moved
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
            (
                Real2::new(angle.cos() * distance, angle.sin() * distance),
                rainbow(along),
            )
        });
        Halo {
            dots,
            dot_radius: HALO_DOT_RADIUS * icon_radius,
        }
    }

    fn add(&self, out: &mut Vec<CircleRenderInfo>, center: Real2, alpha: f32) {
        out.extend(
            self.dots
                .iter()
                .map(|&(offset, [r, g, b])| CircleRenderInfo {
                    x: [center.x + offset.x, center.y + offset.y, 0.0],
                    r: self.dot_radius,
                    color: [r, g, b, alpha],
                }),
        );
    }
}

/// Adds the dashes of a dashed line from `from` to `to` to `lines`, for an icon `radius` large,
/// starting with a dash at `from`.
fn dashed_line(
    lines: &mut Vec<LineRenderInfo>,
    from: Real2,
    to: Real2,
    radius: f32,
    color: [f32; 4],
) {
    let length = distance(from, to);
    let width = LEADER_WIDTH * radius;
    let period = (DASH_LENGTH + DASH_GAP) * radius;
    if !(length > 0.0 && period > 0.0) {
        return;
    }
    // Lines have round caps sticking out half their width at both ends, which count towards the
    // dash length.
    let dash = (DASH_LENGTH * radius - width).max(0.0);
    let dashes = ((length / period).ceil() as usize).min(256);
    for k in 0..dashes {
        let start = k as f32 * period;
        let end = (start + dash).min(length);
        lines.push(LineRenderInfo::new(
            lerp(from, to, start / length),
            lerp(from, to, end / length),
            width,
            color,
        ));
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

fn lerp(a: Real2, b: Real2, t: f32) -> Real2 {
    Real2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn distance(a: Real2, b: Real2) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

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

    #[test]
    fn dashed_lines_start_with_a_dash_and_stay_on_the_line() {
        let (from, to, radius) = (Real2::new(1.0, 2.0), Real2::new(11.0, 2.0), 1.0);
        let mut lines = Vec::new();
        dashed_line(&mut lines, from, to, radius, [1.0; 4]);
        let period = (DASH_LENGTH + DASH_GAP) * radius;
        assert_eq!(lines.len(), (10.0 / period).ceil() as usize);
        assert!(lines[0].start().same_bits(from));
        for (k, line) in lines.iter().enumerate() {
            assert!((line.start().x - (from.x + k as f32 * period)).abs() < 1e-4);
            // Round caps stick out half the width at both ends, within the dash length.
            let drawn = line.end().x - line.start().x + line.width();
            assert!(drawn <= DASH_LENGTH * radius + 1e-4, "{k}: {drawn}");
            assert!(line.end().x <= to.x + 1e-4 && line.start().y == 2.0);
            assert!((line.width() - LEADER_WIDTH * radius).abs() < 1e-6);
        }
    }

    #[test]
    fn dashed_lines_of_no_length_have_no_dashes() {
        let mut lines = Vec::new();
        let at = Real2::new(3.0, 4.0);
        dashed_line(&mut lines, at, at, 1.0, [1.0; 4]);
        dashed_line(&mut lines, at, Real2::new(f32::NAN, 0.0), 1.0, [1.0; 4]);
        assert!(lines.is_empty());
    }

    #[test]
    fn the_halo_rings_the_icon_and_spins_clockwise() {
        let center = Real2::new(10.0, -5.0);
        let mut circles = Vec::new();
        Halo::new(0, 2.0).add(&mut circles, center, 0.5);
        assert_eq!(circles.len(), HALO_DOTS);
        for circle in &circles {
            let offset = Real2::new(circle.x[0] - center.x, circle.x[1] - center.y);
            assert!((offset.x.hypot(offset.y) - HALO_RADIUS * 2.0).abs() < 1e-4);
            assert!((circle.r - HALO_DOT_RADIUS * 2.0).abs() < 1e-6);
            assert_eq!(circle.color[3], 0.5);
        }
        assert!((circles[0].x[0] - center.x - HALO_RADIUS * 2.0).abs() < 1e-4);

        circles.clear();
        Halo::new(10, 2.0).add(&mut circles, center, 0.5);
        assert!(circles[0].x[1] < center.y, "the first dot turned clockwise");
    }

    #[test]
    fn combo_colors_cycle_within_the_games_range() {
        let period = TAU / COMBO_SPEED;
        for frame_number in (0..2000).step_by(7) {
            let color = combo_color(frame_number);
            for channel in color {
                assert!(
                    (COMBO_BASE - COMBO_AMPLITUDE - 1e-6..=COMBO_BASE + COMBO_AMPLITUDE + 1e-6)
                        .contains(&channel)
                );
            }
        }
        let red = combo_color(0)[0];
        assert!((red - (COMBO_BASE + COMBO_AMPLITUDE)).abs() < 1e-6);
        let half_cycle = combo_color((period / 2.0).round() as i32)[0];
        assert!((half_cycle - (COMBO_BASE - COMBO_AMPLITUDE)).abs() < 1e-4);
    }
}
