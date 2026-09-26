# Checks that a built DLL holds no paths from the machine that built it. Rather than looking for
# names we know to avoid, it requires every path in the DLL to have one of the expected forms:
#
# - No absolute paths at all (drive letters, UNC shares, Unix home folders), as bytes or UTF-16.
# - Every Rust source path (Rust embeds them for panic messages) starts at one of the neutral roots
#   the release build maps paths to (`/cargo`, `/rustup`, `/build`), at the ones the Rust project's
#   own builds use (`/rustc/<commit>`, `/rust/deps`), or at a workspace crate's folder.
# - The PDB is named by its file name only.
#
# Usage: check-paths.ps1 <dll>. Exits with an error listing whatever doesn't fit.
param([Parameter(Mandatory)][string]$Dll)
$ErrorActionPreference = 'Stop'

$bytes = [IO.File]::ReadAllBytes((Resolve-Path $Dll))
# Text is stored as bytes or as UTF-16, at either alignment. Only runs of printable characters are
# searched, so machine code doesn't look like a path by chance.
$texts = @(
    [Text.Encoding]::Latin1.GetString($bytes)
    [Text.Encoding]::Unicode.GetString($bytes)
    [Text.Encoding]::Unicode.GetString($bytes, 1, $bytes.Length - 1)
)
$strings = $texts | ForEach-Object { [regex]::Matches($_, '[\x20-\x7e]{6,}') } | ForEach-Object Value

$problems = [Collections.Generic.List[string]]::new()
$roots = [ordered]@{
    '/cargo (crates)'   = '/cargo[\\/]registry[\\/]src[\\/]'
    '/rustup (std)'     = '/rustup[\\/]toolchains[\\/]'
    '/rustc (std)'      = '/rustc/[0-9a-f]{40}/'
    # Crates std itself uses; the Rust project's builds already map them here.
    '/rust/deps (std)'  = '/rust/deps[\\/]'
    '/build (the repo)' = '/build[\\/]'
    'the repo'          = '^(src|modkit|protocol|hot_reload)[\\/]'
}
$counts = [ordered]@{}
$roots.Keys | ForEach-Object { $counts[$_] = 0 }

foreach ($s in $strings) {
    foreach ($m in [regex]::Matches($s, '(?<![A-Za-z0-9])[A-Za-z]:[\\/][^\\/]*[\\/]|\\\\[A-Za-z0-9][A-Za-z0-9.-]*\\|/(home|Users)/')) {
        $problems.Add("absolute path: $s")
    }
    foreach ($m in [regex]::Matches($s, '[\w.+\-/\\]+\.rs(?!\w)')) {
        $path = $m.Value
        # Other text can sit right before a path in the binary (e.g. "...< 40/rustc/..."), so the
        # neutral roots may come after a prefix; a relative path (anchored with ^) must start it.
        $root = $roots.Keys | Where-Object { $path -match $roots[$_] } | Select-Object -First 1
        if ($root) { $counts[$root]++ } else { $problems.Add("source path with an unexpected root: $path") }
    }
}

# The CodeView record names the PDB: "RSDS", a 16-byte GUID, a 4-byte age, then the name.
$latin1 = $texts[0]
$pdbs = [regex]::Matches($latin1, 'RSDS[\s\S]{20}([^\x00]*\.pdb)\x00') | ForEach-Object { $_.Groups[1].Value }
if (-not $pdbs) { $problems.Add('no CodeView record naming the PDB') }
foreach ($pdb in $pdbs) {
    if ($pdb -match '[\\/:]') { $problems.Add("PDB named by a path: $pdb") }
}

"$Dll"
"  PDB: $($pdbs -join ', ')"
$counts.GetEnumerator() | ForEach-Object { "  source paths under $($_.Key): $($_.Value)" }
if ($problems.Count) {
    $unique = $problems | Sort-Object -Unique
    $unique | Select-Object -First 20 | ForEach-Object { "  $_" }
    if ($unique.Count -gt 20) { "  ... and $($unique.Count - 20) more" }
    throw "$Dll has paths that don't fit the expected forms"
}
"  every path has an expected form"
