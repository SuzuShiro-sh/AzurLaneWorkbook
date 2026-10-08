# 导入和导出受锁文件约束的离线依赖归档缓存包。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Lock.ps1')

function Export-AzlwDependencyCacheBundle {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $LockPath,

        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [string] $BundlePath
    )

    [pscustomobject] $lock = Get-AzlwDependencyLock -Path $LockPath
    [string] $resolvedDependencyRoot = Get-AzlwFullPath -Path $DependencyRoot
    [string] $resolvedBundle = Get-AzlwFullPath -Path $BundlePath
    Assert-AzlwPathHasNoReparsePoints -Path $resolvedDependencyRoot -Label '依赖根'
    [string] $staging = Join-Path $resolvedDependencyRoot ('.bundle-' + [guid]::NewGuid().ToString('N'))
    [string] $archiveStaging = Join-Path $staging 'archives'
    [void](New-Item -ItemType Directory -Path $archiveStaging -Force)
    try {
        [System.Collections.Generic.List[object]] $entries = [System.Collections.Generic.List[object]]::new()
        foreach ($dependency in @($lock.dependencies | Where-Object { [bool]$_.cache_bundle })) {
            [string] $archive = Get-AzlwArchivePath `
                -DependencyRoot $resolvedDependencyRoot `
                -Dependency $dependency
            if (-not (Test-AzlwArchive -Path $archive -Dependency $dependency)) {
                throw "缓存包缺少有效归档: $($dependency.id)"
            }
            [string] $name = [System.IO.Path]::GetFileName($archive)
            Copy-Item -LiteralPath $archive -Destination (Join-Path $archiveStaging $name) -ErrorAction Stop
            $entries.Add([ordered]@{
                id       = [string]$dependency.id
                file_name = $name
                sha256   = ([string]$dependency.archive.sha256).ToLowerInvariant()
                size_bytes = [long]$dependency.archive.size_bytes
            })
        }
        $manifest = [ordered]@{
            schema      = 1
            lock_sha256 = Get-AzlwFileSha256 -Path $LockPath
            archives    = @($entries)
        }
        [System.IO.File]::WriteAllText(
            (Join-Path $staging 'bundle-manifest.json'),
            (($manifest | ConvertTo-Json -Depth 8) + "`n"),
            [System.Text.UTF8Encoding]::new($false)
        )
        [string] $bundleParent = [System.IO.Path]::GetDirectoryName($resolvedBundle)
        [void](New-Item -ItemType Directory -Path $bundleParent -Force)
        if (Test-Path -LiteralPath $resolvedBundle) {
            throw "缓存包目标已经存在: $resolvedBundle"
        }
        Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $resolvedBundle
    } finally {
        if (Test-Path -LiteralPath $staging) {
            Remove-AzlwContainedItem -Root $resolvedDependencyRoot -Path $staging
        }
    }
}

function Import-AzlwDependencyCacheBundle {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $LockPath,

        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [string] $BundlePath
    )

    [string] $resolvedDependencyRoot = Get-AzlwFullPath -Path $DependencyRoot
    [string] $resolvedBundle = Get-AzlwFullPath -Path $BundlePath
    if (-not (Test-Path -LiteralPath $resolvedBundle -PathType Leaf)) {
        throw "缓存包不存在: $resolvedBundle"
    }
    [void](New-Item -ItemType Directory -Path $resolvedDependencyRoot -Force)
    Assert-AzlwPathHasNoReparsePoints -Path $resolvedDependencyRoot -Label '依赖根'
    [string] $staging = Join-Path $resolvedDependencyRoot ('.import-' + [guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $staging -Force)
    try {
        Assert-AzlwZipArchiveEntries -Path $resolvedBundle -Label '依赖缓存包'
        [System.IO.Compression.ZipFile]::ExtractToDirectory($resolvedBundle, $staging, $true)
        [string] $manifestPath = Join-Path $staging 'bundle-manifest.json'
        if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
            throw '缓存包缺少 bundle-manifest.json'
        }
        [pscustomobject] $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding utf8 |
            ConvertFrom-Json
        if ($manifest.schema -ne 1 -or [string]$manifest.lock_sha256 -ne (Get-AzlwFileSha256 -Path $LockPath)) {
            throw '缓存包与当前依赖锁不匹配'
        }
        [pscustomobject] $lock = Get-AzlwDependencyLock -Path $LockPath
        [System.Collections.Generic.HashSet[string]] $importedIds =
            [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
        [object[]] $approvedDependencies = @($lock.dependencies | Where-Object { [bool]$_.cache_bundle })
        [string] $archiveDirectory = Join-Path $resolvedDependencyRoot 'archives'
        Assert-AzlwPathHasNoReparsePoints -Path $archiveDirectory -Label '依赖归档目录'
        [void](New-Item -ItemType Directory -Path $archiveDirectory -Force)
        Assert-AzlwPathHasNoReparsePoints -Path $archiveDirectory -Label '依赖归档目录'
        foreach ($entry in @($manifest.archives)) {
            Assert-AzlwPlainFileName -Name ([string]$entry.file_name) -Label '缓存包 archive.file_name'
            if (-not $importedIds.Add([string]$entry.id)) {
                throw "缓存包包含重复依赖: $($entry.id)"
            }
            [object[]] $matches = @($lock.dependencies | Where-Object {
                [string]$_.id -eq [string]$entry.id -and [bool]$_.cache_bundle
            })
            if ($matches.Count -ne 1) {
                throw "缓存包包含当前锁未批准导出的依赖: $($entry.id)"
            }
            [pscustomobject] $dependency = $matches[0]
            [string] $expectedName = [System.IO.Path]::GetFileName((Get-AzlwArchivePath `
                -DependencyRoot $resolvedDependencyRoot `
                -Dependency $dependency))
            if ([string]$entry.file_name -ne $expectedName -or
                [string]$entry.sha256 -ne ([string]$dependency.archive.sha256).ToLowerInvariant() -or
                [long]$entry.size_bytes -ne [long]$dependency.archive.size_bytes) {
                throw "缓存包依赖元数据与当前锁不匹配: $($entry.id)"
            }
            [string] $source = Join-Path (Join-Path $staging 'archives') ([string]$entry.file_name)
            if (-not (Test-Path -LiteralPath $source -PathType Leaf) -or
                (Get-AzlwFileSha256 -Path $source) -ne [string]$entry.sha256 -or
                (Get-Item -LiteralPath $source).Length -ne [long]$entry.size_bytes) {
                throw "缓存包归档校验失败: $($entry.id)"
            }
            [string] $destination = Join-Path $archiveDirectory ([string]$entry.file_name)
            Assert-AzlwPathHasNoReparsePoints -Path $destination -Label '依赖缓存目标'
            if (-not (Test-AzlwArchive -Path $destination -Dependency $dependency)) {
                if (Test-Path -LiteralPath $destination) {
                    Remove-AzlwContainedItem -Root $resolvedDependencyRoot -Path $destination
                }
                Copy-Item -LiteralPath $source -Destination $destination -ErrorAction Stop
            }
        }
        if ($importedIds.Count -ne $approvedDependencies.Count) {
            [string[]] $missing = @($approvedDependencies | Where-Object {
                -not $importedIds.Contains([string]$_.id)
            } | ForEach-Object { [string]$_.id })
            throw "缓存包缺少当前锁批准的依赖: $($missing -join ', ')"
        }
    } finally {
        if (Test-Path -LiteralPath $staging) {
            Remove-AzlwContainedItem -Root $resolvedDependencyRoot -Path $staging
        }
    }
}
