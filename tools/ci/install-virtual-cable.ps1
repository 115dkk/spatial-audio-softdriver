# Installs VB-CABLE on a GitHub-hosted Windows runner so the WASAPI paths have
# a real render endpoint ("Speakers (VB-Audio Virtual Cable)") and a loopback
# capture of it ("CABLE Output"). CI only; nothing here ships.
#
# VB-CABLE is donationware by Vincent Burel (www.vb-cable.com). Its licence
# allows using and copying the package as is; we download the unmodified
# package at CI time, pinned by SHA-256, and never redistribute it.
#
# The device node is created the way devcon does it (SetupDiCreateDeviceInfo,
# DIF_REGISTERDEVICE, UpdateDriverForPlugAndPlayDevices), through SetupAPI
# from C# (tools/ci/RootDevice.cs, shared with install-driver-test.ps1), so the
# runner needs no devcon.exe.

$ErrorActionPreference = 'Stop'

$Url = 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack43.zip'
$Sha256 = '66fd0a4d9f4896ff41632b7e3d53892c085c4561f53e8ae8d0f0bc10eedd1cdd'
$HardwareId = 'VBAudioVACWDM'
$Inf = 'vbMmeCable64_win7.inf'
$Sys = 'vbaudio_cable64_win7.sys'

$work = Join-Path $env:RUNNER_TEMP 'vbcable'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$zip = Join-Path $work 'vbcable.zip'

for ($attempt = 1; $attempt -le 4; $attempt++) {
    try {
        Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing
        break
    } catch {
        if ($attempt -eq 4) { throw }
        Start-Sleep -Seconds ([math]::Pow(2, $attempt))
    }
}
$actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
if ($actual -ne $Sha256) {
    throw "VB-CABLE package hash $actual does not match the pinned $Sha256"
}
Expand-Archive -LiteralPath $zip -DestinationPath $work -Force

# Trust the driver's publisher so the install does not stop at the
# 'Would you like to install this device software?' prompt, which a service
# session cannot answer.
$signature = Get-AuthenticodeSignature (Join-Path $work $Sys)
if ($null -eq $signature.SignerCertificate) {
    throw "$Sys carries no Authenticode signature ($($signature.Status))"
}
$store = New-Object System.Security.Cryptography.X509Certificates.X509Store('TrustedPublisher', 'LocalMachine')
$store.Open('ReadWrite')
$store.Add($signature.SignerCertificate)
$store.Close()
Write-Host "Trusted publisher: $($signature.SignerCertificate.Subject)"

Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'RootDevice.cs'))

$reboot = [RootDevice]::Install((Join-Path $work $Inf), $HardwareId)
Write-Host "VB-CABLE installed (reboot requested: $reboot)"

# Desktop apps need microphone consent to open a capture endpoint
# (E_ACCESSDENIED otherwise), which the loopback check records from.
$consent = 'SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone'
foreach ($hive in 'HKLM:', 'HKCU:') {
    New-Item -Force -Path "$hive\$consent\NonPackaged" | Out-Null
    Set-ItemProperty -Path "$hive\$consent" -Name Value -Value 'Allow'
    Set-ItemProperty -Path "$hive\$consent\NonPackaged" -Name Value -Value 'Allow'
}
$privacy = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\AppPrivacy'
New-Item -Force -Path $privacy | Out-Null
Set-ItemProperty -Path $privacy -Name LetAppsAccessMicrophone -Value 1 -Type DWord

# Windows Server leaves the audio services on manual start.
foreach ($name in 'AudioEndpointBuilder', 'Audiosrv') {
    Set-Service -Name $name -StartupType Automatic
    Start-Service -Name $name
}

# The endpoints appear a moment after the driver starts.
$deadline = (Get-Date).AddSeconds(60)
do {
    $endpoints = Get-PnpDevice -Class AudioEndpoint -PresentOnly -ErrorAction SilentlyContinue |
        Where-Object { $_.FriendlyName -like '*CABLE*' }
    if (@($endpoints | Where-Object Status -eq 'OK').Count -ge 2) { break }
    Start-Sleep -Seconds 1
} while ((Get-Date) -lt $deadline)
Get-PnpDevice -Class AudioEndpoint -PresentOnly | Format-Table -AutoSize Status, FriendlyName, InstanceId
if (@($endpoints | Where-Object Status -eq 'OK').Count -lt 2) {
    Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $HardwareId } | Format-List *
    throw 'the CABLE endpoints did not appear'
}
