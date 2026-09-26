//! Where everything the bindings use is in the running game, resolved by name from its symbols.
//!
//! Nothing here is hardcoded to a specific game build: addresses and field offsets come from the PDB
//! of the running executable. Layouts we mirror in our own `#[repr(C)]` types are verified against
//! the PDB, so a game update that changes them disables the mod instead of corrupting memory.
//!
//! To bind something new, add it here, then expose it through a safe accessor (`world`, `render`).

use std::mem::{offset_of, size_of};

use super::symbols::Symbols;
use super::types::{
    CircleRenderInfo, IconRenderInfo, LineRenderInfo, Real2, Real3, Real4x4, TooltipState,
    TranslationInfo, WallSample,
};
use crate::{Result, log};

/// Game function addresses and data locations. A binding the features use can't be missing: if it
/// isn't found, the mod doesn't start. One no feature uses is `Optional`.
#[derive(Clone, Debug)]
pub struct Bindings {
    pub render_game: usize,
    pub begin_trace_stage: usize,
    pub draw_cell_icons: usize,
    pub draw_circles: usize,
    /// `void draw_lines(render_context*, line_render_info*, int)`.
    pub draw_lines: usize,
    /// `wall_t wall_map(map_t*, real_2, bool)`: the wall distance field the game's physics uses.
    pub wall_map: usize,
    /// `draw_line(render_context*, real_2, real_2, float, real_4*)`, which shares its name with
    /// overloads.
    pub draw_line: Optional<usize>,
    /// Whether `wall_t.flow` and `wall_t.air_dist` are where `WallSample` has them.
    pub wall_extras: Optional<()>,
    /// `do_tooltip(render_context*, tooltip_t*, ...)`: draws the tooltip of a cell, mutation or body.
    pub do_tooltip: usize,
    /// `float light_value(map_t*, real_2)`: how lit the map is at a position.
    pub light_value: Optional<usize>,
    /// What changing the game's menu buttons needs.
    pub menu: Optional<MenuBindings>,
    /// Which save the current run is kept in.
    pub save_slots: Optional<SaveSlotBindings>,

    /// The global `world w`.
    pub world: usize,
    /// `w.cell_pickups` (`cell_pickup*`) and `w.n_cell_pickups` (`int`), as offsets in `world`.
    pub cell_pickups: usize,
    pub n_cell_pickups: usize,
    /// `w.map` (`map_t`), as an offset in `world`.
    pub map: usize,
    /// `w.map.explored` (`float*`, one value per map hex) and `w.map.map_range` (hex bounds), as offsets
    /// in `world`. A new map gets new ones, so they identify it.
    pub explored: usize,
    pub map_range: MapRange,
    /// `w.frame_number` (`int`), which counts simulation steps.
    pub frame_number: usize,
    /// `w.camera_pos` (`real_2`): where the camera, which follows the player, is centered. The map is
    /// explored around it.
    pub camera_pos: usize,
    /// `w.vision_radius` (`float`): how far around `camera_pos` the fog of war clears on the map
    /// (`walls.glsl`), taken from the player's body.
    pub vision_radius: usize,
    /// `w.seed` (`unsigned int`): the world's seed, which a saved run keeps.
    pub seed: usize,
    /// `w.run.start_time` (`double`): when the run was started, a timestamp saved with the run.
    pub run_start_time: usize,
    /// The `w.map_mode` bitfield: byte offset of its `u32` storage and bit position.
    pub map_mode: (usize, u32),
    /// `w.tooltip` (`tooltip_t`) and `w.tooltip_active` (`bool`): the tooltip of the pickup under the
    /// mouse in the world, as offsets in `world`.
    pub tooltip: usize,
    pub tooltip_active: usize,
    /// `w.em.cell_items` (`cell_item*`), `w.em.n_cell_items` (`int`) and `w.em.max_genome_size`
    /// (`float`): the player's cells and genome size, as offsets in `world`.
    pub cell_items: usize,
    pub n_cell_items: usize,
    pub max_genome_size: usize,

    pub cell_item_size: usize,
    pub cell_item_type: usize,
    pub cell_item_material_index: usize,

    pub pickup_size: usize,
    pub pickup_material_index: usize,
    pub pickup_x: usize,
    pub pickup_r: usize,
    pub pickup_alpha: usize,
    /// The `is_combo` bitfield: byte offset of its `u32` storage and bit position.
    pub pickup_is_combo: (usize, u32),

    /// Globals `material_t* materials_list` and `int n_materials`.
    pub materials_list: usize,
    pub n_materials: usize,
    pub material_size: usize,
    /// `material_t.id` (`unsigned int`), which identifies a material in any game session.
    pub material_id: usize,
    pub material_base_color: usize,
    pub material_uv: usize,
    pub material_genome_size: usize,

    /// `float map_icon_alpha`, a static in `render_game`: 0 when the map is closed, fading to 1 while
    /// it is open.
    pub map_icon_alpha: usize,

    /// `render_context.camera` (world-to-clip matrix) and `render_context.camera_pos`.
    pub rc_camera: usize,
    pub rc_camera_pos: usize,

    /// `user_input.mouse` (`real_2`), in UI units.
    pub input_mouse: usize,
}

/// A binding that no feature needs in order to run. If it can't be resolved, the mod still starts,
/// and a feature that uses it is turned off when it does.
#[derive(Clone, Debug)]
pub struct Optional<T>(std::result::Result<T, String>);

impl<T: Copy> Optional<T> {
    fn new(name: &str, resolved: Result<T>) -> Self {
        if let Err(error) = &resolved {
            log::warn(&format!(
                "{name} is unavailable; a feature that uses it will be turned off: {error}"
            ));
        }
        Optional(resolved)
    }

    pub fn is_available(&self) -> bool {
        self.0.is_ok()
    }

    /// The resolved value, if there is one.
    pub fn ok(&self) -> Option<T> {
        self.0.as_ref().ok().copied()
    }

    /// The resolved value. Panics if it couldn't be resolved, which turns off the feature that
    /// called this (see `Feature`).
    pub fn get(&self) -> T {
        match &self.0 {
            Ok(value) => *value,
            Err(error) => panic!("{error}"),
        }
    }
}

/// The game's text buttons (`do_text_button`, which draws the menus' buttons), and drawing text the
/// way they do.
#[derive(Clone, Copy, Debug)]
pub struct MenuBindings {
    /// `button do_text_button(render_context*, user_input*, real_3 pos, real_2 half_size, char*)`.
    pub do_text_button: usize,
    /// `void draw_text(char*, float x, float y, real_4 color, real_2 align, font_info*,
    /// text_params*)`.
    pub draw_text: usize,
    /// `real_2 get_text_size(char*, font_info, text_params)`, which shares its name with an
    /// overload.
    pub get_text_size: usize,
    /// The global `char* version_string`, which the main and pause menus show as a button.
    pub version_string: usize,
    /// The global `text_params default_shadow`, which buttons draw their text with, and the size of
    /// a `text_params`.
    pub default_shadow: usize,
    pub text_params_size: usize,
    /// `render_context.foreground_color` (`real_4`): the color of a button's text.
    pub rc_foreground_color: usize,
    /// `render_context.default_font` (`font_info`), the buttons' font, and its size.
    pub rc_default_font: usize,
    pub font_info_size: usize,
}

/// The global `saver_t saver`, whose `save_dir` points at the folder of the save in use: its
/// `normal_save_dir` for a normal run, or its `sandbox_save_dir` for a sandbox.
#[derive(Clone, Copy, Debug)]
pub struct SaveSlotBindings {
    pub saver: usize,
    /// Offsets in `saver_t`: `save_dir` (`char*`), and the two folders it can point at.
    pub save_dir: usize,
    pub normal_save_dir: usize,
    pub sandbox_save_dir: usize,
}

/// Offsets of `bounding_box_2 { int_2 l, u; }` fields, relative to `world`.
#[derive(Clone, Copy, Debug)]
pub struct MapRange {
    pub lower_x: usize,
    pub lower_y: usize,
    pub upper_x: usize,
    pub upper_y: usize,
}

impl Bindings {
    pub fn resolve(symbols: &Symbols) -> Result<Self> {
        let world = symbols.layout("world")?;
        let map = symbols.layout("map_t")?;
        let bounds = symbols.layout("bounding_box_2")?;
        let int2 = symbols.layout("int_2")?;
        let pickup = symbols.layout("cell_pickup")?;
        let material = symbols.layout("material_t")?;
        let edit_menu = symbols.layout("edit_menu")?;
        let cell_item = symbols.layout("cell_item")?;
        let render_context = symbols.layout("render_context")?;
        let input = symbols.layout("user_input")?;
        let run_stats = symbols.layout("run_stats")?;

        verify_mirrored_layouts(symbols)?;
        expect_size(symbols, "materials_list", size_of::<usize>())?;
        expect_size(symbols, "n_materials", size_of::<i32>())?;
        // A static inside render_game that it fades between 0 and 1 as the map closes and opens.
        let (map_icon_alpha, alpha_size) =
            symbols.function_static("render_game", "map_icon_alpha")?;
        expect(
            alpha_size == size_of::<f32>(),
            "map_icon_alpha is no longer a float",
        )?;
        expect(
            int2.size == 8 && int2.offset("x")? == 0 && int2.offset("y")? == 4,
            "int_2 layout changed",
        )?;

        // Every field read or written is checked for its size as well as found: a field whose type
        // changed but kept its name would otherwise be read or overwritten along with its neighbors.
        let map_offset = world.offset_sized("map", map.size)?;
        let em = world.offset_sized("em", edit_menu.size)?;
        let range = map_offset + map.offset_sized("map_range", bounds.size)?;
        let (lower, upper) = (
            bounds.offset_sized("l", int2.size)?,
            bounds.offset_sized("u", int2.size)?,
        );

        let bindings = Bindings {
            render_game: symbols.address("render_game")?,
            begin_trace_stage: symbols.address("begin_trace_stage")?,
            draw_cell_icons: symbols.address("draw_cell_icons")?,
            draw_circles: symbols.address("draw_circles")?,
            draw_lines: symbols.address("draw_lines")?,
            wall_map: symbols.address("wall_map")?,
            draw_line: Optional::new("draw_line", symbols.function("draw_line", 5)),
            wall_extras: Optional::new("wall_t.flow and air_dist", verify_wall_extras(symbols)),
            do_tooltip: symbols.function("do_tooltip", 9)?,
            light_value: Optional::new("light_value", symbols.function("light_value", 2)),
            menu: Optional::new("menu buttons", resolve_menu(symbols)),
            save_slots: Optional::new("save slots", resolve_save_slots(symbols)),

            world: symbols.address("w")?,
            cell_pickups: world.offset_of::<usize>("cell_pickups")?,
            n_cell_pickups: world.offset_of::<i32>("n_cell_pickups")?,
            map: map_offset,
            explored: map_offset + map.offset_of::<usize>("explored")?,
            map_range: MapRange {
                lower_x: range + lower,
                lower_y: range + lower + 4,
                upper_x: range + upper,
                upper_y: range + upper + 4,
            },
            frame_number: world.offset_of::<i32>("frame_number")?,
            camera_pos: world.offset_of::<Real2>("camera_pos")?,
            vision_radius: world.offset_of::<f32>("vision_radius")?,
            seed: world.offset_of::<u32>("seed")?,
            run_start_time: world.offset_sized("run", run_stats.size)?
                + run_stats.offset_of::<f64>("start_time")?,
            map_mode: world.flag("map_mode")?,
            tooltip: world.offset_of::<TooltipState>("tooltip")?,
            tooltip_active: world.offset_of::<bool>("tooltip_active")?,
            cell_items: em + edit_menu.offset_of::<usize>("cell_items")?,
            n_cell_items: em + edit_menu.offset_of::<i32>("n_cell_items")?,
            max_genome_size: em + edit_menu.offset_of::<f32>("max_genome_size")?,

            cell_item_size: cell_item.size,
            cell_item_type: cell_item.offset_of::<i32>("type")?,
            cell_item_material_index: cell_item.offset_of::<i32>("material_index")?,

            pickup_size: pickup.size,
            pickup_material_index: pickup.offset_of::<i32>("material_index")?,
            pickup_x: pickup.offset_of::<Real2>("x")?,
            pickup_r: pickup.offset_of::<f32>("r")?,
            pickup_alpha: pickup.offset_of::<f32>("alpha")?,
            pickup_is_combo: pickup.flag("is_combo")?,

            materials_list: symbols.address("materials_list")?,
            n_materials: symbols.address("n_materials")?,
            material_size: material.size,
            material_id: material.offset_of::<u32>("id")?,
            material_base_color: material.offset_of::<[f32; 4]>("base_color")?,
            material_uv: material.offset_of::<[f32; 2]>("uv")?,
            material_genome_size: material.offset_of::<f32>("genome_size")?,

            map_icon_alpha,

            rc_camera: render_context.offset_of::<Real4x4>("camera")?,
            rc_camera_pos: render_context.offset_of::<[f32; 3]>("camera_pos")?,

            input_mouse: input.offset_of::<Real2>("mouse")?,
        };
        log::info(&format!("resolved game bindings: {bindings:x?}"));
        Ok(bindings)
    }
}

/// Our `#[repr(C)]` mirrors of game types must match the game's layout exactly, since we pass them to
/// game functions.
fn verify_mirrored_layouts(symbols: &Symbols) -> Result<()> {
    let icon = symbols.layout("icon_render_info")?;
    let matches = icon.size == size_of::<IconRenderInfo>()
        && icon.offset("x")? == offset_of!(IconRenderInfo, x)
        && icon.offset("r")? == offset_of!(IconRenderInfo, r)
        && icon.offset("color")? == offset_of!(IconRenderInfo, color)
        && icon.offset("uv")? == offset_of!(IconRenderInfo, uv);
    expect(matches, "icon_render_info layout changed")?;
    let circle = symbols.layout("circle_render_info")?;
    let matches = circle.size == size_of::<CircleRenderInfo>()
        && circle.offset("x")? == offset_of!(CircleRenderInfo, x)
        && circle.offset("r")? == offset_of!(CircleRenderInfo, r)
        && circle.offset("color")? == offset_of!(CircleRenderInfo, color);
    expect(matches, "circle_render_info layout changed")?;
    let line = symbols.layout("line_render_info")?;
    let matches = line.size == size_of::<LineRenderInfo>()
        && line.offset("x")? == offset_of!(LineRenderInfo, x)
        && line.offset("d")? == offset_of!(LineRenderInfo, d)
        && line.offset("r")? == offset_of!(LineRenderInfo, r)
        && line.offset("color")? == offset_of!(LineRenderInfo, color);
    expect(matches, "line_render_info layout changed")?;
    // `wall_map` returns the whole struct, so its size must match; the fields are checked here as
    // far as the features need them.
    let wall = symbols.layout("wall_t")?;
    let matches = wall.size == size_of::<WallSample>()
        && wall.offset("dist")? == offset_of!(WallSample, dist)
        && wall.offset("gradient")? == offset_of!(WallSample, gradient);
    expect(matches, "wall_t layout changed")?;
    let tooltip = symbols.layout("tooltip_t")?;
    let matches = tooltip.size == size_of::<TooltipState>()
        && tooltip.offset("box_size")? == offset_of!(TooltipState, box_size)
        && tooltip.offset("pos")? == offset_of!(TooltipState, pos)
        && tooltip.offset("alpha")? == offset_of!(TooltipState, alpha)
        && tooltip.offset("last_hovered_index")? == offset_of!(TooltipState, last_hovered_index)
        && tooltip.offset("last_hovered_type")? == offset_of!(TooltipState, last_hovered_type)
        && tooltip.offset("last_hovered_imbue")? == offset_of!(TooltipState, last_hovered_imbue)
        && tooltip.offset("last_hovered_mutation_pos")?
            == offset_of!(TooltipState, last_hovered_mutation_pos)
        && tooltip.flag("is_combo")? == (offset_of!(TooltipState, flags), 0)
        && tooltip.offset("consumable_instructions")?
            == offset_of!(TooltipState, consumable_instructions);
    expect(matches, "tooltip_t layout changed")?;
    let translation = symbols.layout("translation_info")?;
    let matches = translation.size == size_of::<TranslationInfo>()
        && translation.offset("mutagen_material_index")?
            == offset_of!(TranslationInfo, mutagen_material_index)
        && translation.offset("combine_material_index")?
            == offset_of!(TranslationInfo, combine_material_index);
    expect(matches, "translation_info layout changed")?;
    expect(
        symbols.layout("real_2")?.size == size_of::<Real2>(),
        "real_2 size changed",
    )?;
    expect(
        symbols.layout("real_4x4")?.size == size_of::<Real4x4>(),
        "real_4x4 size changed",
    )?;
    Ok(())
}

fn resolve_save_slots(symbols: &Symbols) -> Result<SaveSlotBindings> {
    let saver = symbols.layout("saver_t")?;
    expect_size(symbols, "saver", saver.size)?;
    Ok(SaveSlotBindings {
        saver: symbols.address("saver")?,
        save_dir: saver.offset_of::<usize>("save_dir")?,
        // Only their addresses are used, never their contents.
        normal_save_dir: saver.offset("normal_save_dir")?,
        sandbox_save_dir: saver.offset("sandbox_save_dir")?,
    })
}

fn resolve_menu(symbols: &Symbols) -> Result<MenuBindings> {
    let render_context = symbols.layout("render_context")?;
    let font_info = symbols.layout("font_info")?;
    let text_params = symbols.layout("text_params")?;
    // Button positions are passed as a `Real3`.
    expect(
        symbols.layout("real_3")?.size == size_of::<Real3>(),
        "real_3 size changed",
    )?;
    expect_size(symbols, "version_string", size_of::<usize>())?;
    expect_size(symbols, "default_shadow", text_params.size)?;
    Ok(MenuBindings {
        do_text_button: symbols.function("do_text_button", 5)?,
        draw_text: symbols.function("draw_text", 7)?,
        get_text_size: symbols.function("get_text_size", 3)?,
        version_string: symbols.address("version_string")?,
        default_shadow: symbols.address("default_shadow")?,
        text_params_size: text_params.size,
        rc_foreground_color: render_context.offset_of::<[f32; 4]>("foreground_color")?,
        rc_default_font: render_context.offset_sized("default_font", font_info.size)?,
        font_info_size: font_info.size,
    })
}

/// The fields of `wall_t` no feature reads.
fn verify_wall_extras(symbols: &Symbols) -> Result<()> {
    let wall = symbols.layout("wall_t")?;
    let matches = wall.offset("flow")? == offset_of!(WallSample, flow)
        && wall.offset("air_dist")? == offset_of!(WallSample, air_dist);
    expect(matches, "wall_t.flow or air_dist moved")
}

fn expect_size(symbols: &Symbols, variable: &str, size: usize) -> Result<()> {
    let actual = symbols.variable_size(variable)?;
    expect(
        actual == size,
        &format!("`{variable}` is {actual} bytes, expected {size}"),
    )
}

fn expect(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW,
    };

    #[test]
    fn a_missing_optional_binding_panics_only_when_used() {
        let missing: Optional<usize> = Optional(Err("draw_line is gone".into()));
        assert!(!missing.is_available());
        let panic = std::panic::catch_unwind(|| missing.get()).unwrap_err();
        assert_eq!(
            panic.downcast_ref::<String>().map(String::as_str),
            Some("draw_line is gone")
        );
        assert_eq!(Optional(Ok(7)).get(), 7);
    }

    /// Resolves the bindings against a real game install, mapping the executable as an image (nothing
    /// in it runs). Run with:
    /// `PRIMORDIALIS_DIR=<game dir> cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a Primordialis install; set PRIMORDIALIS_DIR"]
    fn resolves_against_installed_game() {
        let game_dir =
            PathBuf::from(std::env::var("PRIMORDIALIS_DIR").expect("set PRIMORDIALIS_DIR"));
        let cache_dir = std::env::temp_dir().join("primordialis_qol_test_cache");
        for exe in ["primordialis_avx.exe", "primordialis_sse3.exe"] {
            let path: Vec<u16> = game_dir
                .join(exe)
                .to_string_lossy()
                .encode_utf16()
                .chain(Some(0))
                .collect();
            // SAFETY: Maps the executable as an image resource; none of its code runs.
            let module = unsafe {
                LoadLibraryExW(
                    path.as_ptr(),
                    std::ptr::null_mut(),
                    LOAD_LIBRARY_AS_IMAGE_RESOURCE,
                )
            };
            assert!(!module.is_null(), "cannot map {exe}");
            // Image-resource handles have their low bits set as a marker.
            let base = module as usize & !0b11;
            // SAFETY: `base` is the mapped image, which stays mapped for the rest of the test.
            let symbols =
                unsafe { super::super::symbols::load_for_image(base, &game_dir, &cache_dir) }
                    .unwrap();
            let bindings = Bindings::resolve(&symbols).unwrap();
            println!("{exe}: base {base:#x}\n{bindings:#x?}");
            assert!(bindings.pickup_size > 0 && bindings.material_size > 0);
            assert!(bindings.draw_line.is_available() && bindings.wall_extras.is_available());
            assert!(bindings.light_value.is_available() && bindings.menu.is_available());
            assert!(bindings.save_slots.is_available());
            let pickup = symbols.layout("cell_pickup").unwrap();
            assert!(
                pickup.offset_of::<f64>("alpha").is_err(),
                "a field read as the wrong size is refused"
            );
        }
    }
}
