param([ValidateSet('test','build','release','check','fetch','package')][string]$Action = 'test', [string[]]$CargoArgs = @())
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()
$env:CARGO_HOME = Join-Path $PSScriptRoot '.tools\cargo'
$env:RUSTUP_HOME = Join-Path $PSScriptRoot '.tools\rustup'
$env:TEMP = Join-Path $PSScriptRoot '.tools\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot 'target'
$compilerBin = Join-Path $PSScriptRoot '.tools\llvm-mingw-20260908-msvcrt-x86_64\bin'
$env:PATH = "$PSScriptRoot\.tools\cargo\bin;$compilerBin;$env:PATH"
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = Join-Path $compilerBin 'x86_64-w64-mingw32-clang.exe'
$env:CC = $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER
$env:AR = Join-Path $compilerBin 'llvm-ar.exe'
[System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = 'BelowNormal'
Push-Location $PSScriptRoot
try {
    if ($Action -eq 'release') { cargo build --release }
    elseif ($Action -eq 'package') { cargo build }
    else { cargo $Action @CargoArgs }
    if ($LASTEXITCODE -ne 0) { throw "cargo $Action failed ($LASTEXITCODE)" }
    if ($Action -eq 'package') {
        $package = Join-Path $PSScriptRoot 'dist\VolumeTrail'
        New-Item -ItemType Directory -Path $package -Force | Out-Null
        $consoleExe = Join-Path $package 'VolumeTrail-cli.exe'
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\debug\volumetrail.exe') -Destination $consoleExe -Force
        & (Join-Path $compilerBin 'llvm-strip.exe') --strip-debug $consoleExe
        if ($LASTEXITCODE -ne 0) { throw "llvm-strip failed ($LASTEXITCODE)" }
        $exe = Join-Path $package 'VolumeTrail.exe'
        Copy-Item -LiteralPath $consoleExe -Destination $exe -Force
        & (Join-Path $compilerBin 'llvm-objcopy.exe') --subsystem=windows $exe
        if ($LASTEXITCODE -ne 0) { throw "llvm-objcopy failed ($LASTEXITCODE)" }
        Get-Item -LiteralPath $exe | Select-Object FullName, Length
    }
} finally { Pop-Location }
