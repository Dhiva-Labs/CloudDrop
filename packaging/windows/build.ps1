# Builds the Windows package from a source checkout: the engine (clouddrop.exe,
# clouddrop-agent.exe), the app, and the install scripts, into
# dist\CloudDrop-<version>-windows-x64. If packaging\oauth-client.env exists (not in
# git), its Google client is built in, so sign-in works without a key file.
# Needs Rust (MSVC), Flutter and the Visual Studio C++ build tools.
$ErrorActionPreference = 'Stop'
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

cargo build --release -p clouddrop -p clouddrop-agent
if ($LASTEXITCODE) { throw 'cargo build failed' }

Push-Location winapp
flutter build windows --release
$code = $LASTEXITCODE
Pop-Location
if ($code) { throw 'flutter build failed' }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$out = Join-Path $root "dist\CloudDrop-$version-windows-x64"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force (Join-Path $out 'CloudDrop') | Out-Null
Copy-Item -Recurse winapp\build\windows\x64\runner\Release\* (Join-Path $out 'CloudDrop')
Copy-Item target\release\clouddrop.exe, target\release\clouddrop-agent.exe (Join-Path $out 'CloudDrop')
Copy-Item packaging\windows\install.cmd, packaging\windows\install.ps1, packaging\windows\uninstall.ps1, packaging\windows\README.txt $out
Set-Content (Join-Path $out 'VERSION') $version -NoNewline
Write-Host "Package: $out"
