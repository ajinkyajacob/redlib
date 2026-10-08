# mcp/dev-build.ps1
# Usage: pwsh -File mcp/dev-build.ps1 <cargo args...>
#
# A GNU rustc earlier on PATH makes boring-sys2 fail with
# `could not find native static library 'crypto'` (CMake emits crypto.lib; the GNU
# linker looks for libcrypto.a). So every cargo invocation for this crate must run
# inside the MSVC developer environment with the rustup MSVC toolchain first on PATH.
$ErrorActionPreference = 'Stop'
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) { throw "vcvars64.bat not found at $vcvars" }
$repoRoot = Split-Path -Parent $PSScriptRoot
$argsLine = ($args -join ' ')
$cmd = "call `"$vcvars`" >nul && set `"PATH=$env:USERPROFILE\.cargo\bin;%PATH%`" && cd /d `"$repoRoot`" && cargo $argsLine"
& cmd.exe /c $cmd
exit $LASTEXITCODE