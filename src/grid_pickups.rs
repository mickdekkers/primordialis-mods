//! The pickups whose map icons are spread out on a grid, shared by the map icons (which spread them)
//! with the Echolocation fix (which hides their markers meanwhile).

use std::sync::{Arc, Mutex, TryLockError};

use modkit::game::PickupsId;

/// Only locked inside features' stage callbacks, which all run one after another on the render
/// thread, and with `try_lock`: nothing ever waits for it. `revert` never touches it.
#[derive(Clone, Default)]
pub struct GridPickups(Arc<Mutex<Snapshot>>);

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
            let indices: &[usize] = if snapshot.pickups == Some(pickups) {
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
        match self.0.try_lock() {
            Ok(mut snapshot) => f(&mut snapshot),
            // A feature panicked while holding it: the list is still just a list.
            Err(TryLockError::Poisoned(poisoned)) => f(&mut poisoned.into_inner()),
            // Held by a callback further up the stack, which can't happen: skip it.
            Err(TryLockError::WouldBlock) => {}
        }
    }
}
