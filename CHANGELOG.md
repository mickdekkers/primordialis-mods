# Changelog

## 0.3.1 (2026-10-01)

### Fixed

- Changes to primordialis_qol.toml saved while the game is starting up are no longer ignored until the file is changed again.
## 0.3.0 (2026-09-30)

### Added

- The mod can be loaded by the Nucleus mod loader, alongside Nucleus mods: put it in a primordialis_qol folder inside Nucleus's mods folder (see the README).
## 0.2.5 (2026-09-29)

### Added

- The first published release with the changes from 0.2.3 and 0.2.4, which weren't released on their own: the DLL has a build attestation on GitHub to check your download against (see "Is it safe?" in the README), and comes from a release commit GitHub signed.
## 0.2.4 (2026-09-29)

### Added

- Each release's DLL has a build attestation on GitHub, a signed record that this repository's release workflow built it, which you can check your download against (see "Is it safe?" in the README).
## 0.2.3 (2026-09-29)

### Changed

- Each release now comes from a commit GitHub has signed and shows as Verified, so you can check it was made by this repository's release process.
## 0.2.2 (2026-09-29)

### Added

- The README shows the latest release's VirusTotal result, linking to its full report.
## 0.2.1 (2026-09-29)

### Added

- Screenshots in the README, and badges showing the game version, the latest release and that the mod is for Windows.
## 0.2.0 (2026-09-27)

### Added

- The main menu and pause menu show the mod's version under the game's, so you can tell it's loaded.
- The log from the last time you played is kept as primordialis_qol.log.bak, so it's still there to report a crash after starting the game again.

### Changed

- The map only shows the cells you've been close enough to see: as far as the fog clears around you, and closer the darker it is. Dark areas keep their secrets, and a sandbox is no longer covered in icons. Cells you've seen stay on the map, also after restarting the game.

### Fixed

- Map icons no longer grow or show tooltips under the mouse while the pause menu is open over the map.
## 0.1.0 (2026-09-26)

### Added

- Every cell pickup in an area you've explored shows up on the map with its icon. Combo cells get their shifting colors and a ring of rainbow dots.
- Icons that overlap spread out when you point at them, with lines back to where the pickups are.
- Pointing at an icon shows the game's tooltip for that cell.
- Icons, and the Echolocation mutation's markers, show where pickups will end up instead of inside rock.
