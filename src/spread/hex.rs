//! Hexagonal grid geometry: the lattice icons are spread out on, its spots, the order to search them
//! in, and the hexagon a grid covers.

use modkit::game::Real2;

/// The hexagon through the outermost spots of a grid: the ranges of their cube coordinates (q, r
/// and s = -q - r), each of which runs along one of the grid's three axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Bounds {
    min: [i32; 3],
    max: [i32; 3],
}

impl Bounds {
    pub(super) fn around(hex: Hex) -> Bounds {
        let cube = hex.cube();
        Bounds {
            min: cube,
            max: cube,
        }
    }

    pub(super) fn extend(&mut self, hex: Hex) {
        for (axis, value) in hex.cube().into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    /// Whether `at` is within `margin` (in world units) of the hexagon, or in it.
    pub(super) fn contains(&self, lattice: &Lattice, at: Real2, margin: f32) -> bool {
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
pub(super) struct Spiral {
    pub(super) offsets: Vec<((i32, i32), f32)>,
    /// All offsets up to this many rings out are in `offsets`...
    rings: i32,
    /// ...but only this many of them, the ones nearer than the nearest spot one more ring out, are
    /// sure to be in order.
    complete: usize,
}

impl Spiral {
    /// Makes sure `offsets[i]` exists, and that every offset nearer than it is before it.
    pub(super) fn extend_to(&mut self, i: usize) {
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
pub(super) struct Hex {
    pub(super) q: i32,
    pub(super) r: i32,
}

impl Hex {
    /// Cube coordinates: q, r and s = -q - r.
    fn cube(self) -> [i32; 3] {
        [self.q, self.r, -self.q - self.r]
    }
}

/// The hexagonal grid of spots `spacing` apart, with one at `origin`.
pub(super) struct Lattice {
    pub(super) origin: Real2,
    pub(super) spacing: f32,
}

impl Lattice {
    const SIN_60: f32 = 0.866_025_4;

    pub(super) fn position(&self, hex: Hex) -> Real2 {
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
    pub(super) fn round(&self, at: Real2) -> Hex {
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
pub(super) fn hex_ring(center: Hex, ring: i32) -> impl Iterator<Item = Hex> {
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
    use crate::spread::SPACING;

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
        // Every spot within the last listed distance is listed, including any in the rings past
        // those the spiral holds.
        let reach = listed[complete - 1].1;
        let unit = Lattice {
            origin: Real2::default(),
            spacing: 1.0,
        };
        for hex in (0..=spiral.rings + 2).flat_map(|ring| hex_ring(Hex { q: 0, r: 0 }, ring)) {
            if unit.position(hex).distance(Real2::default()) < reach {
                assert!(listed.iter().any(|&(offset, _)| offset == (hex.q, hex.r)));
            }
        }
    }

    #[test]
    fn rounding_finds_the_nearest_spot() {
        let lattice = Lattice {
            origin: Real2::new(3.0, -2.0),
            spacing: 2.0,
        };
        let spots: Vec<Hex> = (0..=6)
            .flat_map(|ring| hex_ring(Hex { q: 0, r: 0 }, ring))
            .collect();
        // Points all over the middle of the grid, including near the corners of cells.
        for i in -40..=40 {
            for j in -40..=40 {
                let at = Real2::new(3.0 + i as f32 * 0.137, -2.0 + j as f32 * 0.119);
                let distance = |hex: Hex| lattice.position(hex).distance(at);
                let mut by_distance = spots.clone();
                by_distance.sort_by(|&a, &b| distance(a).total_cmp(&distance(b)));
                let [nearest, next, ..] = by_distance[..] else {
                    unreachable!()
                };
                if distance(next) - distance(nearest) > 1e-3 {
                    assert_eq!(lattice.round(at), nearest, "{at:?}");
                }
            }
        }
    }

    #[test]
    fn bounds_cover_the_spots_between_the_outermost() {
        let lattice = Lattice {
            origin: Real2::new(0.0, 0.0),
            spacing: SPACING,
        };
        // A lopsided pair of spots, two apart along r.
        let mut bounds = Bounds::around(Hex { q: 0, r: 0 });
        bounds.extend(Hex { q: 0, r: 2 });
        assert!(bounds.contains(&lattice, lattice.position(Hex { q: 0, r: 1 }), 0.1));
        for outside in [Hex { q: 1, r: 1 }, Hex { q: -1, r: 1 }, Hex { q: 0, r: 3 }] {
            let at = lattice.position(outside);
            assert!(!bounds.contains(&lattice, at, 0.1), "{outside:?}");
        }
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
}
