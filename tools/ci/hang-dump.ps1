# Where the kernel is when a CI step hangs, without a debugger attached.
# CI only; never fails the step it runs in.
#
#     ./tools/ci/hang-dump.ps1 -Processes drvinst, rundll32
#
# A step that waits on the driver (an install that never returns, a READ that
# never completes) shows nothing in its own log. This takes a live kernel
# memory dump through the storage diagnostics (Windows 10 1709 / Server 2019
# and later), prints the kernel stacks of every thread inside the driver,
# PortCls and KS with kd, and the user-mode stacks of the named processes
# with cdb attached non-invasively. If the runner crashed instead, the job has
# lost its runner and this step never runs: that absence is the answer.
param(
    [string] $Out = (Join-Path $env:RUNNER_TEMP 'hang'),
    [string[]] $Modules = @('spatialsoftreceiver', 'portcls', 'ks'),
    [string[]] $Processes = @()
)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$debuggers = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\Debuggers'
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$kd = Join-Path $debuggers "$arch\kd.exe"
$cdb = Join-Path $debuggers "$arch\cdb.exe"
$symbols = 'srv*' + (Join-Path $Out 'symbols') + '*https://msdl.microsoft.com/download/symbols'

foreach ($name in $Processes) {
    foreach ($process in Get-Process -Name $name -ErrorAction SilentlyContinue) {
        Write-Host "== user-mode stacks of $name ($($process.Id))"
        if (Test-Path $cdb) {
            & $cdb -pv -p $process.Id -y $symbols -c '~*kn 30; qd' 2>&1 | Select-Object -Last 200 | Write-Host
        }
    }
}

$subsystem = Get-StorageSubSystem -ErrorAction SilentlyContinue | Where-Object FriendlyName -like 'Windows Storage*' | Select-Object -First 1
if (-not $subsystem) { Write-Host 'no storage subsystem for a live dump'; exit 0 }
Write-Host '== live kernel dump'
$subsystem | Get-StorageDiagnosticInfo -DestinationPath $Out -IncludeLiveDump -ErrorAction Continue | Out-Null
$dump = Get-ChildItem -Path $Out -Recurse -Filter *.dmp -ErrorAction SilentlyContinue | Sort-Object Length -Descending | Select-Object -First 1
if (-not $dump) { Write-Host 'no live dump was written'; exit 0 }
Write-Host "$($dump.FullName) ($([math]::Round($dump.Length / 1MB)) MB)"
if (-not (Test-Path $kd)) { Write-Host "no kd.exe at $kd"; exit 0 }
$commands = @('.reload')
foreach ($module in $Modules) { $commands += "!stacks 2 $module" }
$commands += '!locks', 'q'
& $kd -z $dump.FullName -y $symbols -c ($commands -join '; ') 2>&1 | Select-Object -Last 600 | Write-Host
# The dump can be gigabytes; the stacks above are what the job keeps.
Remove-Item -LiteralPath $dump.FullName -ErrorAction SilentlyContinue
exit 0
