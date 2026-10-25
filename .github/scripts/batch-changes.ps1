# Batches the waiting changes into the next version with changie, and rebuilds CHANGELOG.md.
#
# A version that was batched but never published (its release was left as a draft) is folded into the
# new one: the change files its release commit batched are put back, and its section is removed, so
# the changelog only lists versions players could download, each with every change since the one
# before. Its number isn't reused: the new version comes after every version batched so far.
#
# Needs the full history (to find the release commits) and changie on the PATH.
#
# Usage: batch-changes.ps1 -Bump <auto|major|minor|patch|v1.2.3> -Published <tags of published releases>
# Returns the new version's tag and the tags of the versions folded into it.
param(
    [Parameter(Mandatory)][string]$Bump,
    [string[]]$Published = @()
)
$ErrorActionPreference = 'Stop'

function ConvertTo-Version([string]$tag) {
    [System.Management.Automation.SemanticVersion]$tag.TrimStart('v')
}

$batched = Get-ChildItem .changes -Filter 'v*.md' | ForEach-Object BaseName |
    Sort-Object { ConvertTo-Version $_ }
if (-not $batched) { throw 'No versions in .changes: changie has nothing to bump from.' }
$newest = ConvertTo-Version $batched[-1]

# The versions after the newest published one. Older unpublished ones were dealt with before this
# script existed (0.2.5's notes cover 0.2.3 and 0.2.4), and the newest published release is the one
# players have, so it's what the new release's notes start from.
$lastPublished = $Published | Where-Object { $_ -in $batched } | Sort-Object { ConvertTo-Version $_ } |
    Select-Object -Last 1
if (-not $lastPublished) { throw 'None of the batched versions has a published release.' }
$unpublished = @($batched | Where-Object { (ConvertTo-Version $_) -gt (ConvertTo-Version $lastPublished) })

foreach ($tag in $unpublished) {
    $commits = @(git log --diff-filter=A --format=%H -- ".changes/$tag.md")
    if ($LASTEXITCODE) { throw 'git log failed' }
    if ($commits.Count -ne 1) {
        throw "Expected one commit adding .changes/$tag.md, found $($commits.Count) (is the history shallow?)."
    }
    $commit = $commits[0]
    $files = @(git diff-tree -r --no-renames --diff-filter=D --name-only "$commit^" $commit -- .changes/unreleased |
        Where-Object { $_ -like '*.yaml' })
    if ($LASTEXITCODE) { throw 'git diff-tree failed' }
    if (-not $files) { throw "The release commit of $tag ($commit) batched no change files." }
    git checkout "$commit^" -- @files
    if ($LASTEXITCODE) { throw "cannot restore the change files of $tag" }
    Write-Host "Folding $tag into the new version: put back $($files -join ', ')"
}

# changie bumps from the newest version file, so the version is picked before the unpublished ones'
# files are removed: otherwise it could pick one of their numbers again.
if ($Bump -match '^(auto|major|minor|patch)$') {
    $tag = changie next $Bump
    if ($LASTEXITCODE) { throw "changie next $Bump failed (are there any changes to release?)" }
} else {
    $tag = $Bump
}
if ((ConvertTo-Version $tag) -le $newest) {
    throw "The new version $tag must come after $($batched[-1])."
}

foreach ($folded in $unpublished) {
    Remove-Item ".changes/$folded.md"
}
# Fails if there are no changes to release.
changie batch $tag --allow-no-changes=false | Out-Host
if ($LASTEXITCODE) { throw 'changie batch failed' }
changie merge | Out-Host
if ($LASTEXITCODE) { throw 'changie merge failed' }

[pscustomobject]@{ Tag = $tag; Folded = $unpublished }
