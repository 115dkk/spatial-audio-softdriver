# Driver Verifier for the CI runners, which cannot reboot mid-job.
#
# The way to start Driver Verifier without a reboot differs by Windows version
# (https://learn.microsoft.com/windows-hardware/drivers/devtest/using-volatile-settings):
#
# - Windows 10 / Server 2022 (build < 22000): volatile flags, set before the
#   driver loads, take special pool, IRQL, pool tracking, I/O, deadlock, DMA,
#   security and miscellaneous checks (0x9BB) and randomized low-resources
#   simulation (0x4, `/faults`). `/volatile /faults` replaces the whole flag
#   word with 0x4, so it runs first and the flags are set after it.
# - Windows 11 / Server 2025: `/dif <classes> /now` starts rule classes on a
#   driver that is already running; on one that is not it lands in
#   FailedDrivers with 0xC0000C7B (STATUS_DIF_VOLATILE_DRIVER_IS_NOT_RUNNING).
#   Class 19 with `/faultssystematic enableruntime` fails each distinct
#   allocation call stack once
#   (https://learn.microsoft.com/windows-hardware/drivers/devtest/systematic-low-resource-simulation).
#
# So the workflow calls Enable-SsrVerifier twice, once before the install and
# once after the driver has loaded; each call does the part that belongs to
# that moment on this Windows and nothing on the other. Every verifier.exe call
# is checked: exit 1 is an error, 2 means the setting needs a reboot this job
# will never get, and the step fails unless `/query` then lists the driver.
#
#   . ./tools/ci/verifier.ps1
#   Enable-SsrVerifier -Mode Standard -When BeforeLoad   # before the install
#   Enable-SsrVerifier -Mode Standard -When Loaded       # after the driver loaded
#   Enable-SsrVerifier -Mode Faults ...                  # the same plus allocation failures
#   Get-SsrVerifierState                                 # parsed `verifier /query`
#   Invoke-SsrRestartCycles -InstanceId $id -Count 20
$ErrorActionPreference = 'Stop'

$SsrVerifiedImage = 'spatialsoftreceiver.sys'
# Rule classes for `/dif ... /now` (verifier-command-line): 1 special pool,
# 2 force IRQL checking, 4 pool tracking, 5 I/O verification, 9 security
# checks, 12 miscellaneous checks, 18 DDI compliance checking.
$SsrStandardClasses = @(1, 2, 4, 5, 9, 12, 18)
# The same checks as volatile flags where Windows 10 allows them, plus
# deadlock detection (0x20) and DMA checking (0x80).
$SsrStandardFlags = 0x9BB
$SsrLowResourcesFlag = 0x4
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
    param(
        # Standard: the checks a tester runs with `verifier /standard`, as far
        # as the runner can take them without a reboot. Faults: the same plus
        # injected allocation failures, so error branches run.
        [Parameter(Mandatory)] [ValidateSet('Standard', 'Faults')] [string] $Mode,
        # BeforeLoad: before the driver's first load (volatile flags apply to
        # drivers loaded afterwards). Loaded: once it runs (`/dif ... /now`
        # applies only to a running driver).
        [Parameter(Mandatory)] [ValidateSet('BeforeLoad', 'Loaded')] [string] $When,
        # Randomized failures: chances in 10,000 per eligible call.
        [int] $Probability = 1000
    )
    $dif = Test-SsrDifVerifier
    if ($dif -ne ($When -eq 'Loaded')) {
        Write-Host "Driver Verifier: nothing to do $When on build $([Environment]::OSVersion.Version.Build)"
        return
    }
    if ($dif) {
        $classes = @($SsrStandardClasses)
        if ($Mode -eq 'Faults') { $classes += 19 }
        $output = Invoke-SsrVerifier (@('/dif') + ($classes | ForEach-Object { "$_" }) + @('/now', '/driver', $SsrVerifiedImage))
        $name = [regex]::Escape($SsrVerifiedImage)
        if ($output -match "(?im)FailedDrivers\W*.*\b$name\s+(0x[0-9A-Fa-f]+)") {
            $status = $Matches[1]
            $why = if ($status -ieq '0xC0000C7B') { ': the driver is not running' } else { '' }
            throw "verifier could not start checking $SsrVerifiedImage ($status$why)"
        }
        if ($Mode -eq 'Faults') {
            # Systematic: each distinct call stack in the driver fails once.
            Invoke-SsrVerifier @('/faultssystematic', 'enableruntime') | Out-Null
        }
    } else {
        $flags = $SsrStandardFlags
        if ($Mode -eq 'Faults') {
            # Randomized: no tag or application filter (PnP start runs in the
            # System process), and no delay after boot. The empty filters are
            # positional `""` placeholders, so cmd passes them verbatim. This
            # sets the flag word to 0x4 alone, so it goes first.
            $line = "verifier.exe /volatile /faults $Probability `"`" `"`" 0"
            Write-Host $line
            $output = (& cmd.exe /d /c $line 2>&1 | Out-String).TrimEnd()
            $code = $LASTEXITCODE
            if ($output) { Write-Host $output }
            if ($code -ne 0) { throw "$line failed (exit $code)" }
            $flags = $flags -bor $SsrLowResourcesFlag
        }
        Invoke-SsrVerifier @('/volatile', '/flags', ('0x{0:X}' -f $flags), '/adddriver', $SsrVerifiedImage) | Out-Null
    }
    $state = Get-SsrVerifierState
    if (-not $state.Listed) { throw "Driver Verifier does not list $SsrVerifiedImage after enabling it" }
    if (-not $dif) {
        foreach ($bit in @($SsrPoolTrackingFlag) + @(if ($Mode -eq 'Faults') { $SsrLowResourcesFlag })) {
            if ($null -ne $state.Flags -and -not ($state.Flags -band $bit)) {
                throw ('Driver Verifier flags are 0x{0:X}, without 0x{1:X}' -f $state.Flags, $bit)
            }
        }
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
    # running DriverEntry, AddDevice and StartDevice. Under fault injection a
    # start may fail; that is a handled failure (problem code 10, 31, 37 or 39),
    # not a test failure. Code 38 means the previous image never unloaded: a
    # reference that was not released.
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
