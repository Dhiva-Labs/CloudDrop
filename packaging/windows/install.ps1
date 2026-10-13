# Installs CloudDrop for the current user: copies it to
# %LOCALAPPDATA%\Programs\CloudDrop, adds a Start menu shortcut and an entry in
# Settings > Apps, then opens the app. Running it again upgrades in place.
$ErrorActionPreference = 'Stop'
$src = Join-Path $PSScriptRoot 'CloudDrop'
$dest = Join-Path $env:LOCALAPPDATA 'Programs\CloudDrop'
$version = (Get-Content (Join-Path $PSScriptRoot 'VERSION') -ErrorAction SilentlyContinue) -as [string]
if (-not $version) { $version = '0.1.0' }

# Upgrade: the running agent and app hold their files open.
$wasRunning = $false
$cli = Join-Path $dest 'clouddrop.exe'
if (Test-Path $cli) {
    $wasRunning = [bool](Get-Process clouddrop-agent -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$dest\*" })
    & $cli service stop 2>$null | Out-Null
    for ($i = 0; $i -lt 20 -and (Get-Process clouddrop-agent -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -like "$dest\*" }); $i++) { Start-Sleep -Milliseconds 500 }
    Get-Process clouddrop_app, clouddrop-agent -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$dest\*" } | Stop-Process -Force
}

New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item -Recurse -Force (Join-Path $src '*') $dest
Copy-Item -Force (Join-Path $PSScriptRoot 'uninstall.ps1') $dest

$app = Join-Path $dest 'clouddrop_app.exe'
$icon = "$(Join-Path $dest 'clouddrop-agent.exe'),0"
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'CloudDrop.lnk'))
$link.TargetPath = $app
$link.WorkingDirectory = $dest
$link.IconLocation = $icon
$link.Description = 'Upload a folder to Google Drive'
$link.Save()

$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\CloudDrop'
New-Item -Force $key | Out-Null
$uninstall = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$dest\uninstall.ps1`""
@{
    DisplayName = 'CloudDrop'; DisplayVersion = $version; Publisher = 'Dhiva-Labs'
    DisplayIcon = $icon; InstallLocation = $dest; UninstallString = $uninstall
    URLInfoAbout = 'https://github.com/Dhiva-Labs/CloudDrop'
}.GetEnumerator() | ForEach-Object { Set-ItemProperty $key $_.Key $_.Value }
Set-ItemProperty $key NoModify 1 -Type DWord
Set-ItemProperty $key NoRepair 1 -Type DWord

if ($wasRunning) { & $cli service start | Out-Null }
Start-Process $app
Write-Host "CloudDrop $version is installed. Find it in the Start menu."
