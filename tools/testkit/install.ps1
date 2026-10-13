#Requires -RunAsAdministrator
# Checks a test-signing PC, trusts this kit certificate, and installs BIO Atmos.
param(
    [switch] $AllowWithoutTestSigning,
    [string] $ResultPath = (Join-Path $PSScriptRoot 'install-result.json')
)
$ErrorActionPreference = 'Stop'
$HardwareId = 'Root\BioAtmos'
$Service = 'bioatmos'

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

function Get-BioEndpoints([string] $InstanceId) {
    $states = @{ 1 = 'active'; 2 = 'disabled'; 4 = 'not present'; 8 = 'unplugged' }
    $items = @()
    $root = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
    foreach ($endpoint in Get-ChildItem $root -ErrorAction SilentlyContinue) {
        $properties = Get-ItemProperty (Join-Path $endpoint.PSPath 'Properties') -ErrorAction SilentlyContinue
        $name = $properties.'{a45c254e-df1c-4efd-8020-67d146a850e0},2'
        $owner = $properties.'{b3f8fa53-0004-438e-9003-51a46e139bfc},2'
        if ($name -notlike '*BIO Atmos*' -or $owner -notlike "*$InstanceId") { continue }
        $value = [int](Get-ItemProperty $endpoint.PSPath).DeviceState
        $label = if ($states.ContainsKey($value)) { $states[$value] } else { '0x{0:X}' -f $value }
        $items += [pscustomobject]@{ name = $name; state = $label }
    }
    $items
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
$existing = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $HardwareId })
if ($existing.Count -gt 0) { Stop-Prerequisite 'Root\BioAtmos 장치가 이미 있습니다. 먼저 uninstall.ps1을 실행하십시오.' }

$certificatePath = Join-Path $PSScriptRoot 'bioatmos-test.cer'
$certificate = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($certificatePath)
foreach ($name in 'Root', 'TrustedPublisher') {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($name, 'LocalMachine')
    try { $store.Open('ReadWrite'); $store.Add($certificate) } finally { $store.Close() }
}
Write-Host "시험 드라이버 서명을 신뢰하려고 인증서를 설치했습니다: $($certificate.Subject), $($certificate.Thumbprint)"
Write-Host '시험을 마치면 uninstall.ps1이 이 인증서를 삭제합니다.'

$exitCode = 1
try {
    & pnputil.exe /add-driver (Join-Path $PSScriptRoot 'bioatmos.inf')
    if ($LASTEXITCODE -ne 0) { throw "pnputil /add-driver failed ($LASTEXITCODE)" }
    Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'RootDevice.cs'))
    try {
        $reboot = [RootDevice]::Install((Join-Path $PSScriptRoot 'bioatmos.inf'), $HardwareId)
        Write-Host "root device installed (reboot requested: $reboot)"
    } catch { Write-Warning "device update: $($_.Exception.Message)" }
    Start-Sleep -Seconds 5
    $device = Get-PnpDevice | Where-Object { $_.HardwareID -contains $HardwareId } | Select-Object -First 1
    if (-not $device) { throw 'Root\BioAtmos 장치를 만들지 못했습니다.' }
    $problem = [int](Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode').Data
    $driverInf = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data
    $driverService = Get-CimInstance Win32_SystemDriver -Filter "Name='$Service'" -ErrorAction SilentlyContinue
    $serviceState = if ($driverService) { $driverService.State } else { $null }
    if (-not $driverInf) { throw '설치된 장치에 드라이버 INF가 연결되지 않았습니다.' }
    # The endpoint builder takes a while (more than 10 s on CI runners) to turn
    # a new device's endpoints active; wait for both before judging.
    $deadline = (Get-Date).AddSeconds(30)
    do {
        $endpoints = @(Get-BioEndpoints $device.InstanceId)
        $activeNow = @($endpoints | Where-Object state -eq 'active').Count
        if ($problem -ne 0 -or $activeNow -ge 2) { break }
        Start-Sleep -Seconds 2
    } while ((Get-Date) -lt $deadline)
    foreach ($endpoint in $endpoints) { Write-Host "endpoint $($endpoint.state): $($endpoint.name)" }
    Write-Host '이 이름들이 Windows 설정 > 시스템 > 소리의 출력 장치 목록에 보여야 합니다.'
    $active = @($endpoints | Where-Object state -eq 'active').Count
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
        certificate_thumbprint = $certificate.Thumbprint
        kit_version = $kit.version
        time = (Get-Date).ToUniversalTime().ToString('o')
    }
    $result | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $ResultPath
    Write-Host "device $($device.InstanceId): status $($device.Status), problem $problem, driver $driverInf, service $serviceState"
    if ($problem -eq 52) { $exitCode = 3 }
    elseif (-not $loaded) { $exitCode = 1 }
    elseif ($active -ne 2) { $exitCode = 4 }
    else { $exitCode = 0 }
} catch {
    # Not Write-Error: with ErrorActionPreference Stop it would end the script
    # before the exit code below.
    [Console]::Error.WriteLine("install.ps1: $($_.Exception.Message)")
    $exitCode = 1
}
exit $exitCode
