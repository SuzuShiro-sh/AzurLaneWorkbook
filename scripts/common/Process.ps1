# 为构建验证和设备测试提供进程输出捕获、路径解析与 UTF-8 写入能力。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-AuditFile {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Label
    )

    [System.IO.FileSystemInfo] $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item -isnot [System.IO.FileInfo]) {
        throw "$Label 必须是普通文件: $Path"
    }
    return $item.FullName
}

function Resolve-AuditDirectory {
    [CmdletBinding()]
    [OutputType([string])]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Label
    )

    [System.IO.FileSystemInfo] $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item -isnot [System.IO.DirectoryInfo]) {
        throw "$Label 必须是目录: $Path"
    }
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label 不能是重解析点: $Path"
    }
    return $item.FullName.TrimEnd([System.IO.Path]::DirectorySeparatorChar)
}

function Get-AuditProcessStartInfo {
    [CmdletBinding()]
    [OutputType([System.Diagnostics.ProcessStartInfo])]
    param(
        [Parameter(Mandatory)]
        [string] $FilePath,

        [string[]] $ArgumentList = @(),

        [string] $WorkingDirectory = (Get-Location).Path,

        [hashtable] $Environment = @{}
    )

    [System.Diagnostics.ProcessStartInfo] $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    [System.Text.UTF8Encoding] $utf8 = [System.Text.UTF8Encoding]::new($false)
    $startInfo.StandardOutputEncoding = $utf8
    $startInfo.StandardErrorEncoding = $utf8
    foreach ($argument in $ArgumentList) {
        [void]$startInfo.ArgumentList.Add($argument)
    }
    foreach ($name in $Environment.Keys) {
        $startInfo.Environment[[string] $name] = [string] $Environment[$name]
    }
    return $startInfo
}

function Invoke-AuditCommand {
    [CmdletBinding()]
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory)]
        [string] $FilePath,

        [string[]] $ArgumentList = @(),

        [string] $WorkingDirectory = (Get-Location).Path,

        [hashtable] $Environment = @{},

        [ValidateRange(0, 3600)]
        [int] $TimeoutSeconds = 0,

        [switch] $AllowFailure
    )

    [System.Diagnostics.ProcessStartInfo] $startInfo = Get-AuditProcessStartInfo `
        -FilePath $FilePath `
        -ArgumentList $ArgumentList `
        -WorkingDirectory $WorkingDirectory `
        -Environment $Environment
    [System.Diagnostics.Process] $process = [System.Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw "操作系统没有返回进程句柄: $FilePath"
    }
    try {
        [System.Threading.Tasks.Task[string]] $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        [System.Threading.Tasks.Task[string]] $stderrTask = $process.StandardError.ReadToEndAsync()
        [bool] $completed = if ($TimeoutSeconds -eq 0) {
            $process.WaitForExit()
            $true
        } else {
            $process.WaitForExit($TimeoutSeconds * 1000)
        }
        if (-not $completed) {
            $process.Kill($true)
            $process.WaitForExit()
            throw "$FilePath 超过 $TimeoutSeconds 秒仍未退出，已经终止其进程树"
        }
        [string] $stdout = $stdoutTask.GetAwaiter().GetResult()
        [string] $stderr = $stderrTask.GetAwaiter().GetResult()
        [int] $exitCode = $process.ExitCode
        if ($exitCode -ne 0 -and -not $AllowFailure.IsPresent) {
            throw "外部命令退出码为 $exitCode`: $FilePath`nstdout:`n$stdout`nstderr:`n$stderr"
        }
        return [pscustomobject]@{
            ExitCode = $exitCode
            Stdout   = $stdout
            Stderr   = $stderr
        }
    } finally {
        $process.Dispose()
    }
}

function Write-AuditUtf8 {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Text
    )

    [System.Text.UTF8Encoding] $utf8 = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Text, $utf8)
}
