//! Draws the icon of every cell pickup the player has found while the map is open, using the game's own
//! icon renderer, so the icons look exactly like the ones on pickups in the world. Combo pickups also
//! get a ring of rainbow dots, standing in for the particle ring the game shows around them in the
//! world, which only exists near the player.
//!
//! Pointing at icons with the mouse spreads out the ones that overlap onto a grid (other icons in the
//! way make room on it), and shows the game's own tooltip for the cell under the cursor, the one it
//! shows for pickups in the world. The pickups on the grid are shared (`GridPickups`), so the other
//! features can keep them from showing up twice.

use modkit::Feature;
use modkit::game::{CircleRenderInfo, Frame, Game, IconRenderInfo, LineRenderInfo, Real2, Stage};
use modkit::log;
use modkit::settings::{Setting, Toggle};

use crate::combo::{HALO_ALPHA, Halo, combo_color};
use crate::fades::Fades;
use crate::found_cells::FoundCells;
use crate::grid_pickups::GridPickups;
use crate::leaders::Leader;
use crate::math;
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

pub(crate) static SHOW_TOOLTIPS: Toggle = Toggle::new(
    "show_tooltips",
    true,
    "Show the cell of the map icon under the mouse in a tooltip, as the game does for pickups near you.",
);

/// Icon radius as a fraction of half the screen height, so icons keep the same on-screen size at any
/// map zoom level.
const ICON_SCREEN_RADIUS: f32 = 0.0385;

/// The icon under the mouse is drawn this much larger.
const HOVER_SCALE: f32 = 1.15;

#[derive(Default)]
pub struct MapIcons {
    /// The icons drawn this frame, in the order of their pickups: each one's pickup index, position
    /// and look. Reused every frame, like the rest, to avoid allocating.
    pickups: Vec<usize>,
    positions: Vec<Real2>,
    looks: Vec<Look>,
    /// Where each icon is drawn: its position, unless spread out.
    shown: Vec<Real2>,
    /// For each icon, its index in `Spread::moved`, if it's on the grid.
    moved_index: Vec<Option<usize>>,
    /// The order icons are drawn in, each drawn over the ones before, and how much they're faded.
    order: Vec<(Layer, usize, f32)>,
    icons: Vec<IconRenderInfo>,
    circles: Vec<CircleRenderInfo>,
    lines: Vec<LineRenderInfo>,
    settled: SettledPositions,
    /// The cells the player has been close enough to find.
    found: FoundCells,
    spread: Spread,
    /// The pickups on the grid, shared with the features that hide them elsewhere.
    grid: GridPickups,
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

/// How an icon looks this frame.
struct Look {
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
        if stage == Stage::MENUS {
            self.draw(frame);
        }
    }

    fn revert(&mut self, _game: &Game) {
        // Turned off, or unloading: no icons are on a grid anymore.
        self.grid.withdraw();
    }
}

impl MapIcons {
    pub fn new(grid: GridPickups) -> Self {
        MapIcons {
            grid,
            ..MapIcons::default()
        }
    }

    fn draw(&mut self, frame: &Frame) {
        let game = frame.game();
        self.settled.refresh(game);
        self.found.update(game);
        if !game.map_open() {
            self.spread.close_now();
            self.grid.clear();
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
        // the world camera onto the ground. Nowhere while the map is closing, or behind a menu (the
        // pause menu), where it's only background: nothing is hovered, the tooltip fades out, and the
        // grid collapses (once the game runs again, if it's paused).
        let mouse_on_map = frame
            .mouse()
            .filter(|_| game.map_mode() && !game.menu_open())
            .and_then(|mouse| frame.ui_camera().project(mouse))
            .and_then(|ndc| camera.unproject(ndc));
        self.spread_out(game, radius, mouse_on_map, frame_number);
        let hovered = mouse_on_map.and_then(|mouse| self.hovered(mouse, radius));

        // The icon under the mouse stands out, and the other icons around the mouse fade.
        let pickups = game.pickups();
        self.fades.begin(pickups.id(), pickups.len());
        if let Some(mouse) = mouse_on_map {
            let icons = self.pickups.iter().copied().zip(self.shown.iter().copied());
            let pointed = hovered.map(|icon| self.pickups[icon]);
            self.fades.spotlight(icons, mouse, pointed, radius);
        }
        self.fades.ease(frame_number);
        self.order_icons(hovered);

        self.circles.clear();
        self.lines.clear();
        self.add_leaders(hovered, radius);
        frame.draw_lines(&self.lines);
        self.add_icons(frame_number, radius);
        if !self.logged_first_draw {
            self.logged_first_draw = true;
            log::info(&format!(
                "first map draw: {} of {} pickups found ({} combo, {moved_out_of_walls} moved out \
                 of walls), icon radius {radius:.1} world units",
                self.icons.len(),
                pickups.len(),
                self.looks.iter().filter(|look| look.is_combo).count(),
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
            let pointed =
                hovered.and_then(|icon| Some((pickups.get(self.pickups[icon])?, self.shown[icon])));
            self.tooltip.draw(frame, pointed);
        } else {
            self.tooltip.hide();
        }
    }

    /// Fills `pickups`, `positions` and `looks` with the icons to draw. Returns how many were moved
    /// out of walls.
    fn collect(&mut self, game: &Game) -> usize {
        let fade = game.map_fade();
        let frame_number = game.frame_number();
        let combo_rgb = combo_color(frame_number);
        let (pickups, map) = (game.pickups(), game.map());
        let fix_positions = FIX_ICON_POSITIONS.get();
        self.pickups.clear();
        self.positions.clear();
        self.looks.clear();
        let mut moved = 0;
        for (index, pickup) in pickups.iter().enumerate() {
            let Some(found) = self.found.alpha(index, frame_number) else {
                continue;
            };
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
            let mut color = material.base_color();
            let is_combo = pickup.is_combo();
            if is_combo {
                color[..3].copy_from_slice(&combo_rgb);
            }
            color[3] = color[3].clamp(0.0, 1.0) * fade * found;
            self.pickups.push(index);
            self.positions.push(pos);
            self.looks.push(Look {
                color,
                uv: material.icon_uv(),
                is_combo,
            });
        }
        moved
    }

    /// Spreads out the icons that overlap under the mouse (if that's on), shares which pickups are
    /// on the grid, and works out where each icon is drawn.
    fn spread_out(
        &mut self,
        game: &Game,
        radius: f32,
        mouse_on_map: Option<Real2>,
        frame_number: i32,
    ) {
        let pickups = game.pickups().id();
        if SPREAD_CLUSTERS.get() {
            self.spread.update(
                pickups,
                &self.pickups,
                &self.positions,
                radius,
                mouse_on_map,
                frame_number,
            );
        } else {
            self.spread.close_now();
        }
        let on_grid = self
            .spread
            .moved()
            .iter()
            .map(|moved| self.pickups[moved.icon]);
        self.grid.set(pickups, on_grid);
        self.shown.clone_from(&self.positions);
        self.moved_index.clear();
        self.moved_index.resize(self.pickups.len(), None);
        for (index, moved) in self.spread.moved().iter().enumerate() {
            self.shown[moved.icon] = moved.from.lerp(moved.to, moved.progress);
            self.moved_index[moved.icon] = Some(index);
        }
    }

    /// The icon the mouse at `mouse` points at, if any: the nearest one within reach.
    fn hovered(&self, mouse: Real2, radius: f32) -> Option<usize> {
        let moved = self.spread.moved();
        // Icons on the grid can be pointed at from further, as they spread out.
        let reach = |icon: usize| {
            let progress = self.moved_index[icon].map_or(0.0, |index| moved[index].progress);
            math::lerp(
                spread::HOVER_DISTANCE,
                spread::GRID_HOVER_DISTANCE,
                progress,
            ) * radius
        };
        (0..self.shown.len())
            .map(|icon| (icon, self.shown[icon].distance(mouse)))
            .filter(|&(icon, d)| d <= reach(icon))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(icon, _)| icon)
    }

    /// Orders the icons for drawing, with how faded each is: the grid over the rest, and the icon
    /// under the mouse over everything.
    fn order_icons(&mut self, hovered: Option<usize>) {
        let fades = &self.fades;
        self.order.clear();
        self.order.extend(
            self.pickups
                .iter()
                .enumerate()
                .map(|(icon, &pickup)| (Layer::Rest, icon, fades.get(pickup))),
        );
        for moved in self.spread.moved() {
            self.order[moved.icon].0 = Layer::Grid;
        }
        if let Some(icon) = hovered {
            self.order[icon].0 = Layer::Hovered;
        }
        // Stable, so icons within a layer keep the game's order.
        self.order.sort_by_key(|&(layer, _, _)| layer);
    }

    /// Adds the leaders of the icons on the grid, in the icons' order, so the hovered icon's line
    /// crosses over the rest. They're drawn before the icons, so that icons cover their ends.
    fn add_leaders(&mut self, hovered: Option<usize>, radius: f32) {
        let moved = self.spread.moved();
        for &(_, icon, fade) in &self.order {
            let Some(index) = self.moved_index[icon] else {
                continue;
            };
            let leader = Leader {
                from: moved[index].from,
                to: self.shown[icon],
                pointed: hovered == Some(icon),
                fade,
                visibility: moved[index].progress * self.looks[icon].color[3],
            };
            leader.add(radius, &mut self.lines, &mut self.circles);
        }
    }

    /// Adds the icons in their order, and the halos of combo pickups.
    fn add_icons(&mut self, frame_number: i32, radius: f32) {
        let halo = Halo::new(frame_number, radius);
        self.icons.clear();
        for &(layer, icon, fade) in &self.order {
            let (look, at) = (&self.looks[icon], self.shown[icon]);
            let mut color = look.color;
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
                uv: look.uv,
            });
            if look.is_combo {
                halo.add(&mut self.circles, at, color[3] * HALO_ALPHA);
            }
        }
    }
}
