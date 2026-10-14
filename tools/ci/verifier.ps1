# Driver Verifier for the CI runners, which cannot reboot mid-job.
#
# Volatile settings apply to drivers loaded afterwards, but the set of options
# that may be volatile differs by Windows version
# (https://learn.microsoft.com/windows-hardware/drivers/devtest/using-volatile-settings):
# Windows 10 / Server 2022 (build < 22000) take special pool, IRQL, pool
# tracking, I/O, deadlock, DMA, security and miscellaneous checks (0x9BB) and
# randomized low-resources simulation (/faults); Windows 11 / Server 2025 take
# only a few flags that way and start the other rule classes with
# `/dif <classes> /now` instead, including systematic low-resources simulation
# (class 19, https://learn.microsoft.com/windows-hardware/drivers/devtest/systematic-low-resource-simulation).
# Every verifier.exe call is checked: exit 1 is an error, 2 means the setting
# needs a reboot this job will never get.
#
#   . ./tools/ci/verifier.ps1
#   Enable-SsrVerifier -Mode Standard      # before the driver first loads
#   Enable-SsrVerifier -Mode Faults        # standard checks plus allocation failures
#   Get-SsrVerifierState                   # parsed `verifier /query`
#   Invoke-SsrRestartCycles -InstanceId $id -Count 20
$ErrorActionPreference = 'Stop'

$SsrVerifiedImage = 'spatialsoftreceiver.sys'
# Rule classes for `/dif ... /now` (verifier-command-line): 1 special pool,
# 2 force IRQL checking, 4 pool tracking, 5 I/O verification, 9 security
# checks, 12 miscellaneous checks, 18 DDI compliance checking.
$SsrStandardClasses = @(1, 2, 4, 5, 9, 12, 18)
# The same checks as volatile flags where Windows 10 allows them, plus
# deadlock detection (0x20) and DMA checking (0x80).
$SsrStandardFlags = '0x9BB'

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
        # Randomized failures: chances in 10,000 per eligible call.
        [int] $Probability = 1000
    )
    if (Test-SsrDifVerifier) {
        $classes = @($SsrStandardClasses)
        if ($Mode -eq 'Faults') { $classes += 19 }
        Invoke-SsrVerifier (@('/dif') + ($classes | ForEach-Object { "$_" }) + @('/now', '/driver', $SsrVerifiedImage)) | Out-Null
        if ($Mode -eq 'Faults') {
            # Systematic: each distinct call stack in the driver fails once.
            Invoke-SsrVerifier @('/faultssystematic', 'enableruntime') | Out-Null
        }
    } else {
        Invoke-SsrVerifier @('/volatile', '/flags', $SsrStandardFlags, '/adddriver', $SsrVerifiedImage) | Out-Null
        if ($Mode -eq 'Faults') {
            # Randomized: no tag or application filter (PnP start runs in the
            # System process), and no delay after boot. The empty filters are
            # positional `""` placeholders, so cmd passes them verbatim.
            $line = "verifier.exe /volatile /faults $Probability `"`" `"`" 0"
            Write-Host $line
            $output = (& cmd.exe /d /c $line 2>&1 | Out-String).TrimEnd()
            $code = $LASTEXITCODE
            if ($output) { Write-Host $output }
            if ($code -ne 0) { throw "$line failed (exit $code)" }
        }
    }
    Get-SsrVerifierState | Out-Null
}

function Get-SsrVerifierState {
    # `verifier /query` prints global counters and, per verified driver, a
    # `Name:` line with loads and unloads followed by its pool counters.
    $text = Invoke-SsrVerifier @('/query')
    $state = [ordered]@{
        Text = $text
        Level = $null
        FaultsInjected = $null
        Listed = $false
        Loads = $null
        Unloads = $null
        CurrentNonPagedPoolAllocations = $null
        CurrentPagedPoolAllocations = $null
    }
    if ($text -match '(?im)^\s*Level:\s*([0-9A-Fa-fx]+)') { $state.Level = $Matches[1] }
    if ($text -match '(?im)^\s*AllocationsFailedDeliberately:\s*(\d+)') { $state.FaultsInjected = [long]$Matches[1] }
    $name = [regex]::Escape($SsrVerifiedImage)
    $block = [regex]::Match($text, "(?ims)^\s*Name:\s*$name\b(.*?)(?=^\s*Name:|\z)")
    if ($block.Success) {
        $state.Listed = $true
        $body = $block.Value
        if ($body -match '(?i)loads:\s*(\d+)') { $state.Loads = [long]$Matches[1] }
        if ($body -match '(?i)unloads:\s*(\d+)') { $state.Unloads = [long]$Matches[1] }
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
