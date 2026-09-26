# Primordialis QoL

A small quality-of-life mod for Primordialis: open the map and see *which* cells are lying around, not
just where you've been.

![The map, with an icon for every cell pickup in the explored areas](docs/screenshots/map-icons.png)

## What it does

**See every cell on the map.** Each cell pickup in an area you've explored shows up on the map with
its own icon, drawn just like the pickup looks in the world. Combo cells get their shifting colors and
a ring of rainbow dots, like the sparkle around them in the world.

**Tell piles apart.** Where icons overlap, point at them and they spread out, with a line back to where
each one really is.

![A pile of icons, spread out under the mouse](docs/screenshots/spread.png)

**Read about a cell without flying to it.** Point at an icon to get the same tooltip the game shows for
a pickup right next to you: the cell's name, description, cost and genome size, and what picking it up
would change.

![The game's tooltip, shown for a map icon](docs/screenshots/tooltip.png)

**Icons where the cells really are.** The game leaves far-away pickups where they spawned, sometimes
inside rock, and only pushes them out once you get close. The mod shows them where they'll end up, so
you don't plan a trip to a cell buried in a wall. If you have the Echolocation mutation, its markers
get the same fix.

![Echolocation markers without the mod (left) and with it (right)](docs/screenshots/echolocation.png)

## Why

Building a creature is all about finding the right cells, but the map only shows where you've been, not
what you left behind. Going back for a cell meant remembering where you saw it, and Echolocation's dots
don't say which cell they are. This mod puts that on the map, using the game's own icons and tooltips so
it feels like part of the game.

## Install

1. Download `primordialis_qol.dll` from the [latest release](../../releases/latest).
2. Put it in the game folder, next to `primordialis.exe`. To open the game folder: in Steam, right-click
   Primordialis, then **Manage** → **Browse local files**.
3. In Steam, right-click Primordialis, then **Properties** → **General**. Under **Launch Options**,
   enter:

   ```
   --customdll "primordialis_qol.dll"
   ```
4. Start the game, explore a bit, and open the map.

To update, close the game and replace `primordialis_qol.dll` with the new one.

The mod keeps a few files of its own in the game folder: its settings (`primordialis_qol.toml`), a log
(`primordialis_qol.log`), and a `primordialis_qol_cache` folder (about 10 MB) with the debug information
it reads from the game.

## Is it safe?

- **Your game files and saves are left alone.** The mod loads through `--customdll`, the game's own
  option for loading mods, and doesn't change any game files. It only writes its own settings, log and
  cache. Remove the launch option and the game is exactly as it was.
- **It only changes what you see.** It draws on the map, and doesn't change how the game plays.
- **It never goes online.**
- **It steps aside when something is wrong.** If a game update changes something the mod relies on, the
  mod turns itself off and the game runs normally. If one of its features runs into a problem, that
  feature turns off and the rest keep working.
- **You can check what you download.** The mod is open source. Each release is built by GitHub from the
  code in this repository, not on anyone's PC, and scanned by the antivirus engines on VirusTotal. The
  release notes show the result, link to the full report, and list the file's SHA-256 checksum.

The mod works by hooking into the game's code while it runs, which some antivirus programs are wary of,
so one may occasionally flag it. The VirusTotal report for each release shows which engines, if any, did.

## Settings

To change a setting, open `primordialis_qol.toml` in the game folder with Notepad, change `true` to
`false` (or back), and save. Changes apply right away, even while the game is running. Everything is on
by default.

| Setting | What it does |
|---|---|
| `fix_icon_positions` | Shows map icons where pickups will end up, not inside rock where they spawned. |
| `spread_clusters` | Spreads out icons that overlap when you point at them. |
| `show_tooltips` | Shows the tooltip for the icon you point at. |
| `fix_echolocation_positions` | Moves the Echolocation mutation's markers out of rock too. |

## Troubleshooting

**Nothing changed on the map.** Icons only show for areas you've explored, so explore a little first.
If there are still none, look for `primordialis_qol.log` in the game folder:

- **There's no log:** the game didn't load the mod. Check that `primordialis_qol.dll` is right next to
  `primordialis.exe`, and that the launch option is exactly as above, quotes included.
- **There's a log:** its last lines say what went wrong. If it says the mod is "not active" after a game
  update, the mod needs an update too: check for a [new release](../../releases).

**Reporting a problem.** Please [open an issue](../../issues) and attach `primordialis_qol.log`. The log
shows where your game is installed, but nothing else about you or your PC.

## Uninstall

Remove the launch option. You can then delete `primordialis_qol.dll`, `primordialis_qol.toml`,
`primordialis_qol.log` and the `primordialis_qol_cache` folder from the game folder.

## Compatibility

Made for Primordialis v0.2 beta, and works with both versions the game comes in (AVX and SSE3).
The mod finds what it needs in the game by name rather than relying on one exact version, so it often
keeps working after a game update. When it can't, it turns itself off (see above).

## For developers

How the mod works, how to build and test it, and how releases are made: [DEVELOPMENT.md](DEVELOPMENT.md).

## License

MIT, see [LICENSE](LICENSE). This is a fan-made mod, not affiliated with the developers of Primordialis.
