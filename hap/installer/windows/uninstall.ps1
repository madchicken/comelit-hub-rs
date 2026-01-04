$ServiceName = "ComelitHubHAP"

nssm stop $ServiceName
nssm remove $ServiceName confirm
Remove-Item "C:\Program Files\Comelit" -Recurse -Force
