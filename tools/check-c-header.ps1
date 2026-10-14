# Compiles the generated driver contract header as C89, C11 and C++17 with
# MSVC (/W4 /WX) and runs the result, so the SSR_STATIC_ASSERT lines that
# tests/c_header.rs derives from the Rust layout are checked by a C compiler.
# Used by ci.yml on windows-latest and runnable locally.
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$include = Join-Path $repo 'crates\ssr-abi\include'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'MSVC (VC.Tools.x86.x64) not found' }
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'

$work = Join-Path ([IO.Path]::GetTempPath()) "ssr-abi-header-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
$probe = @'
#include "ssr_abi.h"
int main(void) {
    SSR_RECORD_HEADER h = {0};
    return (int)sizeof(h) - 40 + (IOCTL_SSR_READ == 0x226406 ? 0 : 1);
}
'@
Set-Content -Path (Join-Path $work 'probe.c') -Value $probe -Encoding ascii
Set-Content -Path (Join-Path $work 'probe.cpp') -Value $probe -Encoding ascii

$builds = @(
    @{ Name = 'c89'; Args = "/nologo /W4 /WX /I `"$include`" probe.c /Fe:c89.exe" },
    @{ Name = 'c11'; Args = "/nologo /W4 /WX /std:c11 /I `"$include`" probe.c /Fe:c11.exe" },
    @{ Name = 'cpp17'; Args = "/nologo /W4 /WX /std:c++17 /EHsc /I `"$include`" probe.cpp /Fe:cpp17.exe" },
    @{ Name = 'cpp23'; Args = "/nologo /W4 /WX /std:c++23preview /EHsc /I `"$include`" probe.cpp /Fe:cpp23.exe" }
)
try {
    foreach ($b in $builds) {
        cmd /c "`"$vcvars`" >nul && cd /d `"$work`" && cl $($b.Args)"
        if ($LASTEXITCODE -ne 0) { throw "$($b.Name): cl failed ($LASTEXITCODE)" }
        & (Join-Path $work "$($b.Name).exe")
        if ($LASTEXITCODE -ne 0) { throw "$($b.Name): probe returned $LASTEXITCODE" }
        "$($b.Name): ok"
    }
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
