# 下载、安装并记录固定依赖工具的原子解析状态。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Lock.ps1')

function Get-AzlwDependencyArchive {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [pscustomobject] $Dependency,

        [switch] $Offline,

        [ValidateRange(1, 20)]
        [int] $MaximumAttempts = 3
    )

    [string] $archiveDirectory = Join-Path $DependencyRoot 'archives'
    Assert-AzlwPathHasNoReparsePoints -Path $archiveDirectory -Label '依赖归档目录'
    [void](New-Item -ItemType Directory -Path $archiveDirectory -Force)
    Assert-AzlwPathHasNoReparsePoints -Path $archiveDirectory -Label '依赖归档目录'
    [string] $archivePath = Get-AzlwArchivePath -DependencyRoot $DependencyRoot -Dependency $Dependency
    if (Test-AzlwArchive -Path $archivePath -Dependency $Dependency) {
        return $archivePath
    }
    if ($Offline) {
        if (Test-Path -LiteralPath $archivePath) {
            throw "离线模式发现损坏的依赖归档并保持原文件不变: $($Dependency.id) $archivePath"
        }
        throw "离线模式缺少有效依赖归档: $($Dependency.id) $($Dependency.version)"
    }
    if (Test-Path -LiteralPath $archivePath) {
        Remove-AzlwContainedItem -Root $DependencyRoot -Path $archivePath
    }

    [string] $partialPath = "$archivePath.part"
    Remove-AzlwContainedItem -Root $DependencyRoot -Path $partialPath
    try {
        $previousProgress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        Write-Host "下载依赖: $($Dependency.id) $($Dependency.version)；$($Dependency.archive.url)"
        for ([int] $attempt = 1; $attempt -le $MaximumAttempts; $attempt++) {
            try {
                Invoke-WebRequest -Uri ([string]$Dependency.archive.url) -OutFile $partialPath -ErrorAction Stop
                break
            } catch {
                [System.Exception] $failure = $_.Exception
                [System.Exception] $cause = $failure
                [bool] $retryable = $false
                [bool] $permanentFailure = $false
                while ($null -ne $cause) {
                    if ($cause -is [System.Net.Sockets.SocketException] -or
                        $cause -is [System.TimeoutException] -or
                        $cause -is [System.Net.Http.HttpRequestException] -or
                        $cause.GetType().FullName -ceq 'System.Net.Http.HttpIOException' -or
                        ($cause -is [System.IO.IOException] -and
                            $cause.StackTrace -match '\bSystem\.Net\.Security\.SslStream\.')) {
                        $retryable = $true
                    }
                    if ($cause -is [System.Security.Authentication.AuthenticationException] -or
                        ($cause -is [System.Net.Http.HttpRequestException] -and $null -ne $cause.StatusCode)) {
                        $permanentFailure = $true
                    }
                    $cause = $cause.InnerException
                }
                if (-not $retryable -or $permanentFailure -or $attempt -eq $MaximumAttempts) {
                    throw [System.InvalidOperationException]::new(
                        "依赖下载失败: $($Dependency.id) $($Dependency.version)；$($Dependency.archive.url)；第 $attempt/$MaximumAttempts 次尝试；$($failure.Message)",
                        $failure
                    )
                }
                Remove-AzlwContainedItem -Root $DependencyRoot -Path $partialPath
                Write-Warning "依赖下载中断，将重试: $($Dependency.id)；第 $attempt/$MaximumAttempts 次尝试；$($failure.Message)"
                Start-Sleep -Milliseconds ([Math]::Min(5000, 250 * [Math]::Pow(2, $attempt - 1)))
            }
        }
        if (-not (Test-AzlwArchive -Path $partialPath -Dependency $Dependency)) {
            throw "下载文件大小或 SHA-256 不匹配: $($Dependency.id)"
        }
        try {
            [System.IO.File]::Move($partialPath, $archivePath)
        } catch [System.IO.IOException] {
            if (-not (Test-AzlwArchive -Path $archivePath -Dependency $Dependency)) {
                throw
            }
        }
    } finally {
        $ProgressPreference = $previousProgress
        if (Test-Path -LiteralPath $partialPath) {
            Remove-AzlwContainedItem -Root $DependencyRoot -Path $partialPath
        }
    }
    return $archivePath
}

function Install-AzlwDependency {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [pscustomobject] $Dependency,

        [Parameter(Mandatory)]
        [string] $ArchivePath,

        [switch] $Repair
    )

    if (Test-AzlwInstalledDependency -DependencyRoot $DependencyRoot -Dependency $Dependency) {
        return Get-AzlwInstallPath -DependencyRoot $DependencyRoot -Dependency $Dependency
    }
    [string] $installPath = Get-AzlwInstallPath -DependencyRoot $DependencyRoot -Dependency $Dependency
    Assert-AzlwPathHasNoReparsePoints -Path $installPath -Label "依赖 $($Dependency.id) 安装目录"
    if ((Test-Path -LiteralPath $installPath) -and -not $Repair) {
        throw "依赖安装目录存在但未通过校验，请使用 -Repair: $installPath"
    }

    [string] $stagingRoot = Join-Path $DependencyRoot ('.staging/' + [guid]::NewGuid().ToString('N'))
    [string] $extractedRoot = Join-Path $stagingRoot 'extracted'
    Assert-AzlwPathHasNoReparsePoints -Path $stagingRoot -Label '依赖解压暂存目录'
    [void](New-Item -ItemType Directory -Path $extractedRoot -Force)
    Assert-AzlwPathHasNoReparsePoints -Path $stagingRoot -Label '依赖解压暂存目录'
    try {
        Assert-AzlwZipArchiveEntries -Path $ArchivePath -Label "依赖 $($Dependency.id)"
        [System.IO.Compression.ZipFile]::ExtractToDirectory($ArchivePath, $extractedRoot, $true)
        [string] $sourceRoot = $extractedRoot
        if (-not [string]::IsNullOrWhiteSpace([string]$Dependency.install.strip_prefix)) {
            $sourceRoot = Join-Path $extractedRoot ([string]$Dependency.install.strip_prefix)
        }
        [bool] $sourceIsExtractionRoot = (Get-AzlwFullPath -Path $sourceRoot) -eq
            (Get-AzlwFullPath -Path $extractedRoot)
        if ((-not $sourceIsExtractionRoot -and
            -not (Test-AzlwPathWithinRoot -Root $extractedRoot -Path $sourceRoot)) -or
            -not (Test-Path -LiteralPath $sourceRoot -PathType Container)) {
            throw "依赖 $($Dependency.id) 的 strip_prefix 不存在或越界: $sourceRoot"
        }
        [System.Collections.Generic.List[object]] $requiredFiles =
            [System.Collections.Generic.List[object]]::new()
        foreach ($relativePath in @($Dependency.install.required_paths)) {
            [string] $required = Join-Path $sourceRoot ([string]$relativePath)
            if (-not (Test-AzlwPathWithinRoot -Root $sourceRoot -Path $required) -or
                -not (Test-Path -LiteralPath $required -PathType Leaf)) {
                throw "依赖 $($Dependency.id) 缺少要求文件: $relativePath"
            }
            Assert-AzlwPathHasNoReparsePoints -Path $required -Label "依赖 $($Dependency.id) 必需文件"
            [System.IO.FileInfo] $requiredFile = Get-Item -LiteralPath $required -Force
            $requiredFiles.Add([ordered]@{
                path       = [string]$relativePath
                size_bytes = $requiredFile.Length
                sha256     = Get-AzlwFileSha256 -Path $required
            })
        }
        $marker = [ordered]@{
            schema         = 2
            id             = [string]$Dependency.id
            version        = [string]$Dependency.version
            archive_sha256 = ([string]$Dependency.archive.sha256).ToLowerInvariant()
            required_files = @($requiredFiles)
        }
        [string] $markerJson = ($marker | ConvertTo-Json -Depth 4) + "`n"
        [System.IO.File]::WriteAllText(
            (Join-Path $sourceRoot '.azlw-dependency.json'),
            $markerJson,
            [System.Text.UTF8Encoding]::new($false)
        )

        [string] $installParent = [System.IO.Path]::GetDirectoryName($installPath)
        Assert-AzlwPathHasNoReparsePoints -Path $installParent -Label "依赖 $($Dependency.id) 安装父目录"
        [void](New-Item -ItemType Directory -Path $installParent -Force)
        Assert-AzlwPathHasNoReparsePoints -Path $installParent -Label "依赖 $($Dependency.id) 安装父目录"
        [string] $previousInstall = "$installPath.previous-$([guid]::NewGuid().ToString('N'))"
        [bool] $movedPrevious = $false
        try {
            if (Test-Path -LiteralPath $installPath) {
                Move-AzlwDependencyDirectory -Source $installPath -Destination $previousInstall
                $movedPrevious = $true
            }
            Move-AzlwDependencyDirectory -Source $sourceRoot -Destination $installPath
        } catch {
            if ($movedPrevious -and
                -not (Test-Path -LiteralPath $installPath) -and
                (Test-Path -LiteralPath $previousInstall -PathType Container)) {
                Move-AzlwDependencyDirectory -Source $previousInstall -Destination $installPath
            }
            throw
        }
        if ($movedPrevious -and (Test-Path -LiteralPath $previousInstall)) {
            Remove-AzlwContainedItem -Root $DependencyRoot -Path $previousInstall
        }
    } finally {
        if (Test-Path -LiteralPath $stagingRoot) {
            Remove-AzlwContainedItem -Root $DependencyRoot -Path $stagingRoot
        }
    }
    if (-not (Test-AzlwInstalledDependency -DependencyRoot $DependencyRoot -Dependency $Dependency)) {
        throw "依赖发布后复核失败: $($Dependency.id)"
    }
    return $installPath
}

function Write-AzlwResolvedState {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [Parameter(Mandatory)]
        [string] $LockPath,

        [Parameter(Mandatory)]
        [object[]] $ResolvedDependencies
    )

    $state = [ordered]@{
        schema               = 1
        lock_sha256          = Get-AzlwFileSha256 -Path $LockPath
        resolved_at_utc      = [DateTimeOffset]::UtcNow.ToString('O')
        dependencies         = @($ResolvedDependencies)
    }
    [string] $json = ($state | ConvertTo-Json -Depth 8) + "`n"
    [string] $temporary = Join-Path $DependencyRoot ('.resolved-' + [guid]::NewGuid().ToString('N') + '.json')
    [string] $destination = Join-Path $DependencyRoot 'resolved.json'
    [System.IO.FileStream] $stream = [System.IO.FileStream]::new(
        $temporary,
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None
    )
    try {
        [byte[]] $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($json)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
    if (Test-Path -LiteralPath $destination -PathType Leaf) {
        [string] $backup = "$destination.backup-$([guid]::NewGuid().ToString('N'))"
        try {
            [System.IO.File]::Replace($temporary, $destination, $backup)
        } finally {
            if (Test-Path -LiteralPath $backup) {
                Remove-AzlwContainedItem -Root $DependencyRoot -Path $backup
            }
        }
    } else {
        [System.IO.File]::Move($temporary, $destination)
    }
}

function Invoke-AzlwDependencyBootstrap {
    [CmdletBinding()]
    [OutputType([object[]])]
    param(
        [Parameter(Mandatory)]
        [string] $LockPath,

        [Parameter(Mandatory)]
        [string] $DependencyRoot,

        [switch] $Offline,

        [switch] $Repair
    )

    [string] $resolvedLockPath = Get-AzlwFullPath -Path $LockPath
    [string] $resolvedDependencyRoot = Get-AzlwFullPath -Path $DependencyRoot
    [void](New-Item -ItemType Directory -Path $resolvedDependencyRoot -Force)
    Assert-AzlwPathHasNoReparsePoints -Path $resolvedDependencyRoot -Label '依赖根'
    [pscustomobject] $lock = Get-AzlwDependencyLock -Path $resolvedLockPath
    [System.Collections.Generic.List[object]] $resolved = [System.Collections.Generic.List[object]]::new()
    foreach ($dependency in @($lock.dependencies)) {
        [string] $archive = Get-AzlwDependencyArchive `
            -DependencyRoot $resolvedDependencyRoot `
            -Dependency $dependency `
            -Offline:$Offline
        [string] $install = Install-AzlwDependency `
            -DependencyRoot $resolvedDependencyRoot `
            -Dependency $dependency `
            -ArchivePath $archive `
            -Repair:$Repair
        $resolved.Add([ordered]@{
            id          = [string]$dependency.id
            version     = [string]$dependency.version
            install     = $install
            archive     = $archive
            cache_bundle = [bool]$dependency.cache_bundle
        })
    }
    Write-AzlwResolvedState `
        -DependencyRoot $resolvedDependencyRoot `
        -LockPath $resolvedLockPath `
        -ResolvedDependencies @($resolved)
    return @($resolved)
}

function Move-AzlwDependencyDirectory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Source,

        [Parameter(Mandatory)]
        [string] $Destination,

        [ValidateRange(1, 20)]
        [int] $MaximumAttempts = 10
    )

    for ([int] $attempt = 1; $attempt -le $MaximumAttempts; $attempt++) {
        try {
            [System.IO.Directory]::Move($Source, $Destination)
            return
        } catch [System.UnauthorizedAccessException] {
            [System.Exception] $failure = $_.Exception
        } catch [System.IO.IOException] {
            [System.Exception] $failure = $_.Exception
        }
        [bool] $sourceExists = Test-Path -LiteralPath $Source -PathType Container
        [bool] $destinationExists = Test-Path -LiteralPath $Destination -PathType Container
        if (-not $sourceExists -and $destinationExists) {
            return
        }
        if (-not $sourceExists -or $destinationExists) {
            throw "依赖目录移动返回不确定状态，拒绝继续: $Source -> $Destination；$($failure.Message)"
        }
        if ($attempt -eq $MaximumAttempts) {
            throw "依赖目录连续 $MaximumAttempts 次无法发布: $Source -> $Destination；$($failure.Message)"
        }
        Start-Sleep -Milliseconds ([Math]::Min(5000, 250 * [Math]::Pow(2, $attempt - 1)))
    }
}
