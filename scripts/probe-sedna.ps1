#Requires -Version 7.0
<#
    Reads the OC2R jar to find out what the deployment machine actually is.

    A jar is a zip and a class file carries its constant pool in a documented
    format, so the strings and the numeric constants come out without a
    decompiler. That is enough to answer the questions that matter: where
    memory is, what the console is, how the device tree is built, and what the
    firmware hands to the payload.
#>
[CmdletBinding()]
param(
    [string] $Jar = '',
    [string] $Class = '',
    [switch] $Full
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

if (-not $Jar) {
    $found = Get-ChildItem (Join-Path $root 'tools\ref') -Filter '*.jar' -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if (-not $found) {
        Write-Host 'no jar under tools/ref' -ForegroundColor Red
        Write-Host '  .\run.ps1 -Oc2rJar -Apply' -ForegroundColor DarkGray
        exit 1
    }
    $Jar = $found.FullName
}

function Say { param($k, $t, $c = 'Gray') Write-Host ("{0,-11} {1}" -f $k, $t) -ForegroundColor $c }
function Head { param($t) Write-Host ''; Write-Host $t -ForegroundColor White }

# Class file constant pool. Every entry is a tag byte followed by a fixed or
# length prefixed body, so the whole pool can be walked without understanding
# anything else about the format.
#
# Long and Double take two slots each. Missing that desynchronises the walk and
# turns the rest of the pool into noise, which is why the index advances twice
# for them.
function Read-Constants {
    param([byte[]] $Bytes)

    if ($Bytes.Length -lt 10) { return $null }
    if ($Bytes[0] -ne 0xCA -or $Bytes[1] -ne 0xFE -or
        $Bytes[2] -ne 0xBA -or $Bytes[3] -ne 0xBE) { return $null }

    $count = ($Bytes[8] -shl 8) -bor $Bytes[9]
    $pos = 10
    $strings = [System.Collections.Generic.List[string]]::new()
    $numbers = [System.Collections.Generic.List[long]]::new()

    $i = 1
    while ($i -lt $count -and $pos -lt $Bytes.Length) {
        $tag = $Bytes[$pos]
        $pos++

        switch ($tag) {
            1 {
                if ($pos + 2 -gt $Bytes.Length) { break }
                $len = ($Bytes[$pos] -shl 8) -bor $Bytes[$pos + 1]
                $pos += 2
                if ($pos + $len -le $Bytes.Length) {
                    $strings.Add([System.Text.Encoding]::UTF8.GetString($Bytes, $pos, $len))
                }
                $pos += $len
            }
            3 {
                # Built as a long from unsigned bytes, so 0x80000000 comes out
                # as an address rather than as a negative int.
                $v = 0L
                for ($k = 0; $k -lt 4; $k++) { $v = ($v -shl 8) -bor $Bytes[$pos + $k] }
                $numbers.Add($v)
                $pos += 4
            }
            4 { $pos += 4 }
            5 {
                $v = 0L
                for ($k = 0; $k -lt 8; $k++) { $v = ($v -shl 8) -bor $Bytes[$pos + $k] }
                $numbers.Add($v)
                $pos += 8
                $i++
            }
            6 { $pos += 8; $i++ }
            7  { $pos += 2 }
            8  { $pos += 2 }
            9  { $pos += 4 }
            10 { $pos += 4 }
            11 { $pos += 4 }
            12 { $pos += 4 }
            15 { $pos += 3 }
            16 { $pos += 2 }
            17 { $pos += 4 }
            18 { $pos += 4 }
            19 { $pos += 2 }
            20 { $pos += 2 }
            default { $i = $count }
        }
        $i++
    }

    return @{ Strings = $strings; Numbers = $numbers }
}

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

try {
    Say 'jar' ("{0}  ({1:n1} MB)" -f (Split-Path $Jar -Leaf), ((Get-Item $Jar).Length / 1MB)) 'Green'

    $outer = [System.IO.Compression.ZipFile]::OpenRead($Jar)
    $opened.Add($outer)

    # Forge ships dependencies as whole jars inside the mod jar rather than
    # merging their classes in. Sedna is one of those, and it is where every
    # question about the machine is answered; the outer archive holds the mod
    # and almost nothing else.
    $all = [System.Collections.Generic.List[object]]::new()
    foreach ($e in $outer.Entries) { $all.Add($e) }

    $nested = $outer.Entries | Where-Object { $_.FullName -like '*.jar' }
    foreach ($n in $nested) {
        # A nested archive has to be copied out: ZipArchive needs a seekable
        # stream and the deflate stream an entry gives is not one.
        $temp = Join-Path $env:TEMP ("vexec-nested-{0}" -f (Split-Path $n.FullName -Leaf))
        [System.IO.File]::WriteAllBytes($temp, (Read-Entry $n))
        $inner = [System.IO.Compression.ZipFile]::OpenRead($temp)
        $opened.Add($inner)

        $count = 0
        foreach ($e in $inner.Entries) { $all.Add($e); $count++ }
        Say 'nested' ("{0}  ({1:n1} MB, {2} entries)" -f `
            $n.FullName, ($n.Length / 1MB), $count) 'Cyan'
    }
    $classes = $all | Where-Object { $_.FullName -like '*.class' }
    $resources = $all | Where-Object { $_.FullName -notlike '*.class' -and $_.Length -gt 0 }

    Say 'entries' ("{0} classes, {1} other" -f $classes.Count, $resources.Count) 'DarkGray'

    # ---------------------------------------------------------------- blobs

    Head '1. binary resources'
    Say 'why' 'the firmware and any shipped image are here, and they define the boot contract' 'DarkGray'

    $blobs = $resources | Where-Object {
        $_.FullName -match '\.(bin|img|dtb|elf|dat|raw)$' -or $_.Length -gt 65536
    } | Sort-Object Length -Descending

    if ($blobs) {
        foreach ($b in ($blobs | Select-Object -First 20)) {
            $bytes = Read-Entry $b
            $head = ($bytes[0..([Math]::Min(15, $bytes.Length - 1))] |
                ForEach-Object { '{0:x2}' -f $_ }) -join ' '

            # Signatures worth recognising on sight.
            $kind = 'unknown'
            if ($bytes.Length -ge 4) {
                if ($bytes[0] -eq 0x7f -and $bytes[1] -eq 0x45) { $kind = 'ELF' }
                elseif ($bytes[0] -eq 0xd0 -and $bytes[1] -eq 0x0d -and
                        $bytes[2] -eq 0xfe -and $bytes[3] -eq 0xed) { $kind = 'device tree blob' }
                elseif ($bytes[0] -eq 0x1f -and $bytes[1] -eq 0x8b) { $kind = 'gzip' }
                # A raw riscv payload starts with executable words. The Linux
                # Image header puts a jump instruction first and the magic at
                # offset 56.
                elseif ($bytes.Length -gt 64 -and
                        $bytes[56] -eq 0x52 -and $bytes[57] -eq 0x49 -and
                        $bytes[58] -eq 0x53 -and $bytes[59] -eq 0x43) { $kind = 'linux riscv Image' }
                else { $kind = 'raw' }
            }

            Say '' ("  {0,-46} {1,9:n0} B  {2}" -f $b.FullName, $b.Length, $kind) 'Cyan'
            Say '' ("    {0}" -f $head) 'DarkGray'
        }
    } else {
        Say 'none' 'no binary resources: the firmware is fetched or generated' 'DarkYellow'
    }

    # ---------------------------------------------------------------- packages

    Head '2. sedna packages'

   # By content rather than by an assumed prefix: the package path inside a
    # relocated jar is not necessarily the one upstream uses.
    $sedna = $classes | Where-Object { $_.FullName -match 'sedna' }
    if ($sedna.Count -eq 0) {
        # Fall back to everything that is not the mod itself, so a rename does
        # not make the probe silently find nothing.
        $sedna = $classes | Where-Object { $_.FullName -notmatch 'li/cil/oc2' }
        Say 'note' 'no path contains sedna; looking at every non oc2 class' 'DarkYellow'
    }
    Say 'classes' ("{0} candidate classes" -f $sedna.Count) 'DarkGray'

    $sedna | ForEach-Object {
        $parts = $_.FullName -split '/'
        ($parts[0..($parts.Count - 2)] -join '/')
    } | Group-Object | Sort-Object Name | ForEach-Object {
        Say '' ("  {0,-52} {1,4}" -f $_.Name, $_.Count) 'DarkGray'
    }

    # ---------------------------------------------------------------- key

    Head '3. classes that answer the questions'

    $patterns = [ordered]@{
        'board, the machine'   = 'Board|Machine|System'
        'device tree'          = 'DeviceTree|Fdt|FlattenedDevice'
        'serial console'       = 'UART|Serial|Console|16550'
        'interrupt controller' = 'Interrupt|PLIC|CLINT|Interrupter'
        'memory'               = 'Memory|Ram|PhysicalMemory'
        'boot and firmware'    = 'Boot|Firmware|Loader|Sbi'
        'virtio'               = 'VirtIO|Virtio'
    }

    $keyClasses = @{}
    foreach ($label in $patterns.Keys) {
        $hits = $sedna | Where-Object { $_.FullName -match $patterns[$label] }
        Say $label.Substring(0, [Math]::Min(11, $label.Length)) ("{0} class(es)" -f $hits.Count) 'White'
        foreach ($h in ($hits | Select-Object -First $(if ($Full) { 40 } else { 8 }))) {
            Say '' ("  " + ($h.FullName -replace '^li/cil/sedna/', '')) 'DarkGray'
            $keyClasses[$h.FullName] = $h
        }
        if (-not $Full -and $hits.Count -gt 8) {
            Say '' ("  ... and {0} more, -Full to see them" -f ($hits.Count - 8)) 'DarkGray'
        }
    }

    # ---------------------------------------------------------------- strings

    Head '4. device tree property names and compatible strings'
    Say 'why' 'what the tree contains decides what the platform parser will find' 'DarkGray'

    $treeWords = @(
        'compatible', 'reg', 'device_type', 'timebase-frequency', 'clock-frequency',
        'interrupts', 'interrupt-parent', 'interrupt-controller', 'interrupts-extended',
        '#address-cells', '#size-cells', '#interrupt-cells', 'bootargs', 'stdout-path',
        'chosen', 'cpus', 'memory', 'soc', 'ranges', 'phandle', 'riscv,isa', 'mmu-type',
        'reg-shift', 'reg-io-width', 'status'
    )

    $seenProps = [System.Collections.Generic.HashSet[string]]::new()
    $seenCompat = [System.Collections.Generic.HashSet[string]]::new()
    $seenOther = [System.Collections.Generic.HashSet[string]]::new()

    foreach ($entry in $sedna) {
        $bytes = Read-Entry $entry
        $pool = Read-Constants $bytes
        if (-not $pool) { continue }

        foreach ($s in $pool.Strings) {
            if ($s.Length -lt 2 -or $s.Length -gt 64) { continue }

            if ($treeWords -contains $s) { [void]$seenProps.Add($s); continue }

            # A compatible value is vendor,device. The comma is what separates
            # it from a class name or a method descriptor.
            if ($s -match '^[a-z0-9]+,[a-z0-9._-]+$') { [void]$seenCompat.Add($s); continue }

            # Node names carry a unit address after an at sign.
            if ($s -match '^[a-z][a-z0-9_-]*@[0-9a-f]+$') { [void]$seenOther.Add($s); continue }

            # A kernel command line, if one is baked in.
            if ($s -match 'console=|root=|earlycon') { [void]$seenOther.Add($s) }
        }
    }

    Say 'properties' (($seenProps | Sort-Object) -join ', ') 'Cyan'
    Write-Host ''
    Say 'compatible' '' 'White'
    foreach ($c in ($seenCompat | Sort-Object)) { Say '' ("  " + $c) 'Cyan' }

    if ($seenOther.Count -gt 0) {
        Write-Host ''
        Say 'node names' '' 'White'
        foreach ($o in ($seenOther | Sort-Object)) { Say '' ("  " + $o) 'DarkGray' }
    }

    # ---------------------------------------------------------------- addresses

    Head '5. address constants'
    Say 'why' 'where memory and the devices are, which is what the port has to match' 'DarkGray'

    $addresses = @{}
    foreach ($entry in $sedna) {
        if ($entry.FullName -notmatch 'Board|Machine|Memory|UART|Serial|Interrupt|PLIC|CLINT|VirtIO|Boot|Firmware') {
            continue
        }
        $pool = Read-Constants (Read-Entry $entry)
        if (-not $pool) { continue }

        $short = $entry.FullName -replace '^li/cil/sedna/', '' -replace '\.class$', ''
        foreach ($n in $pool.Numbers) {
            # Page aligned and above a megabyte. Catches bases and sizes,
            # misses register offsets, which is the right trade here.
            if ($n -ge 0x100000 -and $n -le 0xffffffffff -and ($n % 0x1000) -eq 0) {
                $key = '{0:x}' -f $n
                if (-not $addresses.ContainsKey($key)) {
                    $addresses[$key] = [System.Collections.Generic.HashSet[string]]::new()
                }
                [void]$addresses[$key].Add($short)
            }
        }
    }

    foreach ($key in ($addresses.Keys | Sort-Object { [Convert]::ToUInt64($_, 16) })) {
        $where = ($addresses[$key] | Sort-Object | Select-Object -First 3) -join ', '
        Say '' ("  0x{0,-12} {1}" -f $key, $where) 'Cyan'
    }

    # ---------------------------------------------------------------- oc2

    Head '6. how the mod starts a machine'
    Say 'why' 'the address the payload is placed at and the order of the stages' 'DarkGray'

    $oc2 = $classes | Where-Object {
        $_.FullName -match 'li/cil/oc2' -and
        $_.FullName -match 'Vm|Machine|Computer|Board|Firmware|Boot'
    }
    foreach ($c in ($oc2 | Select-Object -First 12)) {
        Say '' ("  " + ($c.FullName -replace '^li/cil/oc2/', '')) 'DarkGray'
    }

    # ---------------------------------------------------------------- one class

    if ($Class) {
        Head "7. $Class"
        $hit = $classes | Where-Object { $_.FullName -like "*$Class*" } | Select-Object -First 1
        if (-not $hit) {
            Say 'absent' "no class matching $Class" 'Red'
        } else {
            Say 'class' $hit.FullName 'Green'
            $pool = Read-Constants (Read-Entry $hit)

            Say 'strings' ("{0}" -f $pool.Strings.Count) 'White'
            foreach ($s in ($pool.Strings | Where-Object { $_.Length -ge 3 -and $_ -notmatch '^\(|^\[|^L[a-z]' })) {
                Say '' ("  " + $s) 'DarkGray'
            }

            Say 'numbers' ("{0}" -f $pool.Numbers.Count) 'White'
            foreach ($n in ($pool.Numbers | Sort-Object -Unique)) {
                if ($n -ge 0x100) {
                    Say '' ("  {0,20} = 0x{0:x}" -f $n) 'DarkGray'
                }
            }
        }
    }

} finally {
    foreach ($z in $opened) { $z.Dispose() }
    Get-ChildItem $env:TEMP -Filter 'vexec-nested-*' -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Say 'next' '-Full for the whole class list' 'DarkGray'
Say '' '-Class R5Board to dump one class' 'DarkGray'