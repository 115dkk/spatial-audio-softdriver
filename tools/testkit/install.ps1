#Requires -RunAsAdministrator
# Checks a test-signing PC, trusts this kit certificate, and installs Spatial SoftReceiver.
param(
    [switch] $AllowWithoutTestSigning,
    # Diagnosis only (driver.yml): install and check the endpoints, but do not
    # apply or probe the exclusive-mode policy, which opens audio streams.
    [switch] $SkipExclusivePolicy,
    [string] $ResultPath = (Join-Path $PSScriptRoot 'install-result.json')
)
$ErrorActionPreference = 'Stop'
$endpointHelper = Join-Path $PSScriptRoot 'endpoints.ps1'
if (-not (Test-Path -LiteralPath $endpointHelper)) { $endpointHelper = Join-Path $PSScriptRoot '..\ci\endpoints.ps1' }
. $endpointHelper
. (Join-Path (Split-Path $endpointHelper) 'package-identities.ps1')
$HardwareId = $SpatialPackage.HardwareId
$Service = $SpatialPackage.Service

function Stop-Prerequisite([string] $Message) {
    [Console]::Error.WriteLine($Message)
    exit 2
}

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-TestSigning {
    $text = (& bcdedit.exe /enum '{current}' 2>&1) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "bcdedit /enum {current} failed ($LASTEXITCODE): $text" }
    [bool]($text -match '(?im)^\s*testsigning\s+Yes\s*$')
}

function Get-SecureBootState {
    try {
        if (-not (Get-Command Confirm-SecureBootUEFI -ErrorAction SilentlyContinue)) { return 'unsupported' }
        if (Confirm-SecureBootUEFI -ErrorAction Stop) { return 'enabled' }
        return 'disabled'
    } catch { return 'unsupported' }
}

function Get-SsrEndpoints([string] $InstanceId) {
    Get-SpatialEndpoints -InstanceId $InstanceId
}

if (-not (Test-Administrator)) { Stop-Prerequisite '관리자 PowerShell에서 install.ps1을 다시 실행하십시오.' }
$os = Get-CimInstance Win32_OperatingSystem
if ([int]$os.BuildNumber -lt 19041) { Stop-Prerequisite 'Windows 10 2004(빌드 19041) 이상에서 실행하십시오.' }
$kit = Get-Content -Raw (Join-Path $PSScriptRoot 'kit.json') | ConvertFrom-Json
$nativeArch = $env:PROCESSOR_ARCHITECTURE
if ($nativeArch -eq 'x86' -and $env:PROCESSOR_ARCHITEW6432) { $nativeArch = $env:PROCESSOR_ARCHITEW6432 }
if ($kit.arch -eq 'x64' -and $nativeArch -notin @('AMD64', 'x64')) {
    Stop-Prerequisite "이 묶음은 x64용이지만 현재 Windows 아키텍처는 $nativeArch 입니다."
}
$testSigning = Get-TestSigning
$secureBoot = Get-SecureBootState
$testSigningGuide = 'Secure Boot를 끈 뒤 관리자 명령으로 `bcdedit /set testsigning on`을 실행하고 재부팅하십시오'
if (-not $testSigning -and -not $AllowWithoutTestSigning) {
    Stop-Prerequisite "$testSigningGuide."
}
if (-not $testSigning) { Write-Warning "testsigning이 켜져 있지 않습니다. CI 허용 옵션으로 계속합니다. $testSigningGuide." }
if ($secureBoot -eq 'enabled') { Write-Host "Secure Boot가 켜져 있습니다. 시험서명 드라이버를 위해 Secure Boot를 꺼야 합니다. $testSigningGuide." }
$existing = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { @($_.HardwareID | Where-Object { $_ -in @($SpatialPackage.HardwareId) + @($SpatialLegacyPackages | ForEach-Object HardwareId) }).Count -gt 0 })
if ($existing.Count -gt 0) { Stop-Prerequisite '현재 또는 이전 시험판 장치가 이미 있습니다. 먼저 uninstall.ps1을 실행하십시오.' }

$certificatePath = Join-Path $PSScriptRoot 'spatialsoftreceiver-test.cer'
$certificate = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($certificatePath)
foreach ($name in 'Root', 'TrustedPublisher') {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($name, 'LocalMachine')
    try { $store.Open('ReadWrite'); $store.Add($certificate) } finally { $store.Close() }
}
Write-Host "시험 드라이버 서명을 신뢰하려고 인증서를 설치했습니다: $($certificate.Subject), $($certificate.Thumbprint)"
Write-Host '시험을 마치면 uninstall.ps1이 이 인증서를 삭제합니다.'

$exitCode = 1
try {
    & pnputil.exe /add-driver (Join-Path $PSScriptRoot 'spatialsoftreceiver.inf')
    if ($LASTEXITCODE -ne 0) { throw "pnputil /add-driver failed ($LASTEXITCODE)" }
    Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'RootDevice.cs'))
    try {
        $reboot = [RootDevice]::Install((Join-Path $PSScriptRoot 'spatialsoftreceiver.inf'), $HardwareId)
        Write-Host "root device installed (reboot requested: $reboot)"
    } catch { Write-Warning "device update: $($_.Exception.Message)" }
    Start-Sleep -Seconds 5
    $device = Get-PnpDevice | Where-Object { $_.HardwareID -contains $HardwareId } | Select-Object -First 1
    if (-not $device) { throw 'Root\SpatialSoftReceiver 장치를 만들지 못했습니다.' }
    $problem = [int](Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode').Data
    $driverInf = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data
    $driverService = Get-CimInstance Win32_SystemDriver -Filter "Name='$Service'" -ErrorAction SilentlyContinue
    $serviceState = if ($driverService) { $driverService.State } else { $null }
    if (-not $driverInf) { throw '설치된 장치에 드라이버 INF가 연결되지 않았습니다.' }
    $parameters = Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Services\$Service\Parameters"
    $mask = if ($null -eq $parameters.EndpointMask -or [int]$parameters.EndpointMask -eq 0) { 0x7 } else { [int]$parameters.EndpointMask }
    # Only endpoints with mini-pairs can appear. This kit currently carries 0, 1 and 2.
    $mask = $mask -band 0x7
    if ($mask -eq 0) { $mask = 0x7 }
    $expectedEndpoints = @(0..2 | Where-Object { $mask -band (1 -shl $_) })
    $expectedCount = $expectedEndpoints.Count
    # The endpoint builder takes a while (more than 10 s on CI runners) to turn
    # a new device's endpoints active; wait for the enabled endpoints before judging.
    $deadline = (Get-Date).AddSeconds(30)
    do {
        $endpoints = @(Get-SsrEndpoints $device.InstanceId)
        $activeNow = @($endpoints | Where-Object state -eq 1).Count
        if ($problem -ne 0 -or $activeNow -ge $expectedCount) { break }
        Start-Sleep -Seconds 2
    } while ((Get-Date) -lt $deadline)
    $stateNames = @{ 1 = 'active'; 2 = 'disabled'; 4 = 'not present'; 8 = 'unplugged' }
    foreach ($endpoint in $endpoints | Sort-Object endpoint) {
        $stateName = if ($stateNames.ContainsKey([int]$endpoint.state)) { $stateNames[[int]$endpoint.state] } else { "state $($endpoint.state)" }
        Write-Host "endpoint $($endpoint.endpoint) ($stateName): $($endpoint.name)"
    }
    Write-Host '이 이름들이 Windows 설정 > 시스템 > 소리의 출력 장치 목록에 보여야 합니다.'
    $activeEndpoints = @($endpoints | Where-Object state -eq 1)
    $active = $activeEndpoints.Count
    $activeNumbers = @($activeEndpoints | ForEach-Object endpoint)
    $identitiesMatch = $true
    foreach ($ep in $expectedEndpoints) {
        if (@($activeNumbers | Where-Object { $_ -eq $ep }).Count -ne 1) { $identitiesMatch = $false }
    }
    $loaded = ($problem -eq 0 -and $serviceState -eq 'Running')
    $result = [ordered]@{
        os_build = [int]$os.BuildNumber
        testsigning = $testSigning
        secure_boot = $secureBoot
        device_instance = $device.InstanceId
        problem_code = $problem
        service_state = $serviceState
        driver_inf = $driverInf
        endpoints = $endpoints
        loaded = $loaded
        exclusive_policy = 'not applied'
        certificate_thumbprint = $certificate.Thumbprint
        kit_version = $kit.version
        time = (Get-Date).ToUniversalTime().ToString('o')
    }
    $result | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $ResultPath
    Write-Host "device $($device.InstanceId): status $($device.Status), problem $problem, driver $driverInf, service $serviceState"
    if ($problem -eq 52) { $exitCode = 3 }
    elseif (-not $loaded) { $exitCode = 1 }
    elseif ($active -ne $expectedCount -or -not $identitiesMatch) { $exitCode = 4 }
    elseif ($SkipExclusivePolicy) {
        $result.exclusive_policy = 'skipped (-SkipExclusivePolicy)'
        $result | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $ResultPath
        $exitCode = 0
    }
    else {
        $policyHelper = Join-Path $PSScriptRoot 'exclusive-policy.ps1'
        if (-not (Test-Path -LiteralPath $policyHelper)) { $policyHelper = Join-Path $PSScriptRoot '..\ci\exclusive-policy.ps1' }
        . $policyHelper
        $result.exclusive_policy = 'applying; not yet verified'
        $result | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $ResultPath
        Set-SsrExclusivePolicy -InstanceId $device.InstanceId -EndpointMask $mask
        # Commit through IPropertyStore, then probe a fresh audio client. Do not
        # restart global audio services (that would interrupt unrelated devices).
        $checker = Join-Path (Split-Path $policyHelper) 'exclusive_policy_check.ps1'
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $checker -ExpectMask $mask -InstanceId $device.InstanceId -Summary (Join-Path $PSScriptRoot 'exclusive-policy-summary.json')
        if ($LASTEXITCODE -ne 0) { throw "exclusive policy check failed ($LASTEXITCODE); see exclusive-policy-summary.json" }
        $result.exclusive_policy = 'applied and behavior checked without service restart'
        $result | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $ResultPath
        $exitCode = 0
    }
} catch {
    # Not Write-Error: with ErrorActionPreference Stop it would end the script
    # before the exit code below.
    [Console]::Error.WriteLine("install.ps1: $($_.Exception.Message)")
    $exitCode = 1
}
exit $exitCode
