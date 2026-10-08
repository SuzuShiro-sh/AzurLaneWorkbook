# 验证生产目录首次发布、状态保留、失败回滚和中断事务恢复。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

[string] $repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
. (Join-Path $repositoryRoot 'scripts/build/Environment.ps1')
. (Join-Path $repositoryRoot 'scripts/build/Lock.ps1')
. (Join-Path $repositoryRoot 'scripts/release/Publish.ps1')

function Assert-Equal {
    param([object] $Expected, [object] $Actual, [string] $Message)
    if ($Expected -ne $Actual) {
        throw "$Message；预期=$Expected，实际=$Actual"
    }
}

function Assert-Throws {
    param([scriptblock] $Operation, [string] $Pattern)
    try {
        & $Operation
    } catch {
        if ($_.Exception.Message -notmatch $Pattern) {
            throw "异常未匹配 '$Pattern': $($_.Exception.Message)"
        }
        return
    }
    throw "操作没有按预期抛出异常: $Pattern"
}

function New-ReleaseFixture {
    param([string] $Path, [string] $Version)

    [void](New-Item -ItemType Directory -Path $Path)
    [string] $resources = Join-Path $Path '.suzushiro'
    [void](New-Item -ItemType Directory -Path $resources)
    [System.IO.File]::WriteAllText(
        (Join-Path $Path 'AzurLaneWorkbook.exe'),
        $Version,
        [System.Text.UTF8Encoding]::new($false)
    )
    foreach ($entry in @{
        'manifest.json' = "manifest-$Version"
        'settings.json' = "settings-$Version"
        'workbook-layout.xlsx' = "layout-$Version"
    }.GetEnumerator()) {
        [System.IO.File]::WriteAllText(
            (Join-Path $resources $entry.Key),
            [string]$entry.Value,
            [System.Text.UTF8Encoding]::new($false)
        )
    }
}

function Assert-FixtureRelease {
    param([string] $ReleaseRoot)

    if (-not (Test-Path -LiteralPath (Join-Path $ReleaseRoot 'AzurLaneWorkbook.exe') -PathType Leaf)) {
        throw 'fixture 缺少 AzurLaneWorkbook.exe'
    }
    foreach ($name in @('manifest.json', 'settings.json', 'workbook-layout.xlsx')) {
        if (-not (Test-Path -LiteralPath (Join-Path $ReleaseRoot ".suzushiro\$name") -PathType Leaf)) {
            throw "fixture 缺少 $name"
        }
    }
    if ((Get-Content -LiteralPath (Join-Path $ReleaseRoot '.suzushiro\manifest.json') -Raw) -eq 'manifest-bad') {
        throw 'fixture validation failed'
    }
}

[string] $releaseFlags = Get-AzlwReleaseRustFlags -RepositoryRoot $repositoryRoot
if ($releaseFlags -notmatch [regex]::Escape('target-feature=+crt-static')) {
    throw "发布 rustflags 缺少 crt-static: $releaseFlags"
}
if ($releaseFlags -notmatch [regex]::Escape('link-arg=/PDBALTPATH:%_PDB%')) {
    throw "发布 rustflags 缺少 PDBALTPATH: $releaseFlags"
}
if ($releaseFlags -notmatch [regex]::Escape('--remap-path-prefix')) {
    throw "发布 rustflags 缺少 remap-path-prefix: $releaseFlags"
}
[string] $repoPrefix = (Get-AzlwFullPath -Path $repositoryRoot).TrimEnd('\')
if ($releaseFlags -notmatch [regex]::Escape("$repoPrefix=.")) {
    throw "发布 rustflags 没有把仓库根目录映射为相对路径: $releaseFlags"
}
if ($releaseFlags -notmatch [regex]::Escape('=cargo-home')) {
    throw "发布 rustflags 没有映射 cargo 主目录: $releaseFlags"
}

[string] $testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('azlw-production-tests-' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $testRoot)
try {
    [string] $childScript = Join-Path $testRoot 'command-output.ps1'
    Write-AuditUtf8 -Path $childScript -Text '[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::Error.Write("x" * 131072); [Console]::Out.Write(''{"message":"命令完成"}'')'
    [string] $pwsh = (Get-Process -Id $PID).Path
    [pscustomobject] $output = Invoke-AuditCommand -FilePath $pwsh `
        -ArgumentList @('-NoProfile', '-File', $childScript) -TimeoutSeconds 15
    Assert-Equal -Expected 131072 -Actual $output.Stderr.Length -Message '错误输出应完整读取'
    Assert-Equal -Expected '命令完成' -Actual (($output.Stdout | ConvertFrom-Json).message) `
        -Message '命令 JSON 输出应保留 UTF-8'
    Write-AuditUtf8 -Path $childScript -Text "[Console]::Error.Write('command-failed'); exit 7"
    Assert-Throws { Invoke-AuditCommand -FilePath $pwsh `
        -ArgumentList @('-NoProfile', '-File', $childScript) -TimeoutSeconds 15 } -Pattern 'command-failed'

    [string] $sharedBuildRoot = Join-Path $testRoot 'shared-build-root'
    [string] $firstProductionRoot = Join-Path $testRoot 'first-production-root'
    [string] $secondProductionRoot = Join-Path $testRoot 'second-production-root'
    [void](New-Item -ItemType Directory -Path $sharedBuildRoot)
    [void](New-Item -ItemType Directory -Path $firstProductionRoot)
    [void](New-Item -ItemType Directory -Path $secondProductionRoot)
    [System.Collections.Generic.List[System.IO.FileStream]] $firstLocks =
        Enter-AzlwBuildLocks -TargetRoots @($sharedBuildRoot, $firstProductionRoot)
    try {
        Assert-Throws `
            -Operation {
                [System.Collections.Generic.List[System.IO.FileStream]] $unexpectedLocks =
                    Enter-AzlwBuildLocks -TargetRoots @($sharedBuildRoot, $secondProductionRoot)
                Exit-AzlwBuildLocks -Locks $unexpectedLocks
            } `
            -Pattern '另一个构建或 Native 测试'
    } finally {
        Exit-AzlwBuildLocks -Locks $firstLocks
    }
    [System.Collections.Generic.List[System.IO.FileStream]] $releasedLocks =
        Enter-AzlwBuildLocks -TargetRoots @($sharedBuildRoot, $secondProductionRoot)
    Exit-AzlwBuildLocks -Locks $releasedLocks

    [string] $production = Join-Path $testRoot 'production'
    [string] $firstCandidate = Join-Path $testRoot '.azlw-production-candidate-first'
    New-ReleaseFixture -Path $firstCandidate -Version 'one'
    [void](Publish-AzlwProductionRelease `
        -CandidatePath $firstCandidate `
        -ProductionPath $production `
        -ValidateRelease ${function:Assert-FixtureRelease} `
        -SkipProcessCheck)
    Assert-Equal -Expected 'one' -Actual (Get-Content -LiteralPath (Join-Path $production 'AzurLaneWorkbook.exe') -Raw) `
        -Message '首次发布没有建立生产目录'

    [System.IO.File]::WriteAllText(
        (Join-Path $production '.suzushiro\settings.json'),
        'settings-user',
        [System.Text.UTF8Encoding]::new($false)
    )
    [System.IO.File]::WriteAllText(
        (Join-Path $production '.suzushiro\workbook-layout.xlsx'),
        'layout-user',
        [System.Text.UTF8Encoding]::new($false)
    )
    [void](New-Item -ItemType Directory -Path (Join-Path $production '.suzushiro\data\future') -Force)
    [System.IO.File]::WriteAllText(
        (Join-Path $production '.suzushiro\data\future\sentinel.bin'),
        'persistent',
        [System.Text.UTF8Encoding]::new($false)
    )
    [string] $secondCandidate = Join-Path $testRoot '.azlw-production-candidate-second'
    New-ReleaseFixture -Path $secondCandidate -Version 'two'
    [pscustomobject] $secondPublish = Publish-AzlwProductionRelease `
        -CandidatePath $secondCandidate `
        -ProductionPath $production `
        -ValidateRelease ${function:Assert-FixtureRelease} `
        -PrepareCopiedState {
            param($root)
            Assert-Equal -Expected $secondCandidate -Actual $root -Message '状态准备必须只作用于候选目录'
            Assert-Equal -Expected 'layout-user' -Actual (Get-Content -LiteralPath (Join-Path $root '.suzushiro/workbook-layout.xlsx') -Raw) -Message '状态准备必须在旧布局复制后执行'
            [System.IO.File]::WriteAllText((Join-Path $root '.suzushiro/workbook-layout.xlsx'), 'layout-updated')
        } `
        -SkipProcessCheck
    Assert-Equal -Expected 'two' -Actual (Get-Content -LiteralPath (Join-Path $production 'AzurLaneWorkbook.exe') -Raw) `
        -Message '增量发布没有替换程序'
    Assert-Equal -Expected 'settings-user' -Actual (Get-Content -LiteralPath (Join-Path $production '.suzushiro\settings.json') -Raw) `
        -Message '增量发布没有保留用户设置'
    Assert-Equal -Expected 'layout-updated' -Actual (Get-Content -LiteralPath (Join-Path $production '.suzushiro\workbook-layout.xlsx') -Raw) `
        -Message '增量发布没有采用准备后的布局'
    Assert-Equal -Expected 'layout-user' -Actual (Get-Content -LiteralPath (Join-Path $secondPublish.rollback '.suzushiro/workbook-layout.xlsx') -Raw) `
        -Message '状态准备修改了旧布局'
    Assert-Equal -Expected 'persistent' -Actual (Get-Content -LiteralPath (Join-Path $production '.suzushiro\data\future\sentinel.bin') -Raw) `
        -Message '增量发布没有保留未知 data 状态'
    if (-not (Test-Path -LiteralPath $secondPublish.rollback -PathType Container)) {
        throw '增量发布没有保留 rollback 目录'
    }

    [string] $migrationFailure = Join-Path $testRoot '.azlw-production-candidate-migration-failure'
    New-ReleaseFixture -Path $migrationFailure -Version 'three'
    Assert-Throws -Operation {
        Publish-AzlwProductionRelease -CandidatePath $migrationFailure -ProductionPath $production `
            -ValidateRelease ${function:Assert-FixtureRelease} `
            -PrepareCopiedState { param($root) throw 'layout migration failure' } -SkipProcessCheck
    } -Pattern 'layout migration failure'
    Assert-Equal -Expected 'two' -Actual (Get-Content -LiteralPath (Join-Path $production 'AzurLaneWorkbook.exe') -Raw) -Message '升级失败改变了旧程序'
    Assert-Equal -Expected $false -Actual (Test-Path -LiteralPath (Join-Path $testRoot '.azlw-production-transaction.json')) -Message '升级失败不应启动发布事务'

    [string] $badCandidate = Join-Path $testRoot '.azlw-production-candidate-bad'
    New-ReleaseFixture -Path $badCandidate -Version 'three'
    Assert-Throws `
        -Operation {
            Publish-AzlwProductionRelease `
                -CandidatePath $badCandidate `
                -ProductionPath $production `
                -ValidateRelease ${function:Assert-FixtureRelease} `
                -ValidatePublishedRelease { param($root) throw 'post publish failure' } `
                -SkipProcessCheck
        } `
        -Pattern '旧版本已恢复'
    Assert-Equal -Expected 'two' -Actual (Get-Content -LiteralPath (Join-Path $production 'AzurLaneWorkbook.exe') -Raw) `
        -Message '发布后验证失败没有恢复旧程序'

    [string] $recoveryProduction = Join-Path $testRoot 'recovery-production'
    [string] $recoveryCandidate = Join-Path $testRoot '.azlw-production-candidate-recovery'
    [string] $recoveryRollback = Join-Path $testRoot 'rollback/.azlw-rollback-recovery'
    New-ReleaseFixture -Path $recoveryProduction -Version 'recovery-old'
    New-ReleaseFixture -Path $recoveryCandidate -Version 'recovery-new'
    [void](New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($recoveryRollback)) -Force)
    [System.IO.Directory]::Move($recoveryProduction, $recoveryRollback)
    $journal = [ordered]@{
        schema = 1
        transaction_id = 'recovery'
        phase = 'old_renamed'
        production = $recoveryProduction
        candidate = $recoveryCandidate
        rollback = $recoveryRollback
        had_previous = $true
    }
    Write-AzlwTransactionJournal `
        -Path (Join-Path $testRoot '.azlw-production-transaction.json') `
        -Journal $journal
    Resolve-AzlwProductionTransaction `
        -ProductionPath $recoveryProduction `
        -ValidateRelease ${function:Assert-FixtureRelease}
    Assert-Equal -Expected 'recovery-old' -Actual (Get-Content -LiteralPath (Join-Path $recoveryProduction 'AzurLaneWorkbook.exe') -Raw) `
        -Message '中断事务没有恢复旧生产目录'

    [string] $preparedProduction = Join-Path $testRoot 'prepared-production'
    [string] $preparedCandidate = Join-Path $testRoot '.azlw-production-candidate-prepared'
    [string] $preparedRollback = Join-Path $testRoot 'rollback/.azlw-rollback-prepared'
    New-ReleaseFixture -Path $preparedProduction -Version 'prepared-old'
    New-ReleaseFixture -Path $preparedCandidate -Version 'prepared-new'
    [void](New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($preparedRollback)) -Force)
    Move-AzlwProductionDirectory -Source $preparedProduction -Destination $preparedRollback
    $preparedJournal = [ordered]@{
        schema = 1
        transaction_id = 'prepared'
        phase = 'prepared'
        production = $preparedProduction
        candidate = $preparedCandidate
        rollback = $preparedRollback
        had_previous = $true
    }
    Write-AzlwTransactionJournal `
        -Path (Join-Path $testRoot '.azlw-production-transaction.json') `
        -Journal $preparedJournal
    Resolve-AzlwProductionTransaction `
        -ProductionPath $preparedProduction `
        -ValidateRelease ${function:Assert-FixtureRelease}
    Assert-Equal -Expected 'prepared-old' -Actual (Get-Content -LiteralPath (Join-Path $preparedProduction 'AzurLaneWorkbook.exe') -Raw) `
        -Message 'prepared 阶段移动生效后没有恢复旧生产目录'

    foreach ($state in @('old-moved', 'old-retained', 'first-publish')) {
        [string] $cleanedProduction = Join-Path $testRoot "cleaned-$state-production"
        [string] $cleanedCandidate = Join-Path $testRoot ".azlw-production-candidate-cleaned-$state"
        [string] $cleanedRollback = Join-Path $testRoot "rollback/.azlw-rollback-cleaned-$state"
        [string] $journalPath = Join-Path $testRoot '.azlw-production-transaction.json'
        [bool] $hadPrevious = $state -ne 'first-publish'
        if ($hadPrevious) {
            New-ReleaseFixture -Path $cleanedProduction -Version 'cleaned-old'
        }
        if ($state -eq 'old-moved') {
            Move-AzlwProductionDirectory -Source $cleanedProduction -Destination $cleanedRollback
        }
        New-ReleaseFixture -Path $cleanedCandidate -Version 'cleaned-new'
        Write-AzlwTransactionJournal -Path $journalPath -Journal ([ordered]@{
            schema = 1
            transaction_id = "cleaned-$state"
            phase = 'prepared'
            production = $cleanedProduction
            candidate = $cleanedCandidate
            rollback = $cleanedRollback
            had_previous = $hadPrevious
        })
        Remove-AzlwContainedItem -Root $testRoot -Path $cleanedCandidate
        Resolve-AzlwProductionTransaction -ProductionPath $cleanedProduction `
            -ValidateRelease ${function:Assert-FixtureRelease}
        Assert-Equal -Expected $hadPrevious -Actual (Test-Path -LiteralPath $cleanedProduction -PathType Container) `
            -Message "候选已清理时生产目录状态错误: $state"
        if ($hadPrevious) {
            Assert-Equal -Expected 'cleaned-old' -Actual (Get-Content -LiteralPath (Join-Path $cleanedProduction 'AzurLaneWorkbook.exe') -Raw) `
                -Message "候选已清理时没有保留旧生产内容: $state"
        }
        Assert-Equal -Expected $false -Actual (Test-Path -LiteralPath $cleanedRollback) `
            -Message "恢复后不应残留本次回滚目录: $state"
        Assert-Equal -Expected $false -Actual (Test-Path -LiteralPath $journalPath) `
            -Message "恢复后应清理事务记录: $state"
    }

    [string] $movedProduction = Join-Path $testRoot 'moved-production'
    [string] $movedCandidate = Join-Path $testRoot '.azlw-production-candidate-moved'
    [string] $movedRollback = Join-Path $testRoot 'rollback/.azlw-rollback-moved'
    New-ReleaseFixture -Path $movedProduction -Version 'moved-old'
    New-ReleaseFixture -Path $movedCandidate -Version 'moved-new'
    Move-AzlwProductionDirectory -Source $movedProduction -Destination $movedRollback
    Move-AzlwProductionDirectory -Source $movedCandidate -Destination $movedProduction
    $movedJournal = [ordered]@{
        schema = 1
        transaction_id = 'moved'
        phase = 'old_renamed'
        production = $movedProduction
        candidate = $movedCandidate
        rollback = $movedRollback
        had_previous = $true
    }
    Write-AzlwTransactionJournal `
        -Path (Join-Path $testRoot '.azlw-production-transaction.json') `
        -Journal $movedJournal
    Resolve-AzlwProductionTransaction `
        -ProductionPath $movedProduction `
        -ValidateRelease ${function:Assert-FixtureRelease}
    Assert-Equal -Expected 'moved-new' -Actual (Get-Content -LiteralPath (Join-Path $movedProduction 'AzurLaneWorkbook.exe') -Raw) `
        -Message 'old_renamed 阶段新目录已生效时没有保留有效新版本'

    [string] $rejectedProduction = Join-Path $testRoot 'rejected-production'
    [string] $rejectedCandidate = Join-Path $testRoot '.azlw-production-candidate-rejected'
    [string] $rejectedRollback = Join-Path $testRoot 'rollback/.azlw-rollback-rejected'
    [string] $rejectedPath = Join-Path $testRoot 'rejected/.azlw-rejected-rejected'
    New-ReleaseFixture -Path $rejectedProduction -Version 'rejected-old'
    New-ReleaseFixture -Path $rejectedCandidate -Version 'rejected-new'
    Move-AzlwProductionDirectory -Source $rejectedProduction -Destination $rejectedRollback
    Move-AzlwProductionDirectory -Source $rejectedCandidate -Destination $rejectedPath
    $rejectedJournal = [ordered]@{
        schema = 1
        transaction_id = 'rejected'
        phase = 'new_renamed'
        production = $rejectedProduction
        candidate = $rejectedCandidate
        rollback = $rejectedRollback
        had_previous = $true
    }
    Write-AzlwTransactionJournal `
        -Path (Join-Path $testRoot '.azlw-production-transaction.json') `
        -Journal $rejectedJournal
    Resolve-AzlwProductionTransaction `
        -ProductionPath $rejectedProduction `
        -ValidateRelease ${function:Assert-FixtureRelease}
    Assert-Equal -Expected 'rejected-old' -Actual (Get-Content -LiteralPath (Join-Path $rejectedProduction 'AzurLaneWorkbook.exe') -Raw) `
        -Message '新包已移入拒绝目录后的中断没有恢复旧生产目录'

    [string] $rolledBackProduction = Join-Path $testRoot 'rolled-back-production'
    [string] $rolledBackCandidate = Join-Path $testRoot '.azlw-production-candidate-rolled-back'
    [string] $rolledBackRollback = Join-Path $testRoot 'rollback/.azlw-rollback-rolled-back'
    [string] $rolledBackRejected = Join-Path $testRoot 'rejected/.azlw-rejected-rolled-back'
    New-ReleaseFixture -Path $rolledBackProduction -Version 'rolled-back-old'
    New-ReleaseFixture -Path $rolledBackRejected -Version 'rolled-back-new'
    $rolledBackJournal = [ordered]@{
        schema = 1
        transaction_id = 'rolled-back'
        phase = 'rolled_back'
        production = $rolledBackProduction
        candidate = $rolledBackCandidate
        rollback = $rolledBackRollback
        had_previous = $true
        rejected = $rolledBackRejected
    }
    Write-AzlwTransactionJournal `
        -Path (Join-Path $testRoot '.azlw-production-transaction.json') `
        -Journal $rolledBackJournal
    Resolve-AzlwProductionTransaction `
        -ProductionPath $rolledBackProduction `
        -ValidateRelease ${function:Assert-FixtureRelease}
    Assert-Equal -Expected 'rolled-back-old' -Actual (Get-Content -LiteralPath (Join-Path $rolledBackProduction 'AzurLaneWorkbook.exe') -Raw) `
        -Message 'rolled_back 阶段没有确认已恢复的旧生产目录'

    $report = [ordered]@{
        passed = 15
        copied_state_prepared = $true
        preparation_failure_preserved_production = $true
        concurrent_build_rejected = $true
        first_publish = $true
        mutable_state_preserved = $true
        failed_publish_rolled_back = $true
        interrupted_transaction_recovered = $true
        prepared_move_recovered = $true
        prepared_cleaned_candidate_recovered = $true
        moved_candidate_reconciled = $true
        rejected_move_recovered = $true
        rolled_back_phase_recovered = $true
    }
    $report | ConvertTo-Json
} finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
