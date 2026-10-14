# Driver Verifier for the CI runners, which cannot reboot mid-job.
#
# The way to start Driver Verifier without a reboot differs by Windows version
# (https://learn.microsoft.com/windows-hardware/drivers/devtest/using-volatile-settings):
#
# - Windows 10 / Server 2022 (build < 22000): volatile flags, set before the
#   driver loads, take special pool, IRQL, pool tracking, I/O, deadlock, DMA,
#   security and miscellaneous checks (0x9BB). I/O verification (0x10) is
#   left out (0x9AB): on Server 2022 a PortCls audio driver's start under it
#   loses the runner, ours and VB-CABLE alike (ADR 0027). Server 2025's rule
#   classes keep I/O verification.
# - Windows 11 / Server 2025: `/dif <classes> /now` starts rule classes on a
#   driver that is already running; on one that is not it lands in
#   FailedDrivers with 0xC0000C7B (STATUS_DIF_VOLATILE_DRIVER_IS_NOT_RUNNING).
#
# So the workflow calls Enable-SsrVerifier twice, once before the install and
# once after the driver has loaded; each call does the part that belongs to
# that moment on this Windows and nothing on the other. Every verifier.exe call
# is checked: exit 1 is an error, 2 means the setting needs a reboot this job
# will never get, and the step fails unless `/query` then lists the driver.
#
# Low-resources simulation is not used. On Server 2022 the volatile settings
# list the driver but intercept none of its calls, and on Server 2025 the
# systematic simulation (class 19, `/faultssystematic enableruntime`) failed
# none of 480 allocations while the driver played and was fuzzed. The driver's
# own fault switch reaches every error branch instead (tools/ci/fault-sweep.ps1,
# ADR 0027, 2026-10-14).
#
#   . ./tools/ci/verifier.ps1
#   Enable-SsrVerifier -When BeforeLoad   # before the install
#   Enable-SsrVerifier -When Loaded       # after the driver loaded
#   Get-SsrVerifierState                  # parsed `verifier /query`
#   Invoke-SsrRestartCycles -InstanceId $id -Count 20
$ErrorActionPreference = 'Stop'

$SsrVerifiedImage = 'spatialsoftreceiver.sys'
# Rule classes for `/dif ... /now` (verifier-command-line): 1 special pool,
# 2 force IRQL checking, 4 pool tracking, 5 I/O verification, 9 security
# checks, 12 miscellaneous checks, 18 DDI compliance checking.
$SsrStandardClasses = @(1, 2, 4, 5, 9, 12, 18)
# The same checks as volatile flags where Windows 10 allows them, plus
# deadlock detection (0x20) and DMA checking (0x80), less I/O verification
# (0x10), under which a PortCls start loses a Server 2022 runner (ADR 0027).
$SsrStandardFlags = 0x9AB
$SsrPoolTrackingFlag = 0x8

function Test-SsrDifVerifier {
    # /dif ... /now exists from Windows 11 (build 22000) and Server 2025.
    [Environment]::OSVersion.Version.Build -ge 22000
}

function Invoke-SsrVerifier([string[]] $Arguments) {
    Write-Host "verifier $($Arguments -join ' ')"
    $output = (& verifier.exe @Arguments 2>&1 | Out-String).TrimEnd()
    $code = $LASTEXITCODE
    if ($output) { Write-Host $output }
    if ($code -eq 2) { throw "verifier $($Arguments -join ' ') needs a reboot (exit 2); this setting cannot apply on a CI runner" }
    if ($code -ne 0) { throw "verifier $($Arguments -join ' ') failed (exit $code)" }
    $output
}

function Enable-SsrVerifier {
    # The checks a tester runs with `verifier /standard`, as far as the runner
    # can take them without a reboot.
    param(
        # BeforeLoad: before the driver's first load (volatile flags apply to
        # drivers loaded afterwards). Loaded: once it runs (`/dif ... /now`
        # applies only to a running driver).
        [Parameter(Mandatory)] [ValidateSet('BeforeLoad', 'Loaded')] [string] $When
    )
    $dif = Test-SsrDifVerifier
    if ($dif -ne ($When -eq 'Loaded')) {
        Write-Host "Driver Verifier: nothing to do $When on build $([Environment]::OSVersion.Version.Build)"
        return
    }
    if ($dif) {
        $output = Invoke-SsrVerifier (@('/dif') + ($SsrStandardClasses | ForEach-Object { "$_" }) + @('/now', '/driver', $SsrVerifiedImage))
        $name = [regex]::Escape($SsrVerifiedImage)
        if ($output -match "(?im)FailedDrivers\W*.*\b$name\s+(0x[0-9A-Fa-f]+)") {
            $status = $Matches[1]
            $why = if ($status -ieq '0xC0000C7B') { ': the driver is not running' } else { '' }
            throw "verifier could not start checking $SsrVerifiedImage ($status$why)"
        }
    } else {
        Invoke-SsrVerifier @('/volatile', '/flags', ('0x{0:X}' -f $SsrStandardFlags), '/adddriver', $SsrVerifiedImage) | Out-Null
    }
    $state = Get-SsrVerifierState
    if (-not $state.Listed) { throw "Driver Verifier does not list $SsrVerifiedImage after enabling it" }
    if (-not $dif -and $null -ne $state.Flags -and -not ($state.Flags -band $SsrPoolTrackingFlag)) {
        throw ('Driver Verifier flags are 0x{0:X}, without 0x{1:X}' -f $state.Flags, $SsrPoolTrackingFlag)
    }
    $state
}

function Get-SsrVerifierState {
    # `verifier /query` has two shapes. Windows 10 and later print
    # `Verifier Flags: 0x...`, `Pool Allocations Failed Deliberately: N` and,
    # per verified driver, `MODULE: x.sys (load: N / unload: M)` followed by
    # `Current Pool Allocations: ( nonpaged / paged )`. Older ones print
    # `Level:`, `AllocationsFailedDeliberately:` and `Name: x.sys, loads: N,
    # unloads: M` with `CurrentNonPagedPoolAllocations:` lines. Both are read.
    $text = Invoke-SsrVerifier @('/query')
    $state = [ordered]@{
        Text = $text
        Level = $null
        Flags = $null
        FaultsInjected = $null
        PoolAttempted = $null
        Listed = $false
        Loads = $null
        Unloads = $null
        CurrentNonPagedPoolAllocations = $null
        CurrentPagedPoolAllocations = $null
    }
    if ($text -match '(?im)^\s*Verifier Flags:\s*(0x[0-9A-Fa-f]+)') {
        $state.Level = $Matches[1]
        $state.Flags = [Convert]::ToInt64($Matches[1].Substring(2), 16)
    } elseif ($text -match '(?im)^\s*Level:\s*(?:0x)?([0-9A-Fa-f]+)') {
        $state.Level = $Matches[1]
        $state.Flags = [Convert]::ToInt64($Matches[1], 16)
    }
    if ($text -match '(?im)^\s*Pool Allocations Attempted:\s*(\d+)') { $state.PoolAttempted = [long]$Matches[1] }
    if ($text -match '(?im)^\s*Pool Allocations Failed Deliberately:\s*(\d+)') { $state.FaultsInjected = [long]$Matches[1] }
    elseif ($text -match '(?im)^\s*AllocationsFailedDeliberately:\s*(\d+)') { $state.FaultsInjected = [long]$Matches[1] }
    $name = [regex]::Escape($SsrVerifiedImage)
    $module = [regex]::Match($text, "(?ims)^\s*MODULE:\s*$name\s*\(load:\s*(\d+)\s*/\s*unload:\s*(\d+)\)(.*?)(?=^\s*MODULE:|\z)")
    $block = [regex]::Match($text, "(?ims)^\s*Name:\s*$name\b(.*?)(?=^\s*Name:|\z)")
    if ($module.Success) {
        $state.Listed = $true
        $state.Loads = [long]$module.Groups[1].Value
        $state.Unloads = [long]$module.Groups[2].Value
        if ($module.Groups[3].Value -match '(?im)^\s*Current Pool Allocations:\s*\(\s*(\d+)\s*/\s*(\d+)\s*\)') {
            $state.CurrentNonPagedPoolAllocations = [long]$Matches[1]
            $state.CurrentPagedPoolAllocations = [long]$Matches[2]
        }
    } elseif ($block.Success) {
        $state.Listed = $true
        $body = $block.Value
        if ($body -match '(?i)\bloads:\s*(\d+)') { $state.Loads = [long]$Matches[1] }
        if ($body -match '(?i)\bunloads:\s*(\d+)') { $state.Unloads = [long]$Matches[1] }
        if ($body -match '(?im)CurrentNonPagedPoolAllocations:\s*(\d+)') { $state.CurrentNonPagedPoolAllocations = [long]$Matches[1] }
        if ($body -match '(?im)CurrentPagedPoolAllocations:\s*(\d+)') { $state.CurrentPagedPoolAllocations = [long]$Matches[1] }
    }
    [pscustomobject]$state
}

function Get-SsrProblemCode([string] $InstanceId) {
    $value = (Get-PnpDeviceProperty -InstanceId $InstanceId -KeyName 'DEVPKEY_Device_ProblemCode' -ErrorAction SilentlyContinue).Data
    if ($null -eq $value) { 0 } else { [int]$value }
}

function Invoke-SsrRestartCycles {
    # Restarts the device $Count times. Each restart removes the device stack,
    # so the image unloads (Pool Tracking reports leaks then) and loads again,
    # running DriverEntry, AddDevice and StartDevice. Each cycle's problem code
    # is returned for the caller to judge; code 38 fails here, because it means
    # the previous image never unloaded: a reference that was not released.
    param(
        [Parameter(Mandatory)] [string] $InstanceId,
        [int] $Count = 10,
        [int] $SettleSeconds = 3
    )
    $results = @()
    for ($i = 1; $i -le $Count; ++$i) {
        $text = (& pnputil.exe /restart-device $InstanceId 2>&1 | Out-String).Trim()
        $exit = $LASTEXITCODE
        Start-Sleep -Seconds $SettleSeconds
        $problem = Get-SsrProblemCode $InstanceId
        $status = (Get-PnpDevice -InstanceId $InstanceId -ErrorAction SilentlyContinue).Status
        Write-Host "restart $i/$Count -> pnputil exit $exit, status $status, problem $problem"
        if ($problem -eq 38) { throw "restart ${i}: the driver image did not unload (problem 38): $text" }
        $results += [pscustomobject]@{ Cycle = $i; PnputilExit = $exit; Status = "$status"; Problem = $problem }
    }
    $results
}
