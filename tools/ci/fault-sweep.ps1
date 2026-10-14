# Sweeps the driver's own fault switch on a CI runner: the N-th resource
# acquisition after the image loads fails once (Parameters\FaultInjectAt),
# for N = 1, 2, 3, ... up to the number a clean load makes. Each instance
# writes what it still held when its image unloads (the volatile Diagnostics
# subkey), and every count must be zero. An instance whose start fails
# unloads at once, so the sweep enables and disables the device in separate
# steps and reads each instance's report right after the step that unloaded
# it; a restart would unload one instance and load the next in one go, and
# the second report would overwrite the first.
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

function Disable-SsrDevice([string] $InstanceId, [int] $SettleSeconds) {
    $text = (& pnputil.exe /disable-device $InstanceId 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "pnputil /disable-device failed ($LASTEXITCODE): $text" }
    Start-Sleep -Seconds $SettleSeconds
}

function Enable-SsrDevice([string] $InstanceId, [int] $SettleSeconds) {
    # The problem code the start left (0 when the device started).
    $text = (& pnputil.exe /enable-device $InstanceId 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "pnputil /enable-device failed ($LASTEXITCODE): $text" }
    Start-Sleep -Seconds $SettleSeconds
    $problem = Get-SsrProblemCode $InstanceId
    if ($problem -eq 38) { throw "the driver image did not unload before this start (problem 38): $text" }
    $problem
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
    $unloads = { $r = Get-SsrUnloadReport; if ($r -and $null -ne $r.Unloads) { [long]$r.Unloads } else { 0 } }

    # One clean instance's whole life (start, settle, unload) bounds the sweep.
    Disable-SsrDevice $InstanceId $SettleSeconds
    Set-SsrFaultInjectAt 0
    $problem = Enable-SsrDevice $InstanceId $SettleSeconds
    if ($problem -ne 0) { throw "a clean start left problem $problem" }
    $before = & $unloads
    Disable-SsrDevice $InstanceId $SettleSeconds
    $clean = Get-SsrUnloadReport
    $problems = @(Test-SsrUnloadReport $clean $before 0)
    if ($problems.Count) { throw "a clean load: $($problems -join '; ')" }
    $calls = [int]$clean.FaultCalls
    if ($calls -lt 1) { throw 'a clean load passed no fault point; the driver has no fault switch' }
    if ($calls -gt $Max) { throw "a clean load passed $calls fault points, more than the sweep's $Max" }
    Write-Host "a clean load passes $calls fault points"

    for ($at = 1; $at -le $calls; ++$at) {
        Set-SsrFaultInjectAt $at
        $before = & $unloads
        $problem = Enable-SsrDevice $InstanceId $SettleSeconds
        # A failed start unloads the image at once; a start that survived the
        # failure unloads at the disable.
        if ((& $unloads) -le $before) {
            Disable-SsrDevice $InstanceId $SettleSeconds
            $report = Get-SsrUnloadReport
        } else {
            $report = Get-SsrUnloadReport
            Disable-SsrDevice $InstanceId $SettleSeconds
        }
        $problems = @(Test-SsrUnloadReport $report $before $at)
        $steps += [pscustomobject]@{ FailedCall = $at; StartProblem = $problem; Problems = $problems; Report = $report }
        if ($problems.Count) { $failures += "call ${at}: $($problems -join '; ')" }
    }
    Set-SsrFaultInjectAt 0
    $problem = Enable-SsrDevice $InstanceId $SettleSeconds
    if ($problem -ne 0) { $failures += "the clean start after the sweep left problem $problem" }
    if ($failures.Count) { throw "fault sweep: $($failures.Count) problems: $($failures -join ' | ')" }
    $steps
}
