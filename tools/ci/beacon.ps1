# Diagnosis only (ADR 0027): reports how far a CI job got as a commit status,
# every few seconds, so a job whose runner is lost (a bugcheck leaves no log
# and no artifact) still says what it was doing and for how long. The status
# lives on GitHub, not on the runner.
#
#   ./tools/ci/beacon.ps1 -Start -Context 'diag/full'   # starts the reporter
#   ./tools/ci/beacon.ps1 -Mark 'installing'            # names the phase
#   ./tools/ci/beacon.ps1 -Finish -Context 'diag/full' -Description 'survived'
#
# The job needs `permissions: statuses: write` and GITHUB_TOKEN and BEACON_SHA
# (the pull request's head) in its environment. The reporter samples the
# device's state with each report; it posts every 3 s for a minute after each
# phase change and every 20 s after that, which keeps a job well inside the
# token's rate limit.
param(
    [switch] $Start,
    [switch] $Run,
    [switch] $Finish,
    [string] $Mark,
    [string] $Context,
    [string] $Description = '',
    [string] $HardwareId = 'Root\SpatialSoftReceiver'
)
$ErrorActionPreference = 'Stop'
$markFile = Join-Path $env:RUNNER_TEMP 'beacon-mark.txt'

function Send-BeaconStatus([string] $State, [string] $Text) {
    if ($Text.Length -gt 140) { $Text = $Text.Substring(0, 140) }
    $body = @{ state = $State; context = $Context; description = $Text } | ConvertTo-Json
    $headers = @{ Authorization = "Bearer $env:GITHUB_TOKEN"; Accept = 'application/vnd.github+json' }
    $uri = "https://api.github.com/repos/$env:GITHUB_REPOSITORY/statuses/$env:BEACON_SHA"
    try { Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -Body $body -TimeoutSec 10 | Out-Null }
    catch { Write-Host "beacon: $($_.Exception.Message)" }
}

if ($Mark) {
    "$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())|$Mark" | Set-Content -Path $markFile -Encoding UTF8
    Write-Host "beacon mark: $Mark"
    exit 0
}

if ($Start) {
    if (-not $Context) { throw 'beacon: -Context is required' }
    $self = $MyInvocation.MyCommand.Path
    $arguments = @('-NoProfile', '-File', $self, '-Run', '-Context', $Context, '-HardwareId', $HardwareId)
    # A separate process outlives this step; the runner ends it with the job.
    Start-Process -FilePath pwsh -ArgumentList $arguments -WindowStyle Hidden | Out-Null
    Write-Host "beacon started for $Context"
    exit 0
}

if ($Finish) {
    if (-not $Context) { throw 'beacon: -Context is required' }
    Send-BeaconStatus 'success' $Description
    exit 0
}

if ($Run) {
    $began = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $lastSent = 0
    $lastText = ''
    while ($true) {
        $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        $since = $began
        $phase = 'started'
        if (Test-Path $markFile) {
            $parts = (Get-Content -Raw $markFile).Trim() -split '\|', 2
            if ($parts.Count -eq 2) { $since = [long]$parts[0]; $phase = $parts[1] }
        }
        $device = Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $HardwareId } | Select-Object -First 1
        $state = if ($device) { "$($device.Status) problem $($device.Problem)" } else { 'no device' }
        $text = "$phase +$($now - $since)s, job +$($now - $began)s; device $state"
        $period = if ($now - $since -lt 60) { 3 } else { 20 }
        if ($now - $lastSent -ge $period -or ($text -split ' \+', 2)[0] -ne ($lastText -split ' \+', 2)[0]) {
            Send-BeaconStatus 'pending' $text
            $lastSent = $now
            $lastText = $text
        }
        Start-Sleep -Seconds 1
    }
}
