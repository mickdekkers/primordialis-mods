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

mod hex;

use modkit::game::{PickupsId, Real2};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::math::{self, TickClock};
use hex::{Bounds, Hex, Lattice, Spiral};

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
/// At most this many ticks count for one frame (a quarter of a second), so that the grid doesn't jump
/// after the game was paused.
const MAX_TICKS_PER_FRAME: i32 = 30;

/// The grid being spread out, or collapsing back.
#[derive(Default)]
pub struct Spread {
    grid: Option<Grid>,
    scratch: Scratch,
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
        let ticks = self.clock.advance(frame_number, MAX_TICKS_PER_FRAME);
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
            grid.grow(&mut self.scratch, pickups, positions, radius);
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
                .map(|icon| Grid::new(&mut self.scratch, pickups, positions, icon, radius))
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

/// What building and growing grids works with, kept from one frame to the next so that a grid open
/// under the mouse doesn't allocate every frame.
#[derive(Default)]
struct Scratch {
    spiral: Spiral,
    /// Icons off the grid, by square (see `Grid::grow`).
    squares: FxHashMap<(i32, i32), Vec<usize>>,
    near: Vec<(f32, usize)>,
    crowded: Vec<usize>,
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
        scratch: &mut Scratch,
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
        grid.join(
            &mut scratch.spiral,
            &lattice,
            pickups[anchor],
            positions[anchor],
        );
        grid.grow(scratch, pickups, positions, radius);
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
    fn grow(&mut self, scratch: &mut Scratch, pickups: &[usize], positions: &[Real2], radius: f32) {
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
        let Scratch {
            spiral,
            squares,
            near,
            crowded,
        } = scratch;
        // Squares are emptied rather than removed, to reuse them, unless there are many more than
        // icons (the squares change size with the icons, so zooming leaves old ones behind).
        if squares.len() > 2 * positions.len() + 64 {
            squares.clear();
        }
        loop {
            // The icons off the grid near its bounds, by `clear` wide squares, so each spot only looks
            // at the icons around it. Icons leave their squares as they join.
            let bounds = self.bounds;
            squares.values_mut().for_each(Vec::clear);
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
                    for &(_, icon) in near.iter() {
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
                for &icon in crowded.iter() {
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

#[cfg(test)]
mod tests {
    use super::hex::hex_ring;
    use super::*;

    /// Where tests put icons: at two map zoom levels (icon radii), one of them far from the origin,
    /// since nothing should depend on either.
    #[derive(Clone, Copy, Debug)]
    struct Place {
        radius: f32,
        origin: Real2,
    }

    const PLACES: [Place; 2] = [
        Place {
            radius: 1.0,
            origin: Real2::new(0.0, 0.0),
        },
        Place {
            radius: 3.0,
            origin: Real2::new(1234.5, -987.25),
        },
    ];

    impl Place {
        /// `x` and `y` icon radii from the origin.
        fn at(self, x: f32, y: f32) -> Real2 {
            Real2::new(
                self.origin.x + x * self.radius,
                self.origin.y + y * self.radius,
            )
        }

        fn all(self, points: &[(f32, f32)]) -> Vec<Real2> {
            points.iter().map(|&(x, y)| self.at(x, y)).collect()
        }
    }

    const SIN_60: f32 = 0.866_025_4;

    fn build(positions: &[Real2], anchor: usize, radius: f32) -> Grid {
        let pickups: Vec<usize> = (0..positions.len()).collect();
        Grid::new(&mut Scratch::default(), &pickups, positions, anchor, radius)
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

    fn on_grid(grid: &Grid) -> Vec<usize> {
        let mut pickups: Vec<usize> = grid.spots.iter().map(|s| s.pickup).collect();
        pickups.sort_unstable();
        pickups
    }

    /// Checks a grid: each pickup is on it once, its icons are a spacing apart, no icon off it is too
    /// close to one on it, and none that the mouse could point at while it's open is overlapped by
    /// another.
    fn check(grid: &Grid, positions: &[Real2], radius: f32) {
        let on = on_grid(grid);
        assert!(on.windows(2).all(|w| w[0] != w[1]), "{on:?}");
        for icon in 0..positions.len() {
            let is_on = grid.on_grid.get(icon).copied().unwrap_or(false);
            assert_eq!(is_on, on.binary_search(&icon).is_ok(), "{icon}");
        }

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
    fn a_pile_spreads_around_the_anchor() {
        for place in PLACES {
            let points: Vec<(f32, f32)> = (0..10)
                .map(|i| ((i % 3) as f32 * 0.1, (i / 3) as f32 * 0.1))
                .collect();
            let positions = place.all(&points);
            let grid = build(&positions, 4, place.radius);
            assert_eq!(grid.spots.len(), 10, "{place:?}");
            assert_eq!(
                grid.spots[0].hex,
                Hex { q: 0, r: 0 },
                "the anchor stays put"
            );
            check(&grid, &positions, place.radius);
        }
    }

    #[test]
    fn icons_near_the_grid_join_it() {
        // A pile of three, an icon that ends up near once they spread out, one near that, and one
        // far away.
        for place in PLACES {
            let positions = place.all(&[
                (0.0, 0.0),
                (0.1, 0.0),
                (0.0, 0.1),
                (3.5, 0.0),
                (5.5, 0.5),
                (40.0, 0.0),
            ]);
            let grid = build(&positions, 0, place.radius);
            assert_eq!(on_grid(&grid), [0, 1, 2, 3, 4], "{place:?}");
            check(&grid, &positions, place.radius);
        }
    }

    #[test]
    fn an_icon_too_close_to_a_spot_joins() {
        // A pair, the second of which moves a spacing along x, and an icon near where it moves to,
        // which overlaps no other.
        for place in PLACES {
            let positions = place.all(&[(0.0, 0.0), (0.1, 0.0), (4.0, 0.0), (9.0, 0.0)]);
            let grid = build(&positions, 0, place.radius);
            assert_eq!(on_grid(&grid), [0, 1, 2], "{place:?}");
            check(&grid, &positions, place.radius);
        }
    }

    #[test]
    fn lone_icons_make_no_grid() {
        for place in PLACES {
            let positions = place.all(&[(0.0, 0.0), (5.0, 0.0)]);
            assert_eq!(build(&positions, 0, place.radius).spots.len(), 1);
        }
    }

    #[test]
    fn the_grid_stays_open_within_its_margin() {
        let margin = KEEP_OPEN_MARGIN;
        for place in PLACES {
            // A pair: the anchor, and the other moved a spacing along x.
            let positions = place.all(&[(0.0, 0.0), (0.1, 0.0)]);
            let grid = build(&positions, 0, place.radius);
            let open = |x: f32, y: f32| grid.keeps_open(place.radius, place.at(x, y));
            assert!(open(1.0, 0.0), "{place:?}");
            // Straight up from its edge, just the margin; out past its right corner, up to 2 / √3
            // times as far.
            assert!(open(0.0, margin - 0.05) && !open(0.0, margin + 0.05));
            let past_corner = SPACING + margin / SIN_60;
            assert!(open(past_corner - 0.05, 0.0) && !open(past_corner + 0.05, 0.0));
        }
    }

    #[test]
    fn the_grid_stays_open_over_its_gaps() {
        // A ring of icons around an empty middle, three spacings out, with two piled up so there's
        // a grid.
        for place in PLACES {
            let lattice = Lattice {
                origin: place.origin,
                spacing: SPACING * place.radius,
            };
            let mut positions: Vec<Real2> = hex_ring(Hex { q: 0, r: 0 }, 3)
                .map(|hex| lattice.position(hex))
                .collect();
            positions.push(positions[0]);
            let grid = build(&positions, 0, place.radius);
            assert_eq!(grid.spots.len(), positions.len());
            check(&grid, &positions, place.radius);
            assert!(grid.keeps_open(place.radius, place.at(0.0, 0.0)));
            assert!(!grid.keeps_open(place.radius, place.at(0.0, 30.0)));
        }
    }

    #[test]
    fn overlapping_icons_within_reach_join() {
        // A pile of two, then past the grid's edge: a pair overlapping each other and a lone icon
        // within the margin, and a pair beyond it.
        for place in PLACES {
            let positions = place.all(&[
                (0.0, 0.0),
                (0.1, 0.0),
                (5.5, 0.0),
                (6.0, 0.5),
                (0.0, 6.0),
                (30.0, 0.0),
                (30.5, 0.0),
            ]);
            let grid = build(&positions, 0, place.radius);
            check(&grid, &positions, place.radius);
            let on = |icon: usize| grid.on_grid[icon];
            assert!(on(2) && on(3), "the pair within reach joins");
            assert!(!on(4), "the lone icon stays");
            assert!(!on(5) && !on(6), "the pair out of reach stays");
        }
    }

    #[test]
    fn icons_within_reach_join_if_overlapped_from_beyond_it() {
        // A pair, the second of which moves a spacing along x. Straight up from the grid's edge: an
        // icon just within reach, overlapped by one further out.
        let reach = KEEP_OPEN_MARGIN + HOVER_DISTANCE;
        for place in PLACES {
            let positions = place.all(&[
                (0.0, 0.0),
                (0.1, 0.0),
                (0.0, reach - 0.1),
                (0.0, reach + 1.9),
            ]);
            let grid = build(&positions, 0, place.radius);
            assert!(grid.on_grid[2], "{place:?}");
            check(&grid, &positions, place.radius);
        }
    }

    #[test]
    fn icons_overlapping_by_a_little_join() {
        // A pile of two, and below the grid, a pair of icons just close enough to overlap.
        let apart = OVERLAP_DISTANCE - 0.2;
        for place in PLACES {
            let positions = place.all(&[(0.0, 0.0), (0.1, 0.0), (0.0, -4.0), (apart, -4.0)]);
            let grid = build(&positions, 0, place.radius);
            assert!(grid.on_grid[2] && grid.on_grid[3], "{place:?}");
            check(&grid, &positions, place.radius);
        }
    }

    #[test]
    fn overlapping_icons_are_found_across_square_borders() {
        // Icons are looked up by squares `clear` wide. Far from the origin: a pile of two, and past
        // it along x, a pair overlapping each other on either side of a border between squares.
        for place in PLACES {
            let (radius, clear) = (place.radius, CLEAR_DISTANCE * place.radius);
            let border = (place.origin.x / clear).round() * clear + 173.0 * clear;
            let at = |x: f32| Real2::new(border + x * radius, place.origin.y);
            let positions = [at(-5.0), at(-4.9), at(-0.25), at(0.35)];
            let grid = build(&positions, 0, radius);
            assert!(grid.on_grid[2] && grid.on_grid[3], "{place:?}");
            check(&grid, &positions, radius);
        }
    }

    #[test]
    fn growing_keeps_icons_on_their_spots() {
        // Icons in a row, 3 radii apart: a pair at one end makes a grid, which zooming out (larger
        // icons) makes reach further along the row.
        for place in PLACES {
            let mut points: Vec<(f32, f32)> = (0..12).map(|i| (i as f32 * 3.0, 0.0)).collect();
            points.push((0.1, 0.0));
            let positions = place.all(&points);
            let pickups: Vec<usize> = (0..positions.len()).collect();
            let mut scratch = Scratch::default();
            let mut grid = Grid::new(&mut scratch, &pickups, &positions, 0, place.radius);
            let before: Vec<(usize, Hex)> = grid.spots.iter().map(|s| (s.pickup, s.hex)).collect();
            let zoomed_out = place.radius * 1.6;
            grid.grow(&mut scratch, &pickups, &positions, zoomed_out);
            assert!(grid.spots.len() > before.len());
            for (spot, &(pickup, hex)) in grid.spots.iter().zip(&before) {
                assert_eq!((spot.pickup, spot.hex), (pickup, hex));
            }
            check(&grid, &positions, zoomed_out);
        }
    }

    #[test]
    fn the_mouse_reaches_between_the_icons_of_a_grid() {
        // The middle of a triangle of neighboring spots is as far from each as the mouse reaches.
        let lattice = Lattice {
            origin: Real2::new(0.0, 0.0),
            spacing: SPACING,
        };
        let triangle = [Hex { q: 0, r: 0 }, Hex { q: 1, r: 0 }, Hex { q: 0, r: 1 }]
            .map(|hex| lattice.position(hex));
        let middle = Real2::new(
            triangle.iter().map(|p| p.x).sum::<f32>() / 3.0,
            triangle.iter().map(|p| p.y).sum::<f32>() / 3.0,
        );
        for corner in triangle {
            assert!((corner.distance(middle) - GRID_HOVER_DISTANCE).abs() < 1e-5);
        }
    }

    /// Three icons piled up.
    fn pile(place: Place) -> Vec<Real2> {
        place.all(&[(0.0, 0.0), (0.1, 0.0), (0.0, 0.1)])
    }

    #[test]
    fn a_pile_spreads_out_under_the_mouse_and_collapses_once_it_leaves() {
        for place in PLACES {
            let id = PickupsId::for_tests(0x1000, 3);
            let (pickups, positions) = ([0, 1, 2], pile(place));
            let mut spread = Spread::default();
            let mut update = |mouse: Real2, frame_number: i32| {
                spread.update(
                    id,
                    &pickups,
                    &positions,
                    place.radius,
                    Some(mouse),
                    frame_number,
                );
                spread.moved().to_vec()
            };
            let progress = |moved: &[Moved]| moved.iter().map(|m| m.progress).collect::<Vec<_>>();
            let (over, away) = (positions[0], place.at(100.0, 0.0));
            assert_eq!(progress(&update(over, 0)), [0.0; 3], "no time has passed");
            let halfway = update(over, OPEN_TICKS as i32 / 2);
            assert_eq!(progress(&halfway), [0.5; 3]);
            let open = update(over, OPEN_TICKS as i32);
            assert_eq!(progress(&open), [1.0; 3]);
            let spots: Vec<Real2> = open.iter().map(|m| m.to).collect();
            assert!((min_gap(&spots) - SPACING * place.radius).abs() < 1e-3);
            for moved in &open {
                assert!(moved.from.same_bits(positions[moved.icon]));
            }

            let closing = update(away, (OPEN_TICKS + CLOSE_TICKS / 2.0) as i32);
            assert_eq!(progress(&closing), [0.5; 3]);
            assert!(update(away, (OPEN_TICKS + CLOSE_TICKS) as i32).is_empty());
            assert!(spread.grid.is_none(), "a collapsed grid is gone");
        }
    }

    #[test]
    fn a_long_frame_collapses_the_grid_at_once() {
        let id = PickupsId::for_tests(0x1000, 3);
        let positions = pile(PLACES[0]);
        let (over, away) = (Some(positions[0]), Some(Real2::new(100.0, 0.0)));
        let mut spread = Spread::default();
        spread.update(id, &[0, 1, 2], &positions, 1.0, over, 0);
        spread.update(id, &[0, 1, 2], &positions, 1.0, over, OPEN_TICKS as i32);
        spread.update(id, &[0, 1, 2], &positions, 1.0, away, 100);
        assert!(spread.moved().is_empty() && spread.grid.is_none());
    }

    #[test]
    fn the_mouse_opens_a_grid_within_reach_of_an_icon() {
        for place in PLACES {
            let id = PickupsId::for_tests(0x1000, 3);
            let positions = pile(place);
            let opens = |x: f32| {
                let mut spread = Spread::default();
                let mouse = Some(place.at(x, 0.0));
                spread.update(id, &[0, 1, 2], &positions, place.radius, mouse, 0);
                !spread.moved().is_empty()
            };
            assert!(opens(-(HOVER_DISTANCE - 0.05)), "{place:?}");
            assert!(!opens(-(HOVER_DISTANCE + 0.05)), "{place:?}");
        }
    }

    #[test]
    fn only_icons_near_each_other_make_a_grid() {
        for place in PLACES {
            let id = PickupsId::for_tests(0x1000, 2);
            let grid_under_mouse = |gap: f32| {
                let positions = place.all(&[(0.0, 0.0), (gap, 0.0)]);
                let mut spread = Spread::default();
                let mouse = Some(positions[0]);
                spread.update(id, &[0, 1], &positions, place.radius, mouse, 0);
                !spread.moved().is_empty()
            };
            assert!(grid_under_mouse(CLEAR_DISTANCE - 0.05), "{place:?}");
            assert!(!grid_under_mouse(CLEAR_DISTANCE + 0.05), "{place:?}");
        }
    }

    #[test]
    fn a_lone_icon_makes_no_grid_even_near_a_pile() {
        // A lone icon, and a pair overlapping each other, which would join a grid around it.
        for place in PLACES {
            let positions = place.all(&[(0.0, 0.0), (4.0, 0.0), (4.0, 0.6)]);
            let id = PickupsId::for_tests(0x1000, 3);
            let mut spread = Spread::default();
            let mouse = Some(positions[0]);
            spread.update(id, &[0, 1, 2], &positions, place.radius, mouse, 0);
            assert!(spread.moved().is_empty(), "{place:?}");
            let mouse = Some(positions[1]);
            spread.update(id, &[0, 1, 2], &positions, place.radius, mouse, 1);
            assert_eq!(spread.moved().len(), 2, "the pair makes one");
        }
    }

    #[test]
    fn icons_without_a_size_collapse_the_grid() {
        let id = PickupsId::for_tests(0x1000, 3);
        let positions = pile(PLACES[0]);
        let over = Some(positions[0]);
        for radius in [0.0, -1.0, f32::INFINITY, f32::NAN] {
            let mut spread = Spread::default();
            spread.update(id, &[0, 1, 2], &positions, 1.0, over, 0);
            spread.update(id, &[0, 1, 2], &positions, 1.0, over, 10);
            spread.update(id, &[0, 1, 2], &positions, radius, over, 11);
            assert!(
                spread.moved().is_empty() && spread.grid.is_none(),
                "{radius}"
            );
        }
    }

    #[test]
    fn a_new_pickup_array_collapses_the_grid_at_once() {
        let pickups = [0, 1, 2];
        let positions = pile(PLACES[0]);
        let mut spread = Spread::default();
        let id = PickupsId::for_tests(0x1000, 3);
        spread.update(id, &pickups, &positions, 1.0, Some(positions[0]), 0);
        spread.update(id, &pickups, &positions, 1.0, Some(positions[0]), 30);
        assert_eq!(spread.moved().len(), 3);
        let other = PickupsId::for_tests(0x2000, 3);
        spread.update(other, &pickups, &positions, 1.0, None, 31);
        assert!(spread.moved().is_empty() && spread.grid.is_none());
    }

    #[test]
    fn pointing_at_another_pile_replaces_the_grid_at_once() {
        let far = Real2::new(50.0, 0.0);
        let mut positions = pile(PLACES[0]);
        let far_pile: Vec<Real2> = positions
            .iter()
            .map(|p| Real2::new(p.x + far.x, p.y + far.y))
            .collect();
        positions.extend(far_pile);
        positions.push(Real2::new(-50.0, 0.0));
        let pickups: Vec<usize> = (0..positions.len()).collect();
        let id = PickupsId::for_tests(0x1000, positions.len());
        let mut spread = Spread::default();
        let on_grid = |spread: &Spread| {
            let mut icons: Vec<usize> = spread.moved().iter().map(|m| m.icon).collect();
            icons.sort_unstable();
            icons
        };
        spread.update(id, &pickups, &positions, 1.0, Some(positions[0]), 0);
        assert_eq!(on_grid(&spread), [0, 1, 2]);
        spread.update(id, &pickups, &positions, 1.0, Some(far), 1);
        assert_eq!(on_grid(&spread), [3, 4, 5]);
        spread.update(id, &pickups, &positions, 1.0, Some(positions[6]), 2);
        assert!(
            spread.moved().iter().all(|m| m.icon != 6),
            "a lone icon makes no grid"
        );
    }

    #[test]
    fn every_icon_of_a_huge_pile_joins() {
        // Thousands of icons on top of each other, as when zoomed all the way out.
        for place in PLACES {
            let points: Vec<(f32, f32)> = (0..3000)
                .map(|i| ((i % 50) as f32 * 0.01, (i / 50) as f32 * 0.01))
                .collect();
            let positions = place.all(&points);
            let grid = build(&positions, 0, place.radius);
            assert_eq!(grid.spots.len(), positions.len());
            let taken: FxHashSet<Hex> = grid.spots.iter().map(|s| s.hex).collect();
            assert_eq!(taken.len(), positions.len(), "one icon per spot");
        }
    }

    #[test]
    fn a_dense_field_joins_entirely() {
        for place in PLACES {
            let points: Vec<(f32, f32)> = (0..400)
                .map(|i| ((i % 20) as f32, (i / 20) as f32))
                .collect();
            let positions = place.all(&points);
            let grid = build(&positions, 210, place.radius);
            assert_eq!(grid.spots.len(), positions.len());
            check(&grid, &positions, place.radius);
        }
    }
}
