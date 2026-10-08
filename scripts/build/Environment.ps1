# 统一调用外部构建命令，并导入 Visual Studio x64 开发环境。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot '../common/Paths.ps1')

function Invoke-AzlwExternalCommand {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $FilePath,

        [Parameter(Mandatory)]
        [string[]] $ArgumentList,

        [Parameter(Mandatory)]
        [string] $WorkingDirectory
    )

    Push-Location -LiteralPath $WorkingDirectory
    try {
        $global:LASTEXITCODE = 0
        & $FilePath @ArgumentList | Out-Host
        [int] $exitCode = if ([System.IO.Path]::GetExtension($FilePath) -ieq '.ps1') {
            if ($?) { 0 } else { 1 }
        } else {
            $LASTEXITCODE
        }
        if ($exitCode -ne 0) {
            throw "$FilePath 退出码为 $exitCode"
        }
    } finally {
        Pop-Location
    }
}

function Add-AzlwRemapPathPrefix {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [System.Collections.Generic.List[string]] $Flags,

        [Parameter(Mandatory)]
        [string] $From,

        [Parameter(Mandatory)]
        [string] $To
    )

    [string] $normalized = Get-AzlwFullPath -Path $From
    $normalized = $normalized.TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    if ([string]::IsNullOrWhiteSpace($normalized)) {
        return
    }
    $Flags.Add('--remap-path-prefix')
    $Flags.Add("$normalized=$To")
    [string] $forward = $normalized.Replace('\', '/')
    if ($forward -ne $normalized) {
        $Flags.Add('--remap-path-prefix')
        $Flags.Add("$forward=$To")
    }
}

function Get-AzlwReleaseRustFlags {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $RepositoryRoot
    )

    [System.Collections.Generic.List[string]] $flags = [System.Collections.Generic.List[string]]::new()
    $flags.Add('-C')
    $flags.Add('target-feature=+crt-static')
    $flags.Add('-C')
    $flags.Add('link-arg=/PDBALTPATH:%_PDB%')
    Add-AzlwRemapPathPrefix -Flags $flags -From $RepositoryRoot -To '.'
    [string] $cargoHome = $env:CARGO_HOME
    if ([string]::IsNullOrWhiteSpace($cargoHome)) {
        $cargoHome = Join-Path $env:USERPROFILE '.cargo'
    }
    if (Test-Path -LiteralPath $cargoHome) {
        Add-AzlwRemapPathPrefix -Flags $flags -From $cargoHome -To 'cargo-home'
    }
    [string] $rustupHome = $env:RUSTUP_HOME
    if ([string]::IsNullOrWhiteSpace($rustupHome)) {
        $rustupHome = Join-Path $env:USERPROFILE '.rustup'
    }
    if (Test-Path -LiteralPath $rustupHome) {
        Add-AzlwRemapPathPrefix -Flags $flags -From $rustupHome -To 'rustup-home'
    }
    return [string]::Join(' ', $flags)
}

function Invoke-AzlwReleaseCargo {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $CargoPath,

        [Parameter(Mandatory)]
        [string[]] $ArgumentList,

        [Parameter(Mandatory)]
        [string] $WorkingDirectory,

        [Parameter(Mandatory)]
        [string] $RepositoryRoot
    )

    [string] $previous = $env:RUSTFLAGS
    $env:RUSTFLAGS = Get-AzlwReleaseRustFlags -RepositoryRoot $RepositoryRoot
    try {
        Invoke-AzlwExternalCommand `
            -FilePath $CargoPath `
            -ArgumentList $ArgumentList `
            -WorkingDirectory $WorkingDirectory
    } finally {
        if ([string]::IsNullOrEmpty($previous)) {
            Remove-Item -Path Env:RUSTFLAGS -ErrorAction SilentlyContinue
        } else {
            $env:RUSTFLAGS = $previous
        }
    }
}

function Get-AzlwVisualStudioInstallation {
    [CmdletBinding()]
    [OutputType([string])]
    param()

    [string] $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        throw '未找到 Visual Studio Installer 的 vswhere.exe；请安装 Visual Studio、Desktop development with C++ 和 Windows SDK。'
    }
    [string[]] $nativeArgs = @(
        '-latest'
        '-products'
        '*'
        '-requires'
        'Microsoft.VisualStudio.Component.VC.Tools.x86.x64'
        '-property'
        'installationPath'
    )
    $global:LASTEXITCODE = 0
    [string] $installation = [string](& $vswhere @nativeArgs | Select-Object -First 1)
    [int] $exitCode = $LASTEXITCODE
    $installation = $installation.Trim()
    if ($exitCode -ne 0 -or [string]::IsNullOrWhiteSpace($installation)) {
        throw '未找到包含 MSVC x64 C++ 工具集的 Visual Studio/Build Tools 安装。'
    }
    return $installation
}

function Enter-AzlwVisualStudioEnvironment {
    [CmdletBinding()]
    [OutputType([string])]
    param()

    [string] $installation = Get-AzlwVisualStudioInstallation
    [string] $vsDevCmd = Join-Path $installation 'Common7\Tools\VsDevCmd.bat'
    if (-not (Test-Path -LiteralPath $vsDevCmd -PathType Leaf)) {
        throw "Visual Studio 安装缺少 VsDevCmd.bat: $vsDevCmd"
    }
    [string] $command = '"' + $vsDevCmd + '" -no_logo -arch=x64 -host_arch=x64 >nul && set'
    $global:LASTEXITCODE = 0
    [string[]] $environmentLines = @(& $env:COMSPEC '/d' '/s' '/c' $command)
    [int] $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        throw "VsDevCmd.bat 初始化失败，退出码为 $exitCode"
    }
    foreach ($line in $environmentLines) {
        [int] $separator = $line.IndexOf('=')
        if ($separator -le 0) {
            continue
        }
        [string] $name = $line.Substring(0, $separator)
        [string] $value = $line.Substring($separator + 1)
        [System.Environment]::SetEnvironmentVariable($name, $value, 'Process')
    }
    return $installation
}
