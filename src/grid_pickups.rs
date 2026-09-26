//! The pickups whose map icons are spread out on a grid, shared by the map icons (which spread them)
//! with the features that hide them elsewhere meanwhile: in the world, and from Echolocation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

use modkit::game::PickupsId;

/// The map icons set it while drawing, in the `menus` stage, which comes after `cell pickups` and
/// `racing_overlay` in a frame: the features hiding the pickups there use the previous frame's grid.
///
/// Only locked inside features' stage callbacks, which all run one after another on the render
/// thread, and with `try_lock`: nothing ever waits for it. `revert` never locks it, only `withdraw`s.
#[derive(Clone, Default)]
pub struct GridPickups(Arc<Shared>);

#[derive(Default)]
struct Shared {
    snapshot: Mutex<Snapshot>,
    /// Set when the map icons stop (see `withdraw`): the snapshot is then out of date for good.
    withdrawn: AtomicBool,
}

#[derive(Default)]
struct Snapshot {
    /// The pickup array `indices` index.
    pickups: Option<PickupsId>,
    /// Sorted.
    indices: Vec<usize>,
}

impl GridPickups {
    /// Replaces the pickups on the grid.
    pub fn set(&self, pickups: PickupsId, indices: impl Iterator<Item = usize>) {
        self.with(|snapshot| {
            snapshot.pickups = Some(pickups);
            snapshot.indices.clear();
            snapshot.indices.extend(indices);
            snapshot.indices.sort_unstable();
        });
        self.0.withdrawn.store(false, Ordering::Relaxed);
    }

    /// No pickups are on a grid until the next `set`, for when the map icons are turned off: nothing
    /// would clear the grid otherwise, and the other features would keep hiding its pickups. Doesn't
    /// lock, so it can be called from `revert`.
    pub fn withdraw(&self) {
        self.0.withdrawn.store(true, Ordering::Relaxed);
    }

    /// Whether the map icons stopped (see `withdraw`), so that nothing on the map stands in for what
    /// the other features would hide.
    pub fn is_withdrawn(&self) -> bool {
        self.0.withdrawn.load(Ordering::Relaxed)
    }

    /// No pickups are on a grid.
    pub fn clear(&self) {
        self.with(|snapshot| {
            snapshot.pickups = None;
            snapshot.indices.clear();
        });
    }

    /// Calls `f` with the indices of the pickups on the grid in the pickup array `pickups`, sorted:
    /// none if the grid is in another one.
    pub fn read(&self, pickups: PickupsId, f: impl FnOnce(&[usize])) {
        let mut f = Some(f);
        self.with(|snapshot| {
            let current = !self.0.withdrawn.load(Ordering::Relaxed);
            let indices: &[usize] = if current && snapshot.pickups == Some(pickups) {
                &snapshot.indices
            } else {
                &[]
            };
            if let Some(f) = f.take() {
                f(indices);
            }
        });
        if let Some(f) = f {
            f(&[]);
        }
    }

    fn with(&self, f: impl FnOnce(&mut Snapshot)) {
        match self.0.snapshot.try_lock() {
            Ok(mut snapshot) => f(&mut snapshot),
            // A feature panicked while holding it: the list is still just a list.
            Err(TryLockError::Poisoned(poisoned)) => f(&mut poisoned.into_inner()),
            // Held by a callback further up the stack, which can't happen: skip it.
            Err(TryLockError::WouldBlock) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(grid: &GridPickups, pickups: PickupsId) -> Vec<usize> {
        let mut read = None;
        grid.read(pickups, |indices| read = Some(indices.to_vec()));
        read.expect("read calls its function")
    }

    #[test]
    fn reads_the_sorted_pickups_of_the_same_array_only() {
        let (a, b) = (
            PickupsId::for_tests(0x1000, 10),
            PickupsId::for_tests(0x1000, 11),
        );
        let grid = GridPickups::default();
        assert_eq!(read(&grid, a), [] as [usize; 0]);
        grid.set(a, [7, 2, 5].into_iter());
        assert_eq!(read(&grid, a), [2, 5, 7]);
        assert_eq!(read(&grid, b), [] as [usize; 0]);
        grid.clear();
        assert_eq!(read(&grid, a), [] as [usize; 0]);
    }

    #[test]
    fn withdrawn_pickups_are_gone_until_set_again() {
        let a = PickupsId::for_tests(0x1000, 10);
        let grid = GridPickups::default();
        grid.set(a, [4].into_iter());
        grid.clone().withdraw();
        assert!(grid.is_withdrawn());
        assert_eq!(read(&grid, a), [] as [usize; 0]);
        grid.set(a, [5].into_iter());
        assert!(!grid.is_withdrawn());
        assert_eq!(read(&grid, a), [5]);
    }

    #[test]
    fn a_nested_read_sees_no_pickups_instead_of_waiting() {
        let a = PickupsId::for_tests(0x1000, 10);
        let grid = GridPickups::default();
        grid.set(a, [1].into_iter());
        let mut nested = None;
        grid.read(a, |_| nested = Some(read(&grid, a)));
        assert_eq!(nested, Some(vec![]));
    }

    #[test]
    fn keeps_working_after_a_panic_while_locked() {
        let a = PickupsId::for_tests(0x1000, 10);
        let grid = GridPickups::default();
        grid.set(a, [3].into_iter());
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            grid.read(a, |_| panic!("a feature panics"));
        }));
        assert!(panicked.is_err());
        assert_eq!(read(&grid, a), [3]);
    }
}
