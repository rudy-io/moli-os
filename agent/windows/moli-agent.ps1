#requires -Version 7.2
<#
.SYNOPSIS
    Moli OS agent for Windows: reports this PC's state to Moli and runs Moli's orders.

.DESCRIPTION
    Every few seconds (the interval Moli asks for, 5 s by default) the agent:
      1. measures the machine without administrator rights (processor, memory, graphics card,
         disks, network, busiest processes, processor temperature when LibreHardwareMonitor runs);
      2. POSTs the report to {moli}/api/machines/{id}/report with the agent token;
      3. runs the orders Moli answers with: shutdown, restart, cancel_shutdown, sleep, remote.

    « remote » (and, by default, the start of the Windows session): the Claude and Codex desktop
    apps are opened if they are not running, so the phone reaches them (Claude's sessions connect
    to Remote Control, Codex's app runs its exec-server towards Codex Cloud). The need: wake
    the PC from the phone and find one's agents there, nothing more.

    Files, next to the configuration (%LOCALAPPDATA%\MoliAgent by default, never synchronised):
      config.json   {"moli": "http://192.168.1.x:8790", "id": "pc-bureau", "remote_at_start": true}
      token.dpapi   agent token, DPAPI-encrypted for the current Windows account (install.ps1 writes it)
      state.json    order ids already handled: no order ever runs twice, even across a reboot
      agent.log     log (1 MB, one rotation); never contains the token

.PARAMETER ConfigPath
    Configuration file; its folder also holds the token, the state and the log.

.PARAMETER Once
    A single cycle (measure, report, orders), then exit.

.PARAMETER DryRun
    Never contacts Moli. With -Once, prints the report as JSON on stdout; power orders are only
    logged, never executed. Writes nothing unless -ConfigPath is given.

.PARAMETER Order
    Orders (JSON object or array) handled as if Moli had sent them. For tests, with -DryRun.

.EXAMPLE
    pwsh -NoProfile -File moli-agent.ps1 -Once -DryRun
#>
[CmdletBinding()]
param(
    [string]$ConfigPath = (Join-Path $env:LOCALAPPDATA 'MoliAgent\config.json'),
    [switch]$Once,
    [switch]$DryRun,
    [string[]]$Order
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if ($Order -and -not $DryRun) { throw '-Order ne sert qu''aux essais : ajouter -DryRun.' }

# ---------------------------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------------------------
$ReportVersion  = 1
$ProcessEvery   = 10        # seconds between two measurements of the processes
$AppsEvery      = 30        # seconds between two looks at the Claude and Codex apps
$RemoteDelay    = 20        # after the session starts, before opening the apps (Windows settles)
$DefaultEvery   = 5
$MaxBackoff     = 60
$AuthRetry      = 60
$KeptOrderIds   = 500
$StatsEvery     = if ($DryRun) { 60 } else { 3600 }
$LhmUrl         = 'http://127.0.0.1:8085/data.json'
$VirtualNic     = 'Virtual|Hyper-V|vEthernet|VPN|TAP-|WireGuard|Wintun|Loopback|Bluetooth|WAN Miniport|Npcap|VMware|VirtualBox'

$ConfigPath = [IO.Path]::GetFullPath($ConfigPath)
$DataDir    = Split-Path -Parent $ConfigPath
$Persist    = (-not $DryRun) -or $PSBoundParameters.ContainsKey('ConfigPath')
$Echo       = $DryRun -or $Once
$LogPath    = Join-Path $DataDir 'agent.log'
$StatePath  = Join-Path $DataDir 'state.json'
$TokenPath  = Join-Path $DataDir 'token.dpapi'
$Utf8       = [Text.UTF8Encoding]::new($false)
$Clock      = [Diagnostics.Stopwatch]::StartNew()
$Self       = [Diagnostics.Process]::GetCurrentProcess()

# Native helpers, compiled once at start (~0.3 s):
#  - KillJob: a Windows job object; closing or terminating it kills every process it holds, so a
#    run (and whatever it spawned) never outlives a cancellation or the agent itself;
#  - ProcessSampler: every process's processor time and memory in one system call, protected
#    processes included (Defender, System), for ~1 ms instead of ~60 ms through Process objects.
$NativeSource = @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Linq;
using System.Runtime.InteropServices;

namespace MoliAgent
{
    public sealed class ProcessRow
    {
        public string Name;
        public double Cpu;
        public long Memory;
        public int Count;
        internal long Ticks;
    }

    public sealed class ProcessSampler
    {
        [DllImport("ntdll.dll")]
        private static extern int NtQuerySystemInformation(int infoClass, IntPtr buffer, int length, out int returnLength);

        private const int SystemProcessInformation = 5;
        private const int InfoLengthMismatch = unchecked((int)0xC0000004);

        // SYSTEM_PROCESS_INFORMATION offsets, 64-bit.
        private const int CreateTimeOffset = 32, UserTimeOffset = 40, KernelTimeOffset = 48;
        private const int NameLengthOffset = 56, NameBufferOffset = 64, ProcessIdOffset = 80, WorkingSetOffset = 144;

        private Dictionary<(long, long), long> previous;
        private long previousAt;
        private int bufferSize = 2 << 20;

        // Processor use since the previous call, summed per program name, in % of all logical
        // cores. Returns null on the first call (no interval yet).
        public ProcessRow[] Sample(int top)
        {
            if (IntPtr.Size != 8) throw new PlatformNotSupportedException("64-bit only");
            IntPtr buffer = IntPtr.Zero;
            try
            {
                while (true)
                {
                    buffer = Marshal.AllocHGlobal(bufferSize);
                    int needed;
                    int status = NtQuerySystemInformation(SystemProcessInformation, buffer, bufferSize, out needed);
                    if (status == InfoLengthMismatch)
                    {
                        Marshal.FreeHGlobal(buffer);
                        buffer = IntPtr.Zero;
                        bufferSize = Math.Max(bufferSize * 2, needed + (1 << 16));
                        continue;
                    }
                    if (status < 0) throw new InvalidOperationException("NtQuerySystemInformation 0x" + status.ToString("X8"));
                    break;
                }
                long now = Stopwatch.GetTimestamp();
                var current = new Dictionary<(long, long), long>(previous == null ? 1024 : previous.Count + 64);
                var byName = new Dictionary<string, ProcessRow>(StringComparer.OrdinalIgnoreCase);
                int offset = 0;
                while (true)
                {
                    IntPtr entry = IntPtr.Add(buffer, offset);
                    long pid = Marshal.ReadIntPtr(entry, ProcessIdOffset).ToInt64();
                    if (pid != 0)
                    {
                        int nameLength = (ushort)Marshal.ReadInt16(entry, NameLengthOffset);
                        IntPtr nameBuffer = Marshal.ReadIntPtr(entry, NameBufferOffset);
                        string name = nameBuffer == IntPtr.Zero || nameLength == 0 ? "System" : Marshal.PtrToStringUni(nameBuffer, nameLength / 2);
                        if (name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)) name = name.Substring(0, name.Length - 4);
                        var key = (pid, Marshal.ReadInt64(entry, CreateTimeOffset));
                        long ticks = Marshal.ReadInt64(entry, UserTimeOffset) + Marshal.ReadInt64(entry, KernelTimeOffset);
                        long delta = 0, before;
                        if (previous != null) delta = previous.TryGetValue(key, out before) ? Math.Max(0, ticks - before) : ticks;
                        current[key] = ticks;
                        ProcessRow row;
                        if (!byName.TryGetValue(name, out row)) { row = new ProcessRow { Name = name }; byName[name] = row; }
                        row.Ticks += delta;
                        row.Memory += Marshal.ReadIntPtr(entry, WorkingSetOffset).ToInt64();
                        row.Count++;
                    }
                    int next = Marshal.ReadInt32(entry, 0);
                    if (next == 0) break;
                    offset += next;
                }
                bool first = previous == null;
                double seconds = (now - previousAt) / (double)Stopwatch.Frequency;
                previous = current;
                previousAt = now;
                if (first || seconds <= 0) return null;
                double capacity = seconds * TimeSpan.TicksPerSecond * Environment.ProcessorCount;
                var rows = byName.Values.OrderByDescending(r => r.Ticks).ThenByDescending(r => r.Memory).Take(top).ToArray();
                foreach (var row in rows) row.Cpu = Math.Round(row.Ticks / capacity * 100, 1);
                return rows;
            }
            finally
            {
                if (buffer != IntPtr.Zero) Marshal.FreeHGlobal(buffer);
            }
        }
    }

    public sealed class KillJob : IDisposable
    {
        [StructLayout(LayoutKind.Sequential)]
        private struct BasicLimits
        {
            public long PerProcessUserTimeLimit;
            public long PerJobUserTimeLimit;
            public uint LimitFlags;
            public UIntPtr MinimumWorkingSetSize;
            public UIntPtr MaximumWorkingSetSize;
            public uint ActiveProcessLimit;
            public UIntPtr Affinity;
            public uint PriorityClass;
            public uint SchedulingClass;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct IoCounters
        {
            public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount;
            public ulong ReadTransferCount, WriteTransferCount, OtherTransferCount;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ExtendedLimits
        {
            public BasicLimits Basic;
            public IoCounters Io;
            public UIntPtr ProcessMemoryLimit;
            public UIntPtr JobMemoryLimit;
            public UIntPtr PeakProcessMemoryUsed;
            public UIntPtr PeakJobMemoryUsed;
        }

        private const int ExtendedLimitInformation = 9;
        private const uint KillOnJobClose = 0x2000;

        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        private static extern IntPtr CreateJobObjectW(IntPtr attributes, string name);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetInformationJobObject(IntPtr job, int infoClass, ref ExtendedLimits info, uint length);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool TerminateJobObject(IntPtr job, uint exitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        private IntPtr handle;

        public KillJob()
        {
            handle = CreateJobObjectW(IntPtr.Zero, null);
            if (handle == IntPtr.Zero) throw new Win32Exception();
            var info = new ExtendedLimits();
            info.Basic.LimitFlags = KillOnJobClose;
            if (!SetInformationJobObject(handle, ExtendedLimitInformation, ref info, (uint)Marshal.SizeOf(typeof(ExtendedLimits))))
            {
                var error = new Win32Exception();
                CloseHandle(handle);
                handle = IntPtr.Zero;
                throw error;
            }
        }

        public void Add(Process process)
        {
            if (!AssignProcessToJobObject(handle, process.Handle)) throw new Win32Exception();
        }

        public void Terminate()
        {
            if (handle != IntPtr.Zero) TerminateJobObject(handle, 1);
        }

        public void Dispose()
        {
            if (handle != IntPtr.Zero)
            {
                CloseHandle(handle);
                handle = IntPtr.Zero;
            }
        }
    }
}
'@

# ---------------------------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------------------------
function Get-Elapsed { $script:Clock.Elapsed.TotalSeconds }

function Get-UnixMs { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }

function Write-Log {
    param([string]$Message, [ValidateSet('INFO', 'WARN', 'ERROR')][string]$Level = 'INFO')
    # Safety net: the token must never reach the log, whatever an exception message contains.
    if ($script:Token -and $Message.Contains($script:Token)) { $Message = $Message.Replace($script:Token, '***') }
    $line = '{0} {1,-5} {2}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $Level, $Message
    if ($script:Echo) { try { [Console]::Error.WriteLine($line) } catch { } }
    if (-not $script:Persist) { return }
    try {
        if (-not [IO.Directory]::Exists($script:DataDir)) { $null = [IO.Directory]::CreateDirectory($script:DataDir) }
        $file = [IO.FileInfo]::new($script:LogPath)
        if ($file.Exists -and $file.Length -gt 1MB) { [IO.File]::Move($script:LogPath, "$($script:LogPath).1", $true) }
        [IO.File]::AppendAllText($script:LogPath, $line + [Environment]::NewLine, $script:Utf8)
    } catch { }
}

function Get-Field($Object, [string]$Name) {
    if ($null -eq $Object) { return $null }
    if ($Object -is [Collections.IDictionary]) { return $Object[$Name] }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Limit-Text([string]$Text, [int]$Max, [switch]$Tail) {
    if ([string]::IsNullOrEmpty($Text) -or $Text.Length -le $Max) { return $Text }
    if ($Tail) { return '…' + $Text.Substring($Text.Length - ($Max - 1)) }
    return $Text.Substring(0, $Max - 1) + '…'
}

function Get-FirstText {
    foreach ($text in $args) { if (-not [string]::IsNullOrWhiteSpace($text)) { return $text.Trim() } }
    return ''
}

function Save-Json([string]$Path, $Value) {
    if (-not $script:Persist) { return }
    try {
        if (-not [IO.Directory]::Exists($script:DataDir)) { $null = [IO.Directory]::CreateDirectory($script:DataDir) }
        $temporary = "$Path.tmp"
        [IO.File]::WriteAllText($temporary, (ConvertTo-Json -InputObject $Value -Depth 8), $script:Utf8)
        [IO.File]::Move($temporary, $Path, $true)
    } catch { Write-Log "Écriture impossible de $Path : $($_.Exception.Message)" 'WARN' }
}

function Read-Json([string]$Path) {
    if (-not [IO.File]::Exists($Path)) { return $null }
    try { return ([IO.File]::ReadAllText($Path) | ConvertFrom-Json -AsHashtable) }
    catch { Write-Log "Fichier illisible ignoré : $Path ($($_.Exception.Message))" 'WARN'; return $null }
}

# ---------------------------------------------------------------------------------------------
# Configuration, token, persisted state
# ---------------------------------------------------------------------------------------------
function New-DefaultConfig {
    @{ moli = $null; id = $env:COMPUTERNAME.ToLowerInvariant(); remote_at_start = $true }
}

function Read-Config {
    $config = New-DefaultConfig
    if (-not [IO.File]::Exists($script:ConfigPath)) { return $config }
    $raw = [IO.File]::ReadAllText($script:ConfigPath) | ConvertFrom-Json
    $moli = Get-Field $raw 'moli'
    if ($moli -is [string] -and $moli -match '^https?://[^\s/]+') { $config.moli = $moli.TrimEnd('/') }
    elseif ($null -ne $moli) { Write-Log 'config.json : adresse de Moli invalide (http://… attendu).' 'WARN' }
    $id = Get-Field $raw 'id'
    if ($id -is [string] -and $id -match '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$') { $config.id = $id }
    elseif ($null -ne $id) { Write-Log 'config.json : identifiant de machine invalide, nom du PC utilisé.' 'WARN' }
    $atStart = Get-Field $raw 'remote_at_start'
    if ($atStart -is [bool]) { $config.remote_at_start = $atStart }
    return $config
}

function Update-Config([switch]$Force) {
    $stamp = [datetime]::MinValue
    if ([IO.File]::Exists($script:ConfigPath)) { $stamp = [IO.File]::GetLastWriteTimeUtc($script:ConfigPath) }
    if (-not $Force -and $stamp -eq $script:ConfigStamp) { return }
    $script:ConfigStamp = $stamp
    try {
        $script:Config = Read-Config
        $address = if ($script:Config.moli) { $script:Config.moli } else { 'adresse non renseignée' }
        Write-Log ('Configuration : Moli {0}, machine « {1} », mode à distance au démarrage : {2}.' -f $address, $script:Config.id, $(if ($script:Config.remote_at_start) { 'oui' } else { 'non' }))
    } catch { Write-Log "config.json illisible, configuration précédente conservée : $($_.Exception.Message)" 'ERROR' }
}

function Update-Token {
    $script:TokenCheckedAt = Get-Elapsed
    $script:Token = $null
    if (-not [IO.File]::Exists($script:TokenPath)) { return }
    try {
        $secure = ConvertTo-SecureString -String ([IO.File]::ReadAllText($script:TokenPath).Trim())
        $plain = [Net.NetworkCredential]::new('', $secure).Password
        if (-not [string]::IsNullOrWhiteSpace($plain)) { $script:Token = $plain.Trim() }
    } catch {
        Write-Log 'Jeton illisible (chiffré pour un autre compte Windows ?) : relancer install.ps1 avec le jeton.' 'ERROR'
    }
}

function Read-State {
    $script:Handled = [Collections.Generic.Dictionary[string, long]]::new()
    $script:PendingDone = [Collections.Generic.List[string]]::new()
    $state = Read-Json $script:StatePath
    if ($null -eq $state) { return }
    $handled = $state['handled']
    if ($handled -is [Collections.IDictionary]) {
        foreach ($key in $handled.Keys) { try { $script:Handled[[string]$key] = [long]$handled[$key] } catch { } }
    }
    foreach ($id in @($state['done'])) {
        if ($id -is [string] -and -not $script:PendingDone.Contains($id)) { $script:PendingDone.Add($id) }
    }
}

function Save-State {
    if ($script:Handled.Count -gt $KeptOrderIds) {
        $newest = @($script:Handled.GetEnumerator() | Sort-Object -Property Value -Descending | Select-Object -First $KeptOrderIds)
        $script:Handled.Clear()
        foreach ($entry in $newest) { $script:Handled[$entry.Key] = $entry.Value }
    }
    Save-Json $script:StatePath ([ordered]@{ handled = $script:Handled; done = $script:PendingDone })
}


# ---------------------------------------------------------------------------------------------
# Child processes
# ---------------------------------------------------------------------------------------------
function Initialize-Native {
    $script:JobsReady = $false
    $script:Sampler = $null
    try {
        if (-not ('MoliAgent.KillJob' -as [type])) { Add-Type -TypeDefinition $NativeSource -Language CSharp }
        $script:JobsReady = $true
        $script:Sampler = [MoliAgent.ProcessSampler]::new()
    } catch {
        Write-Log "Aides natives indisponibles (processus non mesurés) : $($_.Exception.Message)" 'WARN'
    }
}

function New-KillJob([Diagnostics.Process]$Process) {
    # The agent's own helper (nvidia-smi) dies with the agent, never left behind.
    if (-not $script:JobsReady) { return $null }
    $job = $null
    try {
        $job = [MoliAgent.KillJob]::new()
        $job.Add($Process)
        return $job
    } catch {
        if ($null -ne $job) { $job.Dispose() }
        Write-Log "Rattachement du processus $($Process.Id) à un job impossible : $($_.Exception.Message)" 'WARN'
        return $null
    }
}


# ---------------------------------------------------------------------------------------------
# Measurements
# ---------------------------------------------------------------------------------------------
function Initialize-Sensors {
    # Processor: the counter Task Manager shows. Read through PerformanceCounter, which costs
    # nothing per sample, whereas a WMI query on the same counter costs ~70 ms each time.
    $script:CpuCounter = $null
    foreach ($counter in '% Processor Utility', '% Processor Time') {
        try {
            $candidate = [Diagnostics.PerformanceCounter]::new('Processor Information', $counter, '_Total', $true)
            $null = $candidate.NextValue()
            $script:CpuCounter = $candidate
            break
        } catch { }
    }
    $script:MemCounter = $null
    try {
        $candidate = [Diagnostics.PerformanceCounter]::new('Memory', 'Available Bytes', '', $true)
        $null = $candidate.RawValue
        $script:MemCounter = $candidate
    } catch { }
    $script:MemTotal = [double][GC]::GetGCMemoryInfo().TotalAvailableMemoryBytes

    $script:Gpu = @{ Exe = Find-NvidiaSmi; Proc = $null; Job = $null; Pending = $null; Last = $null; LastAt = 0.0; NextStart = 0.0 }
    if ($script:Gpu.Exe) { try { Start-GpuWatch } catch { Write-Log "nvidia-smi ne démarre pas : $($_.Exception.Message)" 'WARN' } }

    $lhmClient = [Net.Http.HttpClient]::new()
    $lhmClient.Timeout = [TimeSpan]::FromSeconds(3)
    $script:Lhm = @{ Client = $lhmClient; Task = $null; Value = $null; ValueAt = 0.0; Next = 0.0 }
    $null = Get-CpuTemperature

    $script:Net = @{ Nics = $null; NicsAt = 0.0; NicsSignature = ''; Physical = $null; PhysicalAt = 0.0; Bytes = $null; BytesAt = 0.0; Signature = ''; Down = $null; Up = $null }
    $null = Get-NetRates

    $script:TopProcesses = $null
    $script:ProcPrev = $null
    $script:ProcAt = 0.0
    Measure-Processes
}

function Wait-Sensors {
    # -Once: give the background readers (nvidia-smi, LibreHardwareMonitor) time to answer.
    $gpu = $script:Gpu
    if ($null -ne $gpu.Pending -and $null -eq $gpu.Last) { try { $null = $gpu.Pending.Wait(2000) } catch { } }
    if ($null -ne $script:Lhm.Task) { try { $null = $script:Lhm.Task.Wait(3500) } catch { } }
}

function Get-CpuLoad {
    $value = $null
    if ($null -ne $script:CpuCounter) {
        try { $value = [double]$script:CpuCounter.NextValue() } catch { $script:CpuCounter = $null }
    }
    if ($null -eq $value) {
        try {
            $sample = Get-CimInstance -ClassName Win32_PerfFormattedData_Counters_ProcessorInformation -Filter "Name='_Total'" -Property PercentProcessorUtility, PercentProcessorTime
            $value = if ($null -ne $sample.PercentProcessorUtility) { [double]$sample.PercentProcessorUtility } else { [double]$sample.PercentProcessorTime }
        } catch { return $null }
    }
    return [math]::Round([math]::Min(100.0, [math]::Max(0.0, $value)), 1)
}

function Get-Memory {
    $total = $script:MemTotal
    $available = $null
    if ($null -ne $script:MemCounter) {
        try { $available = [double]$script:MemCounter.RawValue } catch { $script:MemCounter = $null }
    }
    if ($null -eq $available -or $total -le 0) {
        $os = Get-CimInstance -ClassName Win32_OperatingSystem -Property FreePhysicalMemory, TotalVisibleMemorySize
        $available = [double]$os.FreePhysicalMemory * 1KB
        $total = [double]$os.TotalVisibleMemorySize * 1KB
    }
    $used = [math]::Max(0.0, $total - $available)
    return @{
        ram       = [math]::Round($used / $total * 100, 1)
        ram_used  = [math]::Round($used / 1GB, 1)
        ram_total = [math]::Round($total / 1GB, 1)
    }
}

function Find-NvidiaSmi {
    $command = Get-Command -Name nvidia-smi.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { return $command.Source }
    $path = Join-Path $env:WINDIR 'System32\nvidia-smi.exe'
    if ([IO.File]::Exists($path)) { return $path }
    return $null
}

function Start-GpuWatch {
    # One long-lived nvidia-smi printing a line every 2 s: ~0 CPU per sample, where starting
    # nvidia-smi for each report would cost ~40 ms of processor time every time.
    $gpu = $script:Gpu
    $gpu.NextStart = (Get-Elapsed) + 60
    $info = [Diagnostics.ProcessStartInfo]::new($gpu.Exe)
    foreach ($argument in '--query-gpu=name,utilization.gpu,temperature.gpu,memory.used,memory.total,power.draw',
        '--format=csv,noheader,nounits', '-lms', '2000') { $info.ArgumentList.Add($argument) }
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.StandardOutputEncoding = $script:Utf8
    $process = [Diagnostics.Process]::Start($info)
    $gpu.Job = New-KillJob $process
    $gpu.Proc = $process
    $gpu.Pending = $process.StandardOutput.ReadLineAsync()
}

function Stop-GpuWatch {
    $gpu = $script:Gpu
    if ($null -eq $gpu -or $null -eq $gpu.Proc) { return }
    try { if (-not $gpu.Proc.HasExited) { $gpu.Proc.Kill() } } catch { }
    if ($null -ne $gpu.Job) { $gpu.Job.Dispose() }
    try { $gpu.Proc.Dispose() } catch { }
    $gpu.Proc = $null
    $gpu.Job = $null
    $gpu.Pending = $null
}

function ConvertTo-Number([string]$Text) {
    $number = 0.0
    if ([double]::TryParse($Text, [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$number)) { return $number }
    return $null
}

function ConvertFrom-NvidiaSmi([string]$Line) {
    $fields = $Line.Split(',').Trim()
    if ($fields.Count -lt 6) { return $null }
    $n = $fields.Count
    $load = ConvertTo-Number $fields[$n - 5]
    $temperature = ConvertTo-Number $fields[$n - 4]
    $memoryUsed = ConvertTo-Number $fields[$n - 3]
    $memoryTotal = ConvertTo-Number $fields[$n - 2]
    $power = ConvertTo-Number $fields[$n - 1]
    $gpu = [ordered]@{ name = ($fields[0..($n - 6)] -join ', ') }
    $gpu.load = if ($null -ne $load) { [int]$load } else { $null }
    $gpu.temperature = if ($null -ne $temperature) { [int]$temperature } else { $null }
    $gpu.memory_used = if ($null -ne $memoryUsed) { [math]::Round($memoryUsed / 1024, 1) } else { $null }
    $gpu.memory_total = if ($null -ne $memoryTotal) { [math]::Round($memoryTotal / 1024, 1) } else { $null }
    $gpu.power = if ($null -ne $power) { [math]::Round($power, 1) } else { $null }
    return $gpu
}

function Get-GpuInfo {
    $gpu = $script:Gpu
    if (-not $gpu.Exe) { return $null }
    if (($null -eq $gpu.Proc -or ($null -eq $gpu.Pending -and $gpu.Proc.HasExited)) -and (Get-Elapsed) -ge $gpu.NextStart) {
        try { Stop-GpuWatch; Start-GpuWatch } catch { Write-Log "nvidia-smi ne redémarre pas : $($_.Exception.Message)" 'WARN' }
    }
    $line = $null
    while ($null -ne $gpu.Pending -and $gpu.Pending.IsCompleted) {
        $next = $null
        try { $next = $gpu.Pending.Result } catch { }
        if ($null -eq $next) { $gpu.Pending = $null; break }   # end of output: nvidia-smi stopped
        $line = $next
        $gpu.Pending = $gpu.Proc.StandardOutput.ReadLineAsync()
    }
    if ($null -ne $line) {
        $parsed = ConvertFrom-NvidiaSmi $line
        if ($null -ne $parsed) { $gpu.Last = $parsed; $gpu.LastAt = Get-Elapsed }
    }
    if ($null -ne $gpu.Last -and (Get-Elapsed) - $gpu.LastAt -lt 30) { return $gpu.Last }
    return $null
}

function ConvertFrom-LhmTemperature([string]$Json) {
    # Sensors are flat JSON objects; take "Core (Tctl/Tdie)" (AMD) or "CPU Package" (Intel).
    $best = $null
    $bestRank = 9
    $pattern = '"Text"\s*:\s*"(?<text>[^"]*(?:Tctl/Tdie|CPU Package)[^"]*)"[^{}]*?"Value"\s*:\s*"(?<value>[^"]*)"'
    foreach ($match in [regex]::Matches($Json, $pattern)) {
        $value = [regex]::Match($match.Groups['value'].Value, '(-?\d+(?:[.,]\d+)?)\s*(?:°|\\u00b0)?\s*C')
        if (-not $value.Success) { continue }
        $rank = if ($match.Groups['text'].Value -match 'Tctl/Tdie') { 0 } else { 1 }
        if ($rank -lt $bestRank) {
            $best = ConvertTo-Number ($value.Groups[1].Value.Replace(',', '.'))
            $bestRank = $rank
        }
    }
    if ($null -eq $best) { return $null }
    return [math]::Round($best, 1)
}

function Get-CpuTemperature {
    # LibreHardwareMonitor's web server, read in the background so a closed port never blocks
    # the loop; when it is not running, try again once a minute only.
    $lhm = $script:Lhm
    if ($null -ne $lhm.Task -and $lhm.Task.IsCompleted) {
        try {
            $value = ConvertFrom-LhmTemperature $lhm.Task.Result
            $lhm.Value = $value
            $lhm.ValueAt = Get-Elapsed
            $lhm.Next = if ($null -ne $value) { 0.0 } else { (Get-Elapsed) + 60 }
        } catch {
            $lhm.Value = $null
            $lhm.Next = (Get-Elapsed) + 60
        }
        $lhm.Task = $null
    }
    if ($null -eq $lhm.Task -and (Get-Elapsed) -ge $lhm.Next) {
        try { $lhm.Task = $lhm.Client.GetStringAsync($LhmUrl) } catch { $lhm.Next = (Get-Elapsed) + 60 }
    }
    if ($null -ne $lhm.Value -and (Get-Elapsed) - $lhm.ValueAt -lt 30) { return $lhm.Value }
    return $null
}

function Get-Disks {
    $disks = [Collections.Generic.List[object]]::new()
    foreach ($drive in [IO.DriveInfo]::GetDrives()) {
        try {
            if ($drive.DriveType -ne [IO.DriveType]::Fixed -or -not $drive.IsReady) { continue }
            $total = $drive.TotalSize
            $disks.Add([ordered]@{
                    name  = $drive.Name.TrimEnd('\')
                    label = $drive.VolumeLabel
                    used  = $total - $drive.TotalFreeSpace
                    total = $total
                })
        } catch { }
    }
    return , $disks
}

function Update-Nics {
    $net = $script:Net
    if ((Get-Elapsed) -ge $net.PhysicalAt) {
        # Physical adapters only (no Hyper-V, VPN or WSL switch counted twice), refreshed every 10 min.
        try {
            $physical = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
            foreach ($adapter in Get-NetAdapter -Physical -ErrorAction Stop) { $null = $physical.Add([string]$adapter.InterfaceGuid) }
            $net.Physical = $physical
        } catch { $net.Physical = $null }
        $net.PhysicalAt = (Get-Elapsed) + 600
    }
    $nics = [Collections.Generic.List[object]]::new()
    foreach ($nic in [Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()) {
        if ($nic.OperationalStatus -ne [Net.NetworkInformation.OperationalStatus]::Up) { continue }
        if ($null -ne $net.Physical) {
            if (-not $net.Physical.Contains($nic.Id)) { continue }
        } elseif ($nic.NetworkInterfaceType -in 'Loopback', 'Tunnel' -or $nic.Description -match $VirtualNic) { continue }
        $nics.Add($nic)
    }
    $net.Nics = $nics
    $net.NicsSignature = ($nics | ForEach-Object { $_.Id }) -join ','
    $net.NicsAt = (Get-Elapsed) + 60
}

function Get-NetRates {
    $net = $script:Net
    if ((Get-Elapsed) -ge $net.NicsAt) { Update-Nics }
    [long]$received = 0
    [long]$sent = 0
    foreach ($nic in $net.Nics) {
        try {
            $statistics = $nic.GetIPStatistics()
            $received += $statistics.BytesReceived
            $sent += $statistics.BytesSent
        } catch { }
    }
    $now = Get-Elapsed
    $signature = $net.NicsSignature   # a counter added or removed is not traffic
    if ($null -ne $net.Bytes -and $signature -eq $net.Signature -and $now - $net.BytesAt -gt 0.2) {
        $seconds = $now - $net.BytesAt
        $net.Down = [math]::Round([math]::Max(0, $received - $net.Bytes[0]) * 8 / 1e6 / $seconds, 2)
        $net.Up = [math]::Round([math]::Max(0, $sent - $net.Bytes[1]) * 8 / 1e6 / $seconds, 2)
    }
    $net.Bytes = @($received, $sent)
    $net.BytesAt = $now
    $net.Signature = $signature
    return @{ down = $net.Down; up = $net.Up }
}

function Measure-Processes {
    # The 8 busiest programs since the previous measurement (processes summed per name, like
    # Task Manager groups them); cpu in % of the whole machine, memory = working set in bytes.
    $script:ProcAt = Get-Elapsed
    if ($null -eq $script:Sampler) { return }
    $rows = $null
    try { $rows = $script:Sampler.Sample(8) }
    catch {
        Write-Log "Mesure des processus impossible, abandonnée : $($_.Exception.Message)" 'WARN'
        $script:Sampler = $null
        return
    }
    if ($null -eq $rows) { return }   # first sample: no interval yet
    $top = [Collections.Generic.List[object]]::new()
    foreach ($row in $rows) { $top.Add([ordered]@{ name = $row.Name; cpu = $row.Cpu; memory = $row.Memory; count = $row.Count }) }
    $script:TopProcesses = $top
}

# ---------------------------------------------------------------------------------------------
# Remote mode: the Claude and Codex desktop apps open, so the phone reaches them
# ---------------------------------------------------------------------------------------------
# Claude: its sessions connect to Remote Control (the app's own setting) and the phone's Claude
# app takes them over. Codex: the app runs its exec-server towards Codex Cloud and the phone's
# ChatGPT app drives it. Both only need the app open in the Windows session. The ids are the
# Start menu's (Get-StartApps): the package family names do not change with updates.
$RemoteApps = [ordered]@{
    claude = @{ AppId = 'Claude_pzs8sxrjxfjjc!Claude'; Process = 'claude'; Package = '\WindowsApps\Claude_' }
    codex  = @{ AppId = 'OpenAI.Codex_2p2nqsd0c76g0!App'; Process = 'ChatGPT'; Package = '\WindowsApps\OpenAI.Codex_' }
}

function Test-AppRunning([hashtable]$App) {
    # The desktop app itself, not Claude Code's own claude.exe (elsewhere on disk).
    foreach ($process in [Diagnostics.Process]::GetProcessesByName($App.Process)) {
        try {
            if ($process.Path -like "*$($App.Package)*") { return $true }
        } catch { } finally { $process.Dispose() }
    }
    return $false
}

function Update-Apps {
    $script:AppsAt = Get-Elapsed
    $apps = [ordered]@{}
    foreach ($name in $RemoteApps.Keys) { $apps[$name] = Test-AppRunning $RemoteApps[$name] }
    $script:AppsState = $apps
}

function Start-Remote([string]$Why) {
    foreach ($name in $RemoteApps.Keys) {
        $app = $RemoteApps[$name]
        if (Test-AppRunning $app) { continue }
        if ($DryRun) { Write-Log "Essai à blanc : ouverture de $name simulée ($Why)."; continue }
        try {
            Start-Process -FilePath "shell:AppsFolder\$($app.AppId)"
            Write-Log "Mode à distance ($Why) : $name ouvert."
        } catch { Write-Log "Ouverture de $name impossible : $($_.Exception.Message)" 'WARN' }
    }
    # Looked at again at the next report (an app takes a few seconds to start).
    $script:AppsAt = -1e9
}

# ---------------------------------------------------------------------------------------------
# Orders
# ---------------------------------------------------------------------------------------------
function Invoke-Shutdown([string[]]$Arguments, [string]$What) {
    if ($DryRun) { Write-Log "Essai à blanc : $What simulé (shutdown.exe $($Arguments -join ' '))."; return }
    $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $env:WINDIR 'System32\shutdown.exe'))
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit(15000)) { Write-Log "$What : shutdown.exe ne répond pas." 'WARN'; return }
        switch ($process.ExitCode) {
            0 { Write-Log "$What : fait." }
            1190 { Write-Log "$What : une extinction est déjà programmée." 'WARN' }
            1116 { Write-Log "$What : aucune extinction à annuler." }
            default { Write-Log "$What : shutdown.exe a répondu $($process.ExitCode)." 'WARN' }
        }
    } finally { $process.Dispose() }
}

function Invoke-Sleep {
    Write-Log 'Mise en veille demandée par Moli.'
    Add-Type -AssemblyName System.Windows.Forms
    $slept = [Windows.Forms.Application]::SetSuspendState([Windows.Forms.PowerState]::Suspend, $false, $false)
    if ($slept) { Write-Log 'Sortie de veille.' } else { Write-Log 'Windows a refusé la mise en veille.' 'WARN' }
}

function Add-Done([string]$Id) {
    if (-not $script:PendingDone.Contains($Id)) { $script:PendingDone.Add($Id) }
}

function Invoke-Orders($Orders) {
    foreach ($order in @($Orders)) {
        if ($null -eq $order) { continue }
        $rawId = Get-Field $order 'id'
        if ($null -eq $rawId -or "$rawId" -eq '' -or ($rawId -isnot [string] -and $rawId -isnot [ValueType])) {
            Write-Log 'Ordre sans identifiant ignoré.' 'WARN'
            continue
        }
        $id = [string]$rawId
        if ($script:Handled.ContainsKey($id)) {
            # Already handled (Moli resends until it reads the id in "done"): acknowledge again only.
            Add-Done $id
            continue
        }
        $kind = [string](Get-Field $order 'kind')
        # Recorded before running it: an order runs at most once, even if the PC restarts meanwhile.
        $script:Handled[$id] = Get-UnixMs
        Add-Done $id
        Save-State
        Write-Log "Ordre $id : $kind."
        try {
            switch -Exact ($kind) {
                'shutdown' { Invoke-Shutdown @('/s', '/t', '30', '/c', 'Moli : extinction demandée depuis le tableau de bord') 'Extinction dans 30 s' }
                'restart' { Invoke-Shutdown @('/r', '/t', '30', '/c', 'Moli : redémarrage demandé depuis le tableau de bord') 'Redémarrage dans 30 s' }
                'cancel_shutdown' { Invoke-Shutdown @('/a') 'Annulation de l''extinction' }
                'sleep' {
                    if ($DryRun) { Write-Log 'Essai à blanc : mise en veille simulée.' }
                    else { $script:SleepPending = $true }   # after the next report, so Moli knows first
                }
                'remote' { Start-Remote 'demandé depuis Moli' }
                default { Write-Log "Ordre $id refusé : type inconnu « $kind »." 'WARN' }
            }
        } catch { Write-Log "Ordre $id ($kind) en échec : $($_.Exception.Message)" 'ERROR' }
    }
    if ($script:PendingDone.Count -gt 0) { $script:NextReport = [math]::Min($script:NextReport, (Get-Elapsed) + 1) }
}

# ---------------------------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------------------------
function New-Report {
    $memory = Get-Memory
    $report = [ordered]@{
        version         = $ReportVersion
        user            = [Environment]::UserName
        uptime          = [long][math]::Floor([Environment]::TickCount64 / 1000)
        cpu             = Get-CpuLoad
        cpu_temperature = Get-CpuTemperature
        ram             = $memory.ram
        ram_used        = $memory.ram_used
        ram_total       = $memory.ram_total
    }
    $gpu = Get-GpuInfo
    if ($null -ne $gpu) { $report.gpu = $gpu }
    $report.disks = Get-Disks
    $net = Get-NetRates
    $report.net_down = $net.down
    $report.net_up = $net.up
    if ($null -eq $script:TopProcesses -or (Get-Elapsed) - $script:ProcAt -ge $ProcessEvery) { Measure-Processes }
    $processes = $script:TopProcesses
    if ($null -eq $processes) { $processes = [Collections.Generic.List[object]]::new() }
    $report.processes = $processes
    if ((Get-Elapsed) - $script:AppsAt -ge $AppsEvery) { Update-Apps }
    $report.apps = $script:AppsState
    $report.done = [Collections.Generic.List[string]]::new($script:PendingDone)
    return $report
}

function Send-Report([string]$Body) {
    $url = '{0}/api/machines/{1}/report' -f $script:Config.moli, [Uri]::EscapeDataString($script:Config.id)
    $request = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Post, $url)
    try {
        $request.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $script:Token)
        $request.Content = [Net.Http.StringContent]::new($Body, $script:Utf8, 'application/json')
        $response = $script:Http.SendAsync($request).GetAwaiter().GetResult()
        try { return @{ status = [int]$response.StatusCode; body = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult() } }
        finally { $response.Dispose() }
    } catch {
        $exception = $_.Exception
        while ($null -ne $exception.InnerException) { $exception = $exception.InnerException }
        return @{ status = 0; error = $exception.Message }
    } finally { $request.Dispose() }
}

function Invoke-Report {
    # One exchange with Moli. Returns $true when Moli accepted the report.
    if (-not $script:Config.moli) {
        if (-not $script:WarnedConfig) { Write-Log "Adresse de Moli absente ($script:ConfigPath) : nouvel essai toutes les minutes." 'WARN'; $script:WarnedConfig = $true }
        $script:NextReport = (Get-Elapsed) + 60
        return $false
    }
    $script:WarnedConfig = $false
    if (-not $script:Token -and (Get-Elapsed) - $script:TokenCheckedAt -ge $AuthRetry) { Update-Token }
    if (-not $script:Token) {
        if (-not $script:WarnedToken) { Write-Log 'Aucun jeton : lancer install.ps1 avec le jeton sur l''entrée standard. Nouvel essai toutes les minutes.' 'WARN'; $script:WarnedToken = $true }
        $script:NextReport = (Get-Elapsed) + $AuthRetry
        return $false
    }
    $script:WarnedToken = $false
    $report = New-Report
    $sentDone = [string[]]$report.done
    $answer = Send-Report (ConvertTo-Json -InputObject $report -Depth 8 -Compress)
    $status = $answer.status
    if ($status -ge 200 -and $status -lt 300) {
        if ($script:Failures -gt 0) { Write-Log "Moli joignable de nouveau ($($script:Failures) échec(s) avant)." }
        if ($script:AuthRefused) { Write-Log 'Jeton accepté par Moli.' }
        $script:Failures = 0
        $script:AuthRefused = $false
        if ($sentDone.Count -gt 0) {
            foreach ($id in $sentDone) { $null = $script:PendingDone.Remove($id) }
            Save-State
        }
        $json = $null
        if (-not [string]::IsNullOrWhiteSpace($answer.body)) {
            try { $json = $answer.body | ConvertFrom-Json } catch { Write-Log 'Réponse de Moli illisible (JSON attendu).' 'WARN' }
        }
        $every = Get-Field $json 'every'
        if ($every -is [ValueType]) { try { $script:Every = [math]::Min(300.0, [math]::Max(2.0, [double]$every)) } catch { } }
        $script:NextReport = (Get-Elapsed) + $script:Every
        $orders = Get-Field $json 'orders'
        if ($null -ne $orders) { Invoke-Orders $orders }
        return $true
    }
    if ($status -eq 401 -or $status -eq 403) {
        if (-not $script:AuthRefused) { Write-Log "Jeton refusé par Moli (HTTP $status) : nouvel essai dans $AuthRetry s." 'ERROR'; $script:AuthRefused = $true }
        Update-Token
        $script:NextReport = (Get-Elapsed) + $AuthRetry
        return $false
    }
    $script:Failures++
    $delay = [math]::Min($MaxBackoff, 5 * [math]::Pow(2, $script:Failures - 1))
    $reason = if ($status -eq 0) { $answer.error } else { "HTTP $status" }
    if ($script:Failures -eq 1 -or (Get-Elapsed) - $script:FailureLoggedAt -ge 600) {
        Write-Log ('Moli injoignable ({0}) : nouvel essai dans {1} s{2}.' -f $reason, $delay, $(if ($script:Failures -gt 1) { ", $($script:Failures) échecs d'affilée" } else { '' })) 'WARN'
        $script:FailureLoggedAt = Get-Elapsed
    }
    $script:NextReport = (Get-Elapsed) + $delay
    return $false
}

function Write-Summary($Report) {
    $gpu = if ($Report.Contains('gpu')) { 'gpu {0} % {1} °C' -f $Report.gpu.load, $Report.gpu.temperature } else { 'pas de gpu' }
    $top = ($Report.processes | Select-Object -First 3 | ForEach-Object { '{0} {1} %' -f $_.name, $_.cpu }) -join ', '
    try {
        [Console]::Error.WriteLine(('{0} cpu {1} % | ram {2} % | {3} | réseau {4}/{5} Mbit/s | {6}' -f (Get-Date -Format 'HH:mm:ss'),
                $Report.cpu, $Report.ram, $gpu, $Report.net_down, $Report.net_up, $top))
    } catch { }
}

# ---------------------------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------------------------
$script:Config = New-DefaultConfig
$script:ConfigStamp = [datetime]::MinValue
$script:Token = $null
$script:TokenCheckedAt = -1e9
$script:AppsState = [ordered]@{}
$script:AppsAt = -1e9
$script:RemoteAt = $null
$script:Gpu = $null
$script:NextReport = 0.0
$script:Every = [double]$DefaultEvery
$script:Failures = 0
$script:FailureLoggedAt = 0.0
$script:AuthRefused = $false
$script:WarnedConfig = $false
$script:WarnedToken = $false
$script:SleepPending = $false
$exitCode = 0

$mutex = $null
if (-not $DryRun) {
    try { $mutex = [Threading.Mutex]::new($false, 'Global\MoliAgent') }
    catch [UnauthorizedAccessException] { $mutex = [Threading.Mutex]::new($false, 'Local\MoliAgent') }
    $owned = $false
    try { $owned = $mutex.WaitOne(10000) } catch [Threading.AbandonedMutexException] { $owned = $true }
    if (-not $owned) {
        Write-Log 'Un autre agent Moli tourne déjà sur ce PC : arrêt de celui-ci.' 'WARN'
        exit 0
    }
}

$handler = [Net.Http.SocketsHttpHandler]::new()
$handler.ConnectTimeout = [TimeSpan]::FromSeconds(5)
$handler.PooledConnectionLifetime = [TimeSpan]::FromMinutes(2)
$handler.UseProxy = $false
$script:Http = [Net.Http.HttpClient]::new($handler)
$script:Http.Timeout = [TimeSpan]::FromSeconds(15)
$null = $script:Http.DefaultRequestHeaders.UserAgent.TryParseAdd('moli-agent-windows/1')

try {
    Write-Log ('Agent Moli démarré (pid {0}{1}).' -f $PID, $(if ($DryRun) { ', essai à blanc : rien n''est envoyé' } else { '' }))
    Update-Config -Force
    if (-not $DryRun) { Update-Token }
    Read-State
    Initialize-Native
    Initialize-Sensors
    Update-Apps
    # The session just opened (a PC woken by Moli, or someone at the desk): the apps for the phone.
    if ($script:Config.remote_at_start -and -not $Once) { $script:RemoteAt = (Get-Elapsed) + $RemoteDelay }

    if ($Order) {
        foreach ($text in $Order) {
            $parsed = $null
            try { $parsed = $text | ConvertFrom-Json } catch { throw "-Order : JSON invalide ($($_.Exception.Message))" }
            Invoke-Orders $parsed
        }
    }

    Start-Sleep -Milliseconds 1000   # first interval for the processor, network and process rates

    $statCpu = $Self.TotalProcessorTime
    $statAt = Get-Elapsed
    $statCycles = 0
    while ($true) {
        try {
            Update-Config
            if ($null -ne $script:RemoteAt -and (Get-Elapsed) -ge $script:RemoteAt) {
                $script:RemoteAt = $null
                Start-Remote 'ouverture de la session'
            }
            if ($DryRun -and $Once) {
                break
            } elseif ((Get-Elapsed) -ge $script:NextReport) {
                if ($DryRun) {
                    $report = New-Report
                    $null = ConvertTo-Json -InputObject $report -Depth 8 -Compress   # same work as a real report
                    Write-Summary $report
                    $script:PendingDone.Clear()
                    $script:NextReport = (Get-Elapsed) + $script:Every
                } else {
                    $accepted = Invoke-Report
                    if ($script:SleepPending) {
                        $script:SleepPending = $false
                        Invoke-Sleep
                    }
                    if ($Once) {
                        if (-not $accepted) { $exitCode = 1; break }
                        if ($script:PendingDone.Count -eq 0) { break }
                    }
                }
                $statCycles++
            }
        } catch {
            Write-Log "Erreur inattendue : $($_.Exception.Message) (ligne $($_.InvocationInfo.ScriptLineNumber))" 'ERROR'
            $script:NextReport = (Get-Elapsed) + 5
        }

        if ((Get-Elapsed) - $statAt -ge $StatsEvery -and $statCycles -gt 0) {
            $Self.Refresh()
            $cpuMs = ($Self.TotalProcessorTime - $statCpu).TotalMilliseconds
            $wall = (Get-Elapsed) - $statAt
            Write-Log ('Charge de l''agent : {0:N1} ms de processeur par cycle sur {1} cycles, soit {2:N2} % d''un cœur.' -f ($cpuMs / $statCycles), $statCycles, ($cpuMs / $wall / 10))
            $statCpu = $Self.TotalProcessorTime
            $statAt = Get-Elapsed
            $statCycles = 0
        }

        $wait = if ($DryRun -and $Once) { 1.0 } else { [math]::Min([math]::Max($script:NextReport - (Get-Elapsed), 0.1), $script:Every) }
        Start-Sleep -Milliseconds ([int]($wait * 1000))
    }

    if ($DryRun -and $Once) {
        Wait-Sensors
        $Self.Refresh()
        $before = $Self.TotalProcessorTime
        $report = New-Report
        $json = ConvertTo-Json -InputObject $report -Depth 8
        $Self.Refresh()
        Write-Log ('Coût de cette mesure complète : {0:N0} ms de processeur.' -f ($Self.TotalProcessorTime - $before).TotalMilliseconds)
        Write-Output $json
    }
} finally {
    Stop-GpuWatch
    if ($null -ne $mutex) { try { $mutex.ReleaseMutex() } catch { }; $mutex.Dispose() }
}
exit $exitCode
