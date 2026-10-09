#requires -Version 7.2
<#
.SYNOPSIS
    Installs or updates the Moli agent for the current Windows user. No administrator rights.

.DESCRIPTION
    - copies moli-agent.ps1 to %LOCALAPPDATA%\MoliAgent (the previous copy is kept as .bak-YYYYMMDD);
    - creates config.json when it is missing: Moli's address, the machine id, and the remote mode
      at the start of the session (an existing config.json is never modified);
    - reads the agent token from standard input only (never typed, never displayed) and stores it
      encrypted with DPAPI for this Windows account in token.dpapi; with nothing on standard input
      the saved token is kept;
    - registers the scheduled task "Moli Agent": at this user's logon, without password nor
      elevation, through "conhost --headless" so that no window ever shows, checked again every
      5 minutes so that a stopped agent comes back;
    - restarts the agent so that the new script is used.

    Running it again is safe: it updates the script and the task, and leaves config.json alone.

.PARAMETER Moli
    Moli's address written into a new config.json. Required the first time only.

.PARAMETER Id
    Machine id written into a new config.json (the {id} of /api/machines/{id}/report):
    by default this computer's name, in lower case.

.PARAMETER NoStart
    Registers the task without starting the agent.

.EXAMPLE
    <command printing the token> | pwsh -NoProfile -File .\agent\windows\install.ps1 -Moli http://192.168.1.x:8790

.EXAMPLE
    pwsh -NoProfile -File .\agent\windows\install.ps1
    Updates the script and the task, keeps the saved token and config.json.
#>
param(
    [string]$Moli = '',
    [string]$Id = $env:COMPUTERNAME.ToLowerInvariant(),
    [switch]$NoStart
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$TaskName = 'Moli Agent'
$Source = Join-Path $PSScriptRoot 'moli-agent.ps1'
$Dir = Join-Path $env:LOCALAPPDATA 'MoliAgent'
$Agent = Join-Path $Dir 'moli-agent.ps1'
$ConfigPath = Join-Path $Dir 'config.json'
$TokenPath = Join-Path $Dir 'token.dpapi'
$LogPath = Join-Path $Dir 'agent.log'
$Utf8 = [Text.UTF8Encoding]::new($false)

function Get-AgentProcess {
    Get-CimInstance -ClassName Win32_Process -Filter "Name = 'pwsh.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($Agent, [StringComparison]::OrdinalIgnoreCase) }
}

# --- Token: standard input only -------------------------------------------------------------
$token = $null
if ($MyInvocation.ExpectingInput) { $token = @($input) -join "`n" }
elseif ([Console]::IsInputRedirected) { $token = [Console]::In.ReadToEnd() }
if ($null -ne $token) {
    $token = $token.Trim()
    if (-not $token) { $token = $null }
    elseif ($token -notmatch '^[\x21-\x7E]{8,4096}$') {
        throw 'Jeton refusé : une seule ligne de 8 à 4 096 caractères imprimables, sans espace.'
    }
}

if (-not [IO.File]::Exists($Source)) { throw "moli-agent.ps1 introuvable à côté de install.ps1 ($Source)." }
$null = [IO.Directory]::CreateDirectory($Dir)

# --- Script ----------------------------------------------------------------------------------
if ([IO.File]::Exists($Agent)) {
    Copy-Item -LiteralPath $Agent -Destination "$Agent.bak-$(Get-Date -Format yyyyMMdd)" -Force
}
Copy-Item -LiteralPath $Source -Destination $Agent -Force
Write-Host "Script de l'agent copié dans $Dir."

# --- Configuration (created once, never overwritten) ---------------------------------------
if ([IO.File]::Exists($ConfigPath)) {
    Write-Host "config.json existant conservé ($ConfigPath)."
} else {
    if (-not $Moli) { throw "Première installation : indiquer l'adresse de Moli, par exemple -Moli http://192.168.1.x:8790." }
    $config = [ordered]@{ moli = $Moli.TrimEnd('/'); id = $Id; remote_at_start = $true }
    [IO.File]::WriteAllText($ConfigPath, (ConvertTo-Json -InputObject $config -Depth 4), $Utf8)
    Write-Host ("config.json créé : Moli {0}, machine « {1} », Claude et Codex ouverts à chaque ouverture de session." -f $config.moli, $Id)
}

# --- Token (DPAPI, current user) ------------------------------------------------------------
if ($null -ne $token) {
    $encrypted = ConvertTo-SecureString -String $token -AsPlainText -Force | ConvertFrom-SecureString
    [IO.File]::WriteAllText("$TokenPath.tmp", $encrypted, [Text.Encoding]::ASCII)
    [IO.File]::Move("$TokenPath.tmp", $TokenPath, $true)
    $token = $null
    $encrypted = $null
    Write-Host 'Jeton enregistré, chiffré pour ce compte Windows (DPAPI).'
} elseif ([IO.File]::Exists($TokenPath)) {
    Write-Host 'Aucun jeton sur l''entrée standard : jeton déjà enregistré conservé.'
} else {
    Write-Warning 'Aucun jeton enregistré : l''agent attendra. Le fournir ainsi : <commande qui affiche le jeton> | pwsh -NoProfile -File install.ps1'
}

# --- Scheduled task ----------------------------------------------------------------------------
$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$pwsh = Join-Path $PSHOME 'pwsh.exe'
$conhost = Join-Path $env:WINDIR 'System32\conhost.exe'
$arguments = '--headless "{0}" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{1}"' -f $pwsh, $Agent
$action = New-ScheduledTaskAction -Execute $conhost -Argument $arguments -WorkingDirectory $Dir
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
# Watchdog: the task is triggered again every 5 minutes; ignored while the agent runs.
$trigger.Repetition = (New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval (New-TimeSpan -Minutes 5)).Repetition
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew
$settings.Priority = 6   # normal priority (the Task Scheduler default, 7, is below normal)
$null = Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings `
    -Description 'Agent Moli OS : relevés du PC pour le tableau de bord, veille et extinction, Claude et Codex ouverts pour le téléphone.' -Force
Write-Host "Tâche planifiée « $TaskName » enregistrée (ouverture de session de $user, sans élévation)."

if ($NoStart) { return }

# --- Restart ---------------------------------------------------------------------------------
$task = Get-ScheduledTask -TaskName $TaskName
if ($task.State -eq 'Running') { Stop-ScheduledTask -TaskName $TaskName }
# An agent still alive would keep the single-instance lock and the new one would step aside.
foreach ($process in @(Get-AgentProcess)) { Stop-Process -Id $process.ProcessId -Force -ErrorAction SilentlyContinue }
$deadline = (Get-Date).AddSeconds(10)
while ((Get-AgentProcess) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }

Start-ScheduledTask -TaskName $TaskName
$deadline = (Get-Date).AddSeconds(15)
$started = $null
while (-not $started -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500; $started = Get-AgentProcess | Select-Object -First 1 }
if (-not $started) { throw 'L''agent ne démarre pas : voir le planificateur de tâches (« Moli Agent ») et agent.log.' }
Write-Host "Agent démarré (pid $($started.ProcessId)). Journal : $LogPath"
Start-Sleep -Seconds 5
if ([IO.File]::Exists($LogPath)) { Get-Content -LiteralPath $LogPath -Tail 6 | ForEach-Object { Write-Host "  $_" } }
