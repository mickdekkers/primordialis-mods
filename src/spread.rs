//! Spreads out map icons that overlap while the mouse is over them, so they can be told apart. The
//! icons move onto a hexagonal grid, built around the icon under the mouse. Only where the icons are
//! drawn changes; the pickups stay where they are.
//!
//! The grid stays spread out while the mouse is within its bounds: the hexagon through its outermost
//! icons, with a margin. Icons too close to an icon on the grid join it, each taking the free spot
//! nearest to where it is, as do icons within its bounds that overlap others, until there are none
//! left. So while the grid is open, no icon under the mouse can be covered by another. There's no
//! limit to how many icons a grid takes.
//!
//! Once open, a grid keeps its icons on their spots, and only grows: icons that come near it (say,
//! when zooming out) join it, but nothing leaves or moves until it collapses.
//!
//! Positions and distances are in world units. The grid's spacing scales with the icon radius, so a
//! grid looks the same at any map zoom level.

use modkit::game::{PickupsId, Real2};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::math::{self, TickClock};

/// Distance between neighboring grid spots, in icon radii.
const SPACING: f32 = 2.3;
/// The mouse points at an icon when it's within this many icon radii of its center.
pub const HOVER_DISTANCE: f32 = 1.05;
/// Once spread out, from this far: its cell's corners (the grid spacing / √3), the middle of every
/// triangle of icons, so between adjacent icons the mouse always points at one of them. Where a
/// neighboring spot is empty, this only reaches into its rim, so larger gaps still point at nothing.
pub const GRID_HOVER_DISTANCE: f32 = SPACING * 0.577_350_26;
/// Icons with their centers closer than this many icon radii to an icon on the grid join it: they'd
/// crowd it, and the mouse could point at them from within its hover area.
const CLEAR_DISTANCE: f32 = GRID_HOVER_DISTANCE + HOVER_DISTANCE;
/// The grid stays spread out while the mouse is within this many icon radii of the hexagon through
/// its outermost icons. Icons are a fixed fraction of the screen's height, so this is too.
const KEEP_OPEN_MARGIN: f32 = 3.5;
/// Icons closer than this many icon radii overlap, or nearly. Those the mouse could point at while
/// the grid is open join it.
const OVERLAP_DISTANCE: f32 = 2.1;
/// How long spreading out and collapsing back take, in `frame_number` ticks (120 per second).
const OPEN_TICKS: f32 = 18.0;
const CLOSE_TICKS: f32 = 12.0;

/// The grid being spread out, or collapsing back.
#[derive(Default)]
pub struct Spread {
    grid: Option<Grid>,
    spiral: Spiral,
    /// The pickup array the grid's pickup indices index.
    pickups: Option<PickupsId>,
    clock: TickClock,
    /// The icons drawn elsewhere this frame. Reused every frame, to avoid allocating.
    moved: Vec<Moved>,
}

/// An icon on the grid: its pickup's position, and where to draw it instead.
#[derive(Clone, Copy, Debug)]
pub struct Moved {
    /// Index in the icons passed to `update`.
    pub icon: usize,
    pub from: Real2,
    pub to: Real2,
    /// How far from `from` to `to` it's drawn, from 0 to 1 (eased).
    pub progress: f32,
}

impl Spread {
    /// Call once per frame with every icon on the map: `pickups[i]` (sorted) is drawn at
    /// `positions[i]`, with icon radius `radius`. `mouse` is where the mouse points on the map, if
    /// anywhere. Afterwards, `moved` has the icons to draw elsewhere.
    pub fn update(
        &mut self,
        pickups_id: PickupsId,
        pickups: &[usize],
        positions: &[Real2],
        radius: f32,
        mouse: Option<Real2>,
        frame_number: i32,
    ) {
        self.moved.clear();
        let ticks = self.clock.advance(frame_number, 30);
        if self.pickups != Some(pickups_id) {
            self.pickups = Some(pickups_id);
            self.close_now();
        }
        if !(radius > 0.0 && radius.is_finite()) {
            self.close_now();
            return;
        }
        let spacing = SPACING * radius;

        let inside = self
            .grid
            .as_ref()
            .zip(mouse)
            .is_some_and(|(grid, mouse)| grid.keeps_open(radius, mouse));
        if let (true, Some(grid)) = (inside, &mut self.grid) {
            grid.open = true;
            grid.grow(&mut self.spiral, pickups, positions, radius);
        } else {
            // A new grid around the icon under the mouse, if it overlaps others.
            let under_mouse = mouse.and_then(|mouse| {
                (0..positions.len())
                    .map(|icon| (icon, positions[icon].distance(mouse)))
                    .filter(|&(_, d)| d <= HOVER_DISTANCE * radius)
                    .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
                    .map(|(icon, _)| icon)
            });
            let clear = CLEAR_DISTANCE * radius;
            let new = under_mouse
                .filter(|&icon| {
                    let current = self.grid.as_ref().map(|grid| grid.anchor);
                    current != Some(pickups[icon])
                })
                // Only another icon near this one makes a grid: check before building one.
                .filter(|&icon| {
                    let at = positions[icon];
                    positions
                        .iter()
                        .enumerate()
                        .any(|(other, &p)| other != icon && p.distance(at) < clear)
                })
                .map(|icon| Grid::new(&mut self.spiral, pickups, positions, icon, radius))
                .filter(|grid| grid.spots.len() > 1);
            match (new, &mut self.grid) {
                // A different grid: the previous one collapses at once.
                (Some(new), _) => self.grid = Some(new),
                (None, Some(grid)) => grid.open = false,
                (None, None) => {}
            }
        }
        let Some(grid) = &mut self.grid else {
            return;
        };

        // Icons spread out while the grid is open, including ones that just joined, and collapse
        // back once it isn't.
        for spot in &mut grid.spots {
            spot.progress = if grid.open {
                (spot.progress + ticks / OPEN_TICKS).min(1.0)
            } else {
                (spot.progress - ticks / CLOSE_TICKS).max(0.0)
            };
        }
        if !grid.open && grid.spots.iter().all(|spot| spot.progress == 0.0) {
            self.grid = None;
            return;
        }
        let lattice = grid.lattice(spacing);
        for spot in &grid.spots {
            // Icons that are no longer shown keep their spots, but aren't drawn.
            let Ok(icon) = pickups.binary_search(&spot.pickup) else {
                continue;
            };
            self.moved.push(Moved {
                icon,
                from: positions[icon],
                to: lattice.position(spot.hex),
                progress: math::smoothstep(0.0, 1.0, spot.progress),
            });
        }
    }

    /// The icons to draw elsewhere this frame, as of the last `update`.
    pub fn moved(&self) -> &[Moved] {
        &self.moved
    }

    /// Collapses the grid without animating, e.g. when the map closes.
    pub fn close_now(&mut self) {
        self.grid = None;
        self.moved.clear();
    }
}

/// Icons on a hexagonal grid, and the cells it covers.
struct Grid {
    /// The pickup index of the icon the grid was built around, and where it was then: the spot at
    /// the grid's origin.
    anchor: usize,
    origin: Real2,
    /// Whether the mouse keeps the grid spread out.
    open: bool,
    spots: Vec<Spot>,
    /// Whether each pickup is on the grid, by pickup index.
    on_grid: Vec<bool>,
    /// The spots' cells.
    taken: FxHashSet<Hex>,
    /// The hexagon through the outermost spots.
    bounds: Bounds,
    /// For each cell that an icon was nearest to when it joined: how many of the spots nearest to
    /// it (in `Spiral` order) are known to be taken.
    taken_near: FxHashMap<Hex, usize>,
}

#[derive(Clone, Copy, Debug)]
struct Spot {
    pickup: usize,
    hex: Hex,
    /// How far it's spread out, from 0 to 1.
    progress: f32,
}

impl Grid {
    /// The grid around icon `anchor`, with the other icons on the map at `positions`, and their
    /// pickup indices `pickups`.
    fn new(
        spiral: &mut Spiral,
        pickups: &[usize],
        positions: &[Real2],
        anchor: usize,
        radius: f32,
    ) -> Grid {
        let mut grid = Grid {
            anchor: pickups[anchor],
            origin: positions[anchor],
            open: true,
            spots: Vec::new(),
            on_grid: Vec::new(),
            taken: FxHashSet::default(),
            bounds: Bounds::around(Hex { q: 0, r: 0 }),
            taken_near: FxHashMap::default(),
        };
        let lattice = grid.lattice(SPACING * radius);
        grid.join(spiral, &lattice, pickups[anchor], positions[anchor]);
        grid.grow(spiral, pickups, positions, radius);
        grid
    }

    fn lattice(&self, spacing: f32) -> Lattice {
        Lattice {
            origin: self.origin,
            spacing,
        }
    }

    /// Adds every icon too close to one on the grid, in one pass over the grid's icons in the order
    /// they joined: the icons too close to each join in turn, nearest first, to be passed over
    /// later. Then adds the icons the mouse could point at while the grid is open that overlap
    /// others, passes over those, and so on, until there are none left.
    fn grow(&mut self, spiral: &mut Spiral, pickups: &[usize], positions: &[Real2], radius: f32) {
        let lattice = self.lattice(SPACING * radius);
        let clear = CLEAR_DISTANCE * radius;
        let clear_squared = clear * clear;
        let overlap_squared = (OVERLAP_DISTANCE * radius).powi(2);
        // How far outside its bounds the mouse can point at icons while the grid is open.
        let reach = (KEEP_OPEN_MARGIN + HOVER_DISTANCE) * radius;
        if let Some(&last) = pickups.last()
            && self.on_grid.len() <= last
        {
            self.on_grid.resize(last + 1, false);
        }

        // Only icons this near the grid's bounds can join it, or overlap one that can.
        let nearby = clear.max(reach + OVERLAP_DISTANCE * radius);
        let square = |at: Real2| ((at.x / clear).floor() as i32, (at.y / clear).floor() as i32);
        let mut squares: FxHashMap<(i32, i32), Vec<usize>> = FxHashMap::default();
        let mut near: Vec<(f32, usize)> = Vec::new();
        let mut crowded: Vec<usize> = Vec::new();
        loop {
            // The icons off the grid near its bounds, by `clear` wide squares, so each spot only looks
            // at the icons around it. Icons leave their squares as they join.
            let bounds = self.bounds;
            squares.clear();
            for (icon, &at) in positions.iter().enumerate() {
                if !self.on_grid[pickups[icon]] && bounds.contains(&lattice, at, nearby) {
                    squares.entry(square(at)).or_default().push(icon);
                }
            }

            let mut next = 0;
            loop {
                while next < self.spots.len() {
                    let at = lattice.position(self.spots[next].hex);
                    next += 1;
                    let (x, y) = square(at);
                    near.clear();
                    for dx in -1..=1 {
                        for dy in -1..=1 {
                            if let Some(icons) = squares.get_mut(&(x + dx, y + dy)) {
                                icons.retain(|&icon| {
                                    let d = positions[icon].distance_squared(at);
                                    let is_near = d < clear_squared;
                                    if is_near {
                                        near.push((d, icon));
                                    }
                                    !is_near
                                });
                            }
                        }
                    }
                    near.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                    for &(_, icon) in &near {
                        self.join(spiral, &lattice, pickups[icon], positions[icon]);
                    }
                }

                // Icons off the grid that the mouse could point at while it's open, and that others
                // overlap, so they couldn't be told apart.
                crowded.clear();
                for icons in squares.values() {
                    for &icon in icons {
                        let at = positions[icon];
                        if !self.bounds.contains(&lattice, at, reach) {
                            continue;
                        }
                        let (x, y) = square(at);
                        let overlapped = (-1..=1).any(|dx| {
                            (-1..=1).any(|dy| {
                                squares.get(&(x + dx, y + dy)).is_some_and(|others| {
                                    others.iter().any(|&other| {
                                        other != icon
                                            && positions[other].distance_squared(at)
                                                < overlap_squared
                                    })
                                })
                            })
                        });
                        if overlapped {
                            crowded.push(icon);
                        }
                    }
                }
                if crowded.is_empty() {
                    break;
                }
                crowded.sort_unstable();
                for &icon in &crowded {
                    if let Some(icons) = squares.get_mut(&square(positions[icon]))
                        && let Some(i) = icons.iter().position(|&other| other == icon)
                    {
                        icons.swap_remove(i);
                    }
                    self.join(spiral, &lattice, pickups[icon], positions[icon]);
                }
            }

            // Icons that joined can take the grid near icons not looked at yet: look again.
            if self.bounds == bounds {
                break;
            }
        }
    }

    /// Puts pickup `pickup`, whose icon is at `at`, on the free spot nearest to it.
    fn join(&mut self, spiral: &mut Spiral, lattice: &Lattice, pickup: usize, at: Real2) {
        let hex = self.nearest_free(spiral, lattice, at);
        self.spots.push(Spot {
            pickup,
            hex,
            progress: 0.0,
        });
        if self.on_grid.len() <= pickup {
            self.on_grid.resize(pickup + 1, false);
        }
        self.on_grid[pickup] = true;
        self.taken.insert(hex);
        self.bounds.extend(hex);
    }

    /// The free spot nearest to `at`: searching the spots around the cell `at` is in, nearest
    /// first, starting past the ones known to be taken.
    fn nearest_free(&mut self, spiral: &mut Spiral, lattice: &Lattice, at: Real2) -> Hex {
        let center = lattice.round(at);
        // How far `at` is from its cell's center, in grid spacings: the spots' distances from `at`
        // are within this much of their distances from that center.
        let off_center = at.distance(lattice.position(center)) / lattice.spacing;
        let offset = |i: usize, spiral: &Spiral| {
            let (q, r) = spiral.offsets[i].0;
            Hex {
                q: center.q + q,
                r: center.r + r,
            }
        };

        let mut first = self.taken_near.get(&center).copied().unwrap_or(0);
        loop {
            spiral.extend_to(first);
            if !self.taken.contains(&offset(first, spiral)) {
                break;
            }
            first += 1;
        }
        self.taken_near.insert(center, first);

        // Spots further along can still be nearer to `at`, up to twice its distance off center.
        let hex = offset(first, spiral);
        let mut best = (lattice.position(hex).distance(at) / lattice.spacing, hex);
        let mut i = first + 1;
        loop {
            spiral.extend_to(i);
            if spiral.offsets[i].1 - off_center > best.0 {
                break;
            }
            let hex = offset(i, spiral);
            if !self.taken.contains(&hex) {
                let d = lattice.position(hex).distance(at) / lattice.spacing;
                if d < best.0 {
                    best = (d, hex);
                }
            }
            i += 1;
        }
        best.1
    }

    /// Whether the mouse at `at` keeps the grid spread out, with icons `radius` large.
    fn keeps_open(&self, radius: f32, at: Real2) -> bool {
        let lattice = self.lattice(SPACING * radius);
        self.bounds
            .contains(&lattice, at, KEEP_OPEN_MARGIN * radius)
    }
}

/// The hexagon through the outermost spots of a grid: the ranges of their cube coordinates (q, r
/// and s = -q - r), each of which runs along one of the grid's three axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    min: [i32; 3],
    max: [i32; 3],
}

impl Bounds {
    fn around(hex: Hex) -> Bounds {
        let cube = hex.cube();
        Bounds {
            min: cube,
            max: cube,
        }
    }

    fn extend(&mut self, hex: Hex) {
        for (axis, value) in hex.cube().into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    /// Whether `at` is within `margin` (in world units) of the hexagon, or in it.
    fn contains(&self, lattice: &Lattice, at: Real2, margin: f32) -> bool {
        let (q, r) = lattice.fractional(at);
        // Going 1 along a cube axis moves this far across the lines where it's constant.
        let margin = margin / (lattice.spacing * Lattice::SIN_60);
        [q, r, -q - r].into_iter().enumerate().all(|(axis, value)| {
            value >= self.min[axis] as f32 - margin && value <= self.max[axis] as f32 + margin
        })
    }
}

/// The offsets of the spots of a hexagonal grid from one of them, nearest first, with their
/// distances in grid spacings. Grown as needed.
#[derive(Default)]
struct Spiral {
    offsets: Vec<((i32, i32), f32)>,
    /// All offsets up to this many rings out are in `offsets`...
    rings: i32,
    /// ...but only this many of them, the ones nearer than the nearest spot one more ring out, are
    /// sure to be in order.
    complete: usize,
}

impl Spiral {
    /// Makes sure `offsets[i]` exists, and that every offset nearer than it is before it.
    fn extend_to(&mut self, i: usize) {
        while self.complete <= i {
            self.rings = (self.rings * 2).max(8);
            let center = Hex { q: 0, r: 0 };
            let unit = Lattice {
                origin: Real2::default(),
                spacing: 1.0,
            };
            self.offsets = (0..=self.rings)
                .flat_map(|ring| hex_ring(center, ring))
                .map(|hex| {
                    (
                        (hex.q, hex.r),
                        unit.position(hex).distance(Real2::default()),
                    )
                })
                .collect();
            self.offsets
                .sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let nearest_unlisted = (self.rings + 1) as f32 * Lattice::SIN_60;
            self.complete = self.offsets.partition_point(|&(_, d)| d < nearest_unlisted);
        }
    }
}

/// A spot on a hexagonal grid, in axial coordinates: `q` along the x axis, `r` 60° from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Hex {
    q: i32,
    r: i32,
}

impl Hex {
    /// Cube coordinates: q, r and s = -q - r.
    fn cube(self) -> [i32; 3] {
        [self.q, self.r, -self.q - self.r]
    }
}

/// The hexagonal grid of spots `spacing` apart, with one at `origin`.
struct Lattice {
    origin: Real2,
    spacing: f32,
}

impl Lattice {
    const SIN_60: f32 = 0.866_025_4;

    fn position(&self, hex: Hex) -> Real2 {
        let (q, r) = (hex.q as f32, hex.r as f32);
        Real2::new(
            self.origin.x + self.spacing * (q + r / 2.0),
            self.origin.y + self.spacing * r * Self::SIN_60,
        )
    }

    /// Where `at` is, in fractional axial coordinates (q, r).
    fn fractional(&self, at: Real2) -> (f32, f32) {
        let r = (at.y - self.origin.y) / (self.spacing * Self::SIN_60);
        let q = (at.x - self.origin.x) / self.spacing - r / 2.0;
        (q, r)
    }

    /// The spot nearest to `at`: the one whose cell `at` is in.
    fn round(&self, at: Real2) -> Hex {
        let (q, r) = self.fractional(at);
        // Round in cube coordinates (q + r + s = 0), fixing the one that rounded the most.
        let s = -q - r;
        let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
        let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
        if dq > dr && dq > ds {
            rq = -rr - rs;
        } else if dr > ds {
            rr = -rq - rs;
        }
        Hex {
            q: rq as i32,
            r: rr as i32,
        }
    }
}

/// The spots `ring` steps from `center`.
fn hex_ring(center: Hex, ring: i32) -> impl Iterator<Item = Hex> {
    const DIRECTIONS: [(i32, i32); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (-1, 1), (0, 1)];
    let start = Hex {
        q: center.q + DIRECTIONS[4].0 * ring,
        r: center.r + DIRECTIONS[4].1 * ring,
    };
    let steps = if ring == 0 { 1 } else { 6 * ring as usize };
    (0..steps).scan(start, move |at, step| {
        let here = *at;
        let (dq, dr) = DIRECTIONS[step / ring.max(1) as usize % 6];
        *at = Hex {
            q: at.q + dq,
            r: at.r + dr,
        };
        Some(here)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(positions: &[Real2], anchor: usize, radius: f32) -> Grid {
        let pickups: Vec<usize> = (0..positions.len()).collect();
        Grid::new(&mut Spiral::default(), &pickups, positions, anchor, radius)
    }

    fn min_gap(points: &[Real2]) -> f32 {
        let mut gap = f32::INFINITY;
        for (i, &a) in points.iter().enumerate() {
            for &b in &points[i + 1..] {
                gap = gap.min(a.distance(b));
            }
        }
        gap
    }

    /// Checks a grid: its icons are a spacing apart, no icon off it is too close to one on it, and
    /// none that the mouse could point at while it's open is overlapped by another.
    fn check(grid: &Grid, positions: &[Real2], radius: f32) {
        let lattice = grid.lattice(SPACING * radius);
        let spots: Vec<Real2> = grid.spots.iter().map(|s| lattice.position(s.hex)).collect();
        assert!(min_gap(&spots) > SPACING * radius - 1e-3, "{spots:?}");
        let off: Vec<usize> = (0..positions.len())
            .filter(|&icon| !grid.on_grid.get(icon).copied().unwrap_or(false))
            .collect();
        let reach = (KEEP_OPEN_MARGIN + HOVER_DISTANCE) * radius;
        for &icon in &off {
            let p = positions[icon];
            for &spot in &spots {
                assert!(p.distance(spot) >= CLEAR_DISTANCE * radius - 1e-3, "{icon}");
            }
            if grid.bounds.contains(&lattice, p, reach) {
                for &other in &off {
                    let d = positions[other].distance(p);
                    assert!(
                        other == icon || d >= OVERLAP_DISTANCE * radius,
                        "{icon}, {other}"
                    );
                }
            }
        }
    }

    #[test]
    fn hex_rings_have_the_right_spots() {
        let center = Hex { q: 2, r: -1 };
        assert_eq!(hex_ring(center, 0).collect::<Vec<_>>(), [center]);
        for ring in 1..5 {
            let hexes: Vec<Hex> = hex_ring(center, ring).collect();
            assert_eq!(hexes.len(), 6 * ring as usize);
            for hex in &hexes {
                let (dq, dr) = (hex.q - center.q, hex.r - center.r);
                let steps = (dq.abs() + dr.abs() + (dq + dr).abs()) / 2;
                assert_eq!(steps, ring, "{hexes:?}");
            }
            for (i, a) in hexes.iter().enumerate() {
                assert!(!hexes[i + 1..].contains(a));
            }
        }
    }

    #[test]
    fn the_spiral_is_in_order_and_complete() {
        let mut spiral = Spiral::default();
        spiral.extend_to(2000);
        let complete = spiral.complete;
        assert!(complete > 2000);
        let listed = &spiral.offsets[..complete];
        assert!(listed.windows(2).all(|w| w[0].1 <= w[1].1));
        // Every spot within the last listed distance is listed.
        let reach = listed[complete - 1].1;
        let unit = Lattice {
            origin: Real2::default(),
            spacing: 1.0,
        };
        let within = (0..=spiral.rings)
            .flat_map(|ring| hex_ring(Hex { q: 0, r: 0 }, ring))
            .filter(|&hex| unit.position(hex).distance(Real2::default()) < reach)
            .count();
        assert!(within <= complete);
    }

    #[test]
    fn rounding_finds_the_nearest_spot() {
        let lattice = Lattice {
            origin: Real2::new(3.0, -2.0),
            spacing: 2.0,
        };
        for q in -3..=3 {
            for r in -3..=3 {
                let hex = Hex { q, r };
                let at = lattice.position(hex);
                let nudged = Real2::new(at.x + 0.3, at.y - 0.4);
                assert_eq!(lattice.round(nudged), hex);
            }
        }
    }

    #[test]
    fn a_pile_spreads_around_the_anchor() {
        let positions: Vec<Real2> = (0..10)
            .map(|i| Real2::new((i % 3) as f32 * 0.1, (i / 3) as f32 * 0.1))
            .collect();
        let grid = build(&positions, 4, 1.0);
        assert_eq!(grid.spots.len(), 10);
        assert_eq!(
            grid.spots[0].hex,
            Hex { q: 0, r: 0 },
            "the anchor stays put"
        );
        check(&grid, &positions, 1.0);
        assert!(grid.keeps_open(1.0, positions[4]));
        assert!(!grid.keeps_open(1.0, Real2::new(30.0, 0.0)));
    }

    #[test]
    fn icons_near_the_grid_join_it() {
        // A pile of three, an icon that ends up near once they spread out, one near that, and one
        // far away.
        let positions = [
            Real2::new(0.0, 0.0),
            Real2::new(0.1, 0.0),
            Real2::new(0.0, 0.1),
            Real2::new(3.5, 0.0),
            Real2::new(5.5, 0.5),
            Real2::new(40.0, 0.0),
        ];
        let grid = build(&positions, 0, 1.0);
        let mut pickups: Vec<usize> = grid.spots.iter().map(|s| s.pickup).collect();
        pickups.sort_unstable();
        assert_eq!(pickups, [0, 1, 2, 3, 4]);
        check(&grid, &positions, 1.0);
    }

    #[test]
    fn lone_icons_make_no_grid() {
        let positions = [Real2::new(0.0, 0.0), Real2::new(5.0, 0.0)];
        assert_eq!(build(&positions, 0, 1.0).spots.len(), 1);
    }

    #[test]
    fn bounds_reach_a_margin_past_the_outermost_spots() {
        let lattice = Lattice {
            origin: Real2::new(0.0, 0.0),
            spacing: SPACING,
        };
        let center = Hex { q: 0, r: 0 };
        let mut bounds = Bounds::around(center);
        for hex in hex_ring(center, 3) {
            bounds.extend(hex);
        }
        let margin = 2.0;
        assert!(bounds.contains(&lattice, Real2::new(0.0, 0.0), margin));
        // Straight out from the middle of the top edge (3 rows up), that's exactly the margin; out
        // past the right corner (3 spacings right), up to 2 / √3 times as far.
        let top = 3.0 * SPACING * Lattice::SIN_60;
        assert!(bounds.contains(&lattice, Real2::new(0.0, top + margin - 0.01), margin));
        assert!(!bounds.contains(&lattice, Real2::new(0.0, top + margin + 0.01), margin));
        let right = 3.0 * SPACING;
        let past_corner = margin / Lattice::SIN_60;
        assert!(bounds.contains(
            &lattice,
            Real2::new(right + past_corner - 0.01, 0.0),
            margin
        ));
        assert!(!bounds.contains(
            &lattice,
            Real2::new(right + past_corner + 0.01, 0.0),
            margin
        ));
    }

    #[test]
    fn the_grid_stays_open_over_its_gaps() {
        // A ring of icons around an empty middle, three spacings out, with two piled up so there's
        // a grid.
        let lattice = Lattice {
            origin: Real2::new(0.0, 0.0),
            spacing: SPACING,
        };
        let mut positions: Vec<Real2> = hex_ring(Hex { q: 0, r: 0 }, 3)
            .map(|hex| lattice.position(hex))
            .collect();
        positions.push(positions[0]);
        let grid = build(&positions, 0, 1.0);
        assert_eq!(grid.spots.len(), positions.len());
        check(&grid, &positions, 1.0);
        assert!(grid.keeps_open(1.0, Real2::new(0.0, 0.0)));
        assert!(!grid.keeps_open(1.0, Real2::new(0.0, 30.0)));
    }

    #[test]
    fn overlapping_icons_within_reach_join() {
        // A pile of two, then past the grid's edge: a pair overlapping each other and a lone icon
        // within the margin, and a pair beyond it.
        let positions = [
            Real2::new(0.0, 0.0),
            Real2::new(0.1, 0.0),
            Real2::new(5.5, 0.0),
            Real2::new(6.0, 0.5),
            Real2::new(0.0, 6.0),
            Real2::new(30.0, 0.0),
            Real2::new(30.5, 0.0),
        ];
        let grid = build(&positions, 0, 1.0);
        check(&grid, &positions, 1.0);
        let on = |icon: usize| grid.on_grid[icon];
        assert!(on(2) && on(3), "the pair within reach joins");
        assert!(!on(4), "the lone icon stays");
        assert!(!on(5) && !on(6), "the pair out of reach stays");
    }

    #[test]
    fn growing_keeps_icons_on_their_spots() {
        // Icons in a row, 3 radii apart: a pair at one end makes a grid, which zooming out (larger
        // icons) makes reach further along the row.
        let mut positions: Vec<Real2> = (0..12).map(|i| Real2::new(i as f32 * 3.0, 0.0)).collect();
        positions.push(Real2::new(0.1, 0.0));
        let pickups: Vec<usize> = (0..positions.len()).collect();
        let mut spiral = Spiral::default();
        let mut grid = Grid::new(&mut spiral, &pickups, &positions, 0, 1.0);
        let before: Vec<(usize, Hex)> = grid.spots.iter().map(|s| (s.pickup, s.hex)).collect();
        grid.grow(&mut spiral, &pickups, &positions, 1.6);
        assert!(grid.spots.len() > before.len());
        for (spot, &(pickup, hex)) in grid.spots.iter().zip(&before) {
            assert_eq!((spot.pickup, spot.hex), (pickup, hex));
        }
        check(&grid, &positions, 1.6);
    }

    #[test]
    fn every_icon_of_a_huge_pile_joins() {
        // Thousands of icons on top of each other, as when zoomed all the way out.
        let positions: Vec<Real2> = (0..3000)
            .map(|i| Real2::new((i % 50) as f32 * 0.01, (i / 50) as f32 * 0.01))
            .collect();
        let grid = build(&positions, 0, 1.0);
        assert_eq!(grid.spots.len(), positions.len());
        let taken: FxHashSet<Hex> = grid.spots.iter().map(|s| s.hex).collect();
        assert_eq!(taken.len(), positions.len(), "one icon per spot");
    }

    #[test]
    fn a_dense_field_joins_entirely() {
        let positions: Vec<Real2> = (0..400)
            .map(|i| Real2::new((i % 20) as f32 * 1.0, (i / 20) as f32 * 1.0))
            .collect();
        let grid = build(&positions, 210, 1.0);
        assert_eq!(grid.spots.len(), positions.len());
        check(&grid, &positions, 1.0);
    }
}
