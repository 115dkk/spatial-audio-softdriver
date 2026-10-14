# Package identities only; display names must never select an endpoint.
# Checked against SSR_CONTROL_DEVICE_PATH in the generated ABI header.
$SpatialControlDevicePath = '\\.\SpatialSoftReceiver'
$SpatialPackage = [pscustomobject]@{
    HardwareId = 'ROOT\SpatialSoftReceiver'
    Service = 'SpatialSoftReceiver'
    Inf = 'spatialsoftreceiver.inf'
    CertificateSubject = 'CN=Spatial SoftReceiver test signing*'
}
# All legacy removal identities live here, not in individual callers.
$SpatialLegacyPackages = @(
    [pscustomobject]@{
        HardwareId = 'ROOT\BioAtmos'
        Service = 'bioatmos'
        Inf = 'bioatmos.inf'
        CertificateSubject = 'CN=BIO Atmos test signing*'
    }
)
