//! Changes to the game's pickups that last one rendering stage: made as it begins, and undone as it
//! ends (and in `revert`). Nothing should change the pickups in between. If something did, an index
//! may refer to another pickup by then, so only a pickup still holding what was written is changed
//! back. Leaving them all instead would leave them changed for good.

use modkit::game::{PickupsId, Real2};
use modkit::log;

/// A pickup field that can be changed for a stage, compared by its bits: unlike `==`, that tells
/// `0.0` and `-0.0` apart and matches NaN with itself, to tell whether a value in game memory is
/// still the one written.
pub trait Field: Copy {
    fn same_bits(self, other: Self) -> bool;
}

impl Field for f32 {
    fn same_bits(self, other: Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl Field for Real2 {
    fn same_bits(self, other: Self) -> bool {
        Real2::same_bits(self, other)
    }
}

/// Pickups changed for a stage, to be changed back.
pub struct PickupEdits<T> {
    /// The pickup array they were changed in.
    pickups: Option<PickupsId>,
    /// Index of each changed pickup, its value before, and the value written.
    edits: Vec<(usize, T, T)>,
    /// Whether the pickups changing in between was logged, so that it's only logged once.
    warned: bool,
}

impl<T> Default for PickupEdits<T> {
    fn default() -> Self {
        PickupEdits {
            pickups: None,
            edits: Vec::new(),
            warned: false,
        }
    }
}

impl<T: Field> PickupEdits<T> {
    /// Starts changing pickups of the array `pickups`.
    pub fn begin(&mut self, pickups: PickupsId) {
        self.pickups = Some(pickups);
    }

    /// Records that the pickup at `index`, which held `original`, now holds `written`.
    pub fn record(&mut self, index: usize, original: T, written: T) {
        self.edits.push((index, original, written));
    }

    /// Changes the pickups back, in the array `pickups`: `get` reads the field of the pickup at an
    /// index, if there is one, and `set` writes it. Returns false if that's not the array they were
    /// changed in. Doesn't log, so it can be called from `revert`.
    pub fn undo(
        &mut self,
        pickups: PickupsId,
        get: impl Fn(usize) -> Option<T>,
        mut set: impl FnMut(usize, T),
    ) -> bool {
        if self.edits.is_empty() {
            return true;
        }
        for &(index, original, written) in &self.edits {
            if get(index).is_some_and(|current| current.same_bits(written)) {
                set(index, original);
            }
        }
        self.edits.clear();
        self.pickups == Some(pickups)
    }

    /// Logs `warning` the first time `undo` found the pickup array changed.
    pub fn warn_once(&mut self, warning: &str) {
        if !self.warned {
            self.warned = true;
            log::warn(warning);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Changes the fields in `values` like a feature would.
    fn change(edits: &mut PickupEdits<f32>, values: &mut [f32], changes: &[(usize, f32)]) {
        edits.begin(PickupsId::for_tests(0x1000, values.len()));
        for &(index, written) in changes {
            edits.record(index, values[index], written);
            values[index] = written;
        }
    }

    fn undo(edits: &mut PickupEdits<f32>, values: &mut [f32], address: usize) -> bool {
        let pickups = PickupsId::for_tests(address, values.len());
        let current = values.to_vec();
        edits.undo(pickups, |i| current.get(i).copied(), |i, v| values[i] = v)
    }

    #[test]
    fn changes_are_undone() {
        let mut values = vec![0.5, 0.7, 0.9];
        let mut edits = PickupEdits::default();
        change(&mut edits, &mut values, &[(0, 0.0), (2, 0.0)]);
        assert!(undo(&mut edits, &mut values, 0x1000));
        assert_eq!(values, [0.5, 0.7, 0.9]);
        assert!(
            undo(&mut edits, &mut values, 0x2000),
            "nothing left to undo"
        );
    }

    #[test]
    fn only_pickups_still_as_written_are_changed_back() {
        let mut values = vec![0.5, 0.7, 0.9];
        let mut edits = PickupEdits::default();
        change(&mut edits, &mut values, &[(0, 0.0), (1, 0.0), (2, 0.0)]);
        // The pickups changed: the first is another one now, and the last is gone.
        values[0] = 0.3;
        values.pop();
        assert!(!undo(&mut edits, &mut values, 0x2000), "another array");
        assert_eq!(values, [0.3, 0.7]);
    }

    #[test]
    fn bits_tell_zeros_apart() {
        assert!(0.0f32.same_bits(0.0) && !0.0f32.same_bits(-0.0));
        assert!(f32::NAN.same_bits(f32::NAN));
        let at = Real2::new(1.0, f32::NAN);
        assert!(Field::same_bits(at, at));
        assert!(!Field::same_bits(at, Real2::new(1.0, 0.0)));
        assert!(!Field::same_bits(
            Real2::new(0.0, 2.0),
            Real2::new(-0.0, 2.0)
        ));
    }

    #[test]
    fn remembers_that_it_warned() {
        let mut edits = PickupEdits::<f32>::default();
        assert!(!edits.warned);
        edits.warn_once("the pickups changed");
        assert!(edits.warned);
        edits.warn_once("the pickups changed");
        assert!(edits.warned);
    }
}
