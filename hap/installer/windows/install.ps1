$ServiceName = "ComelitHubHAP"
$ExePath = "C:\Program Files\Comelit\comelit-hub-hap.exe"
$LogDir = "C:\ProgramData\Comelit\logs"
$ConfigDir = "C:\ProgramData\Comelit\config"

$User = "comelit"
$Password = [System.Web.Security.Membership]::GeneratePassword(32,6)

if (-not (Get-LocalUser -Name $User -ErrorAction SilentlyContinue))
{
    Write-Host "→ Creating user $User"
    $SecurePassword = ConvertTo-SecureString $Password -AsPlainText -Force
    New-LocalUser `
        -Name $User `
        -Password $SecurePassword `
        -NoPasswordExpiration `
        -AccountNeverExpires `
        -UserMayNotChangePassword
}

if (-not (Get-Command nssm -ErrorAction SilentlyContinue))
{
    Write-Error "NSSM is not installed. Please install NSSM first: https://nssm.cc/"
    exit 1
}

# Create directories
Write-Host "→ Creating directories"
New-Item -ItemType Directory -Force -Path "C:\Program Files\Comelit" | Out-Null
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null

# Copy executable
Write-Host "→ Installing binary"
Copy-Item ".\comelit-hub-hap.exe" $ExePath -Force

# Set directory permissions for the service user
$Acl = Get-Acl $LogDir
$AccessRule = New-Object System.Security.AccessControl.FileSystemAccessRule($User, "Modify", "ContainerInherit,ObjectInherit", "None", "Allow")
$Acl.SetAccessRule($AccessRule)
Set-Acl $LogDir $Acl

$Acl = Get-Acl $ConfigDir
$AccessRule = New-Object System.Security.AccessControl.FileSystemAccessRule($User, "Read", "ContainerInherit,ObjectInherit", "None", "Allow")
$Acl.SetAccessRule($AccessRule)
Set-Acl $ConfigDir $Acl

# Install and configure NSSM service
Write-Host "→ Installing service"
nssm install $ServiceName $ExePath
nssm set $ServiceName AppParameters "--log-dir `"$LogDir`" --log-prefix comelit-hub --log-rotation daily --max-log-files 7"
nssm set $ServiceName ObjectName ".\$User" $Password
nssm set $ServiceName AppEnvironmentExtra "RUST_LOG=comelit_hub_hap=info"
nssm set $ServiceName AppDirectory "C:\ProgramData\Comelit"
nssm set $ServiceName AppStdout "$LogDir\service-stdout.log"
nssm set $ServiceName AppStderr "$LogDir\service-stderr.log"
nssm set $ServiceName AppRotateFiles 1
nssm set $ServiceName AppRotateBytes 10485760

# Start the service
Write-Host "→ Starting service"
nssm start $ServiceName

Write-Host ""
Write-Host "✔ Windows service installed"
Write-Host ""
Write-Host "Note: Log rotation is handled automatically by the application."
Write-Host "      Logs are stored in: $LogDir"
Write-Host "      Configuration: $ConfigDir"
Write-Host ""
Write-Host "To manage the service:"
Write-Host "  nssm status $ServiceName"
Write-Host "  nssm start $ServiceName"
Write-Host "  nssm stop $ServiceName"
Write-Host "  nssm restart $ServiceName"
