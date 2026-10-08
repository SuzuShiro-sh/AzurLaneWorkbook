# 使用锁定 Windows 工具链构建并在唯一 Android x86_64 设备上执行 Native 测试。

[CmdletBinding()]
param(
    [string] $RepositoryRoot = (Split-Path -Parent $PSScriptRoot),

    [string] $DependencyRoot,

    [Parameter(Mandatory)]
    [string] $DeviceSerial,

    [string] $BuildRelative = 'target/native-device-tests',

    [string] $EvidenceDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

. (Join-Path $PSScriptRoot 'dependencies/Lock.ps1')
. (Join-Path $PSScriptRoot 'build/Environment.ps1')
. (Join-Path $PSScriptRoot 'build/Lock.ps1')
. (Join-Path $PSScriptRoot 'common/Process.ps1')

[string] $repository = Get-AzlwFullPath -Path $RepositoryRoot
if (-not (Test-Path -LiteralPath $repository -PathType Container)) {
    throw "仓库根目录不存在: $repository"
}
Assert-AzlwPathHasNoReparsePoints -Path $repository -Label '仓库根目录'

if ([string]::IsNullOrWhiteSpace($DependencyRoot)) {
    $DependencyRoot = Join-Path $repository '.dependencies'
}
[string] $dependencies = Get-AzlwFullPath -Path $DependencyRoot

Assert-AzlwSafeRelativePath -Path $BuildRelative -Label 'Native 构建目录'
if (-not $BuildRelative.StartsWith('target/', [System.StringComparison]::Ordinal)) {
    throw "Native 构建目录必须位于仓库 target 子目录: $BuildRelative"
}

if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    [string] $scratchAlias = Join-Path $HOME 'suzushiro/scratch'
    [System.IO.DirectoryInfo] $scratchItem = Get-Item `
        -LiteralPath $scratchAlias `
        -Force `
        -ErrorAction Stop
    [string] $scratchRoot = $scratchItem.FullName
    if (($scratchItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        [System.IO.FileSystemInfo] $resolvedScratch = $scratchItem.ResolveLinkTarget($true)
        if ($null -eq $resolvedScratch -or $resolvedScratch -isnot [System.IO.DirectoryInfo]) {
            throw "scratch 重解析点没有解析到本地目录: $scratchAlias"
        }
        $scratchRoot = $resolvedScratch.FullName
    }
    [string] $runName = [DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssZ') + '-' +
        [guid]::NewGuid().ToString('N')
    $EvidenceDirectory = Join-Path $scratchRoot "azur-lane-workbook-native-tests/$runName"
}
[string] $evidence = Get-AzlwFullPath -Path $EvidenceDirectory
if ($evidence -ieq $repository -or (Test-AzlwPathWithinRoot -Root $repository -Path $evidence)) {
    throw "Native 测试证据目录必须位于仓库之外: $evidence"
}
if (Test-Path -LiteralPath $evidence) {
    throw "Native 测试证据目录必须是尚不存在的新目录: $evidence"
}
[string] $evidenceParent = [System.IO.Path]::GetDirectoryName($evidence)
Assert-AzlwPathHasNoReparsePoints -Path $evidenceParent -Label 'Native 测试证据父目录'
[void](New-Item -ItemType Directory -Path $evidence -ErrorAction Stop)
$evidence = Resolve-AuditDirectory -Path $evidence -Label 'Native 测试证据目录'

[string] $lockPath = Join-Path $repository 'build/dependencies.lock.json'
[pscustomobject] $lock = Get-AzlwDependencyLock -Path $lockPath
[pscustomobject] $cmake = Get-AzlwInstalledDependency `
    -Lock $lock -Identifier 'cmake' -DependencyRoot $dependencies
[pscustomobject] $ninja = Get-AzlwInstalledDependency `
    -Lock $lock -Identifier 'ninja' -DependencyRoot $dependencies
[pscustomobject] $ndk = Get-AzlwInstalledDependency `
    -Lock $lock -Identifier 'android-ndk' -DependencyRoot $dependencies
[pscustomobject] $platformTools = Get-AzlwInstalledDependency `
    -Lock $lock -Identifier 'platform-tools' -DependencyRoot $dependencies

[string] $cmakeExecutable = Resolve-AuditFile `
    -Path (Join-Path $cmake.path 'bin/cmake.exe') -Label '锁定 CMake'
[string] $ctestExecutable = Resolve-AuditFile `
    -Path (Join-Path $cmake.path 'bin/ctest.exe') -Label '锁定 CTest'
[string] $ninjaExecutable = Resolve-AuditFile `
    -Path (Join-Path $ninja.path 'ninja.exe') -Label '锁定 Ninja'
[string] $ndkToolchain = Resolve-AuditFile `
    -Path (Join-Path $ndk.path 'build/cmake/android.toolchain.cmake') `
    -Label '锁定 Android NDK toolchain'

[string] $adbDirectory = Join-Path $evidence 'runtime/adb'
[void](New-Item -ItemType Directory -Path $adbDirectory -Force -ErrorAction Stop)
foreach ($name in @('adb.exe', 'AdbWinApi.dll', 'NOTICE.txt', 'source.properties')) {
    Copy-Item `
        -LiteralPath (Join-Path $platformTools.path $name) `
        -Destination (Join-Path $adbDirectory $name) `
        -ErrorAction Stop
}

[string] $targetRoot = Join-Path $repository 'target'
[void](New-Item -ItemType Directory -Path $targetRoot -Force -ErrorAction Stop)
[System.Collections.Generic.List[System.IO.FileStream]] $buildLocks =
    Enter-AzlwBuildLocks -TargetRoots @($targetRoot)
try {
    [string] $visualStudio = Enter-AzlwVisualStudioEnvironment
    [string] $cargo = (Get-Command cargo -ErrorAction Stop | Select-Object -First 1).Source
    Invoke-AzlwReleaseCargo `
        -CargoPath $cargo `
        -ArgumentList @(
            'build', '--release', '--locked', '--target', 'x86_64-pc-windows-msvc',
            '--features', 'native-test-runner', '--bin', 'native-test-runner'
        ) `
        -WorkingDirectory $repository `
        -RepositoryRoot $repository
    [string] $runner = Resolve-AuditFile `
        -Path (Join-Path $repository 'target/x86_64-pc-windows-msvc/release/native-test-runner.exe') `
        -Label 'Native 测试运行器'

    $invocation = [ordered]@{
        schema = 1
        status = 'prepared'
        started_utc = [DateTimeOffset]::UtcNow.ToString('O')
        device_serial = $DeviceSerial
        build_relative = $BuildRelative
        dependency_lock_sha256 = Get-AzlwFileSha256 -Path $lockPath
        runner_sha256 = Get-AzlwFileSha256 -Path $runner
        visual_studio = $visualStudio
        dependencies = [ordered]@{
            cmake = [ordered]@{
                version = [string]$cmake.definition.version
                cmake_sha256 = Get-AzlwFileSha256 -Path $cmakeExecutable
                ctest_sha256 = Get-AzlwFileSha256 -Path $ctestExecutable
            }
            ninja = [ordered]@{
                version = [string]$ninja.definition.version
                sha256 = Get-AzlwFileSha256 -Path $ninjaExecutable
            }
            android_ndk = [ordered]@{
                version = [string]$ndk.definition.version
                toolchain_sha256 = Get-AzlwFileSha256 -Path $ndkToolchain
            }
            platform_tools = [ordered]@{
                version = [string]$platformTools.definition.version
                adb_sha256 = Get-AzlwFileSha256 -Path (Join-Path $adbDirectory 'adb.exe')
            }
        }
    }
    [string] $invocationPath = Join-Path $evidence 'native-test-invocation.json'
    Write-AuditUtf8 `
        -Path $invocationPath `
        -Text (($invocation | ConvertTo-Json -Depth 8) + "`n")

    [pscustomobject] $execution = Invoke-AuditCommand `
        -FilePath $runner `
        -ArgumentList @(
            '--repository-root', $repository,
            '--tool-root', $evidence,
            '--cmake', $cmakeExecutable,
            '--ctest', $ctestExecutable,
            '--ninja', $ninjaExecutable,
            '--ndk-toolchain', $ndkToolchain,
            '--build-relative', $BuildRelative,
            '--serial', $DeviceSerial
        ) `
        -WorkingDirectory $repository `
        -AllowFailure
    Write-AuditUtf8 -Path (Join-Path $evidence 'runner.stdout.txt') -Text $execution.Stdout
    Write-AuditUtf8 -Path (Join-Path $evidence 'runner.stderr.txt') -Text $execution.Stderr
    if ($execution.ExitCode -ne 0) {
        throw "Native 测试运行器退出码为 $($execution.ExitCode)，证据保留在 $evidence"
    }

    [System.IO.FileInfo[]] $reports = @(Get-ChildItem `
        -LiteralPath (Join-Path $evidence 'data/logs') `
        -Filter 'native-tests-*.json' `
        -File)
    if ($reports.Count -ne 1) {
        throw "Native 测试运行器必须发布唯一报告，实际为 $($reports.Count) 个"
    }
    [pscustomobject] $report = Get-Content -LiteralPath $reports[0].FullName -Raw -Encoding utf8 |
        ConvertFrom-Json -Depth 64
    $unverifiedArtifacts = @($report.artifacts | Where-Object {
        -not [bool]$_.device_sha256_verified
    })
    $invalidTests = @($report.tests | Where-Object {
        [string]$_.status -cne 'passed' -or
        [int]$_.exit_code -ne 0 -or
        $null -eq $_.stdout -or
        $null -eq $_.stderr -or
        $null -eq $_.remote_process -or
        -not [bool]$_.remote_process.stopped -or
        @('already_stopped', 'pid_reused', 'terminated', 'killed') -cnotcontains `
            [string]$_.remote_process.cleanup_action
    })
    $outputEvidenceFailures = [System.Collections.Generic.List[string]]::new()
    foreach ($test in @($report.tests)) {
        foreach ($streamName in @('stdout', 'stderr')) {
            try {
                [pscustomobject] $stream = $test.$streamName
                [string] $relative = ([string]$stream.path).Replace('\', '/')
                Assert-AzlwSafeRelativePath -Path $relative -Label "Native $streamName 证据路径"
                [string] $outputPath = Get-AzlwFullPath `
                    -Path (Join-Path $evidence $relative)
                if (-not (Test-AzlwPathWithinRoot -Root $evidence -Path $outputPath)) {
                    throw "输出证据越出 Native 证据目录: $relative"
                }
                Assert-AzlwPathHasNoReparsePoints `
                    -Path $outputPath `
                    -Label "Native $streamName 证据"
                [string] $verifiedOutput = Resolve-AuditFile `
                    -Path $outputPath `
                    -Label "Native $streamName 证据"
                [long] $actualSize = (Get-Item -LiteralPath $verifiedOutput).Length
                [string] $actualSha256 = Get-AzlwFileSha256 -Path $verifiedOutput
                if ($actualSize -ne [long]$stream.size_bytes -or
                    $actualSha256 -cne [string]$stream.sha256) {
                    throw "输出证据大小或 SHA-256 不匹配: $relative"
                }
            } catch {
                $outputEvidenceFailures.Add(
                    "$([string]$test.name)/${streamName}: $($_.Exception.Message)"
                )
            }
        }
    }
    if ([string]$report.status -cne 'passed' -or
        [string]$report.serial -cne $DeviceSerial -or
        [string]$report.android_abi -cne 'x86_64' -or
        [int]$report.android_api -lt 21 -or
        [string]$report.android_boot_id -cnotmatch `
            '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' -or
        [int]$report.configured_test_count -le 0 -or
        [int]$report.passed_test_count -ne [int]$report.configured_test_count -or
        [int]$report.failed_test_count -ne 0 -or
        [int]$report.infra_failed_test_count -ne 0 -or
        [int]$report.blocked_test_count -ne 0 -or
        @($report.tests).Count -ne [int]$report.configured_test_count -or
        @($report.tests.name | Select-Object -Unique).Count -ne @($report.tests).Count -or
        @($report.artifacts).Count -eq 0 -or
        @($report.failures).Count -ne 0 -or
        $unverifiedArtifacts.Count -ne 0 -or
        $invalidTests.Count -ne 0 -or
        $outputEvidenceFailures.Count -ne 0 -or
        -not [bool]$report.cleanup.remote_processes_stopped -or
        -not [bool]$report.cleanup.remote_directory_removed -or
        -not [bool]$report.cleanup.adb_process_stopped -or
        -not [bool]$report.cleanup.adb_port_released -or
        -not [bool]$report.cleanup.adb_temporary_root_removed) {
        throw "Native 测试报告未满足全部测试通过、设备摘要和完整清理要求: $($reports[0].FullName)"
    }

    $result = [ordered]@{
        schema = 1
        status = 'passed'
        completed_utc = [DateTimeOffset]::UtcNow.ToString('O')
        evidence_directory = $evidence
        device_serial = $DeviceSerial
        android_boot_id = [string]$report.android_boot_id
        configured_test_count = [int]$report.configured_test_count
        passed_test_count = [int]$report.passed_test_count
        failed_test_count = 0
        infra_failed_test_count = 0
        blocked_test_count = 0
        verified_artifact_count = @($report.artifacts).Count
        verified_output_count = @($report.tests).Count * 2
        report = [System.IO.Path]::GetRelativePath($evidence, $reports[0].FullName).Replace('\', '/')
        report_sha256 = Get-AzlwFileSha256 -Path $reports[0].FullName
        invocation_sha256 = Get-AzlwFileSha256 -Path $invocationPath
    }
    [string] $resultPath = Join-Path $evidence 'native-test-result.json'
    Write-AuditUtf8 -Path $resultPath -Text (($result | ConvertTo-Json -Depth 4) + "`n")
    $result | ConvertTo-Json -Depth 4 -Compress
} finally {
    Exit-AzlwBuildLocks -Locks $buildLocks
}
