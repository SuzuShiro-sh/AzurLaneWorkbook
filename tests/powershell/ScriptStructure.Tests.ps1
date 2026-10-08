# 验证脚本语法和公共文件的独立加载，防止调用方加载顺序掩盖缺失依赖。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

[string] $repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
[string] $scriptsRoot = Join-Path $repositoryRoot 'scripts'
[object[]] $files = @(Get-ChildItem -LiteralPath $scriptsRoot -Filter '*.ps1' -File -Recurse)
[int] $libraryCount = 0
foreach ($file in $files) {
    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $file.FullName, [ref]$tokens, [ref]$parseErrors
    )
    if ($parseErrors.Count -ne 0) {
        throw "脚本语法错误: $($file.FullName)`n$($parseErrors -join "`n")"
    }
    if ($file.DirectoryName -eq $scriptsRoot) { continue }

    # 每个文件使用全新运行空间，不能借用上一文件引入的函数。
    $shell = [powershell]::Create()
    try {
        [void]$shell.AddScript({
            param([string] $Path)
            $ErrorActionPreference = 'Stop'
            . $Path
            $tokens = $null
            $parseErrors = $null
            $ast = [System.Management.Automation.Language.Parser]::ParseFile(
                $Path, [ref]$tokens, [ref]$parseErrors
            )
            $calls = $ast.FindAll({
                param($node)
                $node -is [System.Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -match '^[A-Za-z]+-(Azlw|Audit)'
            }, $true)
            foreach ($call in $calls) {
                [void](Get-Command -Name $call.GetCommandName() -CommandType Function -ErrorAction Stop)
            }
        }).AddArgument($file.FullName)
        [void]$shell.Invoke()
        if ($shell.HadErrors) {
            throw "公共脚本缺少可独立加载的依赖: $($file.FullName)`n$($shell.Streams.Error -join "`n")"
        }
    } finally {
        $shell.Dispose()
    }
    $libraryCount++
}
[ordered]@{
    parsed_scripts = $files.Count
    independently_loaded_libraries = $libraryCount
    status = 'passed'
} | ConvertTo-Json
