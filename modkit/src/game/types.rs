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

    pub fn distance(self, other: Real2) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }

    /// Cheaper than `distance`, for comparing distances.
    pub fn distance_squared(self, other: Real2) -> f32 {
        (self.x - other.x).powi(2) + (self.y - other.y).powi(2)
    }

    /// The point `t` of the way from here to `to`: here at 0, `to` at 1.
    pub fn lerp(self, to: Real2, t: f32) -> Real2 {
        Real2::new(self.x + (to.x - self.x) * t, self.y + (to.y - self.y) * t)
    }
}

/// The game's `real_3`. Its UI functions take positions as one, with a `z` that orders what's drawn
/// over what.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Real3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
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

/// The game's `line_render_info`: one line for `Frame::draw_lines`, with round caps. Make one with
/// `new`: the game stores the second point relative to the first, and half the width.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LineRenderInfo {
    /// The start, in world units.
    pub(crate) x: [f32; 3],
    /// From the start to the end.
    pub(crate) d: [f32; 2],
    /// Half the width, which is also how far the round caps stick out past the ends.
    pub(crate) r: f32,
    /// RGBA.
    pub(crate) color: [f32; 4],
}

impl LineRenderInfo {
    /// A line from `from` to `to`, `width` wide, in world units.
    pub fn new(from: Real2, to: Real2, width: f32, color: [f32; 4]) -> Self {
        LineRenderInfo {
            x: [from.x, from.y, 0.0],
            d: [to.x - from.x, to.y - from.y],
            r: width / 2.0,
            color,
        }
    }

    pub fn start(&self) -> Real2 {
        Real2::new(self.x[0], self.x[1])
    }

    pub fn end(&self) -> Real2 {
        Real2::new(self.x[0] + self.d[0], self.x[1] + self.d[1])
    }

    pub fn width(&self) -> f32 {
        self.r * 2.0
    }

    pub fn color(&self) -> [f32; 4] {
        self.color
    }
}

/// The game's `tooltip_t`: a tooltip that its `do_tooltip` draws and animates from frame to frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TooltipState {
    /// The box's size, easing towards the size of its contents.
    pub box_size: Real2,
    /// The box's position, in UI units.
    pub pos: Real2,
    /// Fades in from 0 to 1 while the tooltip is shown, and back out.
    pub alpha: f32,
    /// What the tooltip is about: for cells (type 0), a material index.
    pub last_hovered_index: i32,
    pub last_hovered_type: i32,
    pub last_hovered_imbue: i32,
    /// Where the tooltip points, in UI units.
    pub last_hovered_mutation_pos: Real2,
    /// Bit 0 is `is_combo`: the cell is a combo pickup.
    pub flags: u32,
    pub consumable_instructions: u32,
}

/// The game's `translation_info`, which `do_tooltip` takes by value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TranslationInfo {
    pub mutagen_material_index: i32,
    pub combine_material_index: i32,
}

/// The game's `wall_t`, as `wall_map` returns it. `flow` and `air_dist` aren't read, only mirrored
/// for the struct's size.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct WallSample {
    pub dist: f32,
    pub gradient: Real2,
    pub flow: Real2,
    pub air_dist: f32,
}

/// A sample of the wall distance field (the game's `wall_t`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wall {
    /// Distance to the nearest wall surface: negative inside a wall.
    pub dist: f32,
    /// Unit direction away from the wall.
    pub gradient: Real2,
}

impl From<WallSample> for Wall {
    fn from(sample: WallSample) -> Self {
        Wall {
            dist: sample.dist,
            gradient: sample.gradient,
        }
    }
}
