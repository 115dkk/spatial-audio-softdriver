$ErrorActionPreference = 'Stop'
$evidence = Join-Path $PSScriptRoot '../../out/runner' | Resolve-Path
$live = Join-Path $PSScriptRoot '../../target/release/bio-live.exe'
$replay = Join-Path $PSScriptRoot '../../target/release/bio-replay.exe'
$failures = [Collections.Generic.List[string]]::new()
function Check($name, [scriptblock]$body) {
    & $body *> "$evidence/$name.txt"
    $code = $LASTEXITCODE
    Get-Content "$evidence/$name.txt"
    if ($code) { $failures.Add("$name (exit $code)") }
}
Check devices { & $live devices }
Check hardware-tests { cargo test --locked --release -p bio-sys --test windows -- --ignored --nocapture --test-threads=1 }
Check cable-play {
    & $live play --from-dump "$evidence/fixtures/pcm71.biodump" --profile "$evidence/fixtures/profile.toml" --output 'CABLE Input' --seconds 5
}
$tone = Join-Path $PSScriptRoot '../../target/release/examples/sal_tone.exe'
$sender = Start-Process $tone -ArgumentList 'HDMI' -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput "$evidence/stimulus.txt" -RedirectStandardError "$evidence/stimulus-error.txt"
try {
    Check sal-record { & $live record --out "$evidence/sal.biodump" --seconds 8 }
    if (!$sender.WaitForExit(20000)) { $sender.Kill(); $failures.Add('stimulus timed out') }
    elseif ($sender.ExitCode) { $failures.Add("stimulus exit $($sender.ExitCode)") }
} finally { if (!$sender.HasExited) { $sender.Kill() } }
if (Test-Path "$evidence/sal.biodump") {
    Check sal-inspect { & $replay inspect "$evidence/sal.biodump" }
    Check sal-render {
        & $replay render "$evidence/sal.biodump" --profile "$evidence/fixtures/profile.toml" --out "$evidence/sal-render.wav" --report "$evidence/sal-render.json"
    }
}
# The device smoke test accepts NotPresent for ordinary developer PCs. This
# experiment requires actual SAL presence, regardless of that test's exit code.
if (!(Select-String "$evidence/hardware-tests.txt" -SimpleMatch 'SAL open: success' -Quiet)) {
    $failures.Add('SAL ring did not open')
}
$summary = @('## M2 SAL runner experiment', '', "Failed probes: $($failures -join '; ')", '',
    'Artifacts include device enumeration, installation logs, hardware test output, BIO playback and SAL capture.',
    'Synthetic PCM is a transport probe. Licensed game/Apple Music sources and a listening/baseline comparison remain separate M2 checks.')
$summary | Set-Content "$evidence/summary.md"
$summary | Add-Content $env:GITHUB_STEP_SUMMARY
if ($failures.Count) { throw ($failures -join '; ') }
