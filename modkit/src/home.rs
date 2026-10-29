//! Where a mod keeps its files (its log, settings, symbol cache and storage): the game folder, next
//! to the game's executable, rather than next to the DLL. Every copy of the mod then shares them,
//! however it's loaded: mod managers delete a mod's folder whenever they update it, and a player who
//! loads the mod in another way (or from another mod manager profile) would otherwise start over
//! without their files.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Moves into `home` the files older versions of the mod kept in `old`, next to the DLL: its
/// settings and the storage files named in `storage_files` (by suffix, as for `storage::path`). One
/// that `home` already has is left where it is: another copy of the mod moved its own first. The
/// log and symbol cache are deleted instead, since they're made again. Nothing happens when `old` is
/// `home`, also written another way, or when either can't be found. Returns what it did, and what
/// failed, for the log.
pub(crate) fn move_old_files(
    old: &Path,
    home: &Path,
    name: &str,
    storage_files: &[&str],
) -> Vec<Result<String, String>> {
    let mut done = Vec::new();
    match (fs::canonicalize(old), fs::canonicalize(home)) {
        (Ok(old), Ok(home)) if old != home => {}
        _ => return done,
    }
    let kept = std::iter::once(format!("{name}.toml")).chain(
        storage_files
            .iter()
            .map(|suffix| format!("{name}_{suffix}")),
    );
    for file in kept {
        let (from, to) = (old.join(&file), home.join(&file));
        if !from.is_file() {
            continue;
        }
        if to.exists() {
            done.push(Ok(format!(
                "left {}: the game folder has its own",
                from.display()
            )));
            continue;
        }
        done.push(match move_file(&from, &to) {
            Ok(()) => Ok(format!("moved {} into the game folder", from.display())),
            Err(e) => Err(format!("cannot move {}: {e}", from.display())),
        });
    }
    for file in [format!("{name}.log"), format!("{name}.log.bak")] {
        let path = old.join(file);
        match fs::remove_file(&path) {
            Ok(()) => done.push(Ok(format!("deleted {}", path.display()))),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => done.push(Err(format!("cannot delete {}: {e}", path.display()))),
        }
    }
    let cache = old.join(format!("{name}_cache"));
    match fs::remove_dir_all(&cache) {
        Ok(()) => done.push(Ok(format!("deleted {}", cache.display()))),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => done.push(Err(format!("cannot delete {}: {e}", cache.display()))),
    }
    done
}

/// Renames `from` to `to`, or copies it and deletes `from` when they're on different drives, which
/// renaming can't do.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    fs::remove_file(from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("modkit_home_{test}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_and_storage_move_and_the_log_and_cache_are_deleted() {
        let dir = temp_dir("moves");
        let (old, home) = (dir.join("old"), dir.join("home"));
        fs::create_dir_all(old.join("my_mod_cache/1234")).unwrap();
        fs::create_dir_all(&home).unwrap();
        for file in [
            "my_mod.toml",
            "my_mod_cells.bin",
            "my_mod.log",
            "my_mod.log.bak",
        ] {
            fs::write(old.join(file), file).unwrap();
        }
        // Files that aren't the mod's own stay, even when they share its name.
        for file in ["my_mod.dll", "my_mod.pdb", "my_mod_other.bin"] {
            fs::write(old.join(file), file).unwrap();
        }

        let done = move_old_files(&old, &home, "my_mod", &["cells.bin"]);

        assert_eq!(done.len(), 5, "{done:?}");
        assert!(done.iter().all(Result::is_ok), "{done:?}");
        for file in ["my_mod.toml", "my_mod_cells.bin"] {
            assert!(!old.join(file).exists());
            assert_eq!(fs::read_to_string(home.join(file)).unwrap(), file);
        }
        for file in ["my_mod.log", "my_mod.log.bak", "my_mod_cache"] {
            assert!(!old.join(file).exists());
            assert!(!home.join(file).exists());
        }
        for file in ["my_mod.dll", "my_mod.pdb", "my_mod_other.bin"] {
            assert!(old.join(file).exists());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_home_already_has_are_left_where_they_are() {
        let dir = temp_dir("left");
        let (old, home) = (dir.join("old"), dir.join("home"));
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(old.join("my_mod.toml"), "old").unwrap();
        fs::write(home.join("my_mod.toml"), "home").unwrap();

        let done = move_old_files(&old, &home, "my_mod", &[]);

        assert_eq!(done.len(), 1, "{done:?}");
        assert!(
            done[0].as_ref().is_ok_and(|line| line.starts_with("left ")),
            "{done:?}"
        );
        assert_eq!(fs::read_to_string(old.join("my_mod.toml")).unwrap(), "old");
        assert_eq!(
            fs::read_to_string(home.join("my_mod.toml")).unwrap(),
            "home"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_happens_in_the_same_folder() {
        let dir = temp_dir("same");
        for file in ["my_mod.toml", "my_mod.log"] {
            fs::write(dir.join(file), file).unwrap();
        }
        fs::create_dir_all(dir.join("my_mod_cache")).unwrap();

        let done = move_old_files(&dir.join("."), &dir, "my_mod", &[]);

        assert!(done.is_empty(), "{done:?}");
        for file in ["my_mod.toml", "my_mod.log", "my_mod_cache"] {
            assert!(dir.join(file).exists());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_to_move_does_nothing() {
        let dir = temp_dir("nothing");
        assert!(move_old_files(&dir, &dir.join("home"), "my_mod", &["cells.bin"]).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
