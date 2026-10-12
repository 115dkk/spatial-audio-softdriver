# Plays a fixture dump through the live path for minutes and fails on any
# fault, device restart, lost byte or underrun while the source streams.
# CI only (ci.yml, audio): the render endpoint is VB-CABLE's "Speakers (VB-Audio Virtual Cable)".
#
#   ./tools/ci/soak.ps1 -BioLive target/release/bio-live.exe -Fixtures $fx `
#       -Name exclusive -Minutes 5 [-Shared] [-CpuLoad] [-PageHeap]
#
# -CpuLoad keeps every logical processor busy at normal priority, so the
# real-time threads have to win through MMCSS. -PageHeap turns on full page
# heap (gflags +hpa) for bio-live.exe, so a heap overrun in the FFI code
# faults at once instead of corrupting memory quietly.
param(
    [Parameter(Mandatory)] [string] $BioLive,
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

$dumpPath = Join-Path $Fixtures "$Dump.biodump"
$profilePath = Join-Path $Fixtures 'profile.toml'
$report = Join-Path $env:RUNNER_TEMP "soak-$Name.json"
$log = Join-Path $env:RUNNER_TEMP "soak-$Name.log"

$seconds = $Minutes * 60

$ifeo = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\bio-live.exe'
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
    Write-Host "bio-live $($arguments -join ' ')"
    & $BioLive @arguments 2>&1 | Tee-Object -FilePath $log
    if ($LASTEXITCODE -ne 0) { throw "bio-live exited with $LASTEXITCODE" }
}
finally {
    $burners | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
    if ($PageHeap) { Remove-Item -Force -Recurse -Path $ifeo -ErrorAction SilentlyContinue }
}

$r = Get-Content $report -Raw | ConvertFrom-Json
$problems = @()
if ($r.faults.Count -gt 0) { $problems += "faults: $($r.faults -join '; ')" }
if ($r.output_restarts -ne 0) { $problems += "$($r.output_restarts) device restarts" }
if ($r.lost_bytes -ne 0) { $problems += "$($r.lost_bytes) bytes lost" }
if ($r.overruns -ne 0) { $problems += "$($r.overruns) overruns" }
if ($r.underruns_while_streaming -ne 0) { $problems += "$($r.underruns_while_streaming) underruns while streaming" }
if ($r.seconds -lt 0.98 * $seconds) { $problems += "ran $($r.seconds) s of $seconds s" }
if ($r.blocks -eq 0) { $problems += 'no blocks rendered' }

$row = "| $Name | $([math]::Round($r.seconds)) s | $($r.underruns_while_streaming) | $($r.output_restarts) | " +
    "$([math]::Round($r.latency_p50_ms, 1)) / $([math]::Round($r.latency_p99_ms, 1)) ms | " +
    "$([math]::Round($r.ring_trough_min_ms, 1)) ms | $([math]::Round($r.clock_ppm)) ppm | " +
    "$(if ($problems) { 'FAIL' } else { 'ok' }) |"
Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value $row
if ($problems) { throw "soak ${Name}: $($problems -join ', ')" }
Write-Host "soak ${Name}: ok"
