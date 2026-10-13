# Removes CloudDrop from this PC. Nothing is deleted from Google Drive. The
# CloudDrop folder becomes an ordinary folder: files with a local copy stay,
# files that were only in Drive are removed locally (they stay in Drive).
# Settings and sign-in are kept in %APPDATA%\clouddrop and %LOCALAPPDATA%\clouddrop.
$dir = $PSScriptRoot
$mine = { $_.Path -like "$dir\*" }

& (Join-Path $dir 'clouddrop.exe') service disable 2>$null | Out-Null
Get-Process clouddrop_app -ErrorAction SilentlyContinue | Where-Object $mine | Stop-Process -Force
# Stops the agent, then takes the folder out of File Explorer's cloud folders.
$p = Start-Process (Join-Path $dir 'clouddrop-agent.exe') -ArgumentList '--unregister' -Wait -PassThru
if ($p.ExitCode -ne 0) {
    Write-Warning "Couldn't fully remove the folder from File Explorer; see $env:LOCALAPPDATA\clouddrop\agent.log"
}
Get-Process clouddrop-agent -ErrorAction SilentlyContinue | Where-Object $mine | Stop-Process -Force

Remove-Item -Force (Join-Path ([Environment]::GetFolderPath('Programs')) 'CloudDrop.lnk') -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\CloudDrop' -ErrorAction SilentlyContinue
# This script runs from the folder being removed, so remove it once the script ends.
Start-Process cmd.exe -WindowStyle Hidden -ArgumentList '/c', "timeout /t 3 >nul & rmdir /s /q `"$dir`""
Write-Host 'CloudDrop was removed.'
