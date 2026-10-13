# Starts and stops bio-live many times on VB-CABLE, exclusive and shared, the
# second half of the runs under CPU load (ADR 0026). CI only.
#
# bio-live died with 0xC0000005 while stopping exclusive streams under CPU
# load, before it printed its totals; every run here must open, play, print its
# totals and exit with 0. WER wrote no dumps on the runners, so the exclusive
# runs under load go through cdb when the Windows SDK debuggers are installed:
# an access violation then prints the faulting stack and every thread.
param(
    [Parameter(Mandatory)] [string] $BioLive,
    [Parameter(Mandatory)] [string] $Fixtures,
    [int] $Runs = 60,
    [double] $Seconds = 2,
    [double] $PeriodMs = 20,
    # Folder with bio_live.pdb, for cdb's stacks.
    [string] $Symbols = ''
)
$ErrorActionPreference = 'Stop'

$dumpPath = Join-Path $Fixtures 'pcm71.biodump'
$profilePath = Join-Path $Fixtures 'profile.toml'
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$cdb = Get-ChildItem -Path (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\Debuggers') -Recurse -Filter cdb.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq $arch } | Select-Object -First 1
if ($cdb) { Write-Host "exclusive runs under CPU load go through $($cdb.FullName)" }
else { Write-Host '::warning::cdb.exe not found; crashes show only their exit code' }
$symbolPath = "$Symbols;srv*$(Join-Path $env:RUNNER_TEMP 'symbols')*https://msdl.microsoft.com/download/symbols"

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
        $report = Join-Path $env:RUNNER_TEMP "restart-$run.json"
        $arguments = @('play', '--from-dump', $dumpPath, '--profile', $profilePath,
            '--output', 'VB-Audio Virtual Cable', '--loop', '--seconds', $Seconds,
            '--report', $report)
        if ($shared) { $arguments += '--shared' } elseif ($PeriodMs -gt 0) { $arguments += @('--period-ms', $PeriodMs) }
        $label = "run $run ($(if ($shared) { 'shared' } else { 'exclusive' })$(if ($loaded) { ', CPU load' }))"
        if ($cdb -and $loaded -and -not $shared) {
            # Break on a first-chance access violation, print the stacks and
            # end the process; a clean run exits through -G without stopping.
            $commands = 'sxe -c ".lastevent; .ecxr; kpn 60; ~*kn 40; q" av; g'
            $output = & $cdb.FullName -G -lines -y $symbolPath -c $commands $BioLive @arguments 2>&1 | Out-String
            $crashed = $output -match 'Access violation|c0000005'
            $code = if ($crashed) { 0xC0000005 } else { 0 }
        } else {
            $output = & $BioLive @arguments 2>&1 | Out-String
            $code = $LASTEXITCODE
        }
        if ($code -ne 0) {
            $failures += ('{0}: exit 0x{1:X8}' -f $label, $code)
            Write-Host "::group::$label output"
            Write-Host $output
            Write-Host '::endgroup::'
        } elseif ($output -notmatch 'stopped: ') {
            $failures += "${label}: no totals printed"
            Write-Host "::group::$label output"
            Write-Host $output
            Write-Host '::endgroup::'
        } elseif (-not (Test-Path $report)) {
            $failures += "${label}: no report written"
        } else {
            $r = Get-Content $report -Raw | ConvertFrom-Json
            $required = @('faults', 'output_restarts', 'underruns_while_streaming',
                'dropped_frames', 'lane_underrun_frames', 'output_overflow_frames',
                'dropped_events', 'idle_blocks', 'endpoints', 'output_reconnects',
                'output_cadence_faults', 'output_mode', 'output_fell_back')
            $missing = @($required | Where-Object { -not $r.PSObject.Properties[$_] })
            $runProblems = @()
            if ($missing) { $runProblems += "missing report keys: $($missing -join ', ')" }
            if ($r.PSObject.Properties['faults'] -and $r.faults.Count -gt 0) {
                $runProblems += "faults: $($r.faults -join '; ')"
            }
            foreach ($counter in @('output_restarts', 'underruns_while_streaming',
                    'dropped_frames', 'lane_underrun_frames', 'output_overflow_frames',
                    'dropped_events', 'output_reconnects', 'output_cadence_faults')) {
                if ($r.PSObject.Properties[$counter] -and $r.$counter -ne 0) {
                    $runProblems += "$counter is $($r.$counter)"
                }
            }
            $expectedMode = if ($shared) { 'shared' } else { 'exclusive' }
            if ($r.PSObject.Properties['output_fell_back'] -and $r.output_fell_back) {
                $runProblems += 'exclusive output fell back to shared mode'
            }
            if ($r.PSObject.Properties['output_mode'] -and $r.output_mode -ne $expectedMode) {
                $runProblems += "output opened $($r.output_mode), expected $expectedMode"
            }
            if ($r.PSObject.Properties['endpoints']) {
                if (@($r.endpoints).Count -ne 4) {
                    $runProblems += 'report must contain four endpoint states'
                }
                foreach ($endpoint in @($r.endpoints)) {
                    $endpointMissing = @('endpoint', 'format', 'epoch', 'unsupported') |
                        Where-Object { -not $endpoint.PSObject.Properties[$_] }
                    if ($endpointMissing) {
                        $runProblems += "endpoint state missing keys: $($endpointMissing -join ', ')"
                    }
                    if ($endpoint.PSObject.Properties['unsupported'] -and $null -ne $endpoint.unsupported) {
                        $runProblems += "endpoint $($endpoint.endpoint) unsupported: $($endpoint.unsupported)"
                    }
                }
            }
            if ($runProblems) { $failures += "${label}: $($runProblems -join ', ')" }
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
