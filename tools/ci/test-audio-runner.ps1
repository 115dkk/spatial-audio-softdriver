$ErrorActionPreference = 'Stop'
$evidence = Join-Path $PSScriptRoot '../../out/runner' | Resolve-Path
$live = Join-Path $PSScriptRoot '../../target/release/bio-live.exe'
$replay = Join-Path $PSScriptRoot '../../target/release/bio-replay.exe'
$failures = [Collections.Generic.List[string]]::new()
function Check($name, [scriptblock]$body) {
    $code = 1
    try {
        & $body *> "$evidence/$name.txt"
        $code = $LASTEXITCODE
    } catch {
        $_.Exception.Message | Add-Content "$evidence/$name.txt"
    }
    Get-Content "$evidence/$name.txt"
    if ($code) { $failures.Add("$name (exit $code)") }
}
Check devices { & $live devices }
Check hardware-tests { cargo test --locked --release -p bio-sys --test windows -- --ignored --nocapture --test-threads=1 }
Check cable-play {
    & $live play --from-dump "$evidence/fixtures/pcm71.biodump" --profile "$evidence/fixtures/profile.toml" --output 'CABLE Input' --seconds 5
}
$tone = Join-Path $PSScriptRoot '../../target/release/examples/sal_tone.exe'
function With-Stimulus($name, [scriptblock]$body) {
    $sender = Start-Process $tone -ArgumentList 'HDMI' -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$evidence/$name-stimulus.txt" -RedirectStandardError "$evidence/$name-stimulus-error.txt"
    try {
        Check $name $body
        if (!$sender.WaitForExit(20000)) { $sender.Kill(); $failures.Add("$name stimulus timed out") }
        elseif ($sender.ExitCode) { $failures.Add("$name stimulus exit $($sender.ExitCode)") }
    } finally { if (!$sender.HasExited) { $sender.Kill() } }
}
With-Stimulus sal-record { & $live record --out "$evidence/sal.biodump" --seconds 8 }
if (Test-Path "$evidence/sal.biodump") {
    Check sal-inspect { & $replay inspect "$evidence/sal.biodump" }
    Check sal-render {
        & $replay render "$evidence/sal.biodump" --profile "$evidence/fixtures/profile.toml" --out "$evidence/sal-render.wav" --report "$evidence/sal-render.json"
    }
}
if (Select-String -Path "$evidence/hardware-tests.txt" -Pattern 'SAL open: success' -SimpleMatch -Quiet) {
    $captureTool = Join-Path $PSScriptRoot '../../target/release/examples/cable_capture.exe'
    $listener = Start-Process $captureTool -ArgumentList "`"$evidence/cable-output.wav`"" -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$evidence/cable-capture.txt" -RedirectStandardError "$evidence/cable-capture-error.txt"
    try {
        With-Stimulus sal-live {
            & $live play --profile "$evidence/fixtures/profile.toml" --output 'CABLE Input' --record "$evidence/sal-live.biodump" --seconds 8
        }
        if (!$listener.WaitForExit(15000)) { $listener.Kill(); $failures.Add('VB-CABLE capture timed out') }
        elseif ($listener.ExitCode) { $failures.Add("VB-CABLE capture exit $($listener.ExitCode)") }
    } finally { if (!$listener.HasExited) { $listener.Kill() } }
    }
    Check sal-live-inspect { & $replay inspect "$evidence/sal-live.biodump" }
    $captured = Get-Content "$evidence/sal-live-inspect.txt" -Raw | ConvertFrom-Json
    if ($captured.data_bytes -le 0 -or $captured.errors.Count) { $failures.Add('live SAL capture was empty or invalid') }
    if (!(Select-String -Path "$evidence/sal-live.txt" -Pattern 'stopped: [1-9][0-9]* blocks' -Quiet) -or
        (Select-String -Path "$evidence/sal-live.txt" -Pattern 'fault:' -Quiet)) { $failures.Add('live SAL processing failed') }
}
# The device smoke test accepts NotPresent for ordinary developer PCs. This
# experiment requires actual SAL presence, regardless of that test's exit code.
if (!(Select-String -Path "$evidence/hardware-tests.txt" -Pattern 'SAL open: success' -SimpleMatch -Quiet)) {
    $failures.Add('SAL ring did not open')
}
$summary = @('## M2 SAL runner experiment', '', "Failed probes: $($failures -join '; ')", '',
    'Artifacts include device enumeration, installation logs, hardware test output, BIO playback and SAL capture.',
    'Synthetic PCM is a transport probe. Licensed game/Apple Music sources and a listening/baseline comparison remain separate M2 checks.')
$summary | Set-Content "$evidence/summary.md"
$summary | Add-Content $env:GITHUB_STEP_SUMMARY
if ($failures.Count) { throw ($failures -join '; ') }
