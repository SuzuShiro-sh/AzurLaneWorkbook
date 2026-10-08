# suzushiro-cli-args

`suzushiro-cli-args` 提供严格的成对命令行参数读取：

- 跳过程序名，要求每个参数名必须携带有效参数值。
- 参数名按 Unicode 严格处理，参数值保留为系统原生 `OsString`。
- 提供防止同名参数重复覆盖的通用校验辅助函数。
