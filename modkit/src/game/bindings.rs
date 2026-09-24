//! Where everything the bindings use is in the running game, resolved by name from its symbols.
//!
//! Nothing here is hardcoded to a specific game build: addresses and field offsets come from the PDB
//! of the running executable. Layouts we mirror in our own `#[repr(C)]` types are verified against
//! the PDB, so a game update that changes them disables the mod instead of corrupting memory.
//!
//! To bind something new, add it here, then expose it through a safe accessor (`world`, `render`).

use std::mem::{offset_of, size_of};

use super::symbols::Symbols;
use super::types::{CircleRenderInfo, IconRenderInfo, Real2, Real4x4, Wall};
use crate::{Result, log};

/// Game function addresses and data locations.
#[derive(Clone, Copy, Debug)]
pub struct Bindings {
    pub render_game: usize,
    pub begin_trace_stage: usize,
    pub draw_cell_icons: usize,
    pub draw_circles: usize,
    /// `wall_t wall_map(map_t*, real_2, bool)`: the wall distance field the game's physics uses.
    pub wall_map: usize,

    /// The global `world w`.
    pub world: usize,
    /// `w.cell_pickups` (`cell_pickup*`) and `w.n_cell_pickups` (`int`), as offsets in `world`.
    pub cell_pickups: usize,
    pub n_cell_pickups: usize,
    /// `w.map` (`map_t`), as an offset in `world`.
    pub map: usize,
    /// `w.map.explored` (`float*`, one value per map hex) and `w.map.map_range` (hex bounds), as offsets
    /// in `world`.
    pub explored: usize,
    pub map_range: MapRange,
    /// `w.frame_number` (`int`), which counts simulation steps.
    pub frame_number: usize,

    pub pickup_size: usize,
    pub pickup_material_index: usize,
    pub pickup_x: usize,
    pub pickup_r: usize,
    /// The `is_combo` bitfield: byte offset of its `u32` storage and bit position.
    pub pickup_is_combo: (usize, u32),

    /// Globals `material_t* materials_list` and `int n_materials`.
    pub materials_list: usize,
    pub n_materials: usize,
    pub material_size: usize,
    pub material_base_color: usize,
    pub material_uv: usize,

    /// `float map_icon_alpha`, a static in `render_game`: 0 when the map is closed, fading to 1 while
    /// it is open.
    pub map_icon_alpha: usize,

    /// `render_context.camera` (world-to-clip matrix) and `render_context.camera_pos`.
    pub rc_camera: usize,
    pub rc_camera_pos: usize,
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
        let render_context = symbols.layout("render_context")?;

        verify_mirrored_layouts(symbols)?;
        expect_size(symbols, "materials_list", size_of::<usize>())?;
        expect_size(symbols, "n_materials", size_of::<i32>())?;
        // A static inside render_game that it fades between 0 and 1 as the map closes and opens.
        let (map_icon_alpha, alpha_size) = symbols.function_static("render_game", "map_icon_alpha")?;
        expect(alpha_size == size_of::<f32>(), "map_icon_alpha is no longer a float")?;
        expect(int2.size == 8 && int2.offset("x")? == 0 && int2.offset("y")? == 4, "int_2 layout changed")?;

        let map_offset = world.offset("map")?;
        let range = map_offset + map.offset("map_range")?;
        let (lower, upper) = (bounds.offset("l")?, bounds.offset("u")?);

        let bindings = Bindings {
            render_game: symbols.address("render_game")?,
            begin_trace_stage: symbols.address("begin_trace_stage")?,
            draw_cell_icons: symbols.address("draw_cell_icons")?,
            draw_circles: symbols.address("draw_circles")?,
            wall_map: symbols.address("wall_map")?,

            world: symbols.address("w")?,
            cell_pickups: world.offset("cell_pickups")?,
            n_cell_pickups: world.offset("n_cell_pickups")?,
            map: map_offset,
            explored: map_offset + map.offset("explored")?,
            map_range: MapRange {
                lower_x: range + lower,
                lower_y: range + lower + 4,
                upper_x: range + upper,
                upper_y: range + upper + 4,
            },
            frame_number: world.offset("frame_number")?,

            pickup_size: pickup.size,
            pickup_material_index: pickup.offset("material_index")?,
            pickup_x: pickup.offset("x")?,
            pickup_r: pickup.offset("r")?,
            pickup_is_combo: pickup.flag("is_combo")?,

            materials_list: symbols.address("materials_list")?,
            n_materials: symbols.address("n_materials")?,
            material_size: material.size,
            material_base_color: material.offset("base_color")?,
            material_uv: material.offset("uv")?,

            map_icon_alpha,

            rc_camera: render_context.offset("camera")?,
            rc_camera_pos: render_context.offset("camera_pos")?,
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
    let wall = symbols.layout("wall_t")?;
    let matches = wall.size == size_of::<Wall>()
        && wall.offset("dist")? == offset_of!(Wall, dist)
        && wall.offset("gradient")? == offset_of!(Wall, gradient);
    expect(matches, "wall_t layout changed")?;
    expect(symbols.layout("real_2")?.size == size_of::<Real2>(), "real_2 size changed")?;
    expect(symbols.layout("real_4x4")?.size == size_of::<Real4x4>(), "real_4x4 size changed")?;
    Ok(())
}

fn expect_size(symbols: &Symbols, variable: &str, size: usize) -> Result<()> {
    let actual = symbols.variable_size(variable)?;
    expect(actual == size, &format!("`{variable}` is {actual} bytes, expected {size}"))
}

fn expect(condition: bool, message: &str) -> Result<()> {
    if condition { Ok(()) } else { Err(message.to_owned()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW};

    /// Resolves the bindings against a real game install, mapping the executable as an image (nothing
    /// in it runs). Run with:
    /// `PRIMORDIALIS_DIR=<game dir> cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a Primordialis install; set PRIMORDIALIS_DIR"]
    fn resolves_against_installed_game() {
        let game_dir = PathBuf::from(std::env::var("PRIMORDIALIS_DIR").expect("set PRIMORDIALIS_DIR"));
        let cache_dir = std::env::temp_dir().join("primordialis_qol_test_cache");
        for exe in ["primordialis_avx.exe", "primordialis_sse3.exe"] {
            let path: Vec<u16> = game_dir.join(exe).to_string_lossy().encode_utf16().chain(Some(0)).collect();
            // SAFETY: Maps the executable as an image resource; none of its code runs.
            let module = unsafe { LoadLibraryExW(path.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_AS_IMAGE_RESOURCE) };
            assert!(!module.is_null(), "cannot map {exe}");
            // Image-resource handles have their low bits set as a marker.
            let base = module as usize & !0b11;
            // SAFETY: `base` is the mapped image, which stays mapped for the rest of the test.
            let symbols = unsafe { super::super::symbols::load_for_image(base, &game_dir, &cache_dir) }.unwrap();
            let bindings = Bindings::resolve(&symbols).unwrap();
            println!("{exe}: base {base:#x}\n{bindings:#x?}");
            assert!(bindings.pickup_size > 0 && bindings.material_size > 0);
        }
    }
}
