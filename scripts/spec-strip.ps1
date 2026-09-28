#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $In,
    [string] $Out = '',
    [switch] $Check
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$src = if ([System.IO.Path]::IsPathRooted($In)) { $In } else { Join-Path $root $In }
if (-not (Test-Path $src)) { throw "no such file: $In" }

# Split first, then strip the carriage return from every line. Replacing the
# pair beforehand misses a lone CR, which then survives into the output and
# makes a file compare unequal to itself.
$lines = ([System.IO.File]::ReadAllText($src) -split "`n") | ForEach-Object { $_.TrimEnd("`r") }
$kept = [System.Collections.Generic.List[string]]::new()
$pending = [System.Collections.Generic.List[string]]::new()
$dropped = @{}

function Note {
    param([string] $Key)
    $script:dropped[$Key] = 1 + ($script:dropped[$Key] ?? 0)
}

# Comments are removed before braces are counted, so a brace inside one cannot
# move the depth. Crude against string literals; there are none in these files.
function Code {
    param([string] $Text)
    $at = $Text.IndexOf('//')
    if ($at -ge 0) { return $Text.Substring(0, $at) }
    return $Text
}

function Delta {
    param([string] $Text)
    $c = Code $Text
    ($c.ToCharArray() | Where-Object { $_ -eq '{' }).Count -
    ($c.ToCharArray() | Where-Object { $_ -eq '}' }).Count
}

function LastChar {
    param([string] $Text)
    $c = (Code $Text).TrimEnd()
    if ($c.Length -eq 0) { return '' }
    return $c[$c.Length - 1]
}

# Consumes a whole item starting at $i and returns the index past it.
#
# The body is open once the depth is above zero at the end of a line, which is
# what separates it from a signature clause holding balanced braces of its own.
# It is closed by a line that ends on a brace with the depth back at zero.
function Skip-Item {
    param([string[]] $Lines, [int] $Start)
    $d = 0
    $open = $false
    $i = $Start
    while ($i -lt $Lines.Count) {
        $d += Delta $Lines[$i]
        if ($d -gt 0) { $open = $true }
        $last = LastChar $Lines[$i]
        $i++
        if ($open -and $d -le 0 -and $last -eq '}') { break }
    }
    return $i
}

$i = 0
$inVerus = $false
$depth = 0

while ($i -lt $lines.Count) {
    $line = $lines[$i]
    $t = $line.Trim()

    # ---------------------------------------------------------------- imports
    if ($t -match '^use\s+vstd::' -or $t -match '^broadcast\s+use\s') {
        Note 'vstd use'; $i++; continue
    }
    if ($t -match '^use\s+verus_builtin') { Note 'builtin use'; $i++; continue }

    # ---------------------------------------------------------------- wrapper
    if (-not $inVerus -and $t -match '^verus!\s*\{') {
        Note 'verus wrapper'
        $inVerus = $true
        $depth = 0
        $i++
        continue
    }
    # The wrapper closes at depth zero. Anything else at that depth is an item
    # of the module and its brace belongs to it.
    if ($inVerus -and $depth -eq 0 -and $t -match '^\}') {
        $inVerus = $false
        $i++
        continue
    }

    # ---------------------------------------------------------------- ghost
    if ($t -match '^(pub\s+)?(open\s+|closed\s+)?spec\s+fn\s' -or
        $t -match '^(pub\s+)?proof\s+fn\s') {
        Note $(if ($t -match 'spec\s+fn') { 'spec fn' } else { 'proof fn' })
        $pending.Clear()
        $i = Skip-Item $lines $i
        continue
    }

    if ($t -match '^(requires|ensures|invariant|decreases|recommends)\b') {
        Note 'clause'
        # Runs to the line that opens the body, which is left for the next pass
        # so that its brace is counted.
        while ($i -lt $lines.Count -and $lines[$i].Trim() -notmatch '^\{') { $i++ }
        continue
    }

    if ($t -match '^proof\s*\{') {
        Note 'proof block'
        $i = Skip-Item $lines $i
        continue
    }

    if ($t -match '^assert\b') {
        Note 'assert'
        $d = 0
        $j = $i
        while ($j -lt $lines.Count) {
            $d += Delta $lines[$j]
            $last = LastChar $lines[$j]
            $j++
            if ($d -le 0 -and ($last -eq ';' -or $last -eq '}')) { break }
        }
        $i = $j
        continue
    }

    # ---------------------------------------------------------------- pending
    # Documentation and attributes are held back until it is known whether the
    # item they belong to survives.
    if ($t -match '^///' -or $t -match '^#\[') {
        $pending.Add($line)
        $i++
        continue
    }

    if ($t -eq '') {
        if ($pending.Count -gt 0) {
            # Nothing attached to them, so they described something dropped.
            Note 'orphan doc'
            $pending.Clear()
        }
        $kept.Add($line)
        $i++
        continue
    }

    foreach ($p in $pending) { $kept.Add($p) }
    $pending.Clear()

    # ---------------------------------------------------------------- rewrite
    if ($line -match '->\s*\(\s*[a-z_][a-z0-9_]*\s*:\s*') {
        $line = $line -replace '->\s*\(\s*[a-z_][a-z0-9_]*\s*:\s*(.+?)\s*\)\s*$', '-> $1'
        Note 'named return'
    }

    $kept.Add($line)
    if ($inVerus) { $depth += Delta $line }
    $i++
}

if ($pending.Count -gt 0) { Note 'orphan doc' }

$body = ($kept -join "`n") -replace "`n{3,}", "`n`n"
$name = [System.IO.Path]::GetFileName($src)

$header = @"
// GENERATED. Do not edit.
//
// Produced from spec/$name by scripts/spec-strip.ps1, which only deletes:
// ghost items, specification clauses, proof blocks and asserts. Every
// executable statement below is the one that was verified, character for
// character.
//
// Regenerate:  .\scripts\spec-strip.ps1 -In spec/$name -Out <path>
// Check:       .\scripts\spec-strip.ps1 -In spec/$name -Out <path> -Check

"@

# One normalisation at the end, covering everything: the stripped body, the
# header written above, and anything a here-string picked up from the line
# endings of this script file. Chasing carriage returns at each source
# separately is how one survives.
$result = ($header + $body.Trim() + "`n") -replace "`r`n", "`n" -replace "`r", "`n"

foreach ($k in ($dropped.Keys | Sort-Object)) {
    Write-Host ("  dropped {0,-16} {1}" -f $k, $dropped[$k]) -ForegroundColor DarkGray
}

# A brace count that does not balance means the strip cut something in half.
$open = ($result.ToCharArray() | Where-Object { $_ -eq '{' }).Count
$close = ($result.ToCharArray() | Where-Object { $_ -eq '}' }).Count
if ($open -ne $close) {
    Write-Host ''
    Write-Host ("UNBALANCED: {0} open, {1} close" -f $open, $close) -ForegroundColor Red
    Write-Host '  the output is not valid rust, nothing was written' -ForegroundColor Red
    exit 1
}

if (-not $Out) {
    Write-Host ("  balanced        {0} braces" -f $open) -ForegroundColor DarkGray
    Write-Host ''
    Write-Host $result
    exit 0
}

$dest = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }

if ($Check) {
    if (-not (Test-Path $dest)) {
        Write-Host "absent: $Out" -ForegroundColor Red
        exit 1
    }
    $have = [System.IO.File]::ReadAllText($dest) -replace "`r`n", "`n" -replace "`r", "`n"
    if ($have -eq $result) {
        Write-Host "in sync: $Out" -ForegroundColor Green
        exit 0
    }

    Write-Host "OUT OF SYNC: $Out" -ForegroundColor Red

    # Where, not just whether. A check that reports a difference without
    # locating it costs more to act on than it saves.
    $a = $result -split "`n"
    $b = $have -split "`n"
    $n = [Math]::Max($a.Count, $b.Count)
    $shown = 0
    for ($k = 0; $k -lt $n -and $shown -lt 6; $k++) {
        $left = if ($k -lt $a.Count) { $a[$k] } else { '<end of file>' }
        $right = if ($k -lt $b.Count) { $b[$k] } else { '<end of file>' }
        if ($left -ne $right) {
            Write-Host ("  line {0}" -f ($k + 1)) -ForegroundColor Yellow
            Write-Host ("    proved  [{0}]" -f $left) -ForegroundColor DarkGray
            Write-Host ("    kernel  [{0}]" -f $right) -ForegroundColor DarkGray
            $shown++
        }
    }
    Write-Host ("  {0} lines proved, {1} lines kernel" -f $a.Count, $b.Count) -ForegroundColor DarkGray
    exit 1
}

New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
[System.IO.File]::WriteAllText($dest, $result, [System.Text.UTF8Encoding]::new($false))
Write-Host "written $Out" -ForegroundColor Cyan