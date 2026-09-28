#Requires -Version 7.0
<#
    Runs Verus over a crate.

    Standalone, not through cargo. Verus injects vstd from its own release, so
    nothing is resolved, fetched or built, and the manifests stay empty.
#>
[CmdletBinding()]
param(
    [string] $Entry = 'crates/exec-core/src/lib.rs',
    [int]    $Rlimit = 60,
    [switch] $NoVstd,
    [switch] $ProfileSolver
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')

$path = Join-Path $env:VEXEC_ROOT $Entry
if (-not (Test-Path $path)) { throw "no such entry: $Entry" }

$a = @($path, '--crate-type=lib', '--time', '--rlimit', $Rlimit)
if ($NoVstd)        { $a += '--no-vstd' }
if ($ProfileSolver) { $a += '--profile' }

& verus @a
exit $LASTEXITCODE