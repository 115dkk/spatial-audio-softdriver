# Reads the policy and actually calls IAudioClient::Initialize; never writes it.
# Close all players first: a busy endpoint is a failure, not a skipped check.
param(
    [Parameter(Mandatory)] [string] $Summary,
    [ValidateRange(1, 7)] [int] $ExpectMask = 7,
    [string] $InstanceId = ''
)
$ErrorActionPreference = 'Stop'
$results = @()
$problems = @()
try {
    . (Join-Path $PSScriptRoot 'exclusive-policy.ps1')
    $targets = @(Get-SsrPolicyTargets -InstanceId $InstanceId | Where-Object { $_.state -eq 1 })
    foreach ($ep in 0..2) {
        $matches = @($targets | Where-Object endpoint -eq $ep)
        if (-not ($ExpectMask -band (1 -shl $ep))) {
            if ($matches.Count) { $problems += "disabled endpoint $ep is active" }
            continue
        }
        if ($matches.Count -ne 1) {
            $problems += "endpoint ${ep}: expected one active device, found $($matches.Count)"
            continue
        }
        $target = $matches[0]
        $control = $null
        $item = [ordered]@{ endpoint = $ep; name = $target.name; id = $target.id; policy = $null; probe = $null; passed = $false; error = $null }
        try {
            $control = [SsrExclusivePolicy]::new($target.id)
            $item.policy = $control.Read()
            $expected = if ($ep -eq 2) { 1 } else { 0 }
            $policyOk = $item.policy.allow_exclusive -eq $expected -and $item.policy.exclusive_priority -eq $expected
            $item.probe = $control.Probe()
            # 0x8889000E is AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED. Windows may give
            # it while checking a format or while opening the stream; either
            # proves the policy, unlike a format the endpoint does not take.
            $refused = $item.probe.support_hr -eq '0x8889000E' -or ($item.probe.supported -and $item.probe.initialize_hr -eq '0x8889000E')
            $opened = $item.probe.supported -and $item.probe.initialize_hr -eq '0x00000000'
            $behaviorOk = if ($ep -eq 2) { $opened } else { $refused }
            $item.passed = $policyOk -and $behaviorOk
            if (-not $item.passed) {
                $want = if ($ep -eq 2) { 'a supported exclusive format must open (0x00000000)' } else { 'exclusive open must be refused with 0x8889000E' }
                $problems += "endpoint ${ep}: policy must be $expected/$expected and $want"
            }
        } catch {
            $item.error = $_.Exception.Message
            $problems += "endpoint ${ep}: $($item.error)"
        } finally {
            if ($control) { $control.Dispose() }
            $results += [pscustomobject]$item
        }
    }
} catch { $problems += $_.Exception.Message }
$report = [ordered]@{
    passed = $problems.Count -eq 0
    expected_mask = $ExpectMask
    policy_source = 'private MMDevice properties {b3f8fa53-0004-438e-9003-51a46e139bfc},3/4; this check does not restart services'
    endpoints = $results
    problems = $problems
}
$report | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 -LiteralPath $Summary
$report | ConvertTo-Json -Depth 8 | Write-Host
exit $(if ($report.passed) { 0 } else { 1 })
