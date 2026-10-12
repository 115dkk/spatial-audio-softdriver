# Records what the audio endpoint builder logs while a device node restarts:
# every ETW provider its service process has registered is enabled at full
# verbosity, the device is restarted, and the events are decoded and printed
# (TraceLogging events decode without a manifest; WPP events do not).
# Diagnosis only; never fails. CI only.
param(
    # Instance id of the driver's device node, e.g. ROOT\MEDIA\0000.
    [Parameter(Mandatory)] [string] $InstanceId,
    [string] $Out = (Join-Path $env:RUNNER_TEMP 'aeb-trace')
)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$etl = Join-Path $Out 'aeb.etl'
$providerFile = Join-Path $Out 'providers.txt'

$servicePid = (Get-CimInstance Win32_Service -Filter "Name='AudioEndpointBuilder'").ProcessId
$listed = logman query providers -pid $servicePid
$guids = $listed | Select-String -Pattern '\{[0-9A-Fa-f-]{36}\}' -AllMatches |
    ForEach-Object { $_.Matches.Value } | Sort-Object -Unique
Write-Host "AudioEndpointBuilder (pid $servicePid) registered $($guids.Count) ETW providers"
$listed | Where-Object { $_ -match '\{' } | ForEach-Object { Write-Host "  $_" }
$guids | ForEach-Object { "$_ 0xffffffffffffffff 0xff" } | Set-Content -Path $providerFile -Encoding ascii

logman stop aeb -ets 2>$null | Out-Null
logman start aeb -o $etl -pf $providerFile -bs 1024 -nb 64 256 -ets | Write-Host
pnputil /restart-device $InstanceId | Write-Host
Start-Sleep -Seconds 20
logman stop aeb -ets | Write-Host

$events = Get-WinEvent -Path $etl -Oldest -ErrorAction SilentlyContinue
Write-Host "decoded $(@($events).Count) events"
foreach ($event in $events) {
    $message = if ($event.Message) { $event.Message -replace '\s+', ' ' } else { '' }
    $values = ($event.Properties | ForEach-Object { "$($_.Value)" }) -join ' | '
    $line = "$($event.TimeCreated.ToString('HH:mm:ss.fff')) $($event.ProviderName) $($event.Id) $message [$values]"
    if ($line.Length -gt 600) { $line = $line.Substring(0, 600) }
    Write-Host $line
}
