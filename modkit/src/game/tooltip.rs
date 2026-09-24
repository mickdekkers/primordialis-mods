//! Cell pickup tooltips: the one the game shows for the pickup under the mouse in the world, and
//! drawing the same tooltip elsewhere with the game's own `do_tooltip`.

use std::ffi::c_void;

use super::render::Frame;
use super::types::{Real2, TooltipState, TranslationInfo};
use super::world::{Material, Pickup};
use super::{Game, read, write};

/// `void do_tooltip(render_context* ui, tooltip_t* tt, float dt, bool is_tooltip_hovered,
/// int anchor_mode, translation_info trans, bool instant_slide, float free_genome_size,
/// bool preview_pickup)`. The 8-byte `translation_info` is passed by value in a register-sized
/// slot, which is what Rust does as well for this signature.
type DoTooltip = unsafe extern "C" fn(
    *const c_void,
    *mut TooltipState,
    f32,
    bool,
    i32,
    TranslationInfo,
    bool,
    f32,
    bool,
);

/// `tooltip_t.last_hovered_type` for a cell.
const TYPE_CELL: i32 = 0;
/// The arguments `render_game` passes to `do_tooltip` for the pickup under the mouse, besides the
/// tooltip itself: constants in its code (not symbols), taken from the current build.
const PICKUP_ANCHOR_MODE: i32 = 1;
const PICKUP_INSTANT_SLIDE: bool = true;
const PICKUP_PREVIEW: bool = true;

/// A cell pickup's tooltip as the game draws it for the pickup under the mouse in the world: the
/// cell's name, description, cost and genome size, and what picking it up would change. Holds what
/// the game animates from one frame to the next (fading in and out, the box growing to fit), so
/// keep one per tooltip, and draw it with `Frame::draw_pickup_tooltip`.
#[derive(Default)]
pub struct PickupTooltip(TooltipState);

impl PickupTooltip {
    /// Whether it's showing, or still fading out.
    pub fn is_visible(&self) -> bool {
        self.0.alpha > 0.0
    }

    /// Hides it at once. It fades in from nothing the next time it's shown.
    pub fn hide(&mut self) {
        self.0 = TooltipState::default();
    }
}

impl Frame<'_> {
    /// Draws `tooltip` into the UI, the way the game draws the tooltip of the pickup under the
    /// mouse in the world, pointing at `anchor` (in UI units, see `mouse`). Call it every frame while
    /// the tooltip may be visible: with the pickup it's for to show it, or `None` to fade it out
    /// (still showing the last pickup's cell). The game's other tooltips draw in the same framebuffer
    /// as the menus: draw it from the `menus` stage, or one that draws the UI.
    pub fn draw_pickup_tooltip(
        &self,
        tooltip: &mut PickupTooltip,
        pickup: Option<Pickup>,
        anchor: Real2,
    ) {
        let game = self.game();
        let visible = tooltip.is_visible();
        let state = &mut tooltip.0;
        if let Some(pickup) = pickup {
            if !visible {
                // As the game does: grow the box from nothing.
                state.box_size = Real2::default();
            }
            state.last_hovered_index = pickup.material_index();
            state.last_hovered_type = TYPE_CELL;
            state.last_hovered_imbue = 0;
            state.flags = u32::from(pickup.is_combo());
            state.consumable_instructions = 0;
        } else if !visible {
            return;
        }
        // The game never shows a tooltip for material 0, and `do_tooltip` doesn't check the index.
        if state.last_hovered_index == 0
            || Material::get(*game, state.last_hovered_index).is_none()
            || !anchor.is_finite()
        {
            tooltip.hide();
            return;
        }
        state.last_hovered_mutation_pos = anchor;
        // SAFETY: The game's `do_tooltip`, called on the render thread with the UI render context
        // and a valid cell tooltip, with the arguments `render_game` gives it for the pickup under the
        // mouse. Besides drawing, it only updates the tooltip, and `last_item_count`, a static that
        // only it reads; it changes `w.em.cell_item_counts` while it runs, and changes it back.
        unsafe {
            let draw: DoTooltip = std::mem::transmute(game.bindings.do_tooltip);
            draw(
                self.ui_render_context() as *const c_void,
                state,
                self.dt(),
                pickup.is_some(),
                PICKUP_ANCHOR_MODE,
                TranslationInfo::default(),
                PICKUP_INSTANT_SLIDE,
                free_genome_size(game),
                PICKUP_PREVIEW,
            );
        }
    }
}

/// How much genome size the player has left for new cells, as `render_game` works it out for the
/// pickup tooltip: the edit menu's maximum, minus that of every cell it holds.
fn free_genome_size(game: &Game) -> f32 {
    let bindings = game.bindings;
    // SAFETY: `w.em.cell_items` (a `cell_item*`, or null), `w.em.n_cell_items` and
    // `w.em.max_genome_size`.
    let (items, count, max) = unsafe {
        (
            read::<usize>(bindings.world + bindings.cell_items),
            read::<i32>(bindings.world + bindings.n_cell_items),
            read::<f32>(bindings.world + bindings.max_genome_size),
        )
    };
    if items == 0 {
        return max;
    }
    let used: f32 = (0..count.max(0) as usize)
        .filter_map(|index| {
            let item = items + index * bindings.cell_item_size;
            // SAFETY: `cell_item.type` and `cell_item.material_index` of one of the edit menu's
            // `n_cell_items` items.
            let (kind, material) = unsafe {
                (
                    read::<i32>(item + bindings.cell_item_type),
                    read::<i32>(item + bindings.cell_item_material_index),
                )
            };
            // Type 0 items are cells; the others are bodies.
            (kind == 0).then(|| Material::get(*game, material))?
        })
        .map(|material| material.genome_size())
        .sum();
    max - used
}

/// The tooltip the game shows for the cell pickup under the mouse in the world (`w.tooltip`). The
/// game draws it in the `racing_overlay` stage, while it's active or still fading out.
#[derive(Clone, Copy)]
pub struct WorldTooltip<'a> {
    game: Game<'a>,
}

impl<'a> Game<'a> {
    pub fn world_tooltip(&self) -> WorldTooltip<'a> {
        WorldTooltip { game: *self }
    }
}

impl WorldTooltip<'_> {
    /// Whether the mouse is on a pickup (`w.tooltip_active`): the tooltip fades in while it is, and
    /// out otherwise.
    pub fn active(&self) -> bool {
        // SAFETY: `w.tooltip_active`, a bool, read as a byte in case it holds something else.
        unsafe { read::<u8>(self.address(self.game.bindings.tooltip_active)) != 0 }
    }

    pub fn set_active(&self, active: bool) {
        // SAFETY: As in `active`.
        unsafe { write(self.address(self.game.bindings.tooltip_active), active) }
    }

    /// Opacity, from 0 to 1: the game doesn't draw it at 0 unless it's active.
    pub fn alpha(&self) -> f32 {
        // SAFETY: `w.tooltip.alpha`, a float.
        unsafe { read(self.alpha_address()) }
    }

    pub fn set_alpha(&self, alpha: f32) {
        // SAFETY: As in `alpha`; any float is valid.
        unsafe { write(self.alpha_address(), alpha) }
    }

    fn address(&self, offset: usize) -> usize {
        self.game.bindings.world + offset
    }

    fn alpha_address(&self) -> usize {
        self.address(self.game.bindings.tooltip) + std::mem::offset_of!(TooltipState, alpha)
    }
}
