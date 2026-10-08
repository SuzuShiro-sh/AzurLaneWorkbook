# 为全新克隆准备固定 Rust、子模块和项目内可复用的外部构建依赖。

[CmdletBinding()]
param(
    [string] $RepositoryRoot = (Split-Path -Parent $PSScriptRoot),

    [string] $LockPath,

    [string] $DependencyRoot,

    [string] $CacheBundle,

    [string] $ExportCacheBundle,

    [switch] $Offline,

    [switch] $Repair,

    [switch] $SkipHostChecks,

    [switch] $SkipRustSetup,

    [switch] $SkipSubmodules
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

. (Join-Path $PSScriptRoot 'dependencies/Install.ps1')
. (Join-Path $PSScriptRoot 'dependencies/CacheBundle.ps1')
. (Join-Path $PSScriptRoot 'build/Environment.ps1')

[string] $resolvedRepository = Get-AzlwFullPath -Path $RepositoryRoot
if (-not (Test-Path -LiteralPath (Join-Path $resolvedRepository 'Cargo.toml') -PathType Leaf)) {
    throw "仓库根目录缺少 Cargo.toml: $resolvedRepository"
}
if ([string]::IsNullOrWhiteSpace($LockPath)) {
    $LockPath = Join-Path $resolvedRepository 'build/dependencies.lock.json'
}
if ([string]::IsNullOrWhiteSpace($DependencyRoot)) {
    $DependencyRoot = Join-Path $resolvedRepository '.dependencies'
}

if (-not $SkipHostChecks) {
    foreach ($name in @('git', 'rustup')) {
        if ($null -eq (Get-Command $name -ErrorAction SilentlyContinue | Select-Object -First 1)) {
            throw "开发环境缺少 $name，请先安装并确保当前 PowerShell 可发现该命令。"
        }
    }
    [void](Get-AzlwVisualStudioInstallation)
}

if (-not $SkipSubmodules) {
    if (-not $Offline) {
        Invoke-AzlwExternalCommand `
            -FilePath 'git' `
            -ArgumentList @('submodule', 'update', '--init', '--recursive') `
            -WorkingDirectory $resolvedRepository
    }
    Push-Location -LiteralPath $resolvedRepository
    try {
        [string[]] $submoduleStatus = @(& git 'submodule' 'status' '--recursive')
        [int] $submoduleExitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($submoduleExitCode -ne 0 -or @($submoduleStatus | Where-Object { $_ -match '^[-+U]' }).Count -ne 0) {
        throw "子模块未初始化、提交不匹配或存在冲突:`n$($submoduleStatus -join "`n")"
    }
}

if (-not $SkipRustSetup) {
    [string] $toolchainPath = Join-Path $resolvedRepository 'rust-toolchain.toml'
    [string] $toolchainText = Get-Content -LiteralPath $toolchainPath -Raw -Encoding utf8
    [System.Text.RegularExpressions.Match] $channelMatch = [regex]::Match(
        $toolchainText,
        '(?m)^channel\s*=\s*"([^"]+)"\s*$'
    )
    if (-not $channelMatch.Success) {
        throw "rust-toolchain.toml 缺少唯一 channel: $toolchainPath"
    }
    [string] $channel = $channelMatch.Groups[1].Value
    if ($Offline) {
        [string[]] $installedToolchains = @(& rustup 'toolchain' 'list')
        [int] $toolchainExitCode = $LASTEXITCODE
        if ($toolchainExitCode -ne 0 -or @($installedToolchains | Where-Object {
            $_ -match ('^' + [regex]::Escape($channel) + '(-|\s|$)')
        }).Count -eq 0) {
            throw "离线模式缺少 Rust 工具链 $channel"
        }
        [string[]] $installedTargets = @(& rustup 'target' 'list' '--installed' '--toolchain' $channel)
        [int] $targetExitCode = $LASTEXITCODE
        if ($targetExitCode -ne 0 -or $installedTargets -notcontains 'x86_64-pc-windows-msvc') {
            throw "离线模式的 Rust 工具链 $channel 缺少 x86_64-pc-windows-msvc target"
        }
    } else {
        Invoke-AzlwExternalCommand `
            -FilePath 'rustup' `
            -ArgumentList @('toolchain', 'install', $channel, '--profile', 'minimal') `
            -WorkingDirectory $resolvedRepository
        Invoke-AzlwExternalCommand `
            -FilePath 'rustup' `
            -ArgumentList @('target', 'add', 'x86_64-pc-windows-msvc', '--toolchain', $channel) `
            -WorkingDirectory $resolvedRepository
    }
}

if (-not [string]::IsNullOrWhiteSpace($CacheBundle)) {
    Import-AzlwDependencyCacheBundle `
        -LockPath $LockPath `
        -DependencyRoot $DependencyRoot `
        -BundlePath $CacheBundle
}

[object[]] $resolved = @(Invoke-AzlwDependencyBootstrap `
    -LockPath $LockPath `
    -DependencyRoot $DependencyRoot `
    -Offline:$Offline `
    -Repair:$Repair)

if (-not [string]::IsNullOrWhiteSpace($ExportCacheBundle)) {
    Export-AzlwDependencyCacheBundle `
        -LockPath $LockPath `
        -DependencyRoot $DependencyRoot `
        -BundlePath $ExportCacheBundle
}

$report = [ordered]@{
    repository_root = $resolvedRepository
    dependency_root = Get-AzlwFullPath -Path $DependencyRoot
    offline         = [bool]$Offline
    dependencies    = @($resolved)
}
$report | ConvertTo-Json -Depth 8
