#Requires -Version 7.0
<#
    Runs the kernel image under QEMU virt.

    The reference platform. Correctness is defined against it, and Sedna is
    checked against the behaviour established here rather than the other way
    round.

    QEMU is located on PATH, then in the usual install directories, then from
    VEXEC_QEMU or -QemuPath. Its installer does not extend PATH, so searching
    is the normal case rather than a fallback.

    Exit the guest with Ctrl-A then X.
#>
[CmdletBinding()]
param(
    [switch] $Dev,
    [int]    $MemoryMb = 128,
    [switch] $Gdb,
    [switch] $DumpDtb,
    [string] $QemuPath = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

function Resolve-Qemu {
    param([string] $Explicit)

    # An explicit argument may be the executable or the directory holding it.
    if ($Explicit) {
        if (Test-Path $Explicit -PathType Leaf) { return $Explicit }
        $p = Join-Path $Explicit 'qemu-system-riscv64.exe'
        if (Test-Path $p) { return $p }
        throw "no qemu-system-riscv64 at $Explicit"
    }

    if ($env:VEXEC_QEMU) {
        $p = Join-Path $env:VEXEC_QEMU 'qemu-system-riscv64.exe'
        if (Test-Path $p) { return $p }
        if (Test-Path $env:VEXEC_QEMU -PathType Leaf) { return $env:VEXEC_QEMU }
    }

    $onPath = Get-Command qemu-system-riscv64 -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }

    $candidates = @(
        (Join-Path $env:ProgramFiles 'qemu'),
        (Join-Path ${env:ProgramFiles(x86)} 'qemu'),
        'C:\Program Files\qemu',
        'C:\qemu',
        (Join-Path $env:LOCALAPPDATA 'Programs\qemu')
    )
    foreach ($c in $candidates) {
        if (-not $c) { continue }
        $p = Join-Path $c 'qemu-system-riscv64.exe'
        if (Test-Path $p) { return $p }
    }

    return $null
}

$qemu = Resolve-Qemu $QemuPath
if (-not $qemu) {
    Write-Host 'qemu-system-riscv64 not found' -ForegroundColor Red
    Write-Host '  searched PATH, VEXEC_QEMU, and the usual install directories' -ForegroundColor DarkGray
    Write-Host '  pass -QemuPath "D:\tools\qemu", or set VEXEC_QEMU' -ForegroundColor DarkGray
    exit 1
}
Write-Host "qemu:  $qemu" -ForegroundColor DarkGray

if ($DumpDtb) {
    # The device tree QEMU generates for this configuration, without booting
    # anything. The reference against which Sedna's tree is read.
    $out = Join-Path $root 'artifacts\qemu-virt.dtb'
    New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
    [string[]] $a = @(
        '-machine', "virt,dumpdtb=$out",
        '-cpu', 'rv64',
        '-smp', '1',
        '-m', "${MemoryMb}M",
        '-nographic',
        '-bios', 'default'
    )
    & $qemu @a
    Write-Host $out -ForegroundColor Cyan
    exit 0
}

$dir = if ($Dev) { 'debug' } else { 'release' }
$elf = Join-Path $root "target\riscv64imac-unknown-none-elf\$dir\exec"
if (-not (Test-Path $elf)) {
    Write-Host "no image at $elf" -ForegroundColor Red
    Write-Host '  .\scripts\build.ps1' -ForegroundColor DarkGray
    exit 1
}

# -bios default is OpenSBI, the same firmware Sedna runs, so the boot contract
# is identical on both.
[string[]] $a = @(
    '-machine', 'virt',
    '-cpu', 'rv64',
    '-smp', '1',
    '-m', "${MemoryMb}M",
    '-nographic',
    '-bios', 'default',
    '-kernel', $elf
)

if ($Gdb) {
    # -S holds the machine before the first instruction so a debugger can
    # attach: target remote :1234
    $a += @('-s', '-S')
    Write-Host 'stopped, waiting for gdb on :1234' -ForegroundColor Yellow
}

Write-Host "image: $elf" -ForegroundColor DarkGray
Write-Host 'exit with ctrl-a x' -ForegroundColor DarkGray
Write-Host ''
& $qemu @a