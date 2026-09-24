//! Features: what a mod is made of.

use std::panic::{self, AssertUnwindSafe};

use crate::game::{Frame, Game, Stage};
use crate::log;
use crate::settings::Setting;

/// One thing a mod does. The framework calls it on the render thread as the game renders, with safe
/// access to the game's state for the duration of each call.
///
/// A feature that panics is turned off until the mod is reloaded: its changes are undone (see
/// [`Feature::revert`]) and the other features keep running.
pub trait Feature: Send {
    /// For logs, e.g. "map icons".
    fn name(&self) -> &'static str;

    /// The feature's settings, declared as `static`s (e.g. [`crate::settings::Toggle`]) and read
    /// from there. They are loaded before the feature first runs, and kept up to date.
    fn settings(&self) -> Vec<&'static dyn Setting> {
        Vec::new()
    }

    /// A rendering stage begins.
    fn stage_begin(&mut self, _frame: &Frame, _stage: Stage) {}

    /// A rendering stage ends: when the next one begins, or when the frame is done.
    fn stage_end(&mut self, _frame: &Frame, _stage: Stage) {}

    /// Undoes every change the feature made to the game's state that it would otherwise undo later
    /// (e.g. at the end of a stage), because it's about to stop: the mod is being unloaded, or the
    /// feature panicked. Called with the game's other threads paused, or on the render thread: keep
    /// it short, don't call game functions, and don't wait for other threads.
    fn revert(&mut self, _game: &Game) {}
}

/// The running features, each turned off if it panics.
pub(crate) struct Features {
    features: Vec<(Box<dyn Feature>, bool)>,
}

impl Features {
    pub fn new(features: Vec<Box<dyn Feature>>) -> Self {
        Features { features: features.into_iter().map(|feature| (feature, true)).collect() }
    }

    /// Calls `f` on every feature that is on. A feature that panics is turned off, and `revert` is
    /// then called on it.
    pub fn each(&mut self, mut f: impl FnMut(&mut dyn Feature), mut revert: impl FnMut(&mut dyn Feature)) {
        for (feature, on) in &mut self.features {
            if !*on {
                continue;
            }
            let feature = &mut **feature;
            if panic::catch_unwind(AssertUnwindSafe(|| f(feature))).is_ok() {
                continue;
            }
            *on = false;
            let reverted = panic::catch_unwind(AssertUnwindSafe(|| revert(feature))).is_ok();
            log::error(&format!(
                "{} panicked, and is off until the mod is reloaded{}",
                feature.name(),
                if reverted { "" } else { " (undoing its changes panicked too)" }
            ));
        }
    }

    /// Calls `revert` on every feature, including ones that are off. Doesn't log: may run while the
    /// game's threads are paused.
    pub fn revert_all(&mut self, mut revert: impl FnMut(&mut dyn Feature)) {
        for (feature, _) in &mut self.features {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| revert(&mut **feature)));
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.features.iter().map(|(feature, _)| feature.name()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Named(&'static str);

    impl Feature for Named {
        fn name(&self) -> &'static str {
            self.0
        }
    }

    #[test]
    fn a_panicking_feature_is_reverted_and_turned_off() {
        let mut features = Features::new(vec![Box::new(Named("a")), Box::new(Named("b")), Box::new(Named("c"))]);
        let (mut called, mut reverted) = (Vec::new(), Vec::new());
        for _ in 0..2 {
            features.each(
                |feature| {
                    called.push(feature.name());
                    assert_ne!(feature.name(), "b", "b fails");
                },
                |feature| reverted.push(feature.name()),
            );
        }
        assert_eq!(called, ["a", "b", "c", "a", "c"]);
        assert_eq!(reverted, ["b"]);

        let mut all = Vec::new();
        features.revert_all(|feature| all.push(feature.name()));
        assert_eq!(all, ["a", "b", "c"]);
    }
}
