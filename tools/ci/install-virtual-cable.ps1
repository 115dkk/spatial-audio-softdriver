# Installs VB-CABLE on a GitHub-hosted Windows runner so the WASAPI paths have
# a real render endpoint ("CABLE Input") and a loopback capture of it
# ("CABLE Output"). CI only; nothing here ships.
#
# VB-CABLE is donationware by Vincent Burel (www.vb-cable.com). Its licence
# allows using and copying the package as is; we download the unmodified
# package at CI time, pinned by SHA-256, and never redistribute it.
#
# The device node is created the way devcon does it (SetupDiCreateDeviceInfo,
# DIF_REGISTERDEVICE, UpdateDriverForPlugAndPlayDevices), through SetupAPI
# from inline C#, so the runner needs no devcon.exe.

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

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class RootDevice
{
    [StructLayout(LayoutKind.Sequential)]
    struct SP_DEVINFO_DATA
    {
        public int cbSize;
        public Guid ClassGuid;
        public int DevInst;
        public IntPtr Reserved;
    }

    const int DICD_GENERATE_ID = 1;
    const int SPDRP_HARDWAREID = 1;
    const int DIF_REGISTERDEVICE = 0x19;
    const int INSTALLFLAG_FORCE = 1;
    static readonly IntPtr INVALID_HANDLE_VALUE = new IntPtr(-1);

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool SetupDiGetINFClassW(string infName, out Guid classGuid,
        StringBuilder className, int classNameSize, out int requiredSize);

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid classGuid, IntPtr hwndParent);

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool SetupDiCreateDeviceInfoW(IntPtr set, string deviceName, ref Guid classGuid,
        string description, IntPtr hwndParent, int flags, ref SP_DEVINFO_DATA data);

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set, ref SP_DEVINFO_DATA data,
        int property, byte[] buffer, int size);

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiCallClassInstaller(int function, IntPtr set, ref SP_DEVINFO_DATA data);

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);

    [DllImport("newdev.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr hwndParent, string hardwareId,
        string fullInfPath, int flags, out bool rebootRequired);

    static Exception Fail(string what)
    {
        return new Win32Exception(Marshal.GetLastWin32Error(), what);
    }

    // Returns true when Windows asks for a reboot.
    public static bool Install(string infPath, string hardwareId)
    {
        Guid classGuid;
        var className = new StringBuilder(64);
        int required;
        if (!SetupDiGetINFClassW(infPath, out classGuid, className, className.Capacity, out required))
            throw Fail("SetupDiGetINFClass");
        IntPtr set = SetupDiCreateDeviceInfoList(ref classGuid, IntPtr.Zero);
        if (set == INVALID_HANDLE_VALUE) throw Fail("SetupDiCreateDeviceInfoList");
        try
        {
            var data = new SP_DEVINFO_DATA();
            data.cbSize = Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
            if (!SetupDiCreateDeviceInfoW(set, className.ToString(), ref classGuid, null,
                    IntPtr.Zero, DICD_GENERATE_ID, ref data))
                throw Fail("SetupDiCreateDeviceInfo");
            // REG_MULTI_SZ: the id, then an empty string.
            byte[] ids = Encoding.Unicode.GetBytes(hardwareId + "\0\0");
            if (!SetupDiSetDeviceRegistryPropertyW(set, ref data, SPDRP_HARDWAREID, ids, ids.Length))
                throw Fail("SetupDiSetDeviceRegistryProperty");
            if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, ref data))
                throw Fail("SetupDiCallClassInstaller(DIF_REGISTERDEVICE)");
        }
        finally
        {
            SetupDiDestroyDeviceInfoList(set);
        }
        bool reboot;
        if (!UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hardwareId, infPath, INSTALLFLAG_FORCE, out reboot))
            throw Fail("UpdateDriverForPlugAndPlayDevices");
        return reboot;
    }
}
'@

$reboot = [RootDevice]::Install((Join-Path $work $Inf), $HardwareId)
Write-Host "VB-CABLE installed (reboot requested: $reboot)"

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
