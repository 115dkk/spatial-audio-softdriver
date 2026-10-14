# Shared read/control access to IAudioEndpointVolume. Dot-source; no changes on load.
if (-not ('SsrEndpointVolume' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public sealed class SsrVolumeSnapshot {
    public float master_db;
    public float[] channels_db;
    public bool mute;
    public uint hardware_support;
    public float minimum_db, maximum_db, step_db;
}

public sealed class SsrEndpointVolume : IDisposable {
    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    class EnumeratorClass { }
    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Enumerator {
        [PreserveSig] int EnumAudioEndpoints(int flow, uint mask, out IntPtr devices);
        [PreserveSig] int GetDefaultAudioEndpoint(int flow, int role, out IntPtr device);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out Device device);
        [PreserveSig] int RegisterEndpointNotificationCallback(IntPtr callback);
        [PreserveSig] int UnregisterEndpointNotificationCallback(IntPtr callback);
    }
    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Device {
        [PreserveSig] int Activate(ref Guid iid, uint context, IntPtr activation, [MarshalAs(UnmanagedType.IUnknown)] out object instance);
        [PreserveSig] int OpenPropertyStore(uint access, out IntPtr properties);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
        [PreserveSig] int GetState(out uint state);
    }
    [ComImport, Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Volume {
        [PreserveSig] int RegisterControlChangeNotify(IntPtr callback);
        [PreserveSig] int UnregisterControlChangeNotify(IntPtr callback);
        [PreserveSig] int GetChannelCount(out uint channels);
        [PreserveSig] int SetMasterVolumeLevel(float db, ref Guid context);
        [PreserveSig] int SetMasterVolumeLevelScalar(float value, ref Guid context);
        [PreserveSig] int GetMasterVolumeLevel(out float db);
        [PreserveSig] int GetMasterVolumeLevelScalar(out float value);
        [PreserveSig] int SetChannelVolumeLevel(uint channel, float db, ref Guid context);
        [PreserveSig] int SetChannelVolumeLevelScalar(uint channel, float value, ref Guid context);
        [PreserveSig] int GetChannelVolumeLevel(uint channel, out float db);
        [PreserveSig] int GetChannelVolumeLevelScalar(uint channel, out float value);
        [PreserveSig] int SetMute([MarshalAs(UnmanagedType.Bool)] bool mute, ref Guid context);
        [PreserveSig] int GetMute([MarshalAs(UnmanagedType.Bool)] out bool mute);
        [PreserveSig] int GetVolumeStepInfo(out uint step, out uint count);
        [PreserveSig] int VolumeStepUp(ref Guid context);
        [PreserveSig] int VolumeStepDown(ref Guid context);
        [PreserveSig] int QueryHardwareSupport(out uint mask);
        [PreserveSig] int GetVolumeRange(out float min, out float max, out float step);
    }
    [DllImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool QueryPerformanceCounter(out long value);
    public static long Qpc() {
        long value;
        if (!QueryPerformanceCounter(out value)) throw new InvalidOperationException("QueryPerformanceCounter failed");
        return value;
    }
    Volume volume;
    Guid context = new Guid("3481a551-10b8-4c0f-9272-94c37d65f180");
    static void Check(int result) { Marshal.ThrowExceptionForHR(result); }
    public SsrEndpointVolume(string id) {
        Enumerator enumerator = (Enumerator)new EnumeratorClass();
        Device device = null;
        object instance = null;
        try {
            Check(enumerator.GetDevice(id, out device));
            Guid iid = typeof(Volume).GUID;
            Check(device.Activate(ref iid, 23, IntPtr.Zero, out instance));
            volume = (Volume)instance;
            instance = null;
        } finally {
            if (instance != null) Marshal.ReleaseComObject(instance);
            if (device != null) Marshal.ReleaseComObject(device);
            Marshal.ReleaseComObject(enumerator);
        }
    }
    public SsrVolumeSnapshot Snapshot() {
        var result = new SsrVolumeSnapshot();
        uint count;
        Check(volume.GetChannelCount(out count));
        if (count == 0 || count > 32) throw new InvalidOperationException("endpoint channel count outside Spatial SoftReceiver contract");
        result.channels_db = new float[count];
        for (uint i = 0; i < count; ++i) Check(volume.GetChannelVolumeLevel(i, out result.channels_db[i]));
        Check(volume.GetMasterVolumeLevel(out result.master_db));
        Check(volume.GetMute(out result.mute));
        Check(volume.QueryHardwareSupport(out result.hardware_support));
        Check(volume.GetVolumeRange(out result.minimum_db, out result.maximum_db, out result.step_db));
        return result;
    }
    public void SetMaster(float db) { Check(volume.SetMasterVolumeLevel(db, ref context)); }
    public void SetChannel(uint channel, float db) { Check(volume.SetChannelVolumeLevel(channel, db, ref context)); }
    public void SetMute(bool mute) { Check(volume.SetMute(mute, ref context)); }
    public void Restore(SsrVolumeSnapshot original) {
        // Try every restoration even when one property call fails. Restore
        // master first, then the saved per-channel balance, then mute.
        Exception failure = null;
        try { SetMaster(original.master_db); } catch (Exception e) { failure = e; }
        for (uint i = 0; i < original.channels_db.Length; ++i) {
            try { SetChannel(i, original.channels_db[i]); } catch (Exception e) { failure = e; }
        }
        try { SetMute(original.mute); } catch (Exception e) { failure = e; }
        if (failure != null) throw new InvalidOperationException("volume restoration incomplete", failure);
    }
    public void Dispose() {
        if (volume != null) { Marshal.ReleaseComObject(volume); volume = null; }
    }
}
'@
}

. (Join-Path $PSScriptRoot 'endpoints.ps1')

function Get-SsrVolumeTargets {
    param([string] $InstanceId = '', [switch] $IncludeLegacy)
    Get-SpatialEndpoints -InstanceId $InstanceId -IncludeLegacy:$IncludeLegacy
}

function Get-SsrVolumeSnapshot {
    foreach ($target in Get-SsrVolumeTargets) {
        $control = $null
        try {
            $control = [SsrEndpointVolume]::new($target.id)
            [pscustomobject]@{ endpoint = $target.endpoint; name = $target.name; state = $target.state; volume = $control.Snapshot(); error = $null }
        } catch {
            [pscustomobject]@{ endpoint = $target.endpoint; name = $target.name; state = $target.state; volume = $null; error = $_.Exception.Message }
        } finally { if ($control) { $control.Dispose() } }
    }
}
