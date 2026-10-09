#requires -RunAsAdministrator
<#
.SYNOPSIS
    Once, as administrator, on the PC: what the Moli agent cannot do without rights.

.DESCRIPTION
    1. Lets the wired network card wake the PC: from sleep (Windows: « Allow this device to wake
       the computer », magic packet only, never on mere traffic) and, when the card offers it,
       from shutdown. Moli's « Allumer » then works on a sleeping PC, whose Windows session (and
       so Claude and Codex, reachable from the phone) is still open. The card resets for a few
       seconds while its settings change: the network drops briefly.
    2. Installs the processor temperature sensors (install-sensors.ps1: LibreHardwareMonitor).

    Running it again is safe.

.EXAMPLE
    # From a terminal opened « as administrator »:
    pwsh -ExecutionPolicy Bypass -File .\agent\windows\setup-admin.ps1
#>
param([switch]$NoSensors)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

# --- 1. The wired card wakes the PC -----------------------------------------------------------
$nic = Get-NetAdapter -Physical | Where-Object { $_.Status -eq 'Up' -and $_.MediaType -eq '802.3' } |
    Sort-Object -Property LinkSpeed -Descending | Select-Object -First 1
if (-not $nic) { throw 'Aucune carte réseau filaire connectée : rien à régler.' }
Write-Host "Carte filaire : $($nic.InterfaceDescription) ($($nic.MacAddress))."

# The card's own switches, by their registry keywords (the same whatever Windows' language).
$keywords = @{ '*WakeOnMagicPacket' = '1'; '*WakeOnPattern' = '0'; 'S5WakeOnLan' = '1'; 'WakeOnLinkChange' = '0' }
foreach ($property in Get-NetAdapterAdvancedProperty -Name $nic.Name -AllProperties -ErrorAction SilentlyContinue) {
    $wanted = $keywords[$property.RegistryKeyword]
    if ($null -eq $wanted -or "$($property.RegistryValue)" -eq $wanted) { continue }
    Set-NetAdapterAdvancedProperty -Name $nic.Name -RegistryKeyword $property.RegistryKeyword -RegistryValue $wanted -NoRestart
    Write-Host "  $($property.DisplayName) : réglé."
}
# Windows' side: the card may wake the PC, on a magic packet only.
Set-NetAdapterPowerManagement -Name $nic.Name -WakeOnMagicPacket Enabled -WakeOnPattern Disabled -NoRestart
& powercfg.exe /deviceenablewake $nic.InterfaceDescription
if ($LASTEXITCODE) { Write-Warning 'powercfg a refusé : régler « Autoriser ce périphérique à sortir l''ordinateur du mode veille » dans le Gestionnaire de périphériques.' }
Restart-NetAdapter -Name $nic.Name
Write-Host 'Réveil par la carte filaire : autorisé (veille, et arrêt si la carte le permet).'
Write-Host 'Appareils qui peuvent réveiller le PC :'
& powercfg.exe /devicequery wake_armed | ForEach-Object { Write-Host "  $_" }

# --- 2. Processor temperature -------------------------------------------------------------------
if (-not $NoSensors) {
    & (Join-Path $PSScriptRoot 'install-sensors.ps1')
}

Write-Host ''
Write-Host 'Fait. Pour retrouver Claude et Codex sur le téléphone : mettre le PC en veille (pas l''éteindre),'
Write-Host 'Moli le réveille avec « Allumer et ouvrir Claude et Codex ».'
