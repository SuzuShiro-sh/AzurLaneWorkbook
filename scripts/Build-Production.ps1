# 增量构建 Windows 主程序和 Android Native 运行时，并安全替换生产交付目录。

[CmdletBinding()]
param(
    [string] $RepositoryRoot = (Split-Path -Parent $PSScriptRoot),

    [string] $DependencyRoot,

    [string] $ProductionPath,

    [ValidateRange(1, 20)]
    [int] $RollbackRetention = 2
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

. (Join-Path $PSScriptRoot 'dependencies/Lock.ps1')
. (Join-Path $PSScriptRoot 'build/Environment.ps1')
. (Join-Path $PSScriptRoot 'build/Lock.ps1')
. (Join-Path $PSScriptRoot 'release/Publish.ps1')

function Invoke-PackagedValidation {
    param([string] $ReleaseRoot)

    [string] $application = Join-Path $ReleaseRoot 'AzurLaneWorkbook.exe'
    Assert-AzlwRegularFile -Path $application -Label '生产程序'
    [pscustomobject] $doctor = Invoke-PackagedJsonCommand `
        -FilePath $application `
        -Argument 'doctor'
    if ([string]$doctor.status -ne 'offline_ready' -or
        [string]$doctor.checks.release.status -ne 'ready') {
        throw "doctor 没有达到 offline_ready: $($doctor | ConvertTo-Json -Depth 8 -Compress)"
    }
}

function Invoke-PackagedJsonCommand {
    [CmdletBinding()]
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        [string] $FilePath,

        [Parameter(Mandatory)]
        [string] $Argument
    )

    [pscustomobject] $result = Invoke-AuditCommand `
        -FilePath $FilePath `
        -ArgumentList @($Argument) `
        -WorkingDirectory ([System.IO.Path]::GetTempPath())
    try {
        return $result.Stdout | ConvertFrom-Json -Depth 32
    } catch {
        throw "$FilePath $Argument 没有返回有效 JSON: $($result.Stdout)"
    }
}

[string] $repository = Get-AzlwFullPath -Path $RepositoryRoot
[string] $lockPath = Join-Path $repository 'build/dependencies.lock.json'
if ([string]::IsNullOrWhiteSpace($DependencyRoot)) {
    $DependencyRoot = Join-Path $repository '.dependencies'
}
if ([string]::IsNullOrWhiteSpace($ProductionPath)) {
    $ProductionPath = Join-Path $repository 'target/production'
}
[string] $dependencies = Get-AzlwFullPath -Path $DependencyRoot
[string] $production = Get-AzlwFullPath -Path $ProductionPath
[string] $targetRoot = [System.IO.Path]::GetDirectoryName($production)
[string] $repositoryTargetRoot = Join-Path $repository 'target'
[void](New-Item -ItemType Directory -Path $targetRoot -Force)
[void](New-Item -ItemType Directory -Path $repositoryTargetRoot -Force)
[System.Collections.Generic.List[System.IO.FileStream]] $buildLocks = Enter-AzlwBuildLocks `
    -TargetRoots @($repositoryTargetRoot, $targetRoot)
try {

    Resolve-AzlwProductionTransaction `
        -ProductionPath $production `
        -ValidateRelease ${function:Invoke-PackagedValidation}

    [pscustomobject] $lock = Get-AzlwDependencyLock -Path $lockPath
    [pscustomobject] $cmake = Get-AzlwInstalledDependency -Lock $lock -Identifier 'cmake' -DependencyRoot $dependencies
    [pscustomobject] $ninja = Get-AzlwInstalledDependency -Lock $lock -Identifier 'ninja' -DependencyRoot $dependencies
    [pscustomobject] $ndk = Get-AzlwInstalledDependency -Lock $lock -Identifier 'android-ndk' -DependencyRoot $dependencies
    [pscustomobject] $platformTools = Get-AzlwInstalledDependency -Lock $lock -Identifier 'platform-tools' -DependencyRoot $dependencies

    [string] $visualStudio = Enter-AzlwVisualStudioEnvironment
    [string] $cargo = (Get-Command cargo -ErrorAction Stop | Select-Object -First 1).Source
    [string] $cmakeExe = Join-Path $cmake.path 'bin/cmake.exe'
    [string] $ninjaExe = Join-Path $ninja.path 'ninja.exe'
    [string] $toolchain = Join-Path $ndk.path 'build/cmake/android.toolchain.cmake'
    [string] $nativeBuild = Join-Path $repository 'target/native-release'

    Invoke-AzlwReleaseCargo `
        -CargoPath $cargo `
        -ArgumentList @(
            'build', '--release', '--locked', '--target', 'x86_64-pc-windows-msvc',
            '--bin', 'AzurLaneWorkbook', '--bin', 'release-assemble'
        ) `
        -WorkingDirectory $repository `
        -RepositoryRoot $repository

    Invoke-AzlwExternalCommand `
        -FilePath $cmakeExe `
        -ArgumentList @(
            '-S', (Join-Path $repository 'native'),
            '-B', $nativeBuild,
            '-G', 'Ninja',
            "-DCMAKE_MAKE_PROGRAM=$ninjaExe",
            "-DCMAKE_TOOLCHAIN_FILE=$toolchain",
            '-DANDROID_ABI=x86_64',
            '-DANDROID_PLATFORM=android-21',
            '-DCMAKE_BUILD_TYPE=Release',
            '-DAZLW_BUILD_TESTS=OFF'
        ) `
        -WorkingDirectory $repository
    Invoke-AzlwExternalCommand `
        -FilePath $cmakeExe `
        -ArgumentList @(
            '--build', $nativeBuild,
            '--target', 'azlw-agent-x86_64', 'azlw-loader-x86_64',
            '--parallel'
        ) `
        -WorkingDirectory $repository

    [string] $transactionId = [guid]::NewGuid().ToString('N')
    [string] $runtimeRoot = Join-Path $targetRoot ('.azlw-runtime-' + $transactionId)
    [string] $candidate = Join-Path $targetRoot ('.azlw-production-candidate-' + $transactionId)
    [void](New-Item -ItemType Directory -Path (Join-Path $runtimeRoot 'runtime/adb') -Force)
    [void](New-Item -ItemType Directory -Path (Join-Path $runtimeRoot 'runtime/inject') -Force)
    [void](New-Item -ItemType Directory -Path (Join-Path $runtimeRoot 'runtime/resources/profiles') -Force)
    try {
        foreach ($name in @('adb.exe', 'AdbWinApi.dll', 'NOTICE.txt', 'source.properties')) {
            Copy-Item `
                -LiteralPath (Join-Path $platformTools.path $name) `
                -Destination (Join-Path $runtimeRoot "runtime/adb/$name") `
                -ErrorAction Stop
        }
        Copy-Item `
            -LiteralPath (Join-Path $nativeBuild 'artifacts/azlw-loader-x86_64') `
            -Destination (Join-Path $runtimeRoot 'runtime/inject/azlw-loader-x86_64') `
            -ErrorAction Stop
        Copy-Item `
            -LiteralPath (Join-Path $nativeBuild 'artifacts/libazlw-agent-x86_64.so') `
            -Destination (Join-Path $runtimeRoot 'runtime/inject/libazlw-agent-x86_64.so') `
            -ErrorAction Stop
        Copy-Item `
            -LiteralPath (Join-Path $repository 'runtime/resources/profiles/default.json') `
            -Destination (Join-Path $runtimeRoot 'runtime/resources/profiles/default.json') `
            -ErrorAction Stop

        [string] $windowsOutput = Join-Path $repository 'target/x86_64-pc-windows-msvc/release'
        [string] $assembler = Join-Path $windowsOutput 'release-assemble.exe'
        Invoke-AzlwExternalCommand `
            -FilePath $assembler `
            -ArgumentList @(
                '--output', $candidate,
                '--executable', (Join-Path $windowsOutput 'AzurLaneWorkbook.exe'),
                '--runtime-root', $runtimeRoot
            ) `
            -WorkingDirectory $repository

        [pscustomobject] $publish = Publish-AzlwProductionRelease `
            -CandidatePath $candidate `
            -ProductionPath $production `
            -ValidateRelease ${function:Invoke-PackagedValidation} `
            -PrepareCopiedState ${function:Update-AzlwCandidateLayout}
        Remove-AzlwOldRollbacks -ProductionPath $production -Keep $RollbackRetention

        $report = [ordered]@{
            production        = $publish.production
            rollback          = $publish.rollback
            transaction_id    = $publish.transaction_id
            dependency_lock   = Get-AzlwFileSha256 -Path $lockPath
            visual_studio     = $visualStudio
        }
        $report | ConvertTo-Json -Depth 6
    } finally {
        if (Test-Path -LiteralPath $candidate) {
            Remove-AzlwContainedItem -Root $targetRoot -Path $candidate
        }
        if (Test-Path -LiteralPath $runtimeRoot) {
            Remove-AzlwContainedItem -Root $targetRoot -Path $runtimeRoot
        }
    }
} finally {
    Exit-AzlwBuildLocks -Locks $buildLocks
}
