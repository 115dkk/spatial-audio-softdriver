# Crash dumps of the CI's own programs, and their stacks once the job is done
# (ADR 0026). CI only.
#
#     ./tools/ci/crash-dumps.ps1 -Folder $dumps                  # turn on WER local dumps
#     ./tools/ci/crash-dumps.ps1 -Folder $dumps -Report -Symbols target/release
#
# A soak saw bio-live end with 0xC0000005 and no output; the dump and cdb's
# stacks say where. -Report never fails the step: the step that ran the
# program has already failed.
param(
    [Parameter(Mandatory)] [string] $Folder,
    [switch] $Report,
    [string] $Symbols = '',
    [string[]] $Programs = @('bio-live.exe', 'bio-replay.exe')
)
$ErrorActionPreference = 'Stop'

if (-not $Report) {
    New-Item -ItemType Directory -Force -Path $Folder | Out-Null
    $wer = 'HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting'
    New-Item -Force -Path "$wer\LocalDumps" | Out-Null
    New-ItemProperty -Force -Path $wer -Name DontShowUI -Value 1 -PropertyType DWord | Out-Null
    New-ItemProperty -Force -Path $wer -Name Disabled -Value 0 -PropertyType DWord | Out-Null
    foreach ($program in $Programs) {
        $key = "$wer\LocalDumps\$program"
        New-Item -Force -Path $key | Out-Null
        New-ItemProperty -Force -Path $key -Name DumpFolder -Value $Folder -PropertyType ExpandString | Out-Null
        # Full dumps: the engine's state is in the heap.
        New-ItemProperty -Force -Path $key -Name DumpType -Value 2 -PropertyType DWord | Out-Null
        New-ItemProperty -Force -Path $key -Name DumpCount -Value 10 -PropertyType DWord | Out-Null
    }
    Set-Service -Name WerSvc -StartupType Manual -ErrorAction SilentlyContinue
    Start-Service -Name WerSvc -ErrorAction SilentlyContinue
    Write-Host "crash dumps of $($Programs -join ', ') go to $Folder"
    return
}

$dumps = @(Get-ChildItem -Path $Folder -Filter *.dmp -ErrorAction SilentlyContinue)
if (-not $dumps) {
    Write-Host 'no crash dumps'
    return
}
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$cdb = Get-ChildItem -Path (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\Debuggers') -Recurse -Filter cdb.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq $arch } | Select-Object -First 1
Write-Host "::error::$($dumps.Count) crash dump(s) in $Folder"
if (-not $cdb) {
    Write-Host '::warning::cdb.exe not found; the dumps are in the artifact'
    return
}
$cache = Join-Path $env:RUNNER_TEMP 'symbols'
$path = "$Symbols;srv*$cache*https://msdl.microsoft.com/download/symbols"
foreach ($dump in $dumps) {
    Write-Host "::group::$($dump.Name)"
    # The faulting thread with source lines, then every thread.
    & $cdb.FullName -z $dump.FullName -y $path -lines -c '.symopt+ 0x40; .exr -1; .ecxr; kpn 60; ~*kn 40; q'
    Write-Host '::endgroup::'
}
