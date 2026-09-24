//! Draws the icon of every cell pickup in explored areas while the map is open, using the game's own
//! icon renderer, so the icons look exactly like the ones on pickups in the world. Combo pickups also
//! get a ring of rainbow dots, standing in for the particle ring the game shows around them in the
//! world, which only exists near the player.
//!
//! Pointing at icons with the mouse spreads out the ones that overlap onto a grid (other icons in the
//! way make room on it), and shows a tooltip naming the cell under the cursor. Meanwhile, the pickups
//! on the grid aren't drawn in the world (if they're near enough to be), so they only show up once.

use modkit::Feature;
use modkit::game::{
    CircleRenderInfo, Frame, Game, IconRenderInfo, LineRenderInfo, PickupsId, Real2, Stage,
};
use modkit::log;
use modkit::settings::{Setting, Toggle};

use crate::grid_pickups::GridPickups;
use crate::settle::SettledPositions;
use crate::spread::{self, Spread};
use crate::tooltip::{Line, Tooltip};

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
    "Name the cell of the map icon under the mouse.",
);

/// Icon radius as a fraction of half the screen height, so icons keep the same on-screen size at any
/// map zoom level.
const ICON_SCREEN_RADIUS: f32 = 0.0264;
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
/// While the mouse points at an icon, every other icon (and the grid's lines and dots) fades to this
/// opacity, and back once it doesn't, each over this many `frame_number` ticks (120 per second).
const UNFOCUSED_ALPHA: f32 = 0.3;
const FOCUS_TICKS: f32 = 8.0;
/// The tooltip's second line, for combo pickups, is this light gray.
const COMBO_LINE_COLOR: [f32; 3] = [0.75, 0.75, 0.75];

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
    /// The pickup array `alphas` is indexed like.
    pickups: Option<PickupsId>,
    /// Opacity of each pickup's icon, from `UNFOCUSED_ALPHA` to 1.
    alphas: Vec<f32>,
    last_frame: Option<i32>,
}

impl Fades {
    /// Eases every icon towards full opacity if it's `hovered` or nothing is, and towards
    /// `UNFOCUSED_ALPHA` otherwise.
    fn update(
        &mut self,
        pickups: PickupsId,
        len: usize,
        hovered: Option<usize>,
        frame_number: i32,
    ) {
        if self.pickups != Some(pickups) {
            self.pickups = Some(pickups);
            self.alphas.clear();
            self.alphas.resize(len, 1.0);
        }
        let ticks = match self.last_frame.replace(frame_number) {
            Some(last) => frame_number.wrapping_sub(last).clamp(0, 30) as f32,
            None => 0.0,
        };
        let step = ticks / FOCUS_TICKS * (1.0 - UNFOCUSED_ALPHA);
        for (pickup, alpha) in self.alphas.iter_mut().enumerate() {
            *alpha = if hovered.is_none_or(|hovered| hovered == pickup) {
                (*alpha + step).min(1.0)
            } else {
                (*alpha - step).max(UNFOCUSED_ALPHA)
            };
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
        } else if stage == Stage::MENUS {
            self.draw(frame);
        }
    }

    fn stage_end(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::CELL_PICKUPS {
            self.show_in_world(frame.game());
        }
    }

    fn revert(&mut self, game: &Game) {
        self.show_in_world(game);
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
    /// to be drawn in the world, until `show_in_world`: their icons stand in for them.
    fn hide_in_world(&mut self, game: &Game) {
        self.show_in_world(game);
        let pickups = game.pickups();
        if self.on_grid.0 != Some(pickups.id()) {
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
        // the world camera onto the ground.
        let mouse = frame.mouse();
        let mouse_on_map = mouse
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

        // The icon under the mouse stands out, and every other icon fades.
        let pickups = game.pickups();
        self.fades.update(
            pickups.id(),
            pickups.len(),
            hovered.map(|icon| self.visible[icon].pickup),
            frame_number,
        );

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

        match (hovered, mouse) {
            (Some(icon), Some(mouse)) if SHOW_TOOLTIPS.get() => {
                self.draw_tooltip(frame, self.visible[icon].pickup, mouse)
            }
            _ => self.tooltip.hide(),
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

    /// Names the cell of pickup `index`: its material's name, as the game's tooltips show it, and for
    /// combo pickups, the game's name for combo cells below it.
    fn draw_tooltip(&mut self, frame: &Frame, index: usize, mouse: Real2) {
        let game = frame.game();
        let pickup = game.pickups().get(index);
        let Some((pickup, name)) =
            pickup.and_then(|pickup| Some((pickup, pickup.material()?.name()?)))
        else {
            self.tooltip.hide();
            return;
        };
        let mut lines = vec![Line {
            text: name,
            color: [1.0, 1.0, 1.0],
        }];
        if pickup.is_combo() {
            lines.push(Line {
                text: game.translation(c"cell_MIXD_name"),
                color: COMBO_LINE_COLOR,
            });
        }
        self.tooltip.draw(
            frame,
            &lines,
            mouse,
            game.map_fade().clamp(0.0, 1.0),
            game.frame_number(),
        );
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
