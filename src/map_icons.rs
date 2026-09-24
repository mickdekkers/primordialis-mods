//! Draws the icon of every cell pickup in explored areas while the map is open, using the game's own
//! icon renderer, so the icons look exactly like the ones on pickups in the world. Combo pickups also
//! get a ring of rainbow dots, standing in for the particle ring the game shows around them in the
//! world, which only exists near the player.
//!
//! Pointing at icons with the mouse spreads out the ones that overlap onto a grid (other icons in the
//! way make room on it), and shows the game's own tooltip for the cell under the cursor, the one it
//! shows for pickups in the world. Meanwhile, the pickups on the grid aren't drawn in the world (if
//! they're near enough to be), so they only show up once. For the same reason, while the map shows
//! tooltips, the game's own tooltip for the pickup under the mouse in the world isn't shown.

use modkit::Feature;
use modkit::game::{
    CircleRenderInfo, Frame, Game, IconRenderInfo, LineRenderInfo, PickupsId, Real2, Stage,
};
use modkit::log;
use modkit::settings::{Setting, Toggle};

use crate::combo::{HALO_ALPHA, Halo, combo_color};
use crate::fades::{
    FADE_TICKS_FAR, FADE_TICKS_NEAR, Fades, SPOTLIGHT_EDGE, SPOTLIGHT_RADIUS, UNFOCUSED_ALPHA,
};
use crate::grid_pickups::GridPickups;
use crate::leaders::{
    LEADER_ALPHA, LEADER_DOT_RADIUS, LEADER_WIDTH, POINTED_LEADER_ALPHA, POINTED_LEADER_SCALE,
    dashed_line,
};
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

/// The icon under the mouse is drawn this much larger.
const HOVER_SCALE: f32 = 1.15;

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
    logged_unrestored: bool,
}

/// Which icons are drawn over which: the spread out grid over the rest, and the icon under the mouse
/// over everything.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Rest,
    Grid,
    Hovered,
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
            self.restore_in_world(frame.game());
        } else if stage == Stage::RACING_OVERLAY {
            self.show_world_tooltip(frame.game());
        }
    }

    fn revert(&mut self, game: &Game) {
        // No logging here: it may run while the game's threads are paused.
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
        self.restore_in_world(game);
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

    /// Shows the pickups hidden in the world again, logging (once) if the pickups changed in between.
    fn restore_in_world(&mut self, game: &Game) {
        if !self.show_in_world(game) && !self.logged_unrestored {
            self.logged_unrestored = true;
            log::warn("pickups changed while hidden in the world; showed those still hidden");
        }
    }

    /// Shows the pickups hidden in the world again. Returns false if the pickup array changed in
    /// between.
    fn show_in_world(&mut self, game: &Game) -> bool {
        if self.hidden.alphas.is_empty() {
            return true;
        }
        let pickups = game.pickups();
        // Nothing should change the pickups in between. If something did, an index may now refer to
        // another pickup, so only a pickup still hidden is shown again. Leaving them all instead would
        // leave them hidden for good.
        for &(index, alpha) in &self.hidden.alphas {
            if let Some(pickup) = pickups.get(index)
                && pickup.alpha().to_bits() == 0f32.to_bits()
            {
                pickup.set_alpha(alpha);
            }
        }
        self.hidden.alphas.clear();
        self.hidden.pickups == Some(pickups.id())
    }

    /// While the map is open and shows tooltips, keeps the game from drawing its tooltip for the
    /// pickup under the mouse in the world (it does in this stage) until `show_world_tooltip`: it
    /// would show up on the map, next to the one for the icon under the mouse.
    fn hide_world_tooltip(&mut self, game: &Game) {
        self.show_world_tooltip(game);
        if !game.map_open() || !SHOW_TOOLTIPS.get() {
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
            self.shown[moved.icon] = moved.from.lerp(moved.to, moved.progress);
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
            math::lerp(
                spread::HOVER_DISTANCE,
                spread::GRID_HOVER_DISTANCE,
                progress,
            ) * radius
        };
        let hovered = mouse_on_map.and_then(|mouse| {
            (0..shown.len())
                .map(|icon| (icon, shown[icon].distance(mouse)))
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
                let d = shown[icon].distance(mouse);
                let alpha = match hovered {
                    Some(hovered) if hovered != icon => {
                        math::lerp(UNFOCUSED_ALPHA, 1.0, math::smoothstep(inner, outer, d))
                    }
                    _ => 1.0,
                };
                let ticks = math::lerp(FADE_TICKS_NEAR, FADE_TICKS_FAR, (d / outer).min(1.0));
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
                let alpha = math::lerp(LEADER_ALPHA, POINTED_LEADER_ALPHA, fade) * visibility;
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
            color[3] = color[3].clamp(0.0, 1.0)
                * fade
                * math::smoothstep(EXPLORED_MIN, EXPLORED_FULL, explored);
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
