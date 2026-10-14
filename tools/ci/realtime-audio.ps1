# The audio environment the live tests need on a CI runner (ADR 0026), set up
# the same way in every job that plays through ssr-live. CI only.
#
#     ./tools/ci/realtime-audio.ps1 -CrashDumps (Join-Path $env:RUNNER_TEMP 'dumps')
#
# - VB-CABLE, the output the tests record back (install-virtual-cable.ps1),
#   with its internal rate and pipe size when -CableRate and -CableMaxLatency
#   are given (on Server 2022 only, ADR 0026).
# - Local crash dumps of ssr-live and ssr-replay (crash-dumps.ps1); a later
#   `crash-dumps.ps1 -Report` step prints their stacks.
# - The system timer held at 1 ms for the rest of the job
#   (timer-resolution.ps1): VB-CABLE's exclusive-mode events advance on the
#   system timer, one per 15.6 ms tick whatever the period, and a 1 ms tick
#   lets it keep the periods the engine asks for, as hardware does.
# - MMCSS running. It boosts the real-time threads; the Server images leave it
#   stopped (manual start) until the first registration starts it, which lands
#   in the first second of a run, while desktops run it already.
param(
    [Parameter(Mandatory)] [string] $CrashDumps,
    [int] $CableRate = 0,
    [int] $CableMaxLatency = 0
)
$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'install-virtual-cable.ps1') -InternalRate $CableRate -MaxLatency $CableMaxLatency
& (Join-Path $PSScriptRoot 'crash-dumps.ps1') -Folder $CrashDumps
& (Join-Path $PSScriptRoot 'timer-resolution.ps1')

Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile' |
    Select-Object SystemResponsiveness, NetworkThrottlingIndex | Format-List
Start-Service -Name MMCSS
$mmcss = Get-Service -Name MMCSS
Write-Host "MMCSS $($mmcss.Status) ($($mmcss.StartType))"
if ($mmcss.Status -ne 'Running') { throw "MMCSS is $($mmcss.Status)" }
