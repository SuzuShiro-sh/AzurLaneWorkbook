# 管理共享构建目录的排他锁，协调生产构建与 Native 测试。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot '../common/Paths.ps1')

function Enter-AzlwBuildLock {
    [CmdletBinding()]
    [OutputType([System.IO.FileStream])]
    param(
        [Parameter(Mandatory)]
        [string] $TargetRoot
    )

    [string] $resolvedRoot = Get-AzlwFullPath -Path $TargetRoot
    Assert-AzlwRegularDirectory -Path $resolvedRoot -Label '构建目标根'
    [string] $lockPath = Join-Path $resolvedRoot '.azlw-production-build.lock'
    try {
        [System.IO.FileStream] $stream = [System.IO.FileStream]::new(
            $lockPath,
            [System.IO.FileMode]::OpenOrCreate,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None,
            4096,
            [System.IO.FileOptions]::WriteThrough
        )
    } catch [System.IO.IOException] {
        throw "另一个构建或 Native 测试正在使用目标目录: $resolvedRoot"
    }
    try {
        $owner = [ordered]@{
            schema         = 1
            process_id     = $PID
            acquired_at_utc = [DateTimeOffset]::UtcNow.ToString('O')
        }
        [byte[]] $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes(
            (($owner | ConvertTo-Json -Compress) + "`n")
        )
        $stream.SetLength(0)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
        return $stream
    } catch {
        $stream.Dispose()
        throw
    }
}

function Enter-AzlwBuildLocks {
    [CmdletBinding()]
    [OutputType([System.Collections.Generic.List[System.IO.FileStream]])]
    param(
        [Parameter(Mandatory)]
        [string[]] $TargetRoots
    )

    [string[]] $orderedRoots = @($TargetRoots | ForEach-Object {
        Get-AzlwFullPath -Path $_
    } | Sort-Object -Unique)
    [System.Collections.Generic.List[System.IO.FileStream]] $locks =
        [System.Collections.Generic.List[System.IO.FileStream]]::new()
    try {
        foreach ($root in $orderedRoots) {
            $locks.Add((Enter-AzlwBuildLock -TargetRoot $root))
        }
        return ,$locks
    } catch {
        for ([int] $index = $locks.Count - 1; $index -ge 0; $index--) {
            $locks[$index].Dispose()
        }
        throw
    }
}

function Exit-AzlwBuildLocks {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [System.Collections.Generic.List[System.IO.FileStream]] $Locks
    )

    for ([int] $index = $Locks.Count - 1; $index -ge 0; $index--) {
        $Locks[$index].Dispose()
    }
}
