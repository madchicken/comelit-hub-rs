$ServiceName = "ComelitHubHAP"
$LogDir = "C:\ProgramData\Comelit\logs"
$ConfigDir = "C:\ProgramData\Comelit\config"

# Stop and remove the service
Write-Host "→ Stopping service"
nssm stop $ServiceName 2>$null
Write-Host "→ Removing service"
nssm remove $ServiceName confirm 2>$null

# Remove program files
Write-Host "→ Removing program files"
Remove-Item "C:\Program Files\Comelit" -Recurse -Force -ErrorAction SilentlyContinue

# Remove config directory
if (Test-Path $ConfigDir)
{
    Write-Host "→ Removing configuration directory"
    Remove-Item $ConfigDir -Recurse -Force -ErrorAction SilentlyContinue
}

# Ask before removing logs
if (Test-Path $LogDir)
{
    $response = Read-Host "Remove log directory $LogDir? [y/N]"
    if ($response -eq 'y' -or $response -eq 'Y')
    {
        Remove-Item $LogDir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "✔ Log directory removed"
    } else
    {
        Write-Host "→ Log directory preserved at $LogDir"
    }
}

# Clean up empty parent directory
$parentDir = "C:\ProgramData\Comelit"
if ((Test-Path $parentDir) -and ((Get-ChildItem $parentDir -Force | Measure-Object).Count -eq 0))
{
    Remove-Item $parentDir -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "✔ Uninstalled"
