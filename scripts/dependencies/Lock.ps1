# 解析固定依赖锁，并验证内容寻址归档和已安装工具状态。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot '../common/Paths.ps1')

function Get-AzlwDependencyLock {
    [CmdletBinding()]
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    [string] $resolvedPath = Get-AzlwFullPath -Path $Path
    if (-not (Test-Path -LiteralPath $resolvedPath -PathType Leaf)) {
        throw "依赖锁文件不存在: $resolvedPath"
    }
    [pscustomobject] $lock = Get-Content -LiteralPath $resolvedPath -Raw -Encoding utf8 |
        ConvertFrom-Json -Depth 32
    if ($lock.schema -ne 1) {
        throw "依赖锁 schema 必须是 1，实际为 $($lock.schema)"
    }
    if ([string]$lock.platform -ne 'windows-x64') {
        throw "依赖锁 platform 必须是 windows-x64，实际为 $($lock.platform)"
    }
    [object[]] $dependencies = @($lock.dependencies)
    if ($dependencies.Count -eq 0) {
        throw '依赖锁至少需要一个 dependencies 条目'
    }

    [System.Collections.Generic.HashSet[string]] $identifiers =
        [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($dependency in $dependencies) {
        [string] $identifier = [string]$dependency.id
        if ($identifier -notmatch '^[a-z0-9][a-z0-9._-]*$') {
            throw "依赖 id 格式无效: $identifier"
        }
        if (-not $identifiers.Add($identifier)) {
            throw "依赖 id 重复: $identifier"
        }
        if ([string]::IsNullOrWhiteSpace([string]$dependency.version)) {
            throw "依赖 $identifier 缺少 version"
        }
        if ([string]$dependency.archive.url -notmatch '^https://') {
            throw "依赖 $identifier 的 archive.url 必须使用 https"
        }
        Assert-AzlwPlainFileName -Name ([string]$dependency.archive.file_name) `
            -Label "依赖 $identifier archive.file_name"
        if ([long]$dependency.archive.size_bytes -le 0) {
            throw "依赖 $identifier 的 archive.size_bytes 必须大于 0"
        }
        if ([string]$dependency.archive.sha256 -notmatch '^[0-9a-fA-F]{64}$') {
            throw "依赖 $identifier 的 archive.sha256 必须是 64 位十六进制"
        }
        if ([System.IO.Path]::GetExtension([string]$dependency.archive.file_name) -ine '.zip') {
            throw "依赖 $identifier 的 archive.file_name 必须是 ZIP 归档"
        }
        if ($dependency.cache_bundle -isnot [bool]) {
            throw "依赖 $identifier 的 cache_bundle 必须是布尔值"
        }
        Assert-AzlwSafeRelativePath -Path ([string]$dependency.install.directory) `
            -Label "依赖 $identifier install.directory"
        if (-not [string]::IsNullOrWhiteSpace([string]$dependency.install.strip_prefix)) {
            Assert-AzlwSafeRelativePath -Path ([string]$dependency.install.strip_prefix) `
                -Label "依赖 $identifier install.strip_prefix"
        }
        [string[]] $requiredPaths = @($dependency.install.required_paths | ForEach-Object { [string]$_ })
        if ($requiredPaths.Count -eq 0) {
            throw "依赖 $identifier 至少需要一个 install.required_paths 条目"
        }
        foreach ($requiredPath in $requiredPaths) {
            Assert-AzlwSafeRelativePath -Path $requiredPath `
                -Label "依赖 $identifier install.required_paths"
        }
        if ([string]::IsNullOrWhiteSpace([string]$dependency.license.name) -or
            [string]$dependency.license.url -notmatch '^https://') {
            throw "依赖 $identifier 缺少有效的 license.name 或 license.url"
        }
    }
    return $lock
}

function Get-AzlwArchivePath {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [pscustomobject] $Dependency
    )

    [string] $extension = [System.IO.Path]::GetExtension([string]$Dependency.archive.file_name)
    [string] $digest = ([string]$Dependency.archive.sha256).ToLowerInvariant()
    return Join-Path $DependencyRoot "archives/$digest$extension"
}

function Get-AzlwInstallPath {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [pscustomobject] $Dependency
    )

    [string] $path = Join-Path $DependencyRoot ([string]$Dependency.install.directory)
    if (-not (Test-AzlwPathWithinRoot -Root $DependencyRoot -Path $path)) {
        throw "依赖 $($Dependency.id) 的安装目录越出依赖根: $path"
    }
    return Get-AzlwFullPath -Path $path
}

function Test-AzlwArchive {
    [CmdletBinding()]
    [OutputType([bool])]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [pscustomobject] $Dependency
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $false
    }
    [System.IO.FileInfo] $file = Get-Item -LiteralPath $Path -Force
    if ($file.Length -ne [long]$Dependency.archive.size_bytes) {
        return $false
    }
    return (Get-AzlwFileSha256 -Path $Path) -eq ([string]$Dependency.archive.sha256).ToLowerInvariant()
}

function Test-AzlwInstalledDependency {
    [CmdletBinding()]
    [OutputType([bool])]
    param(
        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [pscustomobject] $Dependency
    )

    [string] $installPath = Get-AzlwInstallPath -DependencyRoot $DependencyRoot -Dependency $Dependency
    [string] $markerPath = Join-Path $installPath '.azlw-dependency.json'
    Assert-AzlwPathHasNoReparsePoints -Path $installPath -Label "依赖 $($Dependency.id) 安装目录"
    Assert-AzlwPathHasNoReparsePoints -Path $markerPath -Label "依赖 $($Dependency.id) 安装标记"
    if (-not (Test-Path -LiteralPath $installPath -PathType Container) -or
        -not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
        return $false
    }
    try {
        [pscustomobject] $marker = Get-Content -LiteralPath $markerPath -Raw -Encoding utf8 |
            ConvertFrom-Json
    } catch {
        return $false
    }
    if ($marker.schema -ne 2 -or
        [string]$marker.id -ne [string]$Dependency.id -or
        [string]$marker.version -ne [string]$Dependency.version -or
        [string]$marker.archive_sha256 -ne ([string]$Dependency.archive.sha256).ToLowerInvariant()) {
        return $false
    }
    [object[]] $recordedFiles = @($marker.required_files)
    [string[]] $requiredPaths = @($Dependency.install.required_paths | ForEach-Object { [string]$_ })
    if ($recordedFiles.Count -ne $requiredPaths.Count) {
        return $false
    }
    foreach ($relativePath in $requiredPaths) {
        [object[]] $fileRecords = @($recordedFiles | Where-Object { [string]$_.path -ceq $relativePath })
        if ($fileRecords.Count -ne 1 -or
            [string]$fileRecords[0].sha256 -notmatch '^[0-9a-f]{64}$' -or
            [long]$fileRecords[0].size_bytes -lt 0) {
            return $false
        }
        [string] $required = Join-Path $installPath $relativePath
        Assert-AzlwPathHasNoReparsePoints -Path $required -Label "依赖 $($Dependency.id) 必需文件"
        if (-not (Test-AzlwPathWithinRoot -Root $installPath -Path $required) -or
            -not (Test-Path -LiteralPath $required -PathType Leaf)) {
            return $false
        }
        [System.IO.FileInfo] $requiredFile = Get-Item -LiteralPath $required -Force
        if ($requiredFile.Length -ne [long]$fileRecords[0].size_bytes -or
            (Get-AzlwFileSha256 -Path $required) -cne [string]$fileRecords[0].sha256) {
            return $false
        }
    }
    return $true
}

function Get-AzlwInstalledDependency {
    [CmdletBinding()]
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        [pscustomobject] $Lock,

        [Parameter(Mandatory)]
        [string] $Identifier,

        [Parameter(Mandatory)]
        [string] $DependencyRoot
    )

    [object[]] $matches = @($Lock.dependencies | Where-Object { [string]$_.id -ceq $Identifier })
    if ($matches.Count -ne 1) {
        throw "依赖锁没有唯一 $Identifier 条目"
    }
    [pscustomobject] $dependency = $matches[0]
    if (-not (Test-AzlwInstalledDependency -DependencyRoot $DependencyRoot -Dependency $dependency)) {
        throw "依赖 $Identifier 未准备或缓存无效，请先运行 scripts/Bootstrap-Dev.ps1"
    }
    return [pscustomobject]@{
        definition = $dependency
        path       = Get-AzlwInstallPath -DependencyRoot $DependencyRoot -Dependency $dependency
    }
}
