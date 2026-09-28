#Requires -Version 7.0
<#
    Builds the flat image the mod copies into memory.

    The reset vector jumps to 0x80000000 in machine mode, so whatever sits at
    the start of the image runs first. The kernel expects supervisor mode with
    an SBI implementation beneath it, which means firmware has to come first
    and the kernel has to sit at the address that firmware jumps to.

    The firmware is the OpenSBI fw_jump the mod already ships. It is taken
    from the jar rather than built, so no external toolchain is involved and
    the binary is the one the mod itself is tested against.

    Layout of the result:

        0x80000000  fw_jump, running in machine mode
        ...         zero padding
        jump target kernel, entered in supervisor mode with a0 and a1 set

    The jump target is compiled into fw_jump as a data word in its entry
    section and is read out of the file rather than assumed. A mismatch
    between it and the linker script produces an image that transfers control
    into padding, which faults with no handler installed and no console
    attached, so it is checked and refused here.
#>
[CmdletBinding()]
param(
    [string] $Jar = '',
    [switch] $Dev,
    [string] $Out = '',
    [string] $JumpAddr = '',
    [switch] $Force
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

function Say { param($k, $t, $c = 'Gray') Write-Host ("{0,-11} {1}" -f $k, $t) -ForegroundColor $c }
function Head { param($t) Write-Host ''; Write-Host $t -ForegroundColor White }

# Base of RAM, fixed by R5MemoryRangeAllocationStrategy, and the address the
# reset vector jumps to, fixed by R5Board.getDefaultProgramStart.
$RAM_BASE = 0x80000000L
# Highest payload address worth considering. Beyond this the address is a
# pointer into some data structure rather than a jump target.
$RAM_LIMIT = 0x88000000L

# fw_jump keeps its jump address and its device tree address as two aligned
# words in the entry section, which is at the very front of the image. Every
# other match in the file is a relocated pointer in data or rodata, so the
# window is the discriminator: widening it produces hundreds of candidates and
# no way to choose between them.
$ENTRY_WINDOW = 0x1000

if (-not $Jar) {
    $found = Get-ChildItem (Join-Path $root 'tools\ref') -Filter '*.jar' -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if (-not $found) {
        Write-Host 'no jar under tools/ref' -ForegroundColor Red
        exit 1
    }
    $Jar = $found.FullName
}

# ---------------------------------------------------------------- firmware

Add-Type -AssemblyName System.IO.Compression.FileSystem

function Read-Entry {
    param($Entry)
    $stream = $Entry.Open()
    try {
        $buffer = [System.IO.MemoryStream]::new()
        $stream.CopyTo($buffer)
        return $buffer.ToArray()
    } finally { $stream.Dispose() }
}

$opened = [System.Collections.Generic.List[object]]::new()
$firmware = $null

try {
    $outer = [System.IO.Compression.ZipFile]::OpenRead($Jar)
    $opened.Add($outer)

    $all = [System.Collections.Generic.List[object]]::new()
    foreach ($e in $outer.Entries) { $all.Add($e) }

    foreach ($n in ($outer.Entries | Where-Object { $_.FullName -like '*.jar' })) {
        $temp = Join-Path $env:TEMP ("vexec-fw-{0}" -f (Split-Path $n.FullName -Leaf))
        [System.IO.File]::WriteAllBytes($temp, (Read-Entry $n))
        $inner = [System.IO.Compression.ZipFile]::OpenRead($temp)
        $opened.Add($inner)
        foreach ($e in $inner.Entries) { $all.Add($e) }
    }

    $entry = $all | Where-Object { $_.FullName -like '*fw_jump.bin' } | Select-Object -First 1
    if (-not $entry) {
        Write-Host 'no fw_jump.bin in the jar' -ForegroundColor Red
        exit 1
    }
    $firmware = Read-Entry $entry
    Say 'firmware' ("{0}  ({1:n0} bytes)" -f $entry.FullName, $firmware.Length) 'Green'

} finally {
    foreach ($z in $opened) { $z.Dispose() }
    Get-ChildItem $env:TEMP -Filter 'vexec-fw-*' -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
}

# The first instruction of an OpenSBI firmware image preserves the boot
# arguments before anything else runs. Anything else means the file is not
# what it claims to be, or is a version this script has not seen.
$first = [System.BitConverter]::ToUInt32($firmware, 0)
if ($first -ne 0x00050433) {
    Say 'warning' ("first word is 0x{0:x8}, expected mv s0,a0" -f $first) 'DarkYellow'
}

# ---------------------------------------------------------------- target

Head 'jump target'

$found = [System.Collections.Generic.List[object]]::new()
$limit = [Math]::Min($ENTRY_WINDOW, $firmware.Length - 8)

for ($i = 0; $i -le $limit; $i += 8) {
    $v = [System.BitConverter]::ToInt64($firmware, $i)
    if ($v -lt $RAM_BASE -or $v -ge $RAM_LIMIT) { continue }
    if (($v % 0x1000) -ne 0) { continue }
    $found.Add([pscustomobject]@{ Offset = $i; Address = $v })
}

foreach ($c in $found) {
    $above = if ($c.Address - $RAM_BASE -ge $firmware.Length) { 'above the firmware' } else { 'inside the firmware' }
    Say '' ("  0x{0:x}  at file offset 0x{1:x}  {2}" -f $c.Address, $c.Offset, $above) 'Cyan'
}

if ($found.Count -eq 0) {
    Say 'undecided' 'no aligned RAM address in the entry section' 'Red'
    Say '' 'the firmware computes its target rather than storing it' 'DarkGray'
    exit 1
}

# A target inside the firmware would have the kernel overwrite the code that
# is about to jump to it.
$viable = $found | Where-Object { ($_.Address - $RAM_BASE) -ge $firmware.Length }
if ($viable.Count -eq 0) {
    Say 'undecided' 'every candidate lies inside the firmware' 'Red'
    exit 1
}

# Two megabyte alignment is what a payload expecting to be mapped with a large
# page needs, and is what the jump address is set to for that reason. Used to
# break a tie rather than as a requirement.
$preferred = $viable | Where-Object { ($_.Address % 0x200000) -eq 0 }
$chosen = if ($preferred.Count -ge 1) { $preferred[0] } else { $viable[0] }

if ($viable.Count -gt 1) {
    Say 'note' ("{0} viable candidates, taking 0x{1:x}" -f $viable.Count, $chosen.Address) 'DarkYellow'
}

$target = $chosen.Address
$jumpOffset = $chosen.Offset

# The address is a word in a data section, so it can be changed. Doing so is
# the only way to shrink the padding without a toolchain, and it is not free:
# the firmware's bss and its per hart scratch area extend past the end of the
# file, and nothing in a flat binary says how far. A target chosen too close
# to the firmware is overwritten while the firmware is still running.
if ($JumpAddr) {
    $new = [Convert]::ToInt64($JumpAddr, 16)
    if ($new -lt $RAM_BASE -or ($new % 0x1000) -ne 0) {
        throw "jump address must be a page aligned RAM address"
    }
    $margin = ($new - $RAM_BASE) - $firmware.Length
    if ($margin -lt 0) {
        throw ("0x{0:x} lands inside the firmware" -f $new)
    }
    [Array]::Copy([System.BitConverter]::GetBytes($new), 0, $firmware, $jumpOffset, 8)
    Say 'patched' ("0x{0:x} -> 0x{1:x}, {2:n0} bytes clear of the firmware image" -f `
        $target, $new, $margin) 'DarkYellow'
    Say '' 'the bss and scratch of the firmware are not in the file and not accounted for' 'DarkGray'
    $target = $new
}

$payloadOffset = $target - $RAM_BASE
Write-Host ''
Say 'target' ("0x{0:x}  payload at file offset 0x{1:x}" -f $target, $payloadOffset) 'Green'

# ---------------------------------------------------------------- kernel

Head 'kernel'

$dir = if ($Dev) { 'debug' } else { 'release' }
$elf = Join-Path $root "target\riscv64imac-unknown-none-elf\$dir\exec"
if (-not (Test-Path $elf)) {
    Write-Host "no image at $elf" -ForegroundColor Red
    Write-Host '  .\scripts\build.ps1' -ForegroundColor DarkGray
    exit 1
}

# Flattens an ELF by placing every loadable segment at its physical address.
#
# What objcopy -O binary does, done here so the image can be built with
# nothing but a Rust toolchain. The layout is decided by the program headers,
# which is the same table the firmware of a real machine would read.
function ConvertTo-Flat {
    param([byte[]] $Elf)

    if ($Elf.Length -lt 64) { throw 'file is too short to be an ELF' }
    if ($Elf[0] -ne 0x7F -or $Elf[1] -ne 0x45 -or $Elf[2] -ne 0x4C -or $Elf[3] -ne 0x46) {
        throw 'not an ELF'
    }
    if ($Elf[4] -ne 2) { throw 'not a 64 bit ELF' }
    if ($Elf[5] -ne 1) { throw 'not little endian' }

    $machine = [System.BitConverter]::ToUInt16($Elf, 0x12)
    if ($machine -ne 243) { throw "machine is $machine, expected 243 for riscv" }

    $phoff = [System.BitConverter]::ToInt64($Elf, 0x20)
    $phentsize = [System.BitConverter]::ToUInt16($Elf, 0x36)
    $phnum = [System.BitConverter]::ToUInt16($Elf, 0x38)

    $segments = [System.Collections.Generic.List[object]]::new()
    for ($k = 0; $k -lt $phnum; $k++) {
        $at = [int]($phoff + $k * $phentsize)
        $type = [System.BitConverter]::ToUInt32($Elf, $at)
        # PT_LOAD. Everything else describes the file rather than the memory
        # image: notes, the dynamic table, the stack flags.
        if ($type -ne 1) { continue }

        $offset = [System.BitConverter]::ToInt64($Elf, $at + 8)
        $paddr = [System.BitConverter]::ToInt64($Elf, $at + 24)
        $filesz = [System.BitConverter]::ToInt64($Elf, $at + 32)
        $memsz = [System.BitConverter]::ToInt64($Elf, $at + 40)
        if ($filesz -le 0) { continue }

        $segments.Add([pscustomobject]@{
            Offset = $offset; Paddr = $paddr; FileSize = $filesz; MemSize = $memsz
        })
    }

    if ($segments.Count -eq 0) { throw 'no loadable segments' }

    $lowest = ($segments | Measure-Object -Property Paddr -Minimum).Minimum
    $highest = 0L
    foreach ($s in $segments) {
        $end = $s.Paddr + $s.FileSize
        if ($end -gt $highest) { $highest = $end }
    }

    # bss is not written. It occupies memory past the end of the file and the
    # entry code clears it, so putting zeroes in the image would only make the
    # image larger.
    $flat = New-Object byte[] ([int]($highest - $lowest))
    foreach ($s in $segments) {
        [Array]::Copy($Elf, [int]$s.Offset, $flat, [int]($s.Paddr - $lowest), [int]$s.FileSize)
    }

    return [pscustomobject]@{ Bytes = $flat; Base = $lowest; Segments = $segments }
}

$flatten = ConvertTo-Flat ([System.IO.File]::ReadAllBytes($elf))
$kernel = $flatten.Bytes

foreach ($s in $flatten.Segments) {
    Say '' ("  segment  {0:x}..{1:x}  {2,8:n0} bytes in file, {3,8:n0} in memory" -f `
        $s.Paddr, ($s.Paddr + $s.MemSize), $s.FileSize, $s.MemSize) 'DarkGray'
}
Say 'flat' ("{0:x}  ({1:n0} bytes)" -f $flatten.Base, $kernel.Length) 'Cyan'

# The kernel is linked to run at the address the firmware jumps to. A mismatch
# means the firmware transfers control into padding.
if ($flatten.Base -ne $target) {
    Write-Host ''
    Say 'mismatch' ("firmware jumps to 0x{0:x}, the image is linked at 0x{1:x}" -f `
        $target, $flatten.Base) 'Red'
    Say '' 'change BASE_ADDRESS in boot/link.ld to match, then rebuild' 'DarkGray'
    if (-not $Force) { exit 1 }
    Say '' 'continuing anyway because -Force was given' 'DarkYellow'
}

# A copy of the flat kernel on its own, which is what a loader placing the
# payload separately would want.
$standalone = Join-Path $root 'artifacts\exec.bin'
New-Item -ItemType Directory -Force -Path (Split-Path $standalone) | Out-Null
[System.IO.File]::WriteAllBytes($standalone, $kernel)

# ---------------------------------------------------------------- assemble

Head 'image'

$total = [int]$payloadOffset + $kernel.Length
$image = New-Object byte[] $total
[Array]::Copy($firmware, 0, $image, 0, $firmware.Length)
[Array]::Copy($kernel, 0, $image, [int]$payloadOffset, $kernel.Length)

if (-not $Out) { $Out = Join-Path $root 'artifacts\eeprom.bin' }
New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
[System.IO.File]::WriteAllBytes($Out, $image)

$padding = [int]$payloadOffset - $firmware.Length
Say 'firmware' ("{0,10:n0} bytes" -f $firmware.Length) 'DarkGray'
Say 'padding' ("{0,10:n0} bytes" -f $padding) 'DarkGray'
Say 'kernel' ("{0,10:n0} bytes" -f $kernel.Length) 'DarkGray'
Say 'total' ("{0,10:n0} bytes  ({1:n1} KiB)" -f $total, ($total / 1KB)) 'Green'
Say 'written' $Out 'Cyan'
Say 'kernel' $standalone 'DarkGray'

if ($padding -gt $kernel.Length) {
    Write-Host ''
    Say 'note' ("padding is {0:n0} bytes, more than the kernel itself" -f $padding) 'DarkYellow'
    Say '' 'the jump address is set for a payload that expects a large page' 'DarkGray'
    Say '' '-JumpAddr 80080000 moves it, and link.ld has to move with it' 'DarkGray'
}

Write-Host ''
Say 'next' 'compare the total against the capacity of the flash item' 'DarkGray'