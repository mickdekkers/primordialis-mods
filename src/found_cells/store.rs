//! The file the found cells are kept in, next to the DLL (`primordialis_qol_detected.bin`), written
//! with postcard. Like the game's saves, it holds one run of each kind: the normal run and the
//! sandbox, each replaced when a new one is started.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Instant;

use modkit::{log, storage};
use serde::{Deserialize, Serialize};

const FILE: &str = "detected.bin";
/// Bumped whenever the format changes: a file of another version is started over.
const VERSION: u32 = 1;

/// Which of the game's saves a run is kept in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Slot {
    Normal,
    Sandbox,
}

/// Identifies a run in any game session: the save it's kept in, its world's seed, and when it was
/// started (the bits of the timestamp the game saves with it), which tells apart runs started from
/// the same seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub slot: Slot,
    pub seed: u32,
    pub started: u64,
}

/// A found cell: which one (its material's id), and where it lies.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub material: u32,
    pub position: [f32; 2],
}

#[derive(Serialize, Deserialize)]
struct Contents {
    version: u32,
    /// At most one per slot.
    runs: Vec<RunCells>,
}

#[derive(Serialize, Deserialize)]
struct RunCells {
    run: Run,
    cells: Vec<Cell>,
}

/// The found cells of every run in the file, read the first time they're needed.
#[derive(Default)]
pub struct Store {
    runs: Option<Vec<RunCells>>,
    /// Whether writing the file failed already, so that it's only logged once.
    warned: bool,
    /// Whether how long saving takes was logged: once, since it's done on the render thread.
    timed: bool,
}

impl Store {
    /// The found cells of `run`: none if the file has no record of it.
    pub fn cells(&mut self, run: Run) -> Vec<Cell> {
        self.runs()
            .iter()
            .find(|recorded| recorded.run == run)
            .map(|recorded| recorded.cells.clone())
            .unwrap_or_default()
    }

    /// Sets the found cells of `run`. They replace those of any other run in its slot: the game
    /// replaced that run's save when this one was started.
    pub fn set(&mut self, run: Run, cells: Vec<Cell>) {
        let runs = self.runs();
        runs.retain(|recorded| recorded.run.slot != run.slot);
        runs.push(RunCells { run, cells });
    }

    /// Writes the file. Returns whether it worked.
    pub fn save(&mut self) -> bool {
        storage::path(FILE).is_some_and(|path| self.save_to(&path))
    }

    fn save_to(&mut self, path: &Path) -> bool {
        let started = Instant::now();
        let contents = Contents {
            version: VERSION,
            runs: std::mem::take(self.runs()),
        };
        let bytes = postcard::to_allocvec(&contents);
        self.runs = Some(contents.runs);
        // Written next to it first, so that a crash halfway never leaves a broken file.
        let partial = path.with_extension("bin.partial");
        let written = match bytes {
            Ok(bytes) => fs::write(&partial, bytes).and_then(|()| fs::rename(&partial, path)),
            Err(error) => Err(std::io::Error::other(error)),
        };
        match written {
            Ok(()) if !self.timed => {
                self.timed = true;
                log::info(&format!(
                    "saved the found cells in {:.1?}",
                    started.elapsed()
                ));
                true
            }
            Ok(()) => true,
            Err(error) => {
                if !self.warned {
                    self.warned = true;
                    log::warn(&format!(
                        "cannot save the found cells to {}: {error}",
                        path.display()
                    ));
                }
                false
            }
        }
    }

    fn runs(&mut self) -> &mut Vec<RunCells> {
        self.runs.get_or_insert_with(|| {
            storage::path(FILE)
                .map(|path| read(&path))
                .unwrap_or_default()
        })
    }
}

/// The runs in the file at `path`, or none if there's no file yet or it can't be used.
fn read(path: &Path) -> Vec<RunCells> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            log::warn(&format!("cannot read {}: {error}", path.display()));
            return Vec::new();
        }
    };
    match postcard::from_bytes::<Contents>(&bytes) {
        Ok(contents) if contents.version == VERSION => contents.runs,
        Ok(contents) => {
            log::warn(&format!(
                "{} is from another version of the mod (format {}), starting over",
                path.display(),
                contents.version
            ));
            Vec::new()
        }
        Err(error) => {
            log::warn(&format!(
                "{} can't be read, starting over: {error}",
                path.display()
            ));
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(slot: Slot, seed: u32, started: u64) -> Run {
        Run {
            slot,
            seed,
            started,
        }
    }

    fn empty() -> Store {
        Store {
            runs: Some(Vec::new()),
            ..Store::default()
        }
    }

    const CELL: Cell = Cell {
        material: 1,
        position: [0.0, 0.0],
    };

    #[test]
    fn contents_round_trip() {
        let contents = Contents {
            version: VERSION,
            runs: vec![RunCells {
                run: run(Slot::Sandbox, 42, 1234.5f64.to_bits()),
                cells: vec![Cell {
                    material: 0x8000_0012,
                    position: [-120.5, 3000.25],
                }],
            }],
        };
        let bytes = postcard::to_allocvec(&contents).unwrap();
        let back: Contents = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.version, VERSION);
        assert_eq!(back.runs[0].run, contents.runs[0].run);
        assert_eq!(back.runs[0].cells, contents.runs[0].cells);
    }

    /// A folder of its own for a test, emptied.
    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "primordialis_qol_store_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn saved_cells_are_read_back() {
        let dir = test_dir("saved");
        let path = dir.join("detected.bin");
        let mut store = empty();
        store.set(run(Slot::Normal, 7, 1), vec![CELL]);
        store.set(run(Slot::Sandbox, 8, 2), Vec::new());
        assert!(store.save_to(&path));
        assert!(store.save_to(&path), "and again, over the last one");

        let runs = read(&path);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run, run(Slot::Normal, 7, 1));
        assert_eq!(runs[0].cells, [CELL]);
        assert_eq!(runs[1].run, run(Slot::Sandbox, 8, 2));
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "nothing is left beside it"
        );
        assert_eq!(
            store.cells(run(Slot::Normal, 7, 1)),
            [CELL],
            "saving keeps them"
        );

        assert!(!store.save_to(&dir.join("missing").join("detected.bin")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_that_cant_be_used_start_over() {
        let dir = test_dir("unusable");
        let path = dir.join("detected.bin");
        assert!(read(&path).is_empty(), "no file yet");
        assert!(read(&dir).is_empty(), "not a file");

        fs::write(&path, b"\xFF\xFF\xFF").unwrap();
        assert!(read(&path).is_empty(), "not the format");

        let contents = Contents {
            version: VERSION + 1,
            runs: vec![RunCells {
                run: run(Slot::Normal, 7, 1),
                cells: vec![CELL],
            }],
        };
        fs::write(&path, postcard::to_allocvec(&contents).unwrap()).unwrap();
        assert!(read(&path).is_empty(), "another version");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_run_on_the_same_seed_starts_with_nothing_found() {
        let mut store = empty();
        store.set(run(Slot::Normal, 7, 1), vec![CELL]);
        assert_eq!(store.cells(run(Slot::Normal, 7, 1)), [CELL]);
        assert!(store.cells(run(Slot::Normal, 7, 2)).is_empty());
    }

    #[test]
    fn normal_runs_and_sandboxes_are_kept_apart() {
        let mut store = empty();
        store.set(run(Slot::Normal, 7, 1), vec![CELL]);
        // A new sandbox, even of the same seed and time, leaves the normal run alone.
        store.set(run(Slot::Sandbox, 7, 1), Vec::new());
        assert_eq!(store.cells(run(Slot::Normal, 7, 1)), [CELL]);
        assert!(store.cells(run(Slot::Sandbox, 7, 1)).is_empty());
        // A new normal run replaces the old one, and leaves the sandbox alone.
        store.set(run(Slot::Sandbox, 8, 2), vec![CELL]);
        store.set(run(Slot::Normal, 9, 3), Vec::new());
        assert!(store.cells(run(Slot::Normal, 7, 1)).is_empty());
        assert_eq!(store.cells(run(Slot::Sandbox, 8, 2)), [CELL]);
        assert_eq!(store.runs().len(), 2);
    }
}
