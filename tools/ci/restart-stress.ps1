# Starts and stops bio-live many times on VB-CABLE, exclusive and shared, the
# second half of the runs under CPU load (ADR 0026). CI only.
#
# A soak saw bio-live die with 0xC0000005 while it stopped under CPU load,
# before it printed its totals; every run here must open, play and exit with 0.
# Crash dumps go where tools/ci/crash-dumps.ps1 pointed WER.
param(
    [Parameter(Mandatory)] [string] $BioLive,
    [Parameter(Mandatory)] [string] $Fixtures,
    [int] $Runs = 40,
    [double] $Seconds = 2,
    [double] $PeriodMs = 20
)
$ErrorActionPreference = 'Stop'

$dumpPath = Join-Path $Fixtures 'pcm71.biodump'
$profilePath = Join-Path $Fixtures 'profile.toml'
$burners = @()
$failures = @()
try {
    foreach ($run in 1..$Runs) {
        $loaded = $run -gt $Runs / 2
        if ($loaded -and -not $burners) {
            foreach ($i in 1..[Environment]::ProcessorCount) {
                $burners += Start-Process -PassThru -WindowStyle Hidden powershell -ArgumentList '-NoProfile', '-Command', 'while ($true) { }'
            }
        }
        $shared = $run % 2 -eq 0
        $arguments = @('play', '--from-dump', $dumpPath, '--profile', $profilePath,
            '--output', 'VB-Audio Virtual Cable', '--loop', '--seconds', $Seconds)
        if ($shared) { $arguments += '--shared' } elseif ($PeriodMs -gt 0) { $arguments += @('--period-ms', $PeriodMs) }
        $output = & $BioLive @arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
        $label = "run $run ($(if ($shared) { 'shared' } else { 'exclusive' })$(if ($loaded) { ', CPU load' }))"
        if ($code -ne 0) {
            $failures += ('{0}: exit 0x{1:X8}' -f $label, $code)
            Write-Host "::group::$label output"
            Write-Host $output
            Write-Host '::endgroup::'
        } elseif ($output -notmatch 'stopped: ') {
            $failures += "${label}: no totals printed"
        }
    }
}
finally {
    $burners | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
}

$verdict = if ($failures) { "FAIL: $($failures -join '; ')" } else { 'ok' }
Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value "`nStart and stop, $Runs runs of $Seconds s: $verdict`n"
if ($failures) { throw "start and stop: $($failures -join '; ')" }
Write-Host "start and stop: $Runs runs ok"
