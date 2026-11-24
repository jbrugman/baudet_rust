param([string]$OutputDirectory = "target/dist")

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not (Get-Command dumpbin.exe -ErrorAction SilentlyContinue) -or
    -not (Get-Command rc.exe -ErrorAction SilentlyContinue)) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    $visualStudio = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $visualStudio) {
        throw "Install Visual Studio Build Tools with the Desktop development with C++ workload."
    }
    & (Join-Path $visualStudio "Common7/Tools/Launch-VsDevShell.ps1") -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
}

$target = "x86_64-pc-windows-msvc"
$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = "-C target-feature=+crt-static"
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
    $zip = Join-Path $OutputDirectory "Baudet-windows-x64.zip"
    Compress-Archive -Path "$work/*" -DestinationPath $zip -Force
    Write-Output $zip
} finally {
    Remove-Item -Recurse -Force $work
}
