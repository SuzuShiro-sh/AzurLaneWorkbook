# 提供脚本共享的路径、归档校验与受控文件系统操作。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-AzlwFullPath {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [string] $BasePath = (Get-Location).Path
    )

    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $BasePath $Path))
}

function Test-AzlwPathWithinRoot {
    [CmdletBinding()]
    [OutputType([bool])]
    param(
        [Parameter(Mandatory)]
        [string] $Root,

        [Parameter(Mandatory)]
        [string] $Path
    )

    [string] $resolvedRoot = Get-AzlwFullPath -Path $Root
    [string] $resolvedPath = Get-AzlwFullPath -Path $Path
    [string] $rootWithSeparator = $resolvedRoot.TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    ) + [System.IO.Path]::DirectorySeparatorChar
    [bool] $runningOnWindows = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [System.Runtime.InteropServices.OSPlatform]::Windows
    )
    [System.StringComparison] $comparison = if ($runningOnWindows) {
        [System.StringComparison]::OrdinalIgnoreCase
    } else {
        [System.StringComparison]::Ordinal
    }
    return $resolvedPath.StartsWith($rootWithSeparator, $comparison)
}

function Assert-AzlwSafeRelativePath {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Label
    )

    if ([string]::IsNullOrWhiteSpace($Path) -or [System.IO.Path]::IsPathRooted($Path)) {
        throw "$Label 必须是非空相对路径: $Path"
    }
    if ($Path.Contains('\')) {
        throw "$Label 必须统一使用正斜杠: $Path"
    }
    [string[]] $segments = @($Path -split '/')
    if ($segments.Count -eq 0 -or @($segments | Where-Object {
        [string]::IsNullOrWhiteSpace($_) -or
        $_ -eq '.' -or
        $_ -eq '..' -or
        $_.Contains(':') -or
        $_ -match '[*?\[\]\x00-\x1f]' -or
        $_ -match '[. ]$' -or
        ([System.IO.Path]::GetFileNameWithoutExtension($_) -match '^(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$')
    }).Count -ne 0) {
        throw "$Label 包含空段、跳转段、通配符、保留名或盘符: $Path"
    }
}

function Assert-AzlwPlainFileName {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Name,

        [Parameter(Mandatory)]
        [string] $Label
    )

    Assert-AzlwSafeRelativePath -Path $Name -Label $Label
    if (($Name -split '/').Count -ne 1) {
        throw "$Label 必须是单个文件名: $Name"
    }
}

function Assert-AzlwPathHasNoReparsePoints {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Label
    )

    [string] $current = Get-AzlwFullPath -Path $Path
    while (-not [string]::IsNullOrWhiteSpace($current)) {
        if (Test-Path -LiteralPath $current) {
            [System.IO.FileSystemInfo] $item = Get-Item -LiteralPath $current -Force
            if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "$Label 的路径链包含链接或重解析点: $current"
            }
        }
        [string] $parent = [System.IO.Path]::GetDirectoryName($current)
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

function Assert-AzlwZipArchiveEntries {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Label
    )

    [System.IO.Compression.ZipArchive] $archive = [System.IO.Compression.ZipFile]::OpenRead($Path)
    try {
        [System.Collections.Generic.HashSet[string]] $entries =
            [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
        foreach ($entry in $archive.Entries) {
            [string] $name = $entry.FullName
            if ($name.Contains('\')) {
                throw "$Label 包含反斜杠路径条目: $name"
            }
            [string] $relative = $name.TrimEnd('/')
            if ([string]::IsNullOrWhiteSpace($relative)) {
                continue
            }
            Assert-AzlwSafeRelativePath -Path $relative -Label "$Label ZIP 条目"
            if (-not $entries.Add($relative)) {
                throw "$Label 包含大小写重复条目: $relative"
            }
            [int] $unixFileType = ($entry.ExternalAttributes -shr 16) -band 0xF000
            if ($unixFileType -eq 0xA000) {
                throw "$Label 包含符号链接条目: $relative"
            }
        }
    } finally {
        $archive.Dispose()
    }
}

function Get-AzlwFileSha256 {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
}


function Remove-AzlwContainedItem {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Root,

        [Parameter(Mandatory)]
        [string] $Path
    )

    Assert-AzlwPathHasNoReparsePoints -Path $Root -Label '依赖根'
    Assert-AzlwPathHasNoReparsePoints -Path $Path -Label '待删除项'
    if (-not (Test-AzlwPathWithinRoot -Root $Root -Path $Path)) {
        throw "拒绝删除依赖根以外的路径: $Path"
    }
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
    }
}

function Assert-AzlwRegularDirectory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Label
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Label 不存在或不是目录: $Path"
    }
    [System.IO.FileSystemInfo] $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label 不允许是链接或重解析点: $Path"
    }
}

function Assert-AzlwRegularFile {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Label
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label 不存在或不是普通文件: $Path"
    }
    [System.IO.FileSystemInfo] $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label 不允许是链接或重解析点: $Path"
    }
}
