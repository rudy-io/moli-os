#requires -Version 7.2
<#
.SYNOPSIS
    Removes the Moli agent: the "Moli Agent" scheduled task, the running agent (and the AI runs it
    started), and the %LOCALAPPDATA%\MoliAgent folder with the token, the runs and the log.

.DESCRIPTION
    Asks for confirmation (pass -Confirm:$false to skip it). LibreHardwareMonitor, installed by
    install-sensors.ps1, is left in place.

.PARAMETER KeepData
    Keeps %LOCALAPPDATA%\MoliAgent (configuration, token, runs, log).

.EXAMPLE
    pwsh -NoProfile -File .\agent\windows\uninstall.ps1
#>
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param([switch]$KeepData)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$TaskName = 'Moli Agent'
$Dir = Join-Path $env:LOCALAPPDATA 'MoliAgent'
$Agent = Join-Path $Dir 'moli-agent.ps1'

$what = if ($KeepData) { "la tâche « $TaskName » et l'agent en cours (dossier $Dir conservé)" }
else { "la tâche « $TaskName », l'agent en cours et le dossier $Dir (jeton compris)" }
if (-not $PSCmdlet.ShouldProcess($what, 'Désinstaller l''agent Moli')) { return }

if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Host "Tâche « $TaskName » supprimée."
}

$agents = @(Get-CimInstance -ClassName Win32_Process -Filter "Name = 'pwsh.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($Agent, [StringComparison]::OrdinalIgnoreCase) })
foreach ($process in $agents) { Stop-Process -Id $process.ProcessId -Force -ErrorAction SilentlyContinue }
if ($agents.Count) { Write-Host "Agent arrêté ($($agents.Count) processus)." }

if (-not $KeepData -and (Test-Path -LiteralPath $Dir)) {
    Start-Sleep -Milliseconds 500   # let the stopped agent release its files
    Remove-Item -LiteralPath $Dir -Recurse -Force
    Write-Host "Dossier $Dir supprimé."
}
