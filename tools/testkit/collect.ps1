#Requires -RunAsAdministrator
# Collects only Spatial SoftReceiver and Windows driver diagnostics into a shareable zip.
param([string] $OutDir = $PSScriptRoot)
$ErrorActionPreference = 'Continue'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$alphabet = 'abcdefghijkmnopqrstuvwxyz23456789'.ToCharArray()
$random = -join (1..6 | ForEach-Object { $alphabet[(Get-Random -Maximum $alphabet.Length)] })
$name = "spatial-softreceiver-test-report-$random-$stamp"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$work = Join-Path $OutDir $name
$zip = Join-Path $OutDir "$name.zip"
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$collected = New-Object System.Collections.Generic.List[string]
$endpointHelper = Join-Path $PSScriptRoot 'endpoints.ps1'
if (-not (Test-Path -LiteralPath $endpointHelper)) { $endpointHelper = Join-Path $PSScriptRoot '..\ci\endpoints.ps1' }
. $endpointHelper

function Save-Text([string] $Name, [scriptblock] $Command) {
    $path = Join-Path $work $Name
    try { & $Command 2>&1 | Out-String -Width 4096 | Set-Content -Encoding UTF8 $path } catch { $_ | Out-String | Set-Content -Encoding UTF8 $path }
    $collected.Add($Name)
}

$os = Get-CimInstance Win32_OperatingSystem
$ubr = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -ErrorAction SilentlyContinue).UBR
[ordered]@{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber; ubr = $ubr;
    architecture = $env:PROCESSOR_ARCHITECTURE } | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $work 'system.json')
$collected.Add('system.json')
Save-Text 'bcdedit-current.txt' { bcdedit.exe /enum '{current}' }
Save-Text 'secure-boot.txt' { try { Confirm-SecureBootUEFI -ErrorAction Stop } catch { $_.Exception.Message } }
Save-Text 'device-guard.txt' { Get-CimInstance -Namespace 'root\Microsoft\Windows\DeviceGuard' -ClassName Win32_DeviceGuard | Select-Object SecurityServicesRunning, VirtualizationBasedSecurityStatus | Format-List }
Save-Text 'verifier-query.txt' { verifier.exe /query }

Save-Text 'devices.txt' {
    Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains 'Root\SpatialSoftReceiver' } | ForEach-Object {
        $problem = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode' -ErrorAction SilentlyContinue).Data
        [pscustomobject]@{ InstanceId = $_.InstanceId; Status = $_.Status; ProblemCode = $problem }
    } | Format-List
}
$driver = Get-CimInstance Win32_SystemDriver -Filter "Name='SpatialSoftReceiver'" -ErrorAction SilentlyContinue
$driverPath = if ($driver) { [Environment]::ExpandEnvironmentVariables($driver.PathName.Trim('"')) } else { $null }
if ($driverPath -and (Test-Path $driverPath)) {
    $file = Get-Item $driverPath
    [ordered]@{ path = $driverPath; file_version = $file.VersionInfo.FileVersion;
        sha256 = (Get-FileHash -Algorithm SHA256 $driverPath).Hash.ToLowerInvariant() } |
        ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $work 'driver-file.json')
    $collected.Add('driver-file.json')
}
$parameterPath = 'HKLM:\SYSTEM\CurrentControlSet\Services\SpatialSoftReceiver\Parameters'
if (Test-Path $parameterPath) {
    $key = Get-Item $parameterPath
    $values = [ordered]@{}
    foreach ($property in 'EndpointMask', 'CodecMask0', 'PcmRateMask0', 'PcmShapeMask0', 'PcmMaxMaskedChannels0', 'PcmDirectChannels0', 'CodecMask1', 'PcmRateMask1', 'PcmShapeMask1', 'PcmMaxMaskedChannels1', 'PcmDirectChannels1', 'CodecMask2', 'PcmRateMask2', 'PcmShapeMask2', 'PcmMaxMaskedChannels2', 'PcmDirectChannels2', 'CodecMask3', 'PcmRateMask3', 'PcmShapeMask3', 'PcmMaxMaskedChannels3', 'PcmDirectChannels3', 'WritePacket') {
        $values[$property] = [ordered]@{ value = $key.GetValue($property); kind = [string]$key.GetValueKind($property) }
    }
    $values | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 (Join-Path $work 'service-parameters.json')
    $collected.Add('service-parameters.json')
}

Save-Text 'endpoints.json' {
    $endpoints = @(Get-SpatialEndpoints)
    foreach ($endpoint in $endpoints) {
        $guid = $endpoint.id.Substring($endpoint.id.LastIndexOf('.') + 1)
        $props = Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\$guid\Properties" -ErrorAction Stop
        $deviceFormat = $props.'{f19f064d-082c-4e27-bc73-6882a1bb8e4c},0'
        $format = if ($deviceFormat) { [BitConverter]::ToString([byte[]]$deviceFormat).Replace('-', '') } else { $null }
        $endpoint | Add-Member -NotePropertyName PKEY_AudioEngine_DeviceFormat -NotePropertyValue $format
    }
    ConvertTo-Json -InputObject $endpoints -Depth 4
}
Save-Text 'endpoint-policy.json' {
    $policyHelper = Join-Path $PSScriptRoot 'exclusive-policy.ps1'
    if (-not (Test-Path -LiteralPath $policyHelper)) { $policyHelper = Join-Path $PSScriptRoot '..\ci\exclusive-policy.ps1' }
    . $policyHelper
    @(Get-SsrPolicySnapshot) | ConvertTo-Json -Depth 6
}
Save-Text 'endpoint-volumes.json' {
    $volumeHelper = Join-Path $PSScriptRoot 'endpoint-volume.ps1'
    if (-not (Test-Path -LiteralPath $volumeHelper)) { $volumeHelper = Join-Path $PSScriptRoot '..\ci\endpoint-volume.ps1' }
    . $volumeHelper
    @(Get-SsrVolumeSnapshot) | ConvertTo-Json -Depth 6
}

$setup = 'C:\Windows\INF\setupapi.dev.log'
if (Test-Path $setup) {
    $setupHits = Select-String -Path $setup -Pattern 'spatialsoftreceiver' -Context 20,40
    $setupHits | Out-String -Width 4096 | Set-Content -Encoding UTF8 (Join-Path $work 'setupapi-spatialsoftreceiver.txt')
    $collected.Add('setupapi-spatialsoftreceiver.txt')
}
$since = (Get-Date).AddDays(-7)
Save-Text 'events-system.txt' {
    Get-WinEvent -FilterHashtable @{ LogName = 'System'; StartTime = $since } -ErrorAction SilentlyContinue |
        Where-Object { $_.ProviderName -match 'Kernel-PnP|BugCheck|Audio' } |
        Select-Object TimeCreated, Id, LevelDisplayName, ProviderName, Message | Format-List
}
Save-Text 'events-code-integrity.txt' {
    Get-WinEvent -FilterHashtable @{ LogName = 'Microsoft-Windows-CodeIntegrity/Operational'; StartTime = $since } -ErrorAction SilentlyContinue |
        Where-Object { $_.Message -match 'spatialsoftreceiver' } |
        Select-Object TimeCreated, Id, LevelDisplayName, ProviderName, Message | Format-List
}

$installed = $null
$installResult = Join-Path $PSScriptRoot 'install-result.json'
if (Test-Path $installResult) {
    $installed = (Get-Item $installResult).LastWriteTime
    Copy-Item $installResult $work
    $collected.Add('install-result.json')
}
$minidumps = @(Get-ChildItem 'C:\Windows\Minidump\*.dmp', 'C:\Windows\MEMORY.DMP' -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending)
$minidumps | Select-Object Name, Length, LastWriteTime | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $work 'minidumps.json')
$collected.Add('minidumps.json')
# Minidumps only: a full MEMORY.DMP is gigabytes and stays in the list above.
$eligibleDumps = if ($installed) { @($minidumps | Where-Object { $_.LastWriteTime -ge $installed -and $_.Name -ne 'MEMORY.DMP' }) } else { @() }
foreach ($dump in @($eligibleDumps | Select-Object -First 3)) {
    Copy-Item $dump.FullName $work
    $collected.Add($dump.Name)
}
# The kit's own results and the M2 runs moved into this folder (play-*.json
# from ssr-live play --report).
foreach ($pattern in '*-summary.json', 'driver-flow*.json', 'play-*.json') {
    foreach ($file in Get-ChildItem (Join-Path $PSScriptRoot $pattern) -File -ErrorAction SilentlyContinue) {
        Copy-Item $file.FullName $work
        $collected.Add($file.Name)
    }
}
$ssrdumps = @(Get-ChildItem (Join-Path $PSScriptRoot '*.ssrdump') -File -ErrorAction SilentlyContinue)
$ssrdumps | Select-Object Name, Length, LastWriteTime | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $work 'ssrdumps.json')
$collected.Add('ssrdumps.json')
$total = ($ssrdumps | Measure-Object Length -Sum).Sum
if (-not $total) { $total = 0 }
if ($total -le 200MB) {
    foreach ($file in $ssrdumps) { Copy-Item $file.FullName $work; $collected.Add($file.Name) }
} else { Write-Warning '.ssrdump 파일 합계가 200 MB를 넘어 목록만 수집합니다.' }

# Recordings (M2 builder loopback WAVs) and screenshots, up to 300 MB together.
$media = @(Get-ChildItem (Join-Path $PSScriptRoot '*') -Include '*.wav', '*.png', '*.jpg' -File -ErrorAction SilentlyContinue)
$media | Select-Object Name, Length, LastWriteTime | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $work 'media.json')
$collected.Add('media.json')
$mediaTotal = ($media | Measure-Object Length -Sum).Sum
if (-not $mediaTotal) { $mediaTotal = 0 }
if ($mediaTotal -le 300MB) {
    foreach ($file in $media) { Copy-Item $file.FullName $work; $collected.Add($file.Name) }
} else { Write-Warning '녹음과 화면 파일 합계가 300 MB를 넘어 목록만 수집합니다.' }

$collected | Sort-Object | Set-Content -Encoding UTF8 (Join-Path $work 'CONTENTS.txt')
Write-Host '수집한 항목:'
Get-Content (Join-Path $work 'CONTENTS.txt') | ForEach-Object { Write-Host "  $_" }
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $work -DestinationPath $zip -CompressionLevel Optimal
if (-not (Test-Path $zip)) { [Console]::Error.WriteLine("collect.ps1: $zip was not written"); exit 1 }
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
Write-Host "시험 보고서: $zip"
# Explicit: the last native command (verifier, bcdedit) must not set the result.
exit 0
