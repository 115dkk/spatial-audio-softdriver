# Sweeps the driver's own fault switch on a CI runner: the N-th resource
# acquisition after the image loads fails once (Parameters\FaultInjectAt),
# for N = 1, 2, 3, ... up to the number a clean load makes. Each step
# restarts the device, so the previous instance unloads and writes what it
# still held (the volatile Diagnostics subkey); every count must be zero.
#
# Driver Verifier cannot do this on the runners: on Server 2022 its volatile
# settings list the driver but intercept none of its calls, and on Server
# 2025 `/dif ... /now` attaches to the running image only and is gone after
# the next unload (ADR 0027, 2026-10-14).
#
#   . ./tools/ci/fault-sweep.ps1
#   $steps = Invoke-SsrFaultSweep -InstanceId $device.InstanceId -Max 400
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'verifier.ps1')

$SsrServiceKey = 'HKLM:\SYSTEM\CurrentControlSet\Services\SpatialSoftReceiver'
$SsrReportNames = @('Unloads', 'FaultCalls', 'FaultInjectedAt', 'OutstandingPool', 'OutstandingTimers', 'OutstandingDma')

function Set-SsrFaultInjectAt([int] $At) {
    $parameters = Join-Path $SsrServiceKey 'Parameters'
    if (-not (Test-Path $parameters)) { New-Item -Path $parameters -Force | Out-Null }
    New-ItemProperty -Path $parameters -Name FaultInjectAt -PropertyType DWord -Value $At -Force | Out-Null
}

function Get-SsrUnloadReport {
    # What the last unloaded instance wrote, or $null before any unload. The
    # driver writes LONG counts as DWORDs, which read back signed, so a count
    # freed twice shows as negative and fails the same check as a leak.
    $key = Join-Path $SsrServiceKey 'Diagnostics'
    if (-not (Test-Path $key)) { return $null }
    $values = Get-ItemProperty -Path $key
    $report = [ordered]@{}
    foreach ($name in $SsrReportNames) {
        $value = $values.$name
        $report[$name] = if ($null -eq $value) { $null } else { [long]$value }
    }
    [pscustomobject]$report
}

function Test-SsrUnloadReport($Report, [long] $PreviousUnloads, [int] $InjectedAt) {
    # Problems with one instance's report; empty when it is clean. $InjectedAt
    # is the call that instance was told to fail (0: none).
    $problems = @()
    if ($null -eq $Report) { return @('the driver wrote no unload report') }
    if ($null -eq $Report.Unloads -or $Report.Unloads -le $PreviousUnloads) {
        $problems += "no new unload report (Unloads $($Report.Unloads) after $PreviousUnloads): the image did not unload"
    }
    foreach ($name in 'OutstandingPool', 'OutstandingTimers', 'OutstandingDma') {
        if ($Report.$name -ne 0) { $problems += "$name $($Report.$name) at unload" }
    }
    if ($InjectedAt -eq 0 -and $Report.FaultInjectedAt -ne 0) {
        $problems += "failed call $($Report.FaultInjectedAt) with the switch off"
    } elseif ($InjectedAt -gt 0 -and $Report.FaultInjectedAt -ne $InjectedAt -and $Report.FaultCalls -ge $InjectedAt) {
        $problems += "passed call $InjectedAt without failing it (FaultInjectedAt $($Report.FaultInjectedAt))"
    }
    $problems
}

function Invoke-SsrFaultSweep {
    param(
        [Parameter(Mandatory)] [string] $InstanceId,
        # Upper bound on the sweep; a clean load that makes more calls than
        # this fails the step rather than leaving the tail untested.
        [int] $Max = 400,
        [int] $SettleSeconds = 3
    )
    $steps = @()
    $failures = @()
    $restart = {
        param([int] $Label)
        $before = Get-SsrUnloadReport
        $previous = if ($before -and $null -ne $before.Unloads) { $before.Unloads } else { 0 }
        $cycle = @(Invoke-SsrRestartCycles -InstanceId $InstanceId -Count 1 -SettleSeconds $SettleSeconds)[0]
        [pscustomobject]@{ Label = $Label; Previous = $previous; Report = (Get-SsrUnloadReport); Cycle = $cycle }
    }

    # Two clean restarts: the second report describes one whole clean load,
    # whose call count bounds the sweep.
    Set-SsrFaultInjectAt 0
    $null = & $restart 0
    $clean = & $restart 0
    $problems = @(Test-SsrUnloadReport $clean.Report $clean.Previous 0)
    if ($problems.Count) { throw "a clean load: $($problems -join '; ')" }
    $calls = [int]$clean.Report.FaultCalls
    if ($calls -lt 1) { throw 'a clean load passed no fault point; the driver has no fault switch' }
    if ($calls -gt $Max) { throw "a clean load passed $calls fault points, more than the sweep's $Max" }
    Write-Host "a clean load passes $calls fault points"

    # Each restart starts the instance told to fail call $next and unloads the
    # one before it, whose report arrives with that restart.
    $injected = 0
    $startProblem = $clean.Cycle.Problem
    for ($at = 1; $at -le $calls + 1; ++$at) {
        $next = if ($at -le $calls) { $at } else { 0 }
        Set-SsrFaultInjectAt $next
        $step = & $restart $next
        $problems = @(Test-SsrUnloadReport $step.Report $step.Previous $injected)
        $steps += [pscustomobject]@{
            FailedCall = $injected
            StartProblem = $startProblem
            Problems = $problems
            Report = $step.Report
        }
        if ($problems.Count) { $failures += "call ${injected}: $($problems -join '; ')" }
        $injected = $next
        $startProblem = $step.Cycle.Problem
    }
    Set-SsrFaultInjectAt 0
    if ($failures.Count) { throw "fault sweep: $($failures.Count) instances left something behind or ignored the switch: $($failures -join ' | ')" }
    $steps
}
