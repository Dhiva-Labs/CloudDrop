# Builds the Windows package from a source checkout: the engine (clouddrop.exe,
# clouddrop-agent.exe), the app, and the install scripts, into
# dist\CloudDrop-<version>-windows-<arch>. If packaging\oauth-client.env exists (not in
# git), its Google client is built in, so sign-in works without a key file.
# Needs Rust (MSVC), Flutter and the Visual Studio C++ build tools.
#
# The package is built for the machine it runs on: x64 on a 64-bit Intel/AMD PC,
# arm64 on a Windows on ARM PC (Rust and Flutter build natively there). -Arch only
# names the architecture to expect and fails when it is not the machine's.
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Arch
)
$ErrorActionPreference = 'Stop'

$hostArch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x64' }
    'ARM64' { 'arm64' }
    default { throw "unsupported PROCESSOR_ARCHITECTURE '$env:PROCESSOR_ARCHITECTURE' (AMD64 or ARM64)" }
}
if (-not $Arch) { $Arch = $hostArch }
if ($Arch -ne $hostArch) { throw "this is a $hostArch machine: build the $Arch package on a $Arch machine" }

# Runs a program. (Windows PowerShell 5.1 turns a program's stderr output into
# errors, which 'Stop' would make fatal, so the exit code decides instead.)
function Run([string]$what, [scriptblock]$command) {
    $ErrorActionPreference = 'Continue'
    & $command 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE) { throw "$what failed" }
}

$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $root

$envFile = Join-Path $root 'packaging\oauth-client.env'
if (Test-Path $envFile) {
    foreach ($line in Get-Content $envFile) {
        if ($line -match '^\s*(CLOUDDROP_CLIENT_(ID|SECRET))\s*=\s*(.+?)\s*$') {
            Set-Item "env:$($Matches[1])" $Matches[3]
        }
    }
    Write-Host 'Building with the built-in Google client.'
} else {
    Write-Host 'No packaging\oauth-client.env: building without a built-in Google client.'
}

Run 'cargo build' { cargo build --release -p clouddrop -p clouddrop-agent }
Push-Location winapp
try { Run 'flutter build' { flutter build windows --release } } finally { Pop-Location }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$out = Join-Path $root "dist\CloudDrop-$version-windows-$Arch"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force (Join-Path $out 'CloudDrop') | Out-Null
Copy-Item -Recurse "winapp\build\windows\$Arch\runner\Release\*" (Join-Path $out 'CloudDrop')
Copy-Item target\release\clouddrop.exe, target\release\clouddrop-agent.exe (Join-Path $out 'CloudDrop')
Copy-Item packaging\windows\install.cmd, packaging\windows\install.ps1, packaging\windows\uninstall.ps1, packaging\windows\README.txt $out
Set-Content (Join-Path $out 'VERSION') $version -NoNewline
Write-Host "Package: $out"
