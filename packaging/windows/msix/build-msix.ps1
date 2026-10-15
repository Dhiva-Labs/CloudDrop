# Packs the Microsoft Store package (.msix) from a Windows build made by
# packaging\windows\build.ps1. It is not signed: the Store signs it. The identity
# defaults to CloudDrop's Partner Center reservation; CI passes a test identity.
# Needs the Windows SDK (makeappx, makepri).
#
# -Arch is the architecture of the build in -Dist (x64 or arm64). It defaults to the
# machine's, and the package is refused when the app's exe is built for another one.
param(
    [string]$Name = 'Dhiva-Labs.CloudDrop',
    [string]$Publisher = 'CN=0F69015A-AA5B-431D-970A-165099309E53',
    [ValidateSet('x64', 'arm64')]
    [string]$Arch,
    [string]$Dist,
    [string]$Out
)
$ErrorActionPreference = 'Stop'

if (-not $Arch) {
    $Arch = switch ($env:PROCESSOR_ARCHITECTURE) {
        'AMD64' { 'x64' }
        'ARM64' { 'arm64' }
        default { throw "unsupported PROCESSOR_ARCHITECTURE '$env:PROCESSOR_ARCHITECTURE' (AMD64 or ARM64)" }
    }
}

# The COFF machine type of a PE file (0x8664 = x64, 0xAA64 = arm64).
function Get-PeMachine([string]$Path) {
    $fs = [IO.File]::OpenRead($Path)
    try {
        $br = New-Object IO.BinaryReader($fs)
        $fs.Position = 0x3C
        $fs.Position = $br.ReadInt32()
        if ($br.ReadUInt32() -ne 0x4550) { throw "$Path is not a PE file" }
        return $br.ReadUInt16()
    } finally { $fs.Dispose() }
}

# Runs a program. (Windows PowerShell 5.1 turns a program's stderr output into
# errors, which 'Stop' would make fatal, so the exit code decides instead.)
function Run([string]$what, [scriptblock]$command) {
    $ErrorActionPreference = 'Continue'
    & $command 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE) { throw "$what failed" }
}

$root = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if (-not $Dist) { $Dist = Join-Path $root "dist\CloudDrop-$version-windows-$Arch\CloudDrop" }
if (-not $Out) { $Out = Join-Path $root "dist\CloudDrop-$version-$Arch.msix" }

$expected = @{ x64 = 0x8664; arm64 = 0xAA64 }[$Arch]
$machine = Get-PeMachine (Join-Path $Dist 'clouddrop_app.exe')
if ($machine -ne $expected) {
    throw ('clouddrop_app.exe in {0} is built for machine 0x{1:X4}, not {2} (0x{3:X4})' -f $Dist, $machine, $Arch, $expected)
}

# The SDK tools of this machine's architecture, else the x64 ones (they run on
# Windows on ARM too, emulated).
$toolDirs = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64', 'x64' } else { 'x64' }
$sdk = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Directory |
    Where-Object Name -match '^10\.' | Sort-Object { [version]$_.Name } -Descending |
    ForEach-Object { $bin = $_.FullName; $toolDirs | ForEach-Object { Join-Path $bin $_ } } |
    Where-Object { Test-Path (Join-Path $_ 'makeappx.exe') } | Select-Object -First 1
if (-not $sdk) { throw 'makeappx.exe not found: install the Windows SDK' }

$stage = Join-Path ([IO.Path]::GetTempPath()) "clouddrop-msix-$PID"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory $stage | Out-Null
try {
    Copy-Item -Recurse (Join-Path $Dist '*') $stage
    Copy-Item -Recurse (Join-Path $PSScriptRoot 'Assets') $stage
    $manifest = (Get-Content (Join-Path $PSScriptRoot 'AppxManifest.xml.in') -Raw).
        Replace('@NAME@', $Name).Replace('@PUBLISHER@', $Publisher).Replace('@VERSION@', "$version.0").Replace('@ARCH@', $Arch)
    [IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest)

    # Resource index, so Windows picks the right image size for each place.
    Push-Location $stage
    try {
        Run 'makepri createconfig' { & "$sdk\makepri.exe" createconfig /cf priconfig.xml /dq en-US /o }
        Run 'makepri new' { & "$sdk\makepri.exe" new /pr . /cf priconfig.xml /of resources.pri /o }
        Remove-Item priconfig.xml
    } finally { Pop-Location }

    New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
    Run 'makeappx pack' { & "$sdk\makeappx.exe" pack /d $stage /p $Out /o }
} finally {
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}
Write-Host "Package: $Out"
