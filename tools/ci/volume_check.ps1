# Validates hardware endpoint volume/mute and their driver-ring records.
# Stop bio-live/other ring readers first. Original volume/balance/mute is restored.
param(
    [string] $Summary = (Join-Path (Get-Location) 'volume-summary.json'),
    [string] $Python = 'python'
)
$ErrorActionPreference = 'Stop'
$Summary = [IO.Path]::GetFullPath($Summary)
$folder = [IO.Path]::GetDirectoryName($Summary)
New-Item -ItemType Directory -Force -Path $folder | Out-Null
$run = Join-Path $folder ('volume-check-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $run | Out-Null
$ready = Join-Path $run 'ready.json'
$stop = Join-Path $run 'stop'
$capture = Join-Path $run 'capture.json'
$actionsPath = Join-Path $run 'actions.json'
$verified = Join-Path $run 'verified.json'
$actions = [ordered]@{ endpoints = @(); errors = @() }
$reader = $null
$exitCode = 1
try {
    . (Join-Path $PSScriptRoot 'endpoint-volume.ps1')
    $helper = Join-Path $PSScriptRoot 'volume_check.py'
    # Start-Process joins ArgumentList: quote paths explicitly for directories
    # containing spaces. This child owns the only control-device handle.
    $arguments = @(('"' + $helper + '"'), 'capture', '--out', ('"' + $capture + '"'),
        '--ready', ('"' + $ready + '"'), '--stop', ('"' + $stop + '"'), '--seconds', '120')
    $reader = Start-Process -FilePath $Python -ArgumentList $arguments -PassThru -NoNewWindow `
        -RedirectStandardOutput (Join-Path $run 'capture.stdout.txt') -RedirectStandardError (Join-Path $run 'capture.stderr.txt')
    $deadline = (Get-Date).AddSeconds(15)
    while (-not (Test-Path -LiteralPath $ready)) {
        if ($reader.HasExited) { throw "ring reader exited ($($reader.ExitCode)); see $run" }
        if ((Get-Date) -gt $deadline) { throw 'ring reader did not become ready' }
        Start-Sleep -Milliseconds 50
    }
    $info = Get-Content -Raw -LiteralPath $ready | ConvertFrom-Json
    $targets = @(Get-BioVolumeTargets | Where-Object { $_.state -eq 1 })
    foreach ($ep in 0..3) {
        if (-not ($info.endpoint_mask -band (1 -shl $ep))) { continue }
        $found = @($targets | Where-Object { $_.endpoint -eq $ep })
        if ($found.Count -ne 1) { throw "endpoint $ep needs one active stable-ID match, got $($found.Count)" }
        $target = $found[0]
        $control = $null
        $original = $null
        $item = [ordered]@{ endpoint = $ep; name = $target.name; original = $null; restored = $null; stages = @() }
        try {
            $control = [BioEndpointVolume]::new($target.id)
            $original = $control.Snapshot()
            $item.original = $original
            if (($original.hardware_support -band 3) -ne 3) { throw "endpoint $ep lacks hardware volume/mute support" }
            # Establish a distinct starting level so an unchanged -20 dB SET
            # cannot pass using only a repeated state. Keep every channel equal
            # during the test, restoring the original balance in finally.
            $control.SetMute($true)
            $control.SetMaster(-30)
            for ($channel = 0; $channel -lt $original.channels_db.Length; $channel++) { $control.SetChannel($channel, -30) }
            $control.SetMute($false)
            Start-Sleep -Milliseconds 200
            foreach ($name in 'level', 'mute_on', 'mute_off') {
                $start = [BioEndpointVolume]::Qpc()
                switch ($name) {
                    'level' { $control.SetMaster(-20) }
                    'mute_on' { $control.SetMute($true) }
                    'mute_off' { $control.SetMute($false) }
                }
                Start-Sleep -Milliseconds 250
                $expected = $control.Snapshot()
                $end = [BioEndpointVolume]::Qpc()
                $item.stages += [ordered]@{ name = $name; start_qpc = $start; end_qpc = $end; expected = $expected }
            }
        } catch {
            $actions.errors += "endpoint ${ep}: $($_.Exception.Message)"
        } finally {
            if ($control) {
                if ($original) {
                    try { $control.Restore($original); $item.restored = $control.Snapshot() }
                    catch { $actions.errors += "endpoint $ep RESTORE FAILED: $($_.Exception.Message)" }
                }
                $control.Dispose()
            }
            $actions.endpoints += $item
        }
    }
} catch {
    $actions.errors += $_.Exception.Message
} finally {
    $actions | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 -LiteralPath $actionsPath
    if ($reader) {
        [IO.File]::WriteAllText($stop, 'stop')
        if (-not $reader.WaitForExit(10000)) {
            Stop-Process -Id $reader.Id -Force
            $actions.errors += 'ring reader did not stop within 10 seconds'
            $actions | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 -LiteralPath $actionsPath
        }
    }
}
try {
    & $Python (Join-Path $PSScriptRoot 'volume_check.py') verify --capture $capture --actions $actionsPath --summary $verified
    if ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $verified)) { $exitCode = 0 }
    elseif (-not (Test-Path -LiteralPath $verified)) { throw "Python verification failed ($LASTEXITCODE) without a summary" }
    Copy-Item -LiteralPath $verified -Destination $Summary -Force
} catch {
    $exitCode = 1
    [ordered]@{ passed = $false; errors = @($_.Exception.Message) + $actions.errors; evidence = $run } |
        ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 -LiteralPath $Summary
}
Write-Host "volume check evidence: $run"
Write-Host "volume summary: $Summary"
exit $exitCode
