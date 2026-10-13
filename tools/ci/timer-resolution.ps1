# Holds the system timer at 1 ms for the rest of a CI job and prints the
# resolution before and after (ADR 0026). CI only.
#
# VB-CABLE is a WaveCyclic driver: its position and its exclusive-mode events
# advance on a kernel timer, so at the default 15.6 ms tick an exclusive stream
# gets one event per tick whatever its period (144 frames per 15.6 ms at a 3 ms
# period). Audio hardware interrupts at its own period. Holding the tick at
# 1 ms lets the virtual cable keep the periods the engine asks for.
#
#     ./tools/ci/timer-resolution.ps1          # start the holder, then check
#     ./tools/ci/timer-resolution.ps1 -Hold    # the holder itself (never returns)
param(
    [switch] $Hold
)
$ErrorActionPreference = 'Stop'

Add-Type -Namespace BioCi -Name Timer -MemberDefinition @'
[DllImport("ntdll.dll")]
public static extern int NtQueryTimerResolution(out uint coarsest, out uint finest, out uint current);
[DllImport("ntdll.dll")]
public static extern int NtSetTimerResolution(uint desired, [MarshalAs(UnmanagedType.U1)] bool set, out uint current);
'@

# 100 ns units.
$Desired = 10000

function Get-TimerResolution {
    $coarsest = [uint32]0; $finest = [uint32]0; $current = [uint32]0
    $status = [BioCi.Timer]::NtQueryTimerResolution([ref]$coarsest, [ref]$finest, [ref]$current)
    if ($status -ne 0) { throw ('NtQueryTimerResolution failed (0x{0:X8})' -f $status) }
    [pscustomobject]@{ Coarsest = $coarsest; Finest = $finest; Current = $current }
}

if ($Hold) {
    $current = [uint32]0
    $status = [BioCi.Timer]::NtSetTimerResolution($Desired, $true, [ref]$current)
    if ($status -ne 0) { throw ('NtSetTimerResolution failed (0x{0:X8})' -f $status) }
    # The request lasts as long as this process; the runner ends it with the job.
    while ($true) { Start-Sleep -Seconds 3600 }
}

$before = Get-TimerResolution
# A hidden window of its own: a holder sharing the step's console would keep
# the step's output pipes open and the step would never end.
Start-Process -FilePath (Get-Process -Id $PID).Path -WindowStyle Hidden `
    -ArgumentList @('-NoProfile', '-File', $PSCommandPath, '-Hold') | Out-Null
$after = $before
foreach ($attempt in 1..40) {
    Start-Sleep -Milliseconds 250
    $after = Get-TimerResolution
    if ($after.Current -le $Desired) { break }
}
$ms = { param($units) '{0:N3} ms' -f ($units / 10000.0) }
Write-Host ("timer resolution {0} -> {1} (finest {2}, coarsest {3})" -f (& $ms $before.Current),
    (& $ms $after.Current), (& $ms $after.Finest), (& $ms $after.Coarsest))
if ($after.Current -gt $Desired) {
    Write-Host "::warning::the system timer stayed at $(& $ms $after.Current); VB-CABLE keeps its 15.6 ms events"
}
