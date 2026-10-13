// Creates a root-enumerated device node and installs its driver, the way
// devcon does (SetupDiCreateDeviceInfo, DIF_REGISTERDEVICE,
// UpdateDriverForPlugAndPlayDevices). CI only: tools/ci/install-*.ps1 load it
// with Add-Type.
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
