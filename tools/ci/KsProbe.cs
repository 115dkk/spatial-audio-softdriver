// Probes KS filters, pin factories, formats and jack properties without starting a stream.
// CI only: load with PowerShell Add-Type. Run and TryCreatePin are deliberately separate
// so Run's diagnostics can be printed before a pin-create request enters the driver.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class KsProbe
{
    // Windows SDK/WDK 10.0.26100.0, shared/ks.h and shared/ksmedia.h.
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ks/ns-ks-ksp_pin
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ks/ns-ks-ksdataformat
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ksmedia/ns-ksmedia-ksdatarange_audio
    // KSPROPERTY: GUID@0, Id@16, Flags@20; KSP_PIN adds PinId@24, Reserved@28.
    // KSDATARANGE: ULONGs@0/4/8/12, GUIDs@16/32/48, alignment 8.
    // AUDIO adds FIVE ULONGs@64/68/72/76/80, then 4 padding bytes: 88, NOT 80.
    const int PropertySize = 24, PinSize = 32, MultipleSize = 8, RangeSize = 64;
    const int AudioRangeSize = 88, WaveFormatExSize = 18, WaveExtensibleSize = 40;
    const int FormatWaveSize = 82, FormatExtensibleSize = 104;
    // ksmedia.h packs KSDATAFORMAT_WAVEFORMATEX and WAVEFORMATEXTENSIBLE to 1.
    // WAVEFORMATEX: WORD@0/2, DWORD@4/8, WORD@12/14/16.
    // EXTENSIBLE adds valid bits@18, channel mask@20, SubFormat GUID@24.
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ks/ns-ks-kspin_connect
    // x64 KSPIN_CONNECT: identifiers@0/24, PinId@48, padding@52, HANDLE@56,
    // KSPRIORITY@64/68; the 104-byte format immediately follows at offset 72.
    const int ConnectSize = 72, ComponentSize = 72, DescriptionSize = 40;
    const int JackSize = 28, Jack2Size = 8, SinkSize = 96, AttributeSize = 24;
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ksmedia/ns-ksmedia-ksjack_description
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ksmedia/ns-ksmedia-_tagksjack_description2
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ksmedia/ns-ksmedia-_tagksjack_sink_information
    // JACK: seven 4-byte fields. JACK2: DWORDs@0/4. SINK: enum@0, WORDs@4/6/8,
    // padding@10, BOOLs@12/16, UCHAR@20, padding@21, WCHAR[32]@22,
    // padding@86, LUID.LowPart@88/HighPart@92. BOOL is 4 bytes, not C# bool.
    // KSPROPERTY_DESCRIPTION: Access@0, Size@4, KSIDENTIFIER@8, Count@32, Reserved@36.
    // KSMULTIPLE_ITEM: Size@0, Count@4. KSATTRIBUTE: Size@0, Flags@4, GUID@8.
    const uint Get = 1, Set = 2, BasicSupport = 0x200;
    const uint IoctlKsProperty = 0x002f0003, ReadWrite = 0xc0000000;
    const int MaxBuffer = 1024 * 1024, MaxPins = 256;
    static readonly IntPtr InvalidHandle = new IntPtr(-1);
    static readonly Guid AudioCategory = new Guid("6994ad04-93ef-11d0-a3cc-00a0c9223196");
    static readonly Guid RenderCategory = new Guid("65e8773e-8f56-11d0-a3b9-00a0c9223196");
    static readonly Guid RealtimeCategory = new Guid("eb115ffc-10c8-4964-831d-6dcb02e6f23f");
    static readonly Guid TopologyCategory = new Guid("dda54a40-1e4c-11d1-a050-405705c10000");
    static readonly Guid PinSet = new Guid("8c134960-51ad-11cf-878a-94f801c10000");
    static readonly Guid GeneralSet = new Guid("1464eda5-6a8f-11d1-9aa7-00a0c9223196");
    static readonly Guid TopologySet = new Guid("720d4ac0-7533-11d0-a5d6-28db04c10000");
    static readonly Guid JackSet = new Guid("4509f757-2d46-4637-8e62-ce7db944f57b");
    static readonly Guid ProcessingSet = new Guid("4f67b528-30c9-40de-b2fb-859ddd1f3470");
    static readonly Guid AudioType = new Guid("73647561-0000-0010-8000-00aa00389b71");
    static readonly Guid PcmSubtype = new Guid("00000001-0000-0010-8000-00aa00389b71");
    static readonly Guid WaveSpecifier = new Guid("05589f81-c356-11ce-bf01-00aa0055595a");
    static readonly Guid StandardInterface = new Guid("1a8766a0-62ce-11cf-a5d6-28db04c10000");
    static readonly Guid StandardMedium = new Guid("4747b320-62ce-11cf-a5d6-28db04c10000");
    static readonly Guid ModeAttribute = new Guid("e1f89eb5-5f46-419b-967b-ff6770b98401");
    static readonly Guid DefaultMode = new Guid("c18e2f7e-933d-4965-b7d1-1eef228d2af3");

    [StructLayout(LayoutKind.Sequential)]
    struct SP_DEVICE_INTERFACE_DATA
    {
        public int cbSize;
        public Guid InterfaceClassGuid;
        public uint Flags;
        public IntPtr Reserved;
    }

    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr SetupDiGetClassDevsW(ref Guid classGuid, string enumerator,
        IntPtr parent, uint flags);
    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiEnumDeviceInterfaces(IntPtr set, IntPtr device,
        ref Guid classGuid, uint index, ref SP_DEVICE_INTERFACE_DATA data);
    [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool SetupDiGetDeviceInterfaceDetailW(IntPtr set,
        ref SP_DEVICE_INTERFACE_DATA data, IntPtr detail, uint size, out uint required, IntPtr device);
    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr security,
        uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateEventW(IntPtr attributes, bool manualReset, bool initial, string name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(IntPtr handle, uint code, IntPtr input, uint inputSize,
        IntPtr output, uint outputSize, out uint returned, IntPtr overlapped);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetOverlappedResult(IntPtr handle, IntPtr overlapped,
        out uint returned, bool wait);
    // The user-mode ksuser.dll export returns a Win32 DWORD, not the kernel NTSTATUS.
    [DllImport("ksuser.dll", ExactSpelling = true)]
    static extern uint KsCreatePin(IntPtr filter, IntPtr connect, uint access, out IntPtr pin);

    sealed class Reply
    {
        public bool Success;
        public int Error;
        public uint Returned;
        public byte[] Data = new byte[0];
    }

    public static List<string> Run(string instanceFragment)
    {
        var lines = new List<string>();
        try
        {
            CheckLayout();
            ValidateFragment(instanceFragment);
            List<string> paths = Enumerate(AudioCategory, instanceFragment);
            Guid[] categories = { RenderCategory, RealtimeCategory, TopologyCategory };
            var membership = new Dictionary<Guid, HashSet<string>>();
            foreach (Guid category in categories)
            {
                try { membership[category] = new HashSet<string>(Enumerate(category, instanceFragment), StringComparer.OrdinalIgnoreCase); }
                catch (Exception e) { lines.Add("ks enumeration " + GuidName(category) + ": " + ExceptionText(e)); }
            }
            if (paths.Count == 0) lines.Add("ks enumeration: no KSCATEGORY_AUDIO interfaces matching " + OneLine(instanceFragment));
            foreach (string path in paths)
            {
                string prefix = "ks " + FilterName(path);
                string tags = " KSCATEGORY_AUDIO=yes";
                foreach (Guid category in categories)
                {
                    HashSet<string> members;
                    string candidate = path.Replace(AudioCategory.ToString("B"), category.ToString("B"));
                    // SetupAPI casing is not contractual, including the embedded GUID.
                    int at = path.IndexOf(AudioCategory.ToString("B"), StringComparison.OrdinalIgnoreCase);
                    if (at >= 0) candidate = path.Substring(0, at) + category.ToString("B") + path.Substring(at + 38);
                    tags += " " + GuidName(category) + "=" + (membership.TryGetValue(category, out members)
                        ? (members.Contains(candidate) ? "yes" : "no") : "unknown");
                }
                lines.Add(prefix + " interface: " + OneLine(path) + tags);
                WithFilter(path, prefix, lines, delegate(IntPtr handle) { ProbeFilter(handle, prefix, lines, false); });
            }
        }
        catch (Exception e) { lines.Add("ks Run: " + ExceptionText(e)); }
        return lines;
    }

    public static List<string> TryCreatePin(string instanceFragment)
    {
        var lines = new List<string>();
        try
        {
            CheckLayout();
            ValidateFragment(instanceFragment);
            List<string> paths = Enumerate(AudioCategory, instanceFragment);
            if (paths.Count == 0) lines.Add("ks enumeration: no KSCATEGORY_AUDIO interfaces matching " + OneLine(instanceFragment));
            foreach (string path in paths)
            {
                string prefix = "ks " + FilterName(path);
                lines.Add(prefix + " interface: " + OneLine(path));
                WithFilter(path, prefix, lines, delegate(IntPtr handle) { ProbeFilter(handle, prefix, lines, true); });
            }
        }
        catch (Exception e) { lines.Add("ks TryCreatePin: " + ExceptionText(e)); }
        return lines;
    }

    static void ValidateFragment(string fragment)
    {
        if (String.IsNullOrWhiteSpace(fragment)) throw new ArgumentException("instanceFragment must not be empty; refusing to probe all devices");
    }

    static List<string> Enumerate(Guid category, string fragment)
    {
        var paths = new List<string>();
        IntPtr set;
        try { set = SetupDiGetClassDevsW(ref category, null, IntPtr.Zero, 0x12); }
        catch (Exception e)
        {
            // Some Linux PowerShell builds throw from their native DLL resolver before
            // producing DllNotFoundException. Preserve that error and identify the call.
            throw new InvalidOperationException("SetupDiGetClassDevsW (setupapi.dll): " + ExceptionText(e), e);
        }
        if (set == InvalidHandle) throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiGetClassDevs");
        try
        {
            for (uint index = 0; ; index++)
            {
                var data = new SP_DEVICE_INTERFACE_DATA();
                data.cbSize = Marshal.SizeOf(typeof(SP_DEVICE_INTERFACE_DATA));
                if (!SetupDiEnumDeviceInterfaces(set, IntPtr.Zero, ref category, index, ref data))
                {
                    int error = Marshal.GetLastWin32Error();
                    if (error == 259) break;
                    throw new Win32Exception(error, "SetupDiEnumDeviceInterfaces");
                }
                uint required;
                bool sized = SetupDiGetDeviceInterfaceDetailW(set, ref data, IntPtr.Zero, 0, out required, IntPtr.Zero);
                int sizeError = sized ? 0 : Marshal.GetLastWin32Error();
                if (!sized && sizeError != 122) throw new Win32Exception(sizeError, "SetupDiGetDeviceInterfaceDetail(size)");
                if (required < 8 || required > MaxBuffer) throw new InvalidOperationException("invalid interface detail size " + required);
                IntPtr detail = Marshal.AllocHGlobal((int)required);
                try
                {
                    // cbSize is 8 on x64, but DevicePath still starts at byte 4 (WCHAR alignment).
                    Marshal.WriteInt32(detail, 8);
                    if (!SetupDiGetDeviceInterfaceDetailW(set, ref data, detail, required, out required, IntPtr.Zero))
                        throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiGetDeviceInterfaceDetail");
                    string path = Marshal.PtrToStringUni(IntPtr.Add(detail, 4), ((int)required - 4) / 2).TrimEnd('\0');
                    if (path.IndexOf(fragment, StringComparison.OrdinalIgnoreCase) >= 0) paths.Add(path);
                }
                finally { Marshal.FreeHGlobal(detail); }
            }
        }
        finally { SetupDiDestroyDeviceInfoList(set); }
        return paths;
    }

    static void WithFilter(string path, string prefix, List<string> lines, Action<IntPtr> action)
    {
        IntPtr handle = InvalidHandle;
        try
        {
            handle = CreateFileW(path, ReadWrite, 3, IntPtr.Zero, 3, 0x40000000, IntPtr.Zero);
            if (handle == InvalidHandle) { lines.Add(prefix + " CreateFile: " + ErrorText(Marshal.GetLastWin32Error())); return; }
            lines.Add(prefix + " CreateFile: OK (read/write, shared read/write, overlapped)");
            action(handle);
        }
        catch (Exception e) { lines.Add(prefix + ": " + ExceptionText(e)); }
        finally { if (handle != InvalidHandle && handle != IntPtr.Zero) CloseHandle(handle); }
    }

    // METHOD_NEITHER needs buffers AND OVERLAPPED to remain stable while pending. All are
    // unmanaged until GetOverlappedResult(wait=true) completes, including error completions.
    // No timeout/free race: CI must impose a process/job timeout if the driver never completes.
    static Reply Ioctl(IntPtr handle, byte[] input, int outputSize, byte[] initialOutput)
    {
        IntPtr inBuffer = IntPtr.Zero, outBuffer = IntPtr.Zero, ov = IntPtr.Zero, done = IntPtr.Zero;
        try
        {
            inBuffer = Marshal.AllocHGlobal(input.Length);
            Marshal.Copy(input, 0, inBuffer, input.Length);
            if (outputSize > 0)
            {
                outBuffer = Marshal.AllocHGlobal(outputSize);
                byte[] zero = new byte[outputSize];
                if (initialOutput != null) Buffer.BlockCopy(initialOutput, 0, zero, 0, initialOutput.Length);
                Marshal.Copy(zero, 0, outBuffer, outputSize);
            }
            done = CreateEventW(IntPtr.Zero, true, false, null);
            if (done == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateEvent");
            // x64 OVERLAPPED: Internal@0, InternalHigh@8, Offset/OffsetHigh@16/20, event@24.
            ov = Marshal.AllocHGlobal(32);
            Marshal.Copy(new byte[32], 0, ov, 32);
            Marshal.WriteIntPtr(ov, 24, done);
            uint returned;
            bool success = DeviceIoControl(handle, IoctlKsProperty, inBuffer, (uint)input.Length,
                outBuffer, (uint)outputSize, out returned, ov);
            int error = success ? 0 : Marshal.GetLastWin32Error();
            if (!success && error == 997)
            {
                success = GetOverlappedResult(handle, ov, out returned, true);
                error = success ? 0 : Marshal.GetLastWin32Error();
            }
            // InternalHigh carries IoStatus.Information, also for STATUS_BUFFER_OVERFLOW.
            ulong information = unchecked((ulong)Marshal.ReadInt64(ov, 8));
            if (!success && returned == 0 && information <= UInt32.MaxValue) returned = (uint)information;
            var result = new Reply { Success = success, Error = error, Returned = returned };
            int copied = (int)Math.Min(returned, (uint)outputSize);
            result.Data = new byte[copied];
            if (copied > 0) Marshal.Copy(outBuffer, result.Data, 0, copied);
            return result;
        }
        finally
        {
            if (ov != IntPtr.Zero) Marshal.FreeHGlobal(ov);
            if (done != IntPtr.Zero) CloseHandle(done);
            if (outBuffer != IntPtr.Zero) Marshal.FreeHGlobal(outBuffer);
            if (inBuffer != IntPtr.Zero) Marshal.FreeHGlobal(inBuffer);
        }
    }

    static bool SizeError(int error)
    {
        return error == 234 || error == 122 || unchecked((uint)error) == 0x80000005;
    }

    static string Status(Reply reply)
    {
        return (reply.Success ? "OK" : ErrorText(reply.Error)) + " bytes=" + reply.Returned;
    }

    static byte[] Query(IntPtr handle, string label, byte[] request, int fallback,
        List<string> lines, Action<byte[]> decode)
    {
        try
        {
            Reply size = Ioctl(handle, request, 0, null);
            lines.Add(label + " [size]: " + Status(size));
            if (!size.Success && !SizeError(size.Error)) return null;
            uint capacity = size.Returned != 0 ? size.Returned : (uint)fallback;
            // Some PortCls fixed-size handlers give ERROR_INSUFFICIENT_BUFFER without a size.
            // A bounded fallback is also useful for variable properties on older drivers.
            if (capacity == 0) capacity = 4096;
            for (int attempt = 0; attempt < 3; attempt++)
            {
                if (capacity > MaxBuffer) throw new InvalidOperationException("required buffer exceeds 1 MiB: " + capacity);
                Reply result = Ioctl(handle, request, (int)capacity, null);
                lines.Add(label + ": " + Status(result));
                if (result.Success)
                {
                    if (result.Returned > capacity) throw new InvalidOperationException("returned size exceeds output buffer");
                    decode(result.Data);
                    return result.Data;
                }
                if (!SizeError(result.Error)) return null;
                capacity = result.Returned > capacity ? result.Returned : capacity * 2;
            }
            lines.Add(label + ": stopped after 3 buffer retries");
        }
        catch (Exception e) { lines.Add(label + ": " + ExceptionText(e)); }
        return null;
    }

    static void FixedQuery(IntPtr handle, string label, byte[] request, int capacity,
        byte[] initialOutput, List<string> lines, Action<byte[]> decode)
    {
        try
        {
            Reply result = Ioctl(handle, request, capacity, initialOutput);
            lines.Add(label + ": " + Status(result));
            if (result.Success && decode != null)
            {
                if (result.Returned > capacity) throw new InvalidOperationException("returned size exceeds output buffer");
                decode(result.Data);
            }
        }
        catch (Exception e) { lines.Add(label + ": " + ExceptionText(e)); }
    }

    static void ProbeFilter(IntPtr handle, string prefix, List<string> lines, bool createOnly)
    {
        if (!createOnly)
        {
            string component = prefix + " GENERAL_COMPONENTID";
            Query(handle, component, Property(GeneralSet, 0, Get), ComponentSize, lines, delegate(byte[] b)
            {
                Require(b, 0, ComponentSize);
                lines.Add(component + ": Manufacturer=" + G(b, 0) + " Product=" + G(b, 16) +
                    " Component=" + G(b, 32) + " Name=" + G(b, 48) + " Version=" + U32(b, 64) + " Revision=" + U32(b, 68));
            });
            QueryGuidList(handle, prefix + " TOPOLOGY_CATEGORIES", Property(TopologySet, 0, Get), lines);
            QueryGuidList(handle, prefix + " TOPOLOGY_NODES", Property(TopologySet, 1, Get), lines);
            string connection = prefix + " TOPOLOGY_CONNECTIONS";
            Query(handle, connection, Property(TopologySet, 2, Get), 0, lines, delegate(byte[] b)
            {
                uint count = ListCount(b, 16);
                lines.Add(connection + ": Count=" + count);
                for (int i = 0; i < count; i++)
                {
                    int p = MultipleSize + i * 16;
                    lines.Add(connection + " [" + i + "]: FromNode=" + Node(U32(b, p)) + " FromNodePin=" + U32(b, p + 4) +
                        " ToNode=" + Node(U32(b, p + 8)) + " ToNodePin=" + U32(b, p + 12));
                }
            });
        }
        uint countPins = 0;
        Query(handle, prefix + " PIN_CTYPES", Property(PinSet, 1, Get), 4, lines,
            delegate(byte[] b) { countPins = U32(b, 0); lines.Add(prefix + " PIN_CTYPES: " + countPins); });
        if (countPins > MaxPins) { lines.Add(prefix + " PIN_CTYPES: exceeds safety limit " + MaxPins); return; }
        for (uint pin = 0; pin < countPins; pin++)
        {
            string p = prefix + " pin " + pin;
            try { ProbePin(handle, p, pin, lines, createOnly); }
            catch (Exception e) { lines.Add(p + ": " + ExceptionText(e)); }
        }
    }

    static void ProbePin(IntPtr handle, string prefix, uint pin, List<string> lines, bool createOnly)
    {
        uint flow = 0, communication = UInt32.MaxValue;
        Query(handle, prefix + " DATAFLOW", PinProperty(PinSet, 2, Get, pin), 4, lines,
            delegate(byte[] b) { flow = U32(b, 0); lines.Add(prefix + " DATAFLOW: " + flow + " " + (flow == 1 ? "IN" : flow == 2 ? "OUT" : "unknown")); });
        Query(handle, prefix + " COMMUNICATION", PinProperty(PinSet, 7, Get, pin), 4, lines,
            delegate(byte[] b)
            {
                communication = U32(b, 0);
                string[] names = { "NONE", "SINK", "SOURCE", "BOTH", "BRIDGE" };
                lines.Add(prefix + " COMMUNICATION: " + communication + " " + (communication < names.Length ? names[communication] : "unknown"));
            });
        if (createOnly)
        {
            if (flow == 1 && communication == 1) CreateHostPin(handle, prefix, pin, lines);
            else lines.Add(prefix + " KsCreatePin: skipped (requires known DATAFLOW_IN and SINK)");
            return;
        }
        Query(handle, prefix + " CATEGORY", PinProperty(PinSet, 11, Get, pin), 16, lines,
            delegate(byte[] b) { lines.Add(prefix + " CATEGORY: " + G(b, 0)); });
        Query(handle, prefix + " NAME", PinProperty(PinSet, 12, Get, pin), 0, lines,
            delegate(byte[] b) { lines.Add(prefix + " NAME: " + Unicode(b, 0, b.Length)); });
        Query(handle, prefix + " CINSTANCES", PinProperty(PinSet, 0, Get, pin), 8, lines,
            delegate(byte[] b) { lines.Add(prefix + " CINSTANCES: PossibleCount=" + U32(b, 0) + " CurrentCount=" + U32(b, 4)); });
        Query(handle, prefix + " NECESSARYINSTANCES", PinProperty(PinSet, 9, Get, pin), 4, lines,
            delegate(byte[] b) { lines.Add(prefix + " NECESSARYINSTANCES: " + U32(b, 0)); });
        Query(handle, prefix + " DATARANGES", PinProperty(PinSet, 3, Get, pin), 0, lines,
            delegate(byte[] b) { DecodeRanges(b, prefix + " DATARANGES", lines); });
        Query(handle, prefix + " CONSTRAINEDDATARANGES", PinProperty(PinSet, 13, Get, pin), 0, lines,
            delegate(byte[] b) { DecodeRanges(b, prefix + " CONSTRAINEDDATARANGES", lines); });
        Query(handle, prefix + " PHYSICALCONNECTION", PinProperty(PinSet, 10, Get, pin), 0, lines,
            delegate(byte[] b)
            {
                int size = CheckedSize(U32(b, 0));
                Require(b, 0, size);
                if (size < 10) throw new InvalidOperationException("physical connection is shorter than its WCHAR name");
                lines.Add(prefix + " PHYSICALCONNECTION: Size=" + size + " Pin=" + U32(b, 4) + " SymbolicLinkName=" + Unicode(b, 8, size - 8));
            });
        if (communication == 1) ProbeFormats(handle, prefix, pin, lines);
        for (uint id = 1; id <= 3; id++)
        {
            uint jackId = id;
            string name = id == 1 ? "JACK_DESCRIPTION" : id == 2 ? "JACK_DESCRIPTION2" : "JACK_SINK_INFO";
            string label = prefix + " " + name;
            Query(handle, label, PinProperty(JackSet, id, Get, pin), id == 3 ? SinkSize : 0, lines,
                delegate(byte[] b) { DecodeJack(b, jackId, label, lines); });
            Basic(handle, label, PinProperty(JackSet, id, BasicSupport, pin), lines);
        }
    }

    static void ProbeFormats(IntPtr handle, string prefix, uint pin, List<string> lines)
    {
        byte[] pcm = null;
        string[] names = { "PCM-48000-2ch-16bit", "audio-wide", "wildcard" };
        for (int kind = 0; kind < 3; kind++)
        {
            string label = prefix + " DATAINTERSECTION " + names[kind];
            byte[] result = Query(handle, label, Intersection(pin, kind), 0, lines,
                delegate(byte[] b) { DecodeFormat(b, label, lines); });
            if (kind == 0) pcm = result;
        }
        string propose = prefix + " PROPOSEDATAFORMAT";
        Query(handle, propose + " GET", PinProperty(PinSet, 14, Get, pin), 0, lines,
            delegate(byte[] b) { DecodeFormat(b, propose + " GET", lines); });
        if (pcm != null)
        {
            // For IOCTL_KS_PROPERTY SET, the value is the SECOND DeviceIoControl buffer,
            // not part of the KSP_PIN descriptor. This is the only SET in this tool.
            FixedQuery(handle, propose + " SET (PCM intersection result)", PinProperty(PinSet, 14, Set, pin),
                pcm.Length, pcm, lines, null);
        }
        else lines.Add(propose + " SET: skipped (no valid PCM intersection result)");
        Basic(handle, propose, PinProperty(PinSet, 14, BasicSupport, pin), lines);
        // ksmedia.h's enum begins at zero. Keep the requested id=1 probe distinct so its
        // unsupported response is never misreported as failure of the actual MODES property.
        QueryGuidList(handle, prefix + " AUDIOSIGNALPROCESSING_MODES (SDK id=0)", PinProperty(ProcessingSet, 0, Get, pin), lines);
        QueryGuidList(handle, prefix + " AudioSignalProcessing (requested id=1, not SDK MODES)", PinProperty(ProcessingSet, 1, Get, pin), lines);
        string proposal2 = prefix + " PROPOSEDATAFORMAT2 GET AUDIO_SIGNALPROCESSINGMODE_DEFAULT";
        Query(handle, proposal2, Propose2(pin), 0, lines,
            delegate(byte[] b) { DecodeFormat(b, proposal2, lines); });
        Basic(handle, prefix + " PROPOSEDATAFORMAT2", PinProperty(PinSet, 15, BasicSupport, pin), lines);
    }

    static void Basic(IntPtr handle, string label, byte[] request, List<string> lines)
    {
        FixedQuery(handle, label + " BASICSUPPORT ULONG", request, 4, null, lines,
            delegate(byte[] b) { lines.Add(label + " BASICSUPPORT ULONG: AccessFlags=" + Hex(U32(b, 0))); });
        FixedQuery(handle, label + " BASICSUPPORT KSPROPERTY_DESCRIPTION", request, DescriptionSize, null, lines,
            delegate(byte[] b)
            {
                // A legal short response can still contain just the access mask.
                uint access = U32(b, 0);
                if (b.Length == 4) { lines.Add(label + " BASICSUPPORT KSPROPERTY_DESCRIPTION: AccessFlags=" + Hex(access) + " (ULONG only)"); return; }
                Require(b, 0, DescriptionSize);
                lines.Add(label + " BASICSUPPORT KSPROPERTY_DESCRIPTION: AccessFlags=" + Hex(access) + " DescriptionSize=" + U32(b, 4) +
                    " PropTypeSet=" + G(b, 8) + " TypeId=" + U32(b, 24) + " TypeFlags=" + Hex(U32(b, 28)) +
                    " MembersListCount=" + U32(b, 32) + " Reserved=" + U32(b, 36));
            });
    }

    static void QueryGuidList(IntPtr handle, string label, byte[] request, List<string> lines)
    {
        Query(handle, label, request, 0, lines, delegate(byte[] b)
        {
            uint count = ListCount(b, 16);
            lines.Add(label + ": Count=" + count);
            for (int i = 0; i < count; i++) lines.Add(label + " [" + i + "]: " + G(b, MultipleSize + i * 16));
        });
    }

    static uint ListCount(byte[] b, int itemSize)
    {
        int size = CheckedSize(U32(b, 0));
        uint count = U32(b, 4);
        if (size < MultipleSize || size > b.Length || count > (size - MultipleSize) / itemSize)
            throw new InvalidOperationException("malformed KSMULTIPLE_ITEM: Size=" + size + " Count=" + count + " bytes=" + b.Length);
        return count;
    }

    static void DecodeRanges(byte[] b, string label, List<string> lines)
    {
        int end = CheckedSize(U32(b, 0));
        uint count = U32(b, 4);
        if (end < MultipleSize || end > b.Length || count > (end - MultipleSize) / MultipleSize)
            throw new InvalidOperationException("malformed range list");
        lines.Add(label + ": Size=" + end + " Count=" + count);
        int offset = MultipleSize;
        bool attributes = false;
        for (uint index = 0; index < count; index++)
        {
            if (attributes)
            {
                int size = DecodeAttributes(b, offset, end, label + " [" + index + "] attributes", lines);
                offset = Align8(offset + size);
                attributes = false;
                continue;
            }
            RequireWithin(b, offset, RangeSize, end);
            int length = CheckedSize(U32(b, offset));
            if (length < RangeSize) throw new InvalidOperationException("range FormatSize < 64");
            RequireWithin(b, offset, length, end);
            uint flags = U32(b, offset + 4);
            string text = FormatHeader(b, offset);
            if (length >= 84 && ReadGuid(b, offset + 16) == AudioType)
                text += " MaximumChannels=" + U32(b, offset + 64) + " Bits=" + U32(b, offset + 68) + ".." + U32(b, offset + 72) +
                    " Rate=" + U32(b, offset + 76) + ".." + U32(b, offset + 80);
            lines.Add(label + " [" + index + "]: " + text);
            attributes = (flags & 2) != 0;
            offset = Align8(offset + length);
        }
        if (attributes) throw new InvalidOperationException("range flagged ATTRIBUTES but list has no following attribute item");
        if (offset < end) lines.Add(label + ": trailing bytes=" + (end - offset));
    }

    static int DecodeAttributes(byte[] b, int offset, int end, string label, List<string> lines)
    {
        RequireWithin(b, offset, MultipleSize, end);
        int size = CheckedSize(U32(b, offset));
        uint count = U32(b, offset + 4);
        if (size < MultipleSize) throw new InvalidOperationException("attribute list Size < 8");
        RequireWithin(b, offset, size, end);
        int limit = offset + size;
        int cursor = offset + MultipleSize;
        lines.Add(label + ": Size=" + size + " Count=" + count);
        for (uint i = 0; i < count; i++)
        {
            RequireWithin(b, cursor, AttributeSize, limit);
            int length = CheckedSize(U32(b, cursor));
            if (length < AttributeSize) throw new InvalidOperationException("KSATTRIBUTE.Size < 24");
            RequireWithin(b, cursor, length, limit);
            Guid type = ReadGuid(b, cursor + 8);
            string text = "Size=" + length + " Flags=" + Hex(U32(b, cursor + 4)) + " Attribute=" + GuidName(type);
            if (type == ModeAttribute && length >= 40) text += " Mode=" + G(b, cursor + 24);
            lines.Add(label + " [" + i + "]: " + text);
            cursor = Align8(cursor + length);
        }
        return size;
    }

    static string FormatHeader(byte[] b, int offset)
    {
        Require(b, offset, RangeSize);
        return "FormatSize=" + U32(b, offset) + " Flags=" + Hex(U32(b, offset + 4)) + " SampleSize=" + U32(b, offset + 8) +
            " Major=" + G(b, offset + 16) + " Sub=" + G(b, offset + 32) + " Specifier=" + G(b, offset + 48);
    }

    static void DecodeFormat(byte[] b, string label, List<string> lines)
    {
        int size = CheckedSize(U32(b, 0));
        if (size < RangeSize) throw new InvalidOperationException("format size < 64");
        Require(b, 0, size);
        string text = FormatHeader(b, 0);
        if (ReadGuid(b, 48) == WaveSpecifier)
        {
            RequireWithin(b, 0, FormatWaveSize, size);
            ushort tag = U16(b, 64), extra = U16(b, 80);
            RequireWithin(b, 0, FormatWaveSize + extra, size);
            text += " wFormatTag=0x" + tag.ToString("X4") + " Channels=" + U16(b, 66) + " SamplesPerSec=" + U32(b, 68) +
                " AvgBytesPerSec=" + U32(b, 72) + " BlockAlign=" + U16(b, 76) + " BitsPerSample=" + U16(b, 78) + " cbSize=" + extra;
            if (tag == 0xfffe)
            {
                if (extra < 22) throw new InvalidOperationException("EXTENSIBLE cbSize < 22");
                RequireWithin(b, 0, FormatExtensibleSize, size);
                text += " ValidBits=" + U16(b, 82) + " ChannelMask=" + Hex(U32(b, 84)) + " SubFormat=" + G(b, 88);
            }
        }
        lines.Add(label + ": " + text);
        if ((U32(b, 4) & 2) != 0) DecodeAttributes(b, Align8(size), b.Length, label + " attributes", lines);
    }

    static void DecodeJack(byte[] b, uint id, string label, List<string> lines)
    {
        if (id == 3)
        {
            Require(b, 0, SinkSize);
            lines.Add(label + ": ConnType=" + U32(b, 0) + " ManufacturerId=" + U16(b, 4) + " ProductId=" + U16(b, 6) +
                " AudioLatency=" + U16(b, 8) + " HDCPCapable=" + U32(b, 12) + " AICapable=" + U32(b, 16) +
                " SinkDescriptionLength=" + b[20] + " SinkDescription=" + Unicode(b, 22, 64) +
                " PortId=LUID(" + Hex(U32(b, 92)) + ":" + Hex(U32(b, 88)) + ")");
            return;
        }
        int stride = id == 1 ? JackSize : Jack2Size;
        uint count = ListCount(b, stride);
        lines.Add(label + ": Count=" + count);
        for (int i = 0; i < count; i++)
        {
            int p = MultipleSize + i * stride;
            string text = id == 1
                ? "ChannelMapping=" + Hex(U32(b, p)) + " Color=" + Hex(U32(b, p + 4)) + " ConnectionType=" + U32(b, p + 8) +
                    " GeoLocation=" + U32(b, p + 12) + " GenLocation=" + U32(b, p + 16) + " PortConnection=" + U32(b, p + 20) + " IsConnected=" + U32(b, p + 24)
                : "DeviceStateInfo=" + Hex(U32(b, p)) + " JackCapabilities=" + Hex(U32(b, p + 4));
            lines.Add(label + " [" + i + "]: " + text);
        }
    }

    static void CreateHostPin(IntPtr handle, string prefix, uint pin, List<string> lines)
    {
        // PortWaveRT uses KSINTERFACE_STANDARD_LOOPED_STREAMING (id 1). The variants
        // tell a rejected request (interface, priority, format shape) from a
        // rejection inside the miniport's NewStream: only the last differs per row.
        CreateHostPin(handle, prefix, pin, 1, 0, true, "looped, subclass 0, EXTENSIBLE", lines);
        CreateHostPin(handle, prefix, pin, 1, 1, true, "looped, subclass 1, EXTENSIBLE", lines);
        CreateHostPin(handle, prefix, pin, 0, 1, true, "streaming, subclass 1, EXTENSIBLE", lines);
        CreateHostPin(handle, prefix, pin, 1, 1, false, "looped, subclass 1, WAVEFORMATEX", lines);
    }

    static void CreateHostPin(IntPtr handle, string prefix, uint pin, uint interfaceId, uint subclass,
        bool extensible, string variant, List<string> lines)
    {
        byte[] connect = Connection(pin, interfaceId, subclass, extensible);
        IntPtr memory = Marshal.AllocHGlobal(connect.Length);
        IntPtr created = IntPtr.Zero;
        try
        {
            Marshal.Copy(connect, 0, memory, connect.Length);
            uint error = KsCreatePin(handle, memory, 0x40000000, out created);
            lines.Add(prefix + " KsCreatePin PCM-48000-2ch-16bit (" + variant + "): " +
                (error == 0 ? "OK" : ErrorText(unchecked((int)error))));
        }
        catch (Exception e) { lines.Add(prefix + " KsCreatePin (" + variant + "): " + ExceptionText(e)); }
        finally
        {
            if (created != IntPtr.Zero && created != InvalidHandle)
            {
                bool closed = CloseHandle(created);
                int error = closed ? 0 : Marshal.GetLastWin32Error();
                lines.Add(prefix + " CloseHandle(pin): " + (closed ? "OK (no state transition or stream I/O)" : ErrorText(error)));
            }
            Marshal.FreeHGlobal(memory);
        }
    }

    static byte[] Property(Guid set, uint id, uint flags)
    {
        var b = new byte[PropertySize];
        PutGuid(b, 0, set); Put32(b, 16, id); Put32(b, 20, flags);
        return b;
    }

    static byte[] PinProperty(Guid set, uint id, uint flags, uint pin)
    {
        var b = new byte[PinSize];
        Buffer.BlockCopy(Property(set, id, flags), 0, b, 0, PropertySize);
        Put32(b, 24, pin);
        return b;
    }

    static byte[] Range(int kind)
    {
        var b = new byte[kind == 2 ? RangeSize : AudioRangeSize];
        Put32(b, 0, (uint)b.Length);
        if (kind != 2)
        {
            PutGuid(b, 16, AudioType); PutGuid(b, 32, kind == 0 ? PcmSubtype : Guid.Empty); PutGuid(b, 48, WaveSpecifier);
            Put32(b, 64, kind == 0 ? 2u : 8u);
            Put32(b, 68, kind == 0 ? 16u : 8u); Put32(b, 72, kind == 0 ? 16u : 32u);
            Put32(b, 76, kind == 0 ? 48000u : 1u); Put32(b, 80, kind == 0 ? 48000u : 384000u);
        }
        return b;
    }

    static byte[] Intersection(uint pin, int kind)
    {
        byte[] range = Range(kind);
        var b = new byte[PinSize + MultipleSize + range.Length];
        Buffer.BlockCopy(PinProperty(PinSet, 4, Get, pin), 0, b, 0, PinSize);
        Put32(b, PinSize, (uint)(MultipleSize + range.Length)); Put32(b, PinSize + 4, 1);
        Buffer.BlockCopy(range, 0, b, PinSize + MultipleSize, range.Length);
        return b;
    }

    // https://learn.microsoft.com/en-us/windows-hardware/drivers/stream/ksproperty-pin-proposedataformat2
    // KSP_PIN(32) + KSMULTIPLE_ITEM(8) + KSATTRIBUTE_AUDIOSIGNALPROCESSING_MODE(40).
    static byte[] Propose2(uint pin)
    {
        var b = new byte[80];
        Buffer.BlockCopy(PinProperty(PinSet, 15, Get, pin), 0, b, 0, PinSize);
        Put32(b, 32, 48); Put32(b, 36, 1); Put32(b, 40, 40);
        PutGuid(b, 48, ModeAttribute); PutGuid(b, 64, DefaultMode);
        return b;
    }

    static byte[] PcmFormat()
    {
        var b = new byte[FormatExtensibleSize];
        Put32(b, 0, FormatExtensibleSize); Put32(b, 8, 4);
        PutGuid(b, 16, AudioType); PutGuid(b, 32, PcmSubtype); PutGuid(b, 48, WaveSpecifier);
        Put16(b, 64, 0xfffe); Put16(b, 66, 2); Put32(b, 68, 48000); Put32(b, 72, 192000);
        Put16(b, 76, 4); Put16(b, 78, 16); Put16(b, 80, 22); Put16(b, 82, 16);
        Put32(b, 84, 3); PutGuid(b, 88, PcmSubtype);
        return b;
    }

    // KSDATAFORMAT_WAVEFORMATEX: WAVE_FORMAT_PCM, cbSize 0, 82 bytes.
    static byte[] PcmFormatEx()
    {
        var b = new byte[FormatWaveSize];
        Put32(b, 0, FormatWaveSize); Put32(b, 8, 4);
        PutGuid(b, 16, AudioType); PutGuid(b, 32, PcmSubtype); PutGuid(b, 48, WaveSpecifier);
        Put16(b, 64, 1); Put16(b, 66, 2); Put32(b, 68, 48000); Put32(b, 72, 192000);
        Put16(b, 76, 4); Put16(b, 78, 16); Put16(b, 80, 0);
        return b;
    }

    static byte[] Connection(uint pin) { return Connection(pin, 1, 0, true); }

    static byte[] Connection(uint pin, uint interfaceId, uint subclass, bool extensible)
    {
        byte[] format = extensible ? PcmFormat() : PcmFormatEx();
        var b = new byte[ConnectSize + format.Length];
        PutGuid(b, 0, StandardInterface); Put32(b, 16, interfaceId);
        PutGuid(b, 24, StandardMedium); Put32(b, 40, 0); Put32(b, 48, pin);
        Put32(b, 64, 0x40000000); Put32(b, 68, subclass); // KSPRIORITY_NORMAL; PinToHandle is NULL.
        Buffer.BlockCopy(format, 0, b, ConnectSize, format.Length);
        return b;
    }

    static void CheckLayout()
    {
        // Fail before any native call: do not silently use x64 offsets in a 32-bit Add-Type host.
        if (IntPtr.Size != 8 || !BitConverter.IsLittleEndian) throw new InvalidOperationException("layout self-check failed: x64 little-endian process required");
        byte[] pin = PinProperty(PinSet, 4, Get, 7), range = Range(0), format = PcmFormat(), connect = Connection(7), proposal = Propose2(7);
        if (Property(GeneralSet, 0, Get).Length != 24 || pin.Length != 32 || U32(pin, 24) != 7 || U32(pin, 28) != 0 ||
            Range(2).Length != 64 || range.Length != 88 || U32(range, 80) != 48000 || U32(range, 84) != 0 ||
            FormatWaveSize != RangeSize + WaveFormatExSize || format.Length != RangeSize + WaveExtensibleSize ||
            U32(format, 84) != 3 || ReadGuid(format, 88) != PcmSubtype || connect.Length != 176 || U32(connect, 72) != 104 ||
            U32(connect, 48) != 7 || U32(connect, 56) != 0 || U32(connect, 60) != 0 ||
            proposal.Length != 80 || U32(proposal, 32) != 48 || ReadGuid(proposal, 64) != DefaultMode ||
            Intersection(7, 0).Length != 128 || Marshal.SizeOf(typeof(SP_DEVICE_INTERFACE_DATA)) != 32)
            throw new InvalidOperationException("layout self-check failed: KS serializer size/offset mismatch");
    }

    static int Align8(int size) { return checked((size + 7) & ~7); }
    static int CheckedSize(uint size)
    {
        if (size > MaxBuffer) throw new InvalidOperationException("invalid structure size " + size);
        return (int)size;
    }
    static void Require(byte[] b, int offset, int size) { RequireWithin(b, offset, size, b.Length); }
    static void RequireWithin(byte[] b, int offset, int size, int end)
    {
        if (offset < 0 || size < 0 || end > b.Length || offset > end || size > end - offset)
            throw new InvalidOperationException("truncated/malformed response: offset=" + offset + " size=" + size + " end=" + end);
    }
    static uint U32(byte[] b, int offset) { Require(b, offset, 4); return BitConverter.ToUInt32(b, offset); }
    static ushort U16(byte[] b, int offset) { Require(b, offset, 2); return BitConverter.ToUInt16(b, offset); }
    static void Put32(byte[] b, int offset, uint value) { Buffer.BlockCopy(BitConverter.GetBytes(value), 0, b, offset, 4); }
    static void Put16(byte[] b, int offset, ushort value) { Buffer.BlockCopy(BitConverter.GetBytes(value), 0, b, offset, 2); }
    static void PutGuid(byte[] b, int offset, Guid value) { Buffer.BlockCopy(value.ToByteArray(), 0, b, offset, 16); }
    static Guid ReadGuid(byte[] b, int offset)
    {
        Require(b, offset, 16);
        var guid = new byte[16]; Buffer.BlockCopy(b, offset, guid, 0, 16); return new Guid(guid);
    }
    static string G(byte[] b, int offset) { return GuidName(ReadGuid(b, offset)); }
    static string Unicode(byte[] b, int offset, int bytes)
    {
        Require(b, offset, bytes);
        if ((bytes & 1) != 0) throw new InvalidOperationException("odd UTF-16 byte count");
        string text = Encoding.Unicode.GetString(b, offset, bytes);
        int nul = text.IndexOf('\0');
        return OneLine(nul < 0 ? text : text.Substring(0, nul));
    }
    static string Hex(uint value) { return "0x" + value.ToString("X8"); }
    static string Node(uint node) { return node == UInt32.MaxValue ? "filter" : node.ToString(); }
    static string FilterName(string path) { return OneLine(path.Substring(path.LastIndexOf('\\') + 1)); }
    static string OneLine(string text) { return text.Replace("\r", "\\r").Replace("\n", "\\n").Replace("\t", "\\t"); }
    static string ExceptionText(Exception e)
    {
        Win32Exception win32 = e as Win32Exception;
        return win32 != null ? ErrorText(win32.NativeErrorCode) + " " + OneLine(e.Message) : e.GetType().Name + ": " + OneLine(e.Message);
    }
    static string ErrorText(int error)
    {
        string name;
        switch (unchecked((uint)error))
        {
            case 0: name = "ERROR_SUCCESS"; break;
            case 1: name = "ERROR_INVALID_FUNCTION"; break;
            case 2: name = "ERROR_FILE_NOT_FOUND"; break;
            case 3: name = "ERROR_PATH_NOT_FOUND"; break;
            case 5: name = "ERROR_ACCESS_DENIED"; break;
            case 6: name = "ERROR_INVALID_HANDLE"; break;
            case 8: name = "ERROR_NOT_ENOUGH_MEMORY"; break;
            case 13: name = "ERROR_INVALID_DATA"; break;
            case 21: name = "ERROR_NOT_READY"; break;
            case 31: name = "ERROR_GEN_FAILURE"; break;
            case 32: name = "ERROR_SHARING_VIOLATION"; break;
            case 50: name = "ERROR_NOT_SUPPORTED"; break;
            case 87: name = "ERROR_INVALID_PARAMETER"; break;
            case 111: name = "ERROR_BUFFER_OVERFLOW"; break;
            case 116: name = "ERROR_INVALID_VERIFY_SWITCH"; break;
            case 120: name = "ERROR_CALL_NOT_IMPLEMENTED"; break;
            case 122: name = "ERROR_INSUFFICIENT_BUFFER"; break;
            case 123: name = "ERROR_INVALID_NAME"; break;
            case 170: name = "ERROR_BUSY"; break;
            case 234: name = "ERROR_MORE_DATA"; break;
            case 259: name = "ERROR_NO_MORE_ITEMS"; break;
            case 995: name = "ERROR_OPERATION_ABORTED"; break;
            case 996: name = "ERROR_IO_INCOMPLETE"; break;
            case 997: name = "ERROR_IO_PENDING"; break;
            case 1167: name = "ERROR_DEVICE_NOT_CONNECTED"; break;
            case 1168: name = "ERROR_NOT_FOUND"; break;
            case 1169: name = "ERROR_NO_MATCH"; break;
            case 1170: name = "ERROR_SET_NOT_FOUND"; break;
            case 1450: name = "ERROR_NO_SYSTEM_RESOURCES"; break;
            case 1460: name = "ERROR_TIMEOUT"; break;
            case 1784: name = "ERROR_INVALID_USER_BUFFER"; break;
            case 0x80000005: name = "STATUS_BUFFER_OVERFLOW"; break;
            default: name = "WIN32_ERROR"; break;
        }
        return "Win32=" + unchecked((uint)error) + " " + name + " (" + OneLine(new Win32Exception(error).Message) + ")";
    }

    static string GuidName(Guid guid)
    {
        if (guid == Guid.Empty) return "GUID_NULL/WILDCARD";
        if (guid == AudioCategory) return "KSCATEGORY_AUDIO";
        if (guid == RenderCategory) return "KSCATEGORY_RENDER";
        if (guid == RealtimeCategory) return "KSCATEGORY_REALTIME";
        if (guid == TopologyCategory) return "KSCATEGORY_TOPOLOGY";
        if (guid == AudioType) return "KSDATAFORMAT_TYPE_AUDIO";
        if (guid == PcmSubtype) return "KSDATAFORMAT_SUBTYPE_PCM";
        if (guid == WaveSpecifier) return "KSDATAFORMAT_SPECIFIER_WAVEFORMATEX";
        if (guid == DefaultMode) return "AUDIO_SIGNALPROCESSINGMODE_DEFAULT";
        if (guid == ModeAttribute) return "KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE";
        if (guid == StandardInterface) return "KSINTERFACESETID_Standard";
        if (guid == StandardMedium) return "KSMEDIUMSETID_Standard";
        string key = guid.ToString("D");
        switch (key)
        {
            case "d1b9cc2a-f519-417f-91c9-55fa65481001": return "KSNODETYPE_HDMI_INTERFACE";
            case "e47e4031-3ea6-418d-8f9b-b73843ccba97": return "KSNODETYPE_DISPLAYPORT_INTERFACE";
            case "9e90ea20-b493-4fd1-a1a8-7e1361a956cf": return "AUDIO_SIGNALPROCESSINGMODE_RAW";
            case "00000003-0000-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEEE_FLOAT";
            case "00000092-0000-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_DOLBY_AC3_SPDIF";
            case "00000008-0000-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_DTS";
            case "0000000a-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL_PLUS";
            case "0000000b-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DTS_HD";
            case "0000010b-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DTSX_E1";
            case "0000030b-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DTSX_E2";
            case "0000000c-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MLP";
            case "0000010a-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL_PLUS_ATMOS";
            case "0000010c-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MAT20";
            case "0000030c-0cea-0010-8000-00aa00389b71": return "KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MAT21";
            case "e436eb8e-524f-11ce-9f53-0020af0ba770": return "KSDATAFORMAT_SUBTYPE_NONE";
            case "0f6417d6-c318-11d0-a43f-00a0c9223196": return "KSDATAFORMAT_SPECIFIER_NONE";
            default: return guid.ToString("B");
        }
    }
}
