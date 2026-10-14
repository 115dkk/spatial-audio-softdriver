# Plays a fixture dump through the live path for minutes and fails on any
# fault, device restart, lost byte or underrun while the source streams.
# CI only (ci.yml, audio): the render endpoint is VB-CABLE's "Speakers (VB-Audio Virtual Cable)".
#
#   ./tools/ci/soak.ps1 -SsrLive target/release/ssr-live.exe -Fixtures $fx `
#       -Name exclusive -Minutes 5 [-Shared] [-CpuLoad] [-PageHeap]
#
# -CpuLoad keeps every logical processor busy at normal priority, so the
# real-time threads have to win through MMCSS. -PageHeap turns on full page
# heap (gflags +hpa) for ssr-live.exe, so a heap overrun in the FFI code
# faults at once instead of corrupting memory quietly.
param(
    [Parameter(Mandatory)] [string] $SsrLive,
    [Parameter(Mandatory)] [string] $Fixtures,
    [Parameter(Mandatory)] [string] $Name,
    [double] $Minutes = 2,
    [string] $Dump = 'pcm71',
    # Device period for exclusive mode; VB-CABLE keeps nothing under ~16 ms.
    [double] $PeriodMs = 0,
    [switch] $Shared,
    [switch] $CpuLoad,
    [switch] $PageHeap
)
$ErrorActionPreference = 'Stop'

$dumpPath = Join-Path $Fixtures "$Dump.ssrdump"
$profilePath = Join-Path $Fixtures 'profile.toml'
$report = Join-Path $env:RUNNER_TEMP "soak-$Name.json"
$log = Join-Path $env:RUNNER_TEMP "soak-$Name.log"

$seconds = $Minutes * 60

$ifeo = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\ssr-live.exe'
$burners = @()
try {
    if ($PageHeap) {
        New-Item -Force -Path $ifeo | Out-Null
        # FLG_HEAP_PAGE_ALLOCS, full page heap with a guard page after every block.
        Set-ItemProperty -Path $ifeo -Name GlobalFlag -Value 0x02000000 -Type DWord
        Set-ItemProperty -Path $ifeo -Name PageHeapFlags -Value 0x3 -Type DWord
    }
    if ($CpuLoad) {
        foreach ($i in 1..[Environment]::ProcessorCount) {
            $burners += Start-Process -PassThru -WindowStyle Hidden powershell -ArgumentList '-NoProfile', '-Command', 'while ($true) { }'
        }
    }

    $arguments = @('play', '--from-dump', $dumpPath, '--profile', $profilePath,
        '--output', 'VB-Audio Virtual Cable', '--loop', '--seconds', $seconds, '--report', $report)
    if ($Shared) { $arguments += '--shared' }
    if ($PeriodMs -gt 0) { $arguments += @('--period-ms', $PeriodMs) }
    Write-Host "ssr-live $($arguments -join ' ')"
    & $SsrLive @arguments 2>&1 | Tee-Object -FilePath $log
    if ($LASTEXITCODE -ne 0) { throw "ssr-live exited with $LASTEXITCODE" }
}
finally {
    $burners | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
    if ($PageHeap) { Remove-Item -Force -Recurse -Path $ifeo -ErrorAction SilentlyContinue }
}

$r = Get-Content $report -Raw | ConvertFrom-Json
$problems = @()
$required = @('faults', 'output_restarts', 'lost_bytes', 'overruns',
    'underruns_while_streaming', 'dropped_frames', 'lane_underrun_frames',
    'output_overflow_frames', 'dropped_events', 'idle_blocks', 'endpoints',
    'seconds', 'blocks', 'output_reconnects', 'output_cadence_faults',
    'output_mode', 'output_fell_back')
$missing = @($required | Where-Object { -not $r.PSObject.Properties[$_] })
if ($missing) { $problems += "missing report keys: $($missing -join ', ')" }
if ($r.PSObject.Properties['faults'] -and $r.faults.Count -gt 0) { $problems += "faults: $($r.faults -join '; ')" }
foreach ($counter in @('output_restarts', 'lost_bytes', 'overruns',
        'underruns_while_streaming', 'dropped_frames', 'lane_underrun_frames',
        'output_overflow_frames', 'dropped_events', 'output_reconnects',
        'output_cadence_faults')) {
    if ($r.PSObject.Properties[$counter] -and $r.$counter -ne 0) {
        $problems += "$counter is $($r.$counter)"
    }
}
# output_mode is the mode the device was actually opened in.
$expectedMode = if ($Shared) { 'shared' } else { 'exclusive' }
if ($r.PSObject.Properties['output_fell_back'] -and $r.output_fell_back) { $problems += 'exclusive output fell back to shared mode' }
if ($r.PSObject.Properties['output_mode'] -and $r.output_mode -ne $expectedMode) { $problems += "output opened $($r.output_mode), expected $expectedMode" }
if ($r.PSObject.Properties['endpoints']) {
    if (@($r.endpoints).Count -ne 4) { $problems += 'report must contain four endpoint states' }
    foreach ($endpoint in @($r.endpoints)) {
        $endpointMissing = @('endpoint', 'format', 'epoch', 'unsupported') |
            Where-Object { -not $endpoint.PSObject.Properties[$_] }
        if ($endpointMissing) {
            $problems += "endpoint state missing keys: $($endpointMissing -join ', ')"
        }
        if ($endpoint.PSObject.Properties['unsupported'] -and $null -ne $endpoint.unsupported) {
            $problems += "endpoint $($endpoint.endpoint) unsupported: $($endpoint.unsupported)"
        }
    }
}
if ($r.PSObject.Properties['seconds'] -and $r.seconds -lt 0.98 * $seconds) { $problems += "ran $($r.seconds) s of $seconds s" }
if ($r.PSObject.Properties['blocks'] -and $r.blocks -eq 0) { $problems += 'no blocks rendered' }
# Far above any real chain: such a sample means a block carried a source
# time from another clock (see loopback_check.py).
foreach ($key in @('latency_p50_ms', 'latency_p99_ms')) {
    if ($r.PSObject.Properties[$key] -and $r.$key -gt 1000) { $problems += "$key is $($r.$key) ms, above 1000 ms" }
}

$row = "| $Name | $([math]::Round($r.seconds)) s | $($r.underruns_while_streaming) | $($r.output_restarts) | " +
    "$([math]::Round($r.latency_p50_ms, 1)) / $([math]::Round($r.latency_p99_ms, 1)) ms | " +
    "$([math]::Round($r.ring_trough_min_ms, 1)) ms | $([math]::Round($r.clock_ppm)) ppm | " +
    "$(if ($problems) { 'FAIL' } else { 'ok' }) |"
Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value $row
if ($problems) { throw "soak ${Name}: $($problems -join ', ')" }
Write-Host "soak ${Name}: ok"
