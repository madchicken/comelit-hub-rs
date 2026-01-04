$ServiceName = "ComelitHubHAP"
$ExePath = "C:\Program Files\Comelit\comelit-hub-hap.exe"

$User = "comelit"
$Password = [System.Web.Security.Membership]::GeneratePassword(32,6)

if (-not (Get-LocalUser -Name $User -ErrorAction SilentlyContinue)) {
    Write-Host "→ Creazione utente $User"
    $SecurePassword = ConvertTo-SecureString $Password -AsPlainText -Force
    New-LocalUser `
        -Name $User `
        -Password $SecurePassword `
        -NoPasswordExpiration `
        -AccountNeverExpires `
        -UserMayNotChangePassword
}

if (-not (Get-Command nssm -ErrorAction SilentlyContinue)) {
    Write-Error "NSSM non installato"
    exit 1
}

New-Item -ItemType Directory -Force -Path "C:\Program Files\Comelit"
Copy-Item ".\comelit-hub-hap.exe" $ExePath -Force

nssm install ComelitHubHAP $ExePath
nssm set ComelitHubHAP ObjectName ".\comelit" $Password
nssm set ComelitHubHAP AppEnvironmentExtra "RUST_LOG=comelit_hub_hap=info"
nssm start ComelitHubHAP

Write-Host "✔ Servizio Windows installato"
