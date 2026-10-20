# Fails unless every .exe and .dll in -Path (files, or folders searched recursively) is
# built for -Machine, the COFF machine type: 0x8664 = x64, 0xAA64 = arm64.
# With -ElfMachine it also checks the Flutter AOT snapshot, data\app.so of the first
# folder, which is an ELF file even on Windows (e_machine 0x3E = x86-64, 0xB7 = arm64).
# -Require names files that must be among the ones checked.
param(
    [Parameter(Mandatory)][string[]]$Path,
    [Parameter(Mandatory)][string]$Machine,
    [string]$ElfMachine,
    [string[]]$Require = @()
)
$ErrorActionPreference = 'Stop'

function Get-PeMachine([string]$File) {
    $fs = [IO.File]::OpenRead($File)
    try {
        $br = New-Object IO.BinaryReader($fs)
        $fs.Position = 0x3C
        $fs.Position = $br.ReadInt32()
        if ($br.ReadUInt32() -ne 0x4550) { throw "$File is not a PE file" }
        return $br.ReadUInt16()
    } finally { $fs.Dispose() }
}

$expected = [Convert]::ToUInt16($Machine, 16)
$files = @(foreach ($p in $Path) {
    if (Test-Path $p -PathType Leaf) { Get-Item $p }
    else { Get-ChildItem -Path $p -Recurse -File -Include *.exe, *.dll }
})
if ($files.Count -eq 0) { throw "no .exe or .dll in $($Path -join ', ')" }
foreach ($name in $Require) {
    if (-not ($files.Name -contains $name)) { throw "$name is missing from $($Path -join ', ')" }
}
$bad = 0
foreach ($f in $files) {
    $m = Get-PeMachine $f.FullName
    '{0,-40} 0x{1:X4}' -f $f.Name, $m
    if ($m -ne $expected) {
        Write-Host "::error file=$($f.FullName)::machine 0x$('{0:X4}' -f $m), expected $Machine"
        $bad++
    }
}
"checked $($files.Count) PE files"

if ($ElfMachine) {
    $so = Join-Path $Path[0] 'data\app.so'
    $bytes = [IO.File]::ReadAllBytes($so)
    $elf = [BitConverter]::ToUInt16($bytes, 0x12)
    'data\app.so ELF e_machine 0x{0:X2}' -f $elf
    if ($elf -ne [Convert]::ToUInt16($ElfMachine, 16)) {
        Write-Host "::error file=$so::ELF machine 0x$('{0:X2}' -f $elf), expected $ElfMachine"
        $bad++
    }
}
if ($bad) { exit 1 }
