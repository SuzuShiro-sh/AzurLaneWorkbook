# 验证依赖锁、离线内容缓存、损坏检测、路径边界和缓存包往返。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

[string] $repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
. (Join-Path $repositoryRoot 'scripts/dependencies/Install.ps1')
. (Join-Path $repositoryRoot 'scripts/dependencies/CacheBundle.ps1')

function Assert-True {
    param([bool] $Condition, [string] $Message)
    if (-not $Condition) {
        throw $Message
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

function New-FixtureLock {
    param(
        [string] $Root,
        [string] $Archive,
        [string] $InstallDirectory = 'tools/fixture/1.0.0/windows-x64'
    )

    [string] $hash = Get-AzlwFileSha256 -Path $Archive
    [long] $size = (Get-Item -LiteralPath $Archive).Length
    $lock = [ordered]@{
        schema = 1
        platform = 'windows-x64'
        dependencies = @(
            [ordered]@{
                id = 'fixture-tool'
                version = '1.0.0'
                cache_bundle = $true
                archive = [ordered]@{
                    url = 'https://fixture.invalid/fixture-tool.zip'
                    file_name = 'fixture-tool.zip'
                    size_bytes = $size
                    sha256 = $hash
                }
                install = [ordered]@{
                    directory = $InstallDirectory
                    strip_prefix = 'fixture-tool'
                    required_paths = @('bin/tool.exe')
                }
                license = [ordered]@{
                    name = 'Fixture License'
                    url = 'https://fixture.invalid/license'
                }
            }
        )
    }
    [string] $path = Join-Path $Root ('dependencies-' + [guid]::NewGuid().ToString('N') + '.lock.json')
    [System.IO.File]::WriteAllText(
        $path,
        (($lock | ConvertTo-Json -Depth 12) + "`n"),
        [System.Text.UTF8Encoding]::new($false)
    )
    return $path
}

[string] $submoduleManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot '.gitmodules') -Raw
Assert-True -Condition ($submoduleManifest -notmatch 'AndKittyInjector|KittyMemoryEx') -Message '随仓库提供的 Native 源码不应依赖外部子模块提交'
foreach ($relative in @(
    'native/third_party/AndKittyInjector/AndKittyInjector/src/Injector/KittyInjector.cpp',
    'native/third_party/AndKittyInjector/KittyMemoryEx/KittyMemoryEx/KittyMemoryMgr.cpp',
    'native/third_party/AndKittyInjector/LICENSE',
    'native/third_party/AndKittyInjector/KittyMemoryEx/LICENSE'
)) {
    Assert-True -Condition (Test-Path -LiteralPath (Join-Path $repositoryRoot $relative) -PathType Leaf) -Message "Native 依赖源码或许可证缺失: $relative"
}

[string] $testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('azlw-bootstrap-tests-' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $testRoot)
try {
    [string] $payloadRoot = Join-Path $testRoot 'payload/fixture-tool/bin'
    [void](New-Item -ItemType Directory -Path $payloadRoot -Force)
    [System.IO.File]::WriteAllText(
        (Join-Path $payloadRoot 'tool.exe'),
        'fixture',
        [System.Text.UTF8Encoding]::new($false)
    )
    [string] $fixtureArchive = Join-Path $testRoot 'fixture-tool.zip'
    Compress-Archive -LiteralPath (Join-Path $testRoot 'payload/fixture-tool') -DestinationPath $fixtureArchive
    [string] $lockPath = New-FixtureLock -Root $testRoot -Archive $fixtureArchive

    [string] $dependencyRoot = Join-Path $testRoot 'dependencies-one'
    [pscustomobject] $lock = Get-AzlwDependencyLock -Path $lockPath
    [pscustomobject] $dependency = @($lock.dependencies)[0]
    & {
        $state = [pscustomobject]@{ mode = 'reset'; attempts = 0; waits = 0 }
        function Invoke-WebRequest {
            [CmdletBinding()]
            param([string] $Uri, [string] $OutFile)
            Assert-True -Condition ($Uri -ceq $dependency.archive.url) -Message '下载没有使用锁文件中的原始地址'
            Assert-True -Condition (-not (Test-Path -LiteralPath $OutFile)) -Message '重试前没有清理下载残片'
            $state.attempts++
            if ($state.mode -in @('reset', 'eof') -and $state.attempts -gt 2) {
                Copy-Item -LiteralPath $fixtureArchive -Destination $OutFile
                return
            }
            [System.IO.File]::WriteAllText($OutFile, 'partial', [System.Text.UTF8Encoding]::new($false))
            switch ($state.mode) {
                'not-found' { throw [System.Net.Http.HttpRequestException]::new('HTTP 404', $null, [System.Net.HttpStatusCode]::NotFound) }
                'tls' { throw [System.Net.Http.HttpRequestException]::new('TLS validation failed', [System.Security.Authentication.AuthenticationException]::new('invalid certificate')) }
                'disk' { throw [System.IO.IOException]::new('disk full') }
                'disk-eof' { throw [System.IO.IOException]::new('Received an unexpected EOF or 0 bytes from the transport stream.') }
                'checksum' { return }
                default {
                    [System.IO.IOException] $failure = if ($state.mode -in @('eof', 'always-eof')) {
                        [System.IO.IOException]::new('Received an unexpected EOF or 0 bytes from the transport stream.')
                    } else {
                        [System.IO.IOException]::new('connection reset', [System.Net.Sockets.SocketException]::new(10054))
                    }
                    if ($state.mode -in @('eof', 'always-eof')) {
                        [void][System.Runtime.ExceptionServices.ExceptionDispatchInfo]::SetRemoteStackTrace(
                            $failure,
                            '   at System.Net.Security.SslStream.EnsureFullTlsFrameAsync[TIOAdapter](CancellationToken cancellationToken, Int32 estimatedSize)'
                        )
                    }
                    throw [System.AggregateException]::new($failure)
                }
            }
        }
        function Start-Sleep {
            param([int] $Milliseconds)
            $state.waits++
        }

        foreach ($mode in @('reset', 'eof')) {
            $state.mode = $mode
            $state.attempts = 0
            $state.waits = 0
            [string] $downloadRoot = Join-Path $testRoot "download-retry-$mode"
            [string] $downloaded = Get-AzlwDependencyArchive -DependencyRoot $downloadRoot -Dependency $dependency -WarningAction SilentlyContinue
            Assert-True -Condition ($state.attempts -eq 3 -and $state.waits -eq 2) -Message "连接中断后没有按上限重试并成功下载: $mode"
            Assert-True -Condition (Test-AzlwArchive -Path $downloaded -Dependency $dependency) -Message '重试成功后归档校验未通过'
            Assert-True -Condition (-not (Test-Path -LiteralPath "$downloaded.part")) -Message '成功后保留了下载残片'
            Assert-True -Condition ((Get-AzlwDependencyArchive -DependencyRoot $downloadRoot -Dependency $dependency) -ceq $downloaded -and $state.attempts -eq 3) -Message '有效缓存仍然触发了下载'
        }

        foreach ($mode in @('always-reset', 'always-eof', 'not-found', 'tls', 'disk', 'disk-eof', 'checksum')) {
            $state.mode = $mode
            $state.attempts = 0
            $state.waits = 0
            [string] $failureRoot = Join-Path $testRoot "download-$mode"
            [string] $failurePath = Get-AzlwArchivePath -DependencyRoot $failureRoot -Dependency $dependency
            [string] $pattern = if ($mode -eq 'checksum') { 'SHA-256 不匹配: fixture-tool' } else { '依赖下载失败: fixture-tool 1.0.0；https://fixture.invalid/fixture-tool.zip' }
            Assert-Throws -Operation {
                Get-AzlwDependencyArchive -DependencyRoot $failureRoot -Dependency $dependency -WarningAction SilentlyContinue
            } -Pattern $pattern
            [int] $expectedAttempts = if ($mode -in @('always-reset', 'always-eof')) { 3 } else { 1 }
            Assert-True -Condition ($state.attempts -eq $expectedAttempts -and $state.waits -eq ($expectedAttempts - 1)) -Message "下载失败重试次数不正确: $mode"
            Assert-True -Condition (-not (Test-Path -LiteralPath $failurePath) -and -not (Test-Path -LiteralPath "$failurePath.part")) -Message "下载失败后发布了无效归档或保留残片: $mode"
        }
    }
    [string] $archiveCache = Get-AzlwArchivePath -DependencyRoot $dependencyRoot -Dependency $dependency
    [void](New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($archiveCache)) -Force)
    Copy-Item -LiteralPath $fixtureArchive -Destination $archiveCache

    [object[]] $first = @(Invoke-AzlwDependencyBootstrap `
        -LockPath $lockPath `
        -DependencyRoot $dependencyRoot `
        -Offline)
    Assert-True -Condition ($first.Count -eq 1) -Message '首次离线初始化没有解析唯一依赖'
    [string] $installPath = [string]$first[0].install
    Assert-True `
        -Condition (Test-Path -LiteralPath (Join-Path $installPath 'bin/tool.exe') -PathType Leaf) `
        -Message '首次离线初始化没有发布工具文件'
    [pscustomobject] $resolvedFixture = Get-AzlwInstalledDependency `
        -Lock $lock `
        -Identifier 'fixture-tool' `
        -DependencyRoot $dependencyRoot
    Assert-True `
        -Condition ([string]$resolvedFixture.path -ceq $installPath) `
        -Message '共享依赖解析器没有返回已校验的安装目录'
    Assert-Throws `
        -Operation {
            Get-AzlwInstalledDependency `
                -Lock $lock `
                -Identifier 'missing-tool' `
                -DependencyRoot $dependencyRoot
        } `
        -Pattern '没有唯一 missing-tool 条目'

    [DateTime] $markerTime = (Get-Item -LiteralPath (Join-Path $installPath '.azlw-dependency.json')).LastWriteTimeUtc
    Start-Sleep -Milliseconds 20
    [void](Invoke-AzlwDependencyBootstrap -LockPath $lockPath -DependencyRoot $dependencyRoot -Offline)
    Assert-True `
        -Condition ((Get-Item -LiteralPath (Join-Path $installPath '.azlw-dependency.json')).LastWriteTimeUtc -eq $markerTime) `
        -Message '缓存命中时不应重新发布安装目录'

    [string] $bundlePath = Join-Path $testRoot 'dependency-cache.zip'
    Export-AzlwDependencyCacheBundle `
        -LockPath $lockPath `
        -DependencyRoot $dependencyRoot `
        -BundlePath $bundlePath
    [string] $secondRoot = Join-Path $testRoot 'dependencies-two'
    Import-AzlwDependencyCacheBundle `
        -LockPath $lockPath `
        -DependencyRoot $secondRoot `
        -BundlePath $bundlePath
    [object[]] $second = @(Invoke-AzlwDependencyBootstrap `
        -LockPath $lockPath `
        -DependencyRoot $secondRoot `
        -Offline)
    Assert-True `
        -Condition (Test-Path -LiteralPath (Join-Path ([string]$second[0].install) 'bin/tool.exe') -PathType Leaf) `
        -Message '缓存包导入后没有完成离线初始化'

    Remove-AzlwContainedItem -Root $secondRoot -Path ([string]$second[0].install)
    [System.IO.File]::WriteAllText(
        (Get-AzlwArchivePath -DependencyRoot $secondRoot -Dependency $dependency),
        'corrupted'
    )
    Assert-Throws `
        -Operation { Invoke-AzlwDependencyBootstrap -LockPath $lockPath -DependencyRoot $secondRoot -Offline } `
        -Pattern '离线模式发现损坏的依赖归档'
    Import-AzlwDependencyCacheBundle `
        -LockPath $lockPath `
        -DependencyRoot $secondRoot `
        -BundlePath $bundlePath
    [object[]] $repaired = @(Invoke-AzlwDependencyBootstrap `
        -LockPath $lockPath `
        -DependencyRoot $secondRoot `
        -Offline)
    Assert-True `
        -Condition (Test-Path -LiteralPath (Join-Path ([string]$repaired[0].install) 'bin/tool.exe') -PathType Leaf) `
        -Message '有效缓存包没有修复同名损坏归档'
    [System.IO.File]::WriteAllText(
        (Join-Path ([string]$repaired[0].install) 'bin/tool.exe'),
        'tampered',
        [System.Text.UTF8Encoding]::new($false)
    )
    Assert-Throws `
        -Operation { Invoke-AzlwDependencyBootstrap -LockPath $lockPath -DependencyRoot $secondRoot -Offline } `
        -Pattern '请使用 -Repair'
    [void](Invoke-AzlwDependencyBootstrap `
        -LockPath $lockPath `
        -DependencyRoot $secondRoot `
        -Offline `
        -Repair)
    Assert-True `
        -Condition ((Get-Content -LiteralPath (Join-Path ([string]$repaired[0].install) 'bin/tool.exe') -Raw) -eq 'fixture') `
        -Message 'Repair 没有从已校验归档恢复被篡改的工具'

    [string] $unsafeLock = New-FixtureLock `
        -Root $testRoot `
        -Archive $fixtureArchive `
        -InstallDirectory '../outside'
    Assert-Throws -Operation { Get-AzlwDependencyLock -Path $unsafeLock } -Pattern '跳转段'
    [string] $reservedLock = New-FixtureLock `
        -Root $testRoot `
        -Archive $fixtureArchive `
        -InstallDirectory 'tools/CON/fixture'
    Assert-Throws -Operation { Get-AzlwDependencyLock -Path $reservedLock } -Pattern '保留名'

    [string] $maliciousArchive = Join-Path $testRoot 'malicious.zip'
    [System.IO.FileStream] $zipStream = [System.IO.File]::OpenWrite($maliciousArchive)
    [System.IO.Compression.ZipArchive] $zip = [System.IO.Compression.ZipArchive]::new(
        $zipStream,
        [System.IO.Compression.ZipArchiveMode]::Create
    )
    try {
        [void]$zip.CreateEntry('../escape.txt')
    } finally {
        $zip.Dispose()
        $zipStream.Dispose()
    }
    [string] $maliciousLock = New-FixtureLock -Root $testRoot -Archive $maliciousArchive
    [pscustomobject] $maliciousDefinition = @((Get-AzlwDependencyLock -Path $maliciousLock).dependencies)[0]
    [string] $maliciousRoot = Join-Path $testRoot 'dependencies-malicious'
    [string] $maliciousCache = Get-AzlwArchivePath `
        -DependencyRoot $maliciousRoot `
        -Dependency $maliciousDefinition
    [void](New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($maliciousCache)) -Force)
    Copy-Item -LiteralPath $maliciousArchive -Destination $maliciousCache
    Assert-Throws `
        -Operation { Invoke-AzlwDependencyBootstrap -LockPath $maliciousLock -DependencyRoot $maliciousRoot -Offline } `
        -Pattern 'ZIP 条目'
    Assert-True `
        -Condition (-not (Test-Path -LiteralPath (Join-Path $testRoot 'escape.txt'))) `
        -Message '恶意 ZIP 在依赖根外写入了文件'

    [string] $externalRoot = Join-Path $testRoot 'external-dependencies'
    [void](New-Item -ItemType Directory -Path $externalRoot)
    [string] $junctionRoot = Join-Path $testRoot 'dependency-junction'
    [void](New-Item -ItemType Junction -Path $junctionRoot -Target $externalRoot)
    Assert-Throws `
        -Operation { Invoke-AzlwDependencyBootstrap -LockPath $lockPath -DependencyRoot $junctionRoot -Offline } `
        -Pattern '链接或重解析点'

    [string] $childJunctionRoot = Join-Path $testRoot 'dependencies-child-junction'
    [string] $externalTools = Join-Path $testRoot 'external-tools'
    [void](New-Item -ItemType Directory -Path $childJunctionRoot)
    [void](New-Item -ItemType Directory -Path $externalTools)
    [string] $externalInstall = Join-Path $externalTools 'fixture/1.0.0/windows-x64'
    [void](New-Item -ItemType Directory -Path $externalInstall -Force)
    Copy-Item `
        -Path (Join-Path ([string]$repaired[0].install) '*') `
        -Destination $externalInstall `
        -Recurse `
        -Force
    [void](New-Item -ItemType Junction -Path (Join-Path $childJunctionRoot 'tools') -Target $externalTools)
    [string] $childArchive = Get-AzlwArchivePath `
        -DependencyRoot $childJunctionRoot `
        -Dependency $dependency
    [void](New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($childArchive)) -Force)
    Copy-Item -LiteralPath $fixtureArchive -Destination $childArchive
    Assert-Throws `
        -Operation { Invoke-AzlwDependencyBootstrap -LockPath $lockPath -DependencyRoot $childJunctionRoot -Offline } `
        -Pattern '链接或重解析点'
    Assert-True `
        -Condition ((Get-Content -LiteralPath (Join-Path $externalInstall 'bin/tool.exe') -Raw) -eq 'fixture') `
        -Message '依赖子目录链接导致根外有效工具被修改'

    [pscustomobject] $projectLock = Get-AzlwDependencyLock `
        -Path (Join-Path $repositoryRoot 'build/dependencies.lock.json')
    [string[]] $redistributableIds = @($projectLock.dependencies | Where-Object {
        [bool]$_.cache_bundle
    } | ForEach-Object { [string]$_.id } | Sort-Object)
    Assert-True `
        -Condition (($redistributableIds -join ',') -ceq 'cmake,ninja') `
        -Message '项目缓存包再分发白名单必须严格保持为 cmake,ninja'

    $report = [ordered]@{
        passed = 25
        tls_eof_download_retried = $true
        tls_eof_retry_limit_enforced = $true
        eof_message_without_tls_source_not_retried = $true
        interrupted_download_retried = $true
        download_cache_hit = $true
        download_retry_limit_enforced = $true
        http_error_not_retried = $true
        tls_error_not_retried = $true
        disk_error_not_retried = $true
        download_checksum_error_not_retried = $true
        vendored_native_sources_present = $true
        cache_hit = $true
        offline_bootstrap = $true
        cache_bundle_round_trip = $true
        corrupted_archive_rejected = $true
        unsafe_path_rejected = $true
        reserved_path_rejected = $true
        malicious_zip_rejected = $true
        dependency_root_reparse_rejected = $true
        cache_bundle_repaired_archive = $true
        dependency_child_reparse_rejected = $true
        installed_tool_tamper_rejected = $true
        redistribution_allowlist_locked = $true
        installed_dependency_resolved = $true
        missing_dependency_rejected = $true
    }
    $report | ConvertTo-Json
} finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
