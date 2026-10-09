#requires -RunAsAdministrator
<#
.SYNOPSIS
    Gives the Moli agent the processor temperature: installs LibreHardwareMonitor and its web
    server. To run once, as administrator.

.DESCRIPTION
    Windows only lets a program holding a kernel driver read the processor's temperature sensor,
    so this step is separate from the agent (which never needs administrator rights). It:
      1. installs LibreHardwareMonitor with winget, machine-wide (Program Files: the task below runs
         it elevated, so its exe must not be writable without administrator rights); winget also
         installs PawnIO, the driver LibreHardwareMonitor depends on;
      2. turns its web server on, port 8085 (the agent reads http://127.0.0.1:8085/data.json),
         starts it minimised in the notification area, and closing its window keeps it running;
      3. creates the scheduled task "LibreHardwareMonitor" (at logon, highest privileges) and
         starts it;
      4. checks that the processor temperature can be read.
    Works from Windows PowerShell 5.1 as well as PowerShell 7. Running it again is safe.

.PARAMETER User
    Account whose logon starts LibreHardwareMonitor (default: the current account).

.EXAMPLE
    # From a terminal opened "as administrator":
    powershell -ExecutionPolicy Bypass -File .\agent\windows\install-sensors.ps1
#>
param([string]$User = [Security.Principal.WindowsIdentity]::GetCurrent().Name)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$PackageId = 'LibreHardwareMonitor.LibreHardwareMonitor'
$TaskName = 'LibreHardwareMonitor'
$Port = 8085   # fixed: the agent reads this port

function Find-Lhm([string]$Root) {
    if (-not (Test-Path -LiteralPath $Root)) { return $null }
    Get-ChildItem -LiteralPath $Root -Directory -Filter "$PackageId*" -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ChildItem -LiteralPath $_.FullName -Filter 'LibreHardwareMonitor.exe' -File -Recurse -ErrorAction SilentlyContinue } |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
}

# --- 1. Install ------------------------------------------------------------------------------
$machineRoot = Join-Path $env:ProgramFiles 'WinGet\Packages'
$exe = Find-Lhm $machineRoot
if ($exe) {
    Write-Host "LibreHardwareMonitor déjà installé : $($exe.FullName)"
} else {
    if (-not (Get-Command winget.exe -ErrorAction SilentlyContinue)) { throw 'winget introuvable (App Installer) : installer LibreHardwareMonitor à la main.' }
    if (Find-Lhm (Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages')) {
        Write-Warning 'Une copie de LibreHardwareMonitor existe pour ce seul compte ; installation pour toute la machine (la tâche élevée ne lance qu''un exe protégé).'
    }
    Write-Host 'Installation de LibreHardwareMonitor (et du pilote PawnIO) par winget…'
    & winget.exe install --id $PackageId -e --scope machine --silent --accept-package-agreements --accept-source-agreements
    $exe = Find-Lhm $machineRoot
    if (-not $exe) { throw "winget n'a pas installé LibreHardwareMonitor (code $LASTEXITCODE)." }
    Write-Host "Installé : $($exe.FullName)"
}
$exePath = $exe.FullName

# --- 2. Configuration ------------------------------------------------------------------------
# LibreHardwareMonitor writes its settings when it exits: stop it before editing them.
Get-Process -Name LibreHardwareMonitor -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

$configPath = [IO.Path]::ChangeExtension($exePath, '.config')
if (Test-Path -LiteralPath $configPath) {
    Copy-Item -LiteralPath $configPath -Destination "$configPath.bak-$(Get-Date -Format yyyyMMdd)" -Force
    $xml = New-Object Xml.XmlDocument
    $xml.Load($configPath)
} else {
    $xml = New-Object Xml.XmlDocument
    $xml.LoadXml('<?xml version="1.0" encoding="utf-8"?><configuration><appSettings /></configuration>')
}
$settings = $xml.SelectSingleNode('/configuration/appSettings')
if ($null -eq $settings) {
    $root = $xml.SelectSingleNode('/configuration')
    if ($null -eq $root) { $root = $xml.AppendChild($xml.CreateElement('configuration')) }
    $settings = $root.AppendChild($xml.CreateElement('appSettings'))
}
$wanted = [ordered]@{
    runWebServerMenuItem = 'true'    # web server on
    listenerPort         = "$Port"
    startMinMenuItem     = 'true'    # start minimised
    minTrayMenuItem      = 'true'    # minimise to the notification area
    minCloseMenuItem     = 'true'    # closing the window keeps it running
}
foreach ($key in $wanted.Keys) {
    $node = $settings.SelectSingleNode("add[@key='$key']")
    if ($null -eq $node) {
        $node = $xml.CreateElement('add')
        $node.SetAttribute('key', $key)
        $null = $settings.AppendChild($node)
    }
    $node.SetAttribute('value', $wanted[$key])
}
$xml.Save($configPath)
Write-Host "Serveur web activé sur le port $Port ($configPath)."

# --- 3. Scheduled task -----------------------------------------------------------------------
$action = New-ScheduledTaskAction -Execute $exePath -WorkingDirectory (Split-Path -Parent $exePath)
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $User
$principal = New-ScheduledTaskPrincipal -UserId $User -LogonType Interactive -RunLevel Highest
$taskSettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
$null = Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Principal $principal -Settings $taskSettings `
    -Description 'Capteurs du PC (température du processeur) pour l''agent Moli, http://127.0.0.1:8085/data.json' -Force
Write-Host "Tâche « $TaskName » enregistrée (ouverture de session de $User, privilèges les plus élevés)."
Start-ScheduledTask -TaskName $TaskName

# --- 4. Check --------------------------------------------------------------------------------
$deadline = (Get-Date).AddSeconds(40)
$temperature = $null
while (-not $temperature -and (Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 2
    try {
        $json = (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/data.json" -UseBasicParsing -TimeoutSec 3).Content
        $match = [regex]::Match($json, '"Text"\s*:\s*"[^"]*(?:Tctl/Tdie|CPU Package)[^"]*"[^{}]*?"Value"\s*:\s*"([^"]*)"')
        if ($match.Success) { $temperature = $match.Groups[1].Value }
    } catch { }
}
if ($temperature) {
    Write-Host "Température du processeur lue : $temperature. L'agent Moli l'enverra dans ses relevés."
} else {
    Write-Warning "Pas de température lue sur http://127.0.0.1:$Port/data.json au bout de 40 s : ouvrir LibreHardwareMonitor (zone de notification) et vérifier Options > Remote Web Server."
}
