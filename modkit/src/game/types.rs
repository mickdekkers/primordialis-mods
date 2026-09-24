//! Our `#[repr(C)]` mirrors of game types, checked against the game's symbols when the mod starts
//! (see `bindings`), since they are passed to and from game functions.

/// The game's `real_2`: a position or direction in world units.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Real2 {
    pub x: f32,
    pub y: f32,
}

impl Real2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Real2 { x, y }
    }

    /// Whether both are exactly the same bits: unlike `==`, tells `0.0` and `-0.0` apart and matches
    /// NaN with itself. For detecting whether a value in game memory was changed.
    pub fn same_bits(self, other: Real2) -> bool {
        self.x.to_bits() == other.x.to_bits() && self.y.to_bits() == other.y.to_bits()
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// The game's `real_4x4`. The game uploads these with `transpose = GL_TRUE`, so they are stored
/// row-major: element (row, column) is `data[row * 4 + column]`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Real4x4 {
    pub data: [f32; 16],
}

/// The game's `icon_render_info`: one icon for `Frame::draw_cell_icons`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IconRenderInfo {
    /// Center, in world units.
    pub x: [f32; 3],
    /// Radius, in world units.
    pub r: f32,
    /// RGBA.
    pub color: [f32; 4],
    /// Which icon: a cell material's `icon_uv`.
    pub uv: [f32; 2],
}

/// The game's `circle_render_info`: one filled circle for `Frame::draw_circles`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CircleRenderInfo {
    /// Center, in world units.
    pub x: [f32; 3],
    /// Radius, in world units.
    pub r: f32,
    /// RGBA.
    pub color: [f32; 4],
}

/// The game's `wall_t`: a sample of the wall distance field.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Wall {
    /// Distance to the nearest wall surface: negative inside a wall.
    pub dist: f32,
    /// Unit direction away from the wall.
    pub gradient: Real2,
    pub flow: Real2,
    pub air_dist: f32,
}
