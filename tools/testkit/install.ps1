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
$catalog = Import-PowerShellDataFile (Join-Path $PSScriptRoot 'strings.psd1')
$language = if ($env:SSR_LANG -eq 'ko') { 'ko' } elseif ($env:SSR_LANG -eq 'en') { 'en' } elseif ((Get-UICulture).TwoLetterISOLanguageName -eq 'ko') { 'ko' } else { 'en' }
$strings = $catalog[$language]
function Format-Text([string] $Key, [hashtable] $Values = @{}) {
    $text = [string]$strings[$Key]
    foreach ($name in $Values.Keys) { $text = $text.Replace("{$name}", [string]$Values[$name]) }
    $text
}
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

if (-not (Test-Administrator)) { Stop-Prerequisite (Format-Text 'admin_required') }
$os = Get-CimInstance Win32_OperatingSystem
if ([int]$os.BuildNumber -lt 19041) { Stop-Prerequisite (Format-Text 'os_required') }
$kit = Get-Content -Raw (Join-Path $PSScriptRoot 'kit.json') | ConvertFrom-Json
$nativeArch = $env:PROCESSOR_ARCHITECTURE
if ($nativeArch -eq 'x86' -and $env:PROCESSOR_ARCHITEW6432) { $nativeArch = $env:PROCESSOR_ARCHITEW6432 }
if ($kit.arch -eq 'x64' -and $nativeArch -notin @('AMD64', 'x64')) {
    Stop-Prerequisite (Format-Text 'architecture_mismatch' @{ arch = $nativeArch })
}
$testSigning = Get-TestSigning
$secureBoot = Get-SecureBootState
$testSigningGuide = Format-Text 'test_signing_guide'
if (-not $testSigning -and -not $AllowWithoutTestSigning) {
    Stop-Prerequisite (Format-Text 'test_signing_required' @{ guide = $testSigningGuide })
}
if (-not $testSigning) { Write-Warning (Format-Text 'test_signing_warning' @{ guide = $testSigningGuide }) }
if ($secureBoot -eq 'enabled') { Write-Host (Format-Text 'secure_boot_warning' @{ guide = $testSigningGuide }) }
$existing = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { @($_.HardwareID | Where-Object { $_ -in @($SpatialPackage.HardwareId) + @($SpatialLegacyPackages | ForEach-Object HardwareId) }).Count -gt 0 })
if ($existing.Count -gt 0) { Stop-Prerequisite (Format-Text 'existing_device') }

$certificatePath = Join-Path $PSScriptRoot 'spatialsoftreceiver-test.cer'
$certificate = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($certificatePath)
foreach ($name in 'Root', 'TrustedPublisher') {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($name, 'LocalMachine')
    try { $store.Open('ReadWrite'); $store.Add($certificate) } finally { $store.Close() }
}
Write-Host (Format-Text 'certificate_installed' @{ subject = $certificate.Subject; thumbprint = $certificate.Thumbprint })
Write-Host (Format-Text 'certificate_cleanup')

$exitCode = 1
try {
    & pnputil.exe /add-driver (Join-Path $PSScriptRoot 'spatialsoftreceiver.inf')
    if ($LASTEXITCODE -ne 0) { throw "pnputil /add-driver failed ($LASTEXITCODE)" }
    Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'RootDevice.cs'))
    try {
        $reboot = [RootDevice]::Install((Join-Path $PSScriptRoot 'spatialsoftreceiver.inf'), $HardwareId)
        Write-Host (Format-Text 'root_device_installed' @{ reboot = $reboot })
    } catch { Write-Warning (Format-Text 'device_update_warning' @{ detail = $_.Exception.Message }) }
    Start-Sleep -Seconds 5
    $device = Get-PnpDevice | Where-Object { $_.HardwareID -contains $HardwareId } | Select-Object -First 1
    if (-not $device) { throw (Format-Text 'root_device_failed') }
    $problem = [int](Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode').Data
    $driverInf = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data
    $driverService = Get-CimInstance Win32_SystemDriver -Filter "Name='$Service'" -ErrorAction SilentlyContinue
    $serviceState = if ($driverService) { $driverService.State } else { $null }
    if (-not $driverInf) { throw (Format-Text 'driver_inf_missing') }
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
        Write-Host (Format-Text 'endpoint_line' @{ endpoint = $endpoint.endpoint; state = $stateName; name = $endpoint.name })
    }
    Write-Host (Format-Text 'endpoints_visible')
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
    Write-Host (Format-Text 'device_line' @{ instance = $device.InstanceId; status = $device.Status; problem = $problem; driver = $driverInf; service = $serviceState })
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
    [Console]::Error.WriteLine((Format-Text 'install_failed' @{ detail = $_.Exception.Message }))
    $exitCode = 1
}
exit $exitCode
