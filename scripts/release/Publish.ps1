# 保留受验证的可变状态，并以可恢复事务替换生产发布目录。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot '../common/Paths.ps1')
. (Join-Path $PSScriptRoot '../common/Process.ps1')

function Copy-AzlwRegularTree {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Source,

        [Parameter(Mandatory)]
        [string] $Destination
    )

    Assert-AzlwRegularDirectory -Path $Source -Label '状态源目录'
    if (Test-Path -LiteralPath $Destination) {
        throw "状态目标已经存在: $Destination"
    }
    [void](New-Item -ItemType Directory -Path $Destination)
    [System.Collections.Generic.Queue[object]] $queue = [System.Collections.Generic.Queue[object]]::new()
    $queue.Enqueue([pscustomobject]@{ Source = $Source; Destination = $Destination })
    while ($queue.Count -gt 0) {
        [pscustomobject] $current = $queue.Dequeue()
        foreach ($item in @(Get-ChildItem -LiteralPath $current.Source -Force)) {
            if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "状态目录包含链接或重解析点，拒绝迁移: $($item.FullName)"
            }
            [string] $target = Join-Path $current.Destination $item.Name
            if ($item.PSIsContainer) {
                [void](New-Item -ItemType Directory -Path $target)
                $queue.Enqueue([pscustomobject]@{ Source = $item.FullName; Destination = $target })
            } elseif ($item -is [System.IO.FileInfo]) {
                Copy-Item -LiteralPath $item.FullName -Destination $target -ErrorAction Stop
            } else {
                throw "状态目录包含不支持的文件系统项: $($item.FullName)"
            }
        }
    }
}

function Copy-AzlwMutableReleaseState {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $ExistingRelease,

        [Parameter(Mandatory)]
        [string] $CandidateRelease
    )

    foreach ($name in @('settings.json', 'workbook-layout.xlsx')) {
        [string] $source = Join-Path $ExistingRelease ".suzushiro\$name"
        [string] $destination = Join-Path $CandidateRelease ".suzushiro\$name"
        Assert-AzlwRegularFile -Path $source -Label "旧发布 $name"
        Assert-AzlwRegularFile -Path $destination -Label "候选发布 $name"
        Copy-Item -LiteralPath $source -Destination $destination -Force -ErrorAction Stop
    }
    [string] $sourceData = Join-Path $ExistingRelease '.suzushiro\data'
    if (Test-Path -LiteralPath $sourceData) {
        Copy-AzlwRegularTree -Source $sourceData -Destination (Join-Path $CandidateRelease '.suzushiro\data')
    }
}

function Update-AzlwCandidateLayout {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string] $ReleaseRoot)

    [string] $application = Join-Path $ReleaseRoot 'AzurLaneWorkbook.exe'
    [string] $layout = Join-Path $ReleaseRoot '.suzushiro/workbook-layout.xlsx'
    Assert-AzlwRegularFile -Path $application -Label '候选程序'
    Assert-AzlwRegularFile -Path $layout -Label '候选布局'
    $check = Invoke-AuditCommand -FilePath $application -ArgumentList @('layout-check') `
        -WorkingDirectory $ReleaseRoot -AllowFailure
    if ($check.ExitCode -eq 0) { return }
    if ($check.Stderr -notmatch '\[LAYOUT_UPGRADE_REQUIRED\]') {
        throw "候选布局检查失败，未执行升级: $($check.Stderr)`n$($check.Stdout)"
    }

    # 独立资源根避免覆盖用户已保存的 workbook-layout.updated.xlsx。
    [string] $migrationRoot = Join-Path $ReleaseRoot ('.azlw-layout-upgrade-' + [guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $migrationRoot)
    try {
        [string] $migrationResources = Join-Path $migrationRoot '.suzushiro'
        [void](New-Item -ItemType Directory -Path $migrationResources)
        [string] $migrationApplication = Join-Path $migrationRoot 'AzurLaneWorkbook.exe'
        [string] $migrationLayout = Join-Path $migrationResources 'workbook-layout.xlsx'
        Copy-Item -LiteralPath $application -Destination $migrationApplication
        Copy-Item -LiteralPath $layout -Destination $migrationLayout
        [void](Invoke-AuditCommand -FilePath $migrationApplication -ArgumentList @('layout-upgrade') `
            -WorkingDirectory $migrationRoot)
        [string] $updated = Join-Path $migrationResources 'data/workbooks/workbook-layout.updated.xlsx'
        Assert-AzlwRegularFile -Path $updated -Label '升级后的布局'
        Copy-Item -LiteralPath $updated -Destination $migrationLayout -Force
        [void](Invoke-AuditCommand -FilePath $migrationApplication -ArgumentList @('layout-check') `
            -WorkingDirectory $migrationRoot)
        Copy-Item -LiteralPath $updated -Destination $layout -Force
    } finally {
        Remove-AzlwContainedItem -Root $ReleaseRoot -Path $migrationRoot
    }
}

function Assert-AzlwReleaseProcessesStopped {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $ReleaseRoot
    )

    if (-not $IsWindows) {
        return
    }
    [string] $resolvedRoot = Get-AzlwFullPath -Path $ReleaseRoot
    [System.Collections.Generic.List[string]] $blocking = [System.Collections.Generic.List[string]]::new()
    foreach ($process in @(Get-CimInstance Win32_Process -ErrorAction Stop)) {
        [string] $name = [string]$process.Name
        [string] $executablePath = [string]$process.ExecutablePath
        if (-not [string]::IsNullOrWhiteSpace($executablePath) -and
            (Test-AzlwPathWithinRoot -Root $resolvedRoot -Path $executablePath)) {
            $blocking.Add("$name($($process.ProcessId))=$executablePath")
        } elseif ([string]::IsNullOrWhiteSpace($executablePath) -and
            $name -in @('AzurLaneWorkbook.exe', 'adb.exe')) {
            $blocking.Add("$name($($process.ProcessId))=<路径不可读取>")
        }
    }
    if ($blocking.Count -ne 0) {
        throw "旧生产目录仍有进程占用，请先正常退出程序和其 ADB: $($blocking -join '; ')"
    }
}

function Write-AzlwTransactionJournal {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Journal
    )

    [string] $temporary = "$Path.$([guid]::NewGuid().ToString('N')).tmp"
    [string] $json = ($Journal | ConvertTo-Json -Depth 8) + "`n"
    try {
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
        Move-Item -LiteralPath $temporary -Destination $Path -Force -ErrorAction Stop
    } finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
        }
    }
}

function Move-AzlwRejectedRelease {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $TargetRoot,

        [Parameter(Mandatory)]
        [string] $TransactionId
    )

    [string] $rejectedParent = Join-Path $TargetRoot 'rejected'
    [void](New-Item -ItemType Directory -Path $rejectedParent -Force)
    [string] $rejected = Join-Path $rejectedParent ('.azlw-rejected-' + $TransactionId)
    if (Test-Path -LiteralPath $rejected) {
        throw "拒绝覆盖已有诊断目录: $rejected"
    }
    Move-AzlwProductionDirectory -Source $Path -Destination $rejected
    return $rejected
}

function Move-AzlwProductionDirectory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Source,

        [Parameter(Mandatory)]
        [string] $Destination,

        [ValidateRange(1, 20)]
        [int] $MaximumAttempts = 8
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
            throw "生产目录移动返回不确定状态，拒绝继续: $Source -> $Destination；$($failure.Message)"
        }
        if ($attempt -eq $MaximumAttempts) {
            throw "生产目录连续 $MaximumAttempts 次无法移动: $Source -> $Destination；$($failure.Message)"
        }
        Start-Sleep -Milliseconds ([Math]::Min(4000, 200 * [Math]::Pow(2, $attempt - 1)))
    }
}

function Resolve-AzlwProductionTransaction {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $ProductionPath,

        [Parameter(Mandatory)]
        [scriptblock] $ValidateRelease
    )

    [string] $production = Get-AzlwFullPath -Path $ProductionPath
    [string] $targetRoot = [System.IO.Path]::GetDirectoryName($production)
    [string] $journalPath = Join-Path $targetRoot '.azlw-production-transaction.json'
    if (-not (Test-Path -LiteralPath $journalPath -PathType Leaf)) {
        return
    }
    [pscustomobject] $journal = Get-Content -LiteralPath $journalPath -Raw -Encoding utf8 | ConvertFrom-Json
    if ($journal.schema -ne 1 -or [string]$journal.production -ne $production) {
        throw "生产事务 journal 与当前目标不匹配: $journalPath"
    }
    foreach ($path in @([string]$journal.candidate, [string]$journal.rollback)) {
        if (-not (Test-AzlwPathWithinRoot -Root $targetRoot -Path $path)) {
            throw "生产事务 journal 包含目标根以外路径: $path"
        }
    }
    [string] $phase = [string]$journal.phase
    [string] $transactionId = [string]$journal.transaction_id
    if ($transactionId -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$') {
        throw "生产事务 journal 包含无效 transaction_id: $journalPath"
    }
    [string] $rejected = Join-Path $targetRoot ('rejected/.azlw-rejected-' + $transactionId)
    if ($phase -eq 'prepared') {
        [bool] $productionExists = Test-Path -LiteralPath $production -PathType Container
        [bool] $candidateExists = Test-Path -LiteralPath ([string]$journal.candidate) -PathType Container
        [bool] $rollbackExists = Test-Path -LiteralPath ([string]$journal.rollback) -PathType Container
        # 构建清理可能已删除候选目录，旧版本的恢复只依据生产与回滚目录。
        if ([bool]$journal.had_previous) {
            if ($productionExists -and -not $rollbackExists) {
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
            if (-not $productionExists -and $rollbackExists) {
                Move-AzlwProductionDirectory -Source ([string]$journal.rollback) -Destination $production
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
        } else {
            if (-not $productionExists -and -not $rollbackExists) {
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
            if ($productionExists -and -not $candidateExists -and -not $rollbackExists) {
                & $ValidateRelease $production
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
        }
        throw "prepared 状态与目录现状不一致，请保留 journal 人工核对: $journalPath"
    }
    if ($phase -eq 'old_renamed') {
        [bool] $productionExists = Test-Path -LiteralPath $production -PathType Container
        [bool] $candidateExists = Test-Path -LiteralPath ([string]$journal.candidate) -PathType Container
        [bool] $rollbackExists = Test-Path -LiteralPath ([string]$journal.rollback) -PathType Container
        if (-not $productionExists -and $rollbackExists) {
            Move-AzlwProductionDirectory -Source ([string]$journal.rollback) -Destination $production
            Remove-Item -LiteralPath $journalPath -Force
            return
        }
        if ($productionExists -and -not $rollbackExists) {
            & $ValidateRelease $production
            Remove-Item -LiteralPath $journalPath -Force
            return
        }
        if ($productionExists -and -not $candidateExists -and $rollbackExists) {
            try {
                & $ValidateRelease $production
                Remove-Item -LiteralPath $journalPath -Force
                return
            } catch {
                [void](Move-AzlwRejectedRelease `
                    -Path $production `
                    -TargetRoot $targetRoot `
                    -TransactionId ([string]$journal.transaction_id))
                Move-AzlwProductionDirectory -Source ([string]$journal.rollback) -Destination $production
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
        }
        throw "old_renamed 状态无法确定恢复路径，请保留 journal 人工核对: $journalPath"
    }
    if ($phase -in @('new_renamed', 'verified')) {
        if (-not (Test-Path -LiteralPath $production -PathType Container)) {
            if (Test-Path -LiteralPath ([string]$journal.rollback) -PathType Container) {
                Move-AzlwProductionDirectory -Source ([string]$journal.rollback) -Destination $production
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
            if (Test-Path -LiteralPath $rejected -PathType Container) {
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
            throw "事务声明新生产目录已发布，但生产、回滚和拒绝目录均不存在: $production"
        }
        try {
            & $ValidateRelease $production
            Remove-Item -LiteralPath $journalPath -Force
            return
        } catch {
            [void](Move-AzlwRejectedRelease `
                -Path $production `
                -TargetRoot $targetRoot `
                -TransactionId ([string]$journal.transaction_id))
            if (Test-Path -LiteralPath ([string]$journal.rollback) -PathType Container) {
                Move-AzlwProductionDirectory -Source ([string]$journal.rollback) -Destination $production
            }
            Remove-Item -LiteralPath $journalPath -Force
            return
        }
    }
    if ($phase -eq 'rolled_back') {
        [bool] $productionExists = Test-Path -LiteralPath $production -PathType Container
        [bool] $rollbackExists = Test-Path -LiteralPath ([string]$journal.rollback) -PathType Container
        if ([bool]$journal.had_previous) {
            if (-not $productionExists -and $rollbackExists) {
                Move-AzlwProductionDirectory -Source ([string]$journal.rollback) -Destination $production
                $productionExists = $true
                $rollbackExists = $false
            }
            if ($productionExists -and -not $rollbackExists) {
                & $ValidateRelease $production
                Remove-Item -LiteralPath $journalPath -Force
                return
            }
        } elseif (-not $productionExists -and -not $rollbackExists -and
            (Test-Path -LiteralPath $rejected -PathType Container)) {
            Remove-Item -LiteralPath $journalPath -Force
            return
        }
        throw "rolled_back 状态与目录现状不一致，请保留 journal 人工核对: $journalPath"
    }
    throw "不支持的生产事务阶段 $phase，请保留 journal 人工核对: $journalPath"
}

function Publish-AzlwProductionRelease {
    [CmdletBinding()]
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        [string] $CandidatePath,

        [Parameter(Mandatory)]
        [string] $ProductionPath,

        [Parameter(Mandatory)]
        [scriptblock] $ValidateRelease,

        [scriptblock] $ValidatePublishedRelease,

        [scriptblock] $PrepareCopiedState,

        [switch] $SkipProcessCheck
    )

    [string] $candidate = Get-AzlwFullPath -Path $CandidatePath
    [string] $production = Get-AzlwFullPath -Path $ProductionPath
    [string] $targetRoot = [System.IO.Path]::GetDirectoryName($production)
    if ([System.IO.Path]::GetDirectoryName($candidate) -ne $targetRoot) {
        throw '候选目录必须与生产目录位于同一父目录，以便同卷无覆盖发布'
    }
    Assert-AzlwRegularDirectory -Path $candidate -Label '候选发布目录'
    if (Test-Path -LiteralPath (Join-Path $candidate 'data')) {
        throw '候选发布在迁移前不应包含 data 目录'
    }
    & $ValidateRelease $candidate

    [bool] $hadPrevious = Test-Path -LiteralPath $production -PathType Container
    if ($hadPrevious) {
        Assert-AzlwRegularDirectory -Path $production -Label '当前生产目录'
        & $ValidateRelease $production
        if (-not $SkipProcessCheck) {
            Assert-AzlwReleaseProcessesStopped -ReleaseRoot $production
        }
        Copy-AzlwMutableReleaseState -ExistingRelease $production -CandidateRelease $candidate
        if ($null -ne $PrepareCopiedState) {
            & $PrepareCopiedState $candidate
        }
        & $ValidateRelease $candidate
        if (-not $SkipProcessCheck) {
            Assert-AzlwReleaseProcessesStopped -ReleaseRoot $production
        }
    } elseif (Test-Path -LiteralPath $production) {
        throw "生产目标存在但不是普通目录: $production"
    }

    [string] $transactionId = [DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssfffZ') + '-' + [guid]::NewGuid().ToString('N')
    [string] $rollbackParent = Join-Path $targetRoot 'rollback'
    [void](New-Item -ItemType Directory -Path $rollbackParent -Force)
    [string] $rollback = Join-Path $rollbackParent ('.azlw-rollback-' + $transactionId)
    [string] $journalPath = Join-Path $targetRoot '.azlw-production-transaction.json'
    if (Test-Path -LiteralPath $journalPath) {
        throw "已有未完成生产事务: $journalPath"
    }
    $journal = [ordered]@{
        schema          = 1
        transaction_id = $transactionId
        phase           = 'prepared'
        production      = $production
        candidate       = $candidate
        rollback        = $rollback
        had_previous    = $hadPrevious
    }
    Write-AzlwTransactionJournal -Path $journalPath -Journal $journal

    [string] $rejected = ''
    [bool] $oldMoved = $false
    [bool] $newMoved = $false
    try {
        if ($hadPrevious) {
            Move-AzlwProductionDirectory -Source $production -Destination $rollback
            $oldMoved = $true
            $journal.phase = 'old_renamed'
            Write-AzlwTransactionJournal -Path $journalPath -Journal $journal
        }
        Move-AzlwProductionDirectory -Source $candidate -Destination $production
        $newMoved = $true
        $journal.phase = 'new_renamed'
        Write-AzlwTransactionJournal -Path $journalPath -Journal $journal
        & $ValidateRelease $production
        if ($null -ne $ValidatePublishedRelease) {
            & $ValidatePublishedRelease $production
        }
        $journal.phase = 'verified'
        Write-AzlwTransactionJournal -Path $journalPath -Journal $journal
        Remove-Item -LiteralPath $journalPath -Force
    } catch {
        [System.Management.Automation.ErrorRecord] $operation = $_
        try {
            if ($newMoved -and (Test-Path -LiteralPath $production -PathType Container)) {
                $rejected = Move-AzlwRejectedRelease `
                    -Path $production `
                    -TargetRoot $targetRoot `
                    -TransactionId $transactionId
            }
            if ($oldMoved -and (Test-Path -LiteralPath $rollback -PathType Container)) {
                Move-AzlwProductionDirectory -Source $rollback -Destination $production
            }
            $journal.phase = 'rolled_back'
            $journal.rejected = $rejected
            Write-AzlwTransactionJournal -Path $journalPath -Journal $journal
            if (-not [string]::IsNullOrWhiteSpace($rejected)) {
                Copy-Item -LiteralPath $journalPath -Destination (Join-Path $rejected 'transaction.json')
            }
            Remove-Item -LiteralPath $journalPath -Force
        } catch {
            throw "生产发布失败且自动恢复未完成；原错误: $($operation.Exception.Message)；恢复错误: $($_.Exception.Message)；journal: $journalPath"
        }
        [string] $recovery = if ($hadPrevious) { '旧版本已恢复' } else { '没有旧版本需要恢复' }
        throw "生产发布失败，$recovery；原因: $($operation.Exception.Message)；拒绝目录: $rejected"
    }

    return [pscustomobject]@{
        production     = $production
        rollback       = if ($hadPrevious) { $rollback } else { $null }
        transaction_id = $transactionId
    }
}

function Remove-AzlwOldRollbacks {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $ProductionPath,

        [ValidateRange(1, 20)]
        [int] $Keep = 2
    )

    [string] $targetRoot = [System.IO.Path]::GetDirectoryName((Get-AzlwFullPath -Path $ProductionPath))
    [string] $rollbackParent = Join-Path $targetRoot 'rollback'
    if (-not (Test-Path -LiteralPath $rollbackParent -PathType Container)) {
        return
    }
    [object[]] $rollbacks = @(Get-ChildItem -LiteralPath $rollbackParent -Directory -Force |
        Where-Object {
            $_.Name -match '^\.azlw-rollback-[0-9]{8}T[0-9]{9}Z-[0-9a-f]{32}$' -and
            ($_.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0
        } |
        Sort-Object Name -Descending)
    foreach ($rollback in @($rollbacks | Select-Object -Skip $Keep)) {
        Remove-AzlwContainedItem -Root $rollbackParent -Path $rollback.FullName
    }
}
