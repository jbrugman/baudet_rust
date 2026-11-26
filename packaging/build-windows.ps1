param(
    [string]$OutputDirectory = "target/dist",
    [ValidateSet("x64", "arm64")][string]$Architecture = "x64"
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$target, $vsArch, $vsComponent, $machine = if ($Architecture -eq "arm64") {
    "aarch64-pc-windows-msvc", "arm64", "Microsoft.VisualStudio.Component.VC.Tools.ARM64", "AA64"
} else {
    "x86_64-pc-windows-msvc", "amd64", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "8664"
}
if ($env:VSCMD_ARG_TGT_ARCH -ne $vsArch -or
    -not (Get-Command dumpbin.exe -ErrorAction SilentlyContinue) -or
    -not (Get-Command rc.exe -ErrorAction SilentlyContinue)) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    $visualStudio = & $vswhere -latest -products '*' -requires $vsComponent -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $visualStudio) {
        throw "Install Visual Studio Build Tools with the Desktop development with C++ workload and $Architecture tools."
    }
    $hostArch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") { "arm64" } else { "amd64" }
    & (Join-Path $visualStudio "Common7/Tools/Launch-VsDevShell.ps1") -Arch $vsArch -HostArch $hostArch -SkipAutomaticLocation
}

$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$flagVariable = "CARGO_TARGET_$($target.ToUpperInvariant().Replace('-', '_'))_RUSTFLAGS"
[Environment]::SetEnvironmentVariable($flagVariable, "-C target-feature=+crt-static")
rustup target add $target
if ($LASTEXITCODE -ne 0) { throw "Could not install the Windows Rust target." }
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$work = Join-Path ([IO.Path]::GetTempPath()) ("baudet-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    Copy-Item packaging/baudet.ico $work
    Set-Content (Join-Path $work "baudet.rc") '1 ICON "baudet.ico"' -Encoding ascii
    Push-Location $work
    try {
        & rc.exe /nologo /fo baudet.res baudet.rc
        if ($LASTEXITCODE -ne 0) { throw "Could not compile the Windows app icon." }
    } finally {
        Pop-Location
    }
    $resource = Join-Path $work "baudet.res"
    cargo rustc --locked --release -p baudet-ui --bin baudet-ui --target-dir $targetDirectory --target $target -- -C "link-arg=$resource"
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed." }
    Remove-Item $resource, (Join-Path $work "baudet.rc"), (Join-Path $work "baudet.ico")

    $exe = Join-Path $work "Baudet.exe"
    Copy-Item (Join-Path $targetDirectory "$target/release/baudet-ui.exe") $exe
    Copy-Item LICENSE (Join-Path $work "LICENSE.txt")
    Copy-Item packaging/FONT-LICENSES.txt $work

    $headers = & dumpbin.exe /HEADERS $exe
    if ($LASTEXITCODE -ne 0 -or -not ($headers -match "^\s*$machine\s+machine\b")) {
        throw "Baudet.exe does not have the expected $Architecture PE machine type."
    }

    $imports = & dumpbin.exe /DEPENDENTS $exe
    if ($LASTEXITCODE -ne 0) { throw "Could not inspect the executable's DLL imports." }
    $dlls = @($imports | ForEach-Object {
        if ($_ -match '^\s+([\w.-]+\.dll)\s*$') { $Matches[1] }
    } | Sort-Object -Unique)
    if ($dlls.Count -eq 0) { throw "No Windows DLL imports were found." }
    foreach ($dll in $dlls) {
        if ($dll -match '^(vcruntime|msvcp|concrt)\d') {
            throw "Unexpected Visual C++ runtime dependency: $dll"
        }
        if ($dll -notmatch '^(api-ms-win-|ext-ms-win-)' -and
            -not (Test-Path (Join-Path $env:SystemRoot "System32/$dll"))) {
            throw "Unbundled DLL: $dll"
        }
    }
    $dlls | Set-Content (Join-Path $work "Windows-DLLs.txt")

    $helpOutput = Join-Path $work "help.txt"
    $process = Start-Process -FilePath $exe -ArgumentList "--help" -RedirectStandardError $helpOutput -Wait -PassThru
    if ($process.ExitCode -ne 2 -or -not (Select-String -Path $helpOutput -SimpleMatch "usage: baudet-ui")) {
        throw "Baudet.exe did not print its command-line help."
    }
    Remove-Item $helpOutput

    $smoke = Join-Path $work "window-test"
    New-Item -ItemType Directory -Path $smoke | Out-Null
    $screenshot = Join-Path $smoke "window.png"
    $errorOutput = Join-Path $smoke "stderr.txt"
    $arguments = "--demo --home `"$smoke`" --screenshot `"$screenshot`""
    $process = Start-Process -FilePath $exe -ArgumentList $arguments -RedirectStandardError $errorOutput -PassThru
    if (-not $process.WaitForExit(60000)) {
        $process.Kill()
        $process.WaitForExit()
        throw "Baudet.exe did not finish its window smoke test: $(Get-Content -Raw $errorOutput)"
    }
    if ($process.ExitCode -ne 0 -or -not (Test-Path $screenshot) -or (Get-Item $screenshot).Length -eq 0) {
        throw "Baudet.exe could not render its demo window (exit $($process.ExitCode)): $(Get-Content -Raw $errorOutput)"
    }
    Remove-Item -Recurse -Force $smoke

    $zip = Join-Path $OutputDirectory "Baudet-windows-$Architecture.zip"
    Compress-Archive -Path "$work/*" -DestinationPath $zip -Force
    Write-Output $zip
} finally {
    Remove-Item -Recurse -Force $work
}
