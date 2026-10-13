# Dot-source: endpoint policy access only; loading this file changes nothing.
# Private keys, NOT published Windows SDK PKEYs. ADR 0028 requires a behavior
# gate on every supported OS; a successful SetValue is not proof of enforcement.
# https://techcommunity.microsoft.com/discussions/windowspowershell/disable-exclusive-mode-for-all-audio-devices-via-ps/3653408/replies/3881222
# https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdevice-openpropertystore
if (-not ('BioExclusivePolicy' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public sealed class BioPolicyValues {
    public uint? allow_exclusive, exclusive_priority;
    public ushort allow_type, priority_type;
}
public sealed class BioExclusiveProbe {
    public bool supported;
    public int channels, rate, bits;
    public string sample, support_hr, initialize_hr;
    public bool alignment_retry;
}
public sealed class BioExclusivePolicy : IDisposable {
    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    class EnumeratorClass { }
    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Enumerator {
        [PreserveSig] int EnumAudioEndpoints(int flow, uint mask, out IntPtr devices);
        [PreserveSig] int GetDefaultAudioEndpoint(int flow, int role, out IntPtr device);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out Device device);
    }
    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Device {
        [PreserveSig] int Activate(ref Guid iid, uint context, IntPtr activation, [MarshalAs(UnmanagedType.IUnknown)] out object instance);
        [PreserveSig] int OpenPropertyStore(uint access, out Store store);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
        [PreserveSig] int GetState(out uint state);
    }
    [StructLayout(LayoutKind.Sequential)]
    struct Key {
        public Guid fmtid;
        public uint pid;
        public Key(uint id) { fmtid = new Guid("b3f8fa53-0004-438e-9003-51a46e139bfc"); pid = id; }
    }
    // x64/ARM64 PROPVARIANT, including room for the 16-byte BLOB/CA union.
    [StructLayout(LayoutKind.Explicit, Size = 24)]
    struct Variant {
        [FieldOffset(0)] public ushort vt;
        [FieldOffset(8)] public uint value;
    }
    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Store {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int GetAt(uint index, out Key key);
        [PreserveSig] int GetValue(ref Key key, out Variant value);
        [PreserveSig] int SetValue(ref Key key, ref Variant value);
        [PreserveSig] int Commit();
    }
    [ComImport, Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface AudioClient {
        [PreserveSig] int Initialize(int mode, uint flags, long duration, long period, IntPtr format, IntPtr session);
        [PreserveSig] int GetBufferSize(out uint frames);
        [PreserveSig] int GetStreamLatency(out long latency);
        [PreserveSig] int GetCurrentPadding(out uint frames);
        [PreserveSig] int IsFormatSupported(int mode, IntPtr format, IntPtr closest);
        [PreserveSig] int GetMixFormat(out IntPtr format);
        [PreserveSig] int GetDevicePeriod(out long normal, out long minimum);
        [PreserveSig] int Start();
        [PreserveSig] int Stop();
        [PreserveSig] int Reset();
        [PreserveSig] int SetEventHandle(IntPtr handle);
        [PreserveSig] int GetService(ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object instance);
    }
    [DllImport("ole32.dll")] static extern int PropVariantClear(ref Variant value);
    Device device;
    static void Check(int hr) { Marshal.ThrowExceptionForHR(hr); }
    static string Hex(int hr) { return "0x" + unchecked((uint)hr).ToString("X8"); }
    public BioExclusivePolicy(string id) {
        if (IntPtr.Size != 8) throw new PlatformNotSupportedException("Use 64-bit PowerShell");
        Enumerator enumerator = (Enumerator)new EnumeratorClass();
        try { Check(enumerator.GetDevice(id, out device)); }
        finally { Marshal.ReleaseComObject(enumerator); }
    }
    static uint? ReadValue(Store store, uint pid, out ushort type) {
        Key key = new Key(pid);
        Variant value;
        Check(store.GetValue(ref key, out value));
        try {
            type = value.vt;
            if (type == 0) return null;
            if (type == 19) return value.value; // VT_UI4 / REG_DWORD
            throw new InvalidOperationException("policy " + pid + " has unexpected variant type " + type);
        } finally { Check(PropVariantClear(ref value)); }
    }
    public BioPolicyValues Read() {
        Store store;
        Check(device.OpenPropertyStore(0, out store));
        try {
            var result = new BioPolicyValues();
            result.allow_exclusive = ReadValue(store, 3, out result.allow_type);
            result.exclusive_priority = ReadValue(store, 4, out result.priority_type);
            return result;
        } finally { Marshal.ReleaseComObject(store); }
    }
    public void Write(uint? allow, uint? priority) {
        Store store;
        Check(device.OpenPropertyStore(2, out store)); // STGM_READWRITE, elevated
        try {
            uint?[] values = { allow, priority };
            for (uint i = 0; i < 2; ++i) {
                Key key = new Key(3 + i);
                Variant value = new Variant();
                value.vt = (ushort)(values[i].HasValue ? 19 : 0);
                value.value = values[i].GetValueOrDefault();
                Check(store.SetValue(ref key, ref value));
            }
            Check(store.Commit());
        } finally { Marshal.ReleaseComObject(store); }
    }
    AudioClient Activate() {
        object instance;
        Guid iid = typeof(AudioClient).GUID;
        Check(device.Activate(ref iid, 23, IntPtr.Zero, out instance));
        return (AudioClient)instance;
    }
    public BioExclusiveProbe Probe() {
        var result = new BioExclusiveProbe();
        IntPtr format = Marshal.AllocCoTaskMem(40);
        AudioClient client = null;
        try {
            client = Activate();
            // Negotiate a concrete PCM format; GetMixFormat alone does not
            // establish that a driver accepts the format in exclusive mode.
            foreach (int channels in new int[] {2, 6, 8, 12})
            foreach (int rate in new int[] {48000, 44100, 96000, 88200})
            foreach (int shape in new int[] {0, 1, 2}) {
                byte[] wave = new byte[40];
                int bits = shape == 0 ? 16 : 32;
                int valid = shape == 1 ? 24 : bits;
                int block = channels * bits / 8;
                Action<int, short> put16 = (at, v) => Array.Copy(BitConverter.GetBytes(v), 0, wave, at, 2);
                Action<int, int> put32 = (at, v) => Array.Copy(BitConverter.GetBytes(v), 0, wave, at, 4);
                put16(0, unchecked((short)0xfffe)); put16(2, (short)channels);
                put32(4, rate); put32(8, rate * block); put16(12, (short)block);
                put16(14, (short)bits); put16(16, 22); put16(18, (short)valid);
                put32(20, channels == 2 ? 3 : channels == 6 ? 0x3f : channels == 8 ? 0x63f : 0x2d63f);
                Guid sub = new Guid(shape == 2 ? "00000003-0000-0010-8000-00aa00389b71" : "00000001-0000-0010-8000-00aa00389b71");
                Array.Copy(sub.ToByteArray(), 0, wave, 24, 16);
                Marshal.Copy(wave, 0, format, wave.Length);
                int supported = client.IsFormatSupported(1, format, IntPtr.Zero);
                result.support_hr = Hex(supported);
                if (supported != 0) continue;
                result.supported = true; result.channels = channels; result.rate = rate;
                result.bits = bits; result.sample = shape == 2 ? "float" : "PCM";
                long normal, minimum;
                Check(client.GetDevicePeriod(out normal, out minimum));
                long period = Math.Max(normal, minimum);
                if (period <= 0) throw new InvalidOperationException("invalid device period");
                int hr = client.Initialize(1, 0, period, period, format, IntPtr.Zero);
                if (unchecked((uint)hr) == 0x88890019) { // BUFFER_SIZE_NOT_ALIGNED
                    uint frames;
                    Check(client.GetBufferSize(out frames));
                    Marshal.ReleaseComObject(client); client = null;
                    client = Activate();
                    period = (10000000L * frames + rate / 2) / rate;
                    result.alignment_retry = true;
                    hr = client.Initialize(1, 0, period, period, format, IntPtr.Zero);
                }
                result.initialize_hr = Hex(hr);
                return result;
            }
            return result;
        } finally {
            if (client != null) Marshal.ReleaseComObject(client);
            Marshal.FreeCoTaskMem(format);
        }
    }
    public void Dispose() {
        if (device != null) { Marshal.ReleaseComObject(device); device = null; }
    }
}
'@
}

. (Join-Path $PSScriptRoot 'endpoints.ps1')

function Get-BioPolicyTargets {
    param([string] $InstanceId = '', [switch] $IncludeLegacy)
    Get-SpatialEndpoints -InstanceId $InstanceId -IncludeLegacy:$IncludeLegacy
}

function Get-BioPolicySnapshot {
    param([string] $InstanceId = '')
    foreach ($target in Get-BioPolicyTargets -InstanceId $InstanceId) {
        $control = $null
        try {
            $control = [BioExclusivePolicy]::new($target.id)
            [pscustomobject]@{ endpoint = $target.endpoint; name = $target.name; id = $target.id; state = $target.state; policy = $control.Read(); error = $null }
        } catch {
            [pscustomobject]@{ endpoint = $target.endpoint; name = $target.name; id = $target.id; state = $target.state; policy = $null; error = $_.Exception.Message }
        } finally { if ($control) { $control.Dispose() } }
    }
}

function Set-BioExclusivePolicy {
    param([Parameter(Mandatory)] [string] $InstanceId, [int] $EndpointMask = 7)
    $targets = @(Get-BioPolicyTargets -InstanceId $InstanceId | Where-Object { $_.state -eq 1 })
    foreach ($ep in 0..2 | Where-Object { $EndpointMask -band (1 -shl $_) }) {
        $matches = @($targets | Where-Object endpoint -eq $ep)
        if ($matches.Count -ne 1) { throw "expected one active endpoint $ep, found $($matches.Count)" }
        $control = [BioExclusivePolicy]::new($matches[0].id)
        try {
            $value = if ($ep -eq 2) { [uint32]1 } else { [uint32]0 }
            $control.Write($value, $value)
            $readback = $control.Read()
            if ($readback.allow_exclusive -ne $value -or $readback.exclusive_priority -ne $value) { throw "endpoint $ep policy readback differs" }
        } finally { $control.Dispose() }
    }
}

function Clear-BioExclusivePolicy {
    param([Parameter(Mandatory)] [string] $InstanceId)
    # Only the newly installed BIO endpoints: remove both private properties
    # before removing the device, so an MMDevices tombstone keeps no policy.
    foreach ($target in Get-BioPolicyTargets -InstanceId $InstanceId -IncludeLegacy) {
        $control = [BioExclusivePolicy]::new($target.id)
        try {
            $control.Write($null, $null)
            $readback = $control.Read()
            if ($null -ne $readback.allow_exclusive -or $null -ne $readback.exclusive_priority) { throw "endpoint $($target.endpoint) policy was not cleared" }
        } finally { $control.Dispose() }
    }
}
