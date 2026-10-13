# Dot-source for Get-SpatialEndpoints; execute for a JSON array (including []).
# Names are diagnostic data, never identity. Reference strings are minipairs.h.
# https://learn.microsoft.com/en-us/windows/win32/api/devicetopology/nf-devicetopology-iconnector-getdeviceidconnectedto
param([Alias('InstanceId')] [string] $FilterInstanceId = '')
if (-not ('SpatialEndpoints' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text.RegularExpressions;

public sealed class SpatialEndpoint {
    public int endpoint;
    public string id, name;
    public uint state;
}
public static class SpatialEndpoints {
    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    class EnumeratorClass { }
    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Enumerator {
        [PreserveSig] int EnumAudioEndpoints(int flow, uint mask, out Collection devices);
        [PreserveSig] int GetDefaultAudioEndpoint(int flow, int role, out Device device);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out Device device);
    }
    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Collection {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int Item(uint index, out Device device);
    }
    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Device {
        [PreserveSig] int Activate(ref Guid iid, uint context, IntPtr activation, [MarshalAs(UnmanagedType.IUnknown)] out object instance);
        [PreserveSig] int OpenPropertyStore(uint access, out Store store);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
        [PreserveSig] int GetState(out uint state);
    }
    [ComImport, Guid("2A07407E-6497-4A18-9787-32F79BD0D98F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Topology {
        [PreserveSig] int GetConnectorCount(out uint count);
        [PreserveSig] int GetConnector(uint index, out Connector connector);
    }
    // IConnector derives from IUnknown, NOT IPart (IPart is queried separately).
    [ComImport, Guid("9c2c4058-23f5-41de-877a-df3af236a09e"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Connector {
        [PreserveSig] int GetType(out int type);
        [PreserveSig] int GetDataFlow(out int flow);
        [PreserveSig] int ConnectTo(Connector other);
        [PreserveSig] int Disconnect();
        [PreserveSig] int IsConnected([MarshalAs(UnmanagedType.Bool)] out bool connected);
        [PreserveSig] int GetConnectedTo(out Connector other);
        [PreserveSig] int GetConnectorIdConnectedTo([MarshalAs(UnmanagedType.LPWStr)] out string id);
        [PreserveSig] int GetDeviceIdConnectedTo([MarshalAs(UnmanagedType.LPWStr)] out string id);
    }
    [StructLayout(LayoutKind.Sequential)]
    struct Key {
        public Guid fmtid; public uint pid;
        public Key(string guid, uint number) { fmtid = new Guid(guid); pid = number; }
    }
    [StructLayout(LayoutKind.Explicit, Size = 24)]
    struct Variant {
        [FieldOffset(0)] public ushort vt;
        [FieldOffset(8)] public IntPtr text;
    }
    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Store {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int GetAt(uint index, out Key key);
        [PreserveSig] int GetValue(ref Key key, out Variant value);
    }
    [DllImport("ole32.dll")] static extern int PropVariantClear(ref Variant value);
    static void Check(int hr) { Marshal.ThrowExceptionForHR(hr); }
    static void Release(object value) { if (value != null) Marshal.ReleaseComObject(value); }

    // Only a complete KS reference string counts. Also check the adapter's
    // PnP instance: another vendor's TopologyHdmi0 is not our endpoint 0.
    public static int MapFilter(string path, string[] owners) {
        if (path == null || owners == null) return -1;
        var match = Regex.Match(path, @"^(?:\\\\\?\\|\\\?\?\\)([^{}]+)#\{[0-9a-f-]{36}\}\\(?:TopologyHdmi|WaveHdmi)([0-2])$", RegexOptions.IgnoreCase);
        if (!match.Success) return -1;
        string owner = match.Groups[1].Value.Replace('#', '\\');
        foreach (string expected in owners)
            if (String.Equals(owner, expected, StringComparison.OrdinalIgnoreCase))
                return Int32.Parse(match.Groups[2].Value);
        return -1;
    }
    static string Name(Device device) {
        Store store; Check(device.OpenPropertyStore(0, out store));
        try {
            Key key = new Key("a45c254e-df1c-4efd-8020-67d146a850e0", 14); // PKEY_Device_FriendlyName
            Variant value; Check(store.GetValue(ref key, out value));
            try { return value.vt == 31 ? Marshal.PtrToStringUni(value.text) : ""; }
            finally { Check(PropVariantClear(ref value)); }
        } finally { Release(store); }
    }
    public static SpatialEndpoint[] Enumerate(string[] owners) {
        if (IntPtr.Size != 8) throw new PlatformNotSupportedException("Use 64-bit PowerShell");
        var result = new List<SpatialEndpoint>();
        Enumerator enumerator = (Enumerator)new EnumeratorClass();
        Collection devices = null;
        try {
            Check(enumerator.EnumAudioEndpoints(0, 15, out devices)); // eRender, DEVICE_STATEMASK_ALL
            uint count; Check(devices.GetCount(out count));
            for (uint index = 0; index < count; ++index) {
                Device device = null; object topologyObject = null;
                try {
                    Check(devices.Item(index, out device));
                    Guid iid = typeof(Topology).GUID;
                    Check(device.Activate(ref iid, 23, IntPtr.Zero, out topologyObject));
                    Topology topology = (Topology)topologyObject;
                    uint connectors; Check(topology.GetConnectorCount(out connectors));
                    int endpoint = -1;
                    for (uint c = 0; c < connectors; ++c) {
                        Connector connector = null;
                        try {
                            Check(topology.GetConnector(c, out connector));
                            string path;
                            int hr = connector.GetDeviceIdConnectedTo(out path);
                            if (hr == unchecked((int)0x80070490)) continue; // E_NOTFOUND: disconnected
                            Check(hr);
                            int mapped = MapFilter(path, owners);
                            if (mapped < 0) continue;
                            if (endpoint >= 0 && endpoint != mapped) throw new InvalidOperationException("ambiguous endpoint topology");
                            endpoint = mapped;
                        } finally { Release(connector); }
                    }
                    if (endpoint < 0) continue;
                    var item = new SpatialEndpoint(); item.endpoint = endpoint;
                    Check(device.GetId(out item.id)); Check(device.GetState(out item.state));
                    item.name = Name(device); result.Add(item);
                } catch (COMException error) {
                    // A removed/unavailable endpoint may have no topology. Never
                    // guess its number from the name; callers fail missing masks.
                    Console.Error.WriteLine("endpoint topology unavailable: " + error.Message);
                } finally { Release(topologyObject); Release(device); }
            }
        } finally { Release(devices); Release(enumerator); }
        return result.ToArray();
    }
}
'@
}

function Get-SpatialEndpoints {
    param([string] $InstanceId = '', [switch] $IncludeLegacy)
    . (Join-Path $PSScriptRoot 'package-identities.ps1')
    $identities = @($SpatialPackage)
    if ($IncludeLegacy) { $identities += $SpatialLegacyPackages }
    $hardwareIds = @($identities | ForEach-Object HardwareId)
    # InstanceId narrows the owned set; it never bypasses the hardware-ID check.
    $owners = @(Get-PnpDevice -ErrorAction Stop | Where-Object {
        $device = $_
        @($device.HardwareID | Where-Object { $_ -in $hardwareIds }).Count -gt 0 -and
        (-not $InstanceId -or $device.InstanceId -ieq $InstanceId)
    } | ForEach-Object InstanceId)
    if ($owners.Count) { [SpatialEndpoints]::Enumerate([string[]]$owners) }
}
if ($MyInvocation.InvocationName -ne '.') {
    $ErrorActionPreference = 'Stop'
    [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
    ConvertTo-Json -InputObject @(Get-SpatialEndpoints -InstanceId $FilterInstanceId) -Depth 4 -Compress
}
