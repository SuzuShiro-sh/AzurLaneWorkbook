# suzushiro-xlsx-toolkit

`suzushiro-xlsx-toolkit` 提供通用的 Excel（OOXML / XLSX）有界包读取、指定单元格文本编辑及同目录原子替换能力，不依赖具体业务模型。

## 模块划分

- `package`：ZIP 容器有界流式读取、解压大小限制与部件重写。
- `workbook` / `paths` / `worksheet_primitives`：工作表关系解析、内部路径与单元格坐标处理。
- `editor`：将目标单元格内容替换为纯文本并重读校验，原样保留其余未修改部件。
- `atomic`：同目录临时文件创建、权限继承与跨平台原子替换发布。
- `limits`：定义 Excel 文本、行数、坐标及时间戳的边界约束。

## 单元格编辑与原子替换

```rust,no_run
use std::path::Path;
use suzushiro_xlsx_toolkit::editor::edit_text_cell_to_new_file;

fn main() -> Result<(), suzushiro_xlsx_toolkit::XlsxError> {
    let evidence = edit_text_cell_to_new_file(
        Path::new("source.xlsx"),
        Path::new("output.xlsx"),
        "Records",
        "A1",
        "changed",
        |_| false, // 拒绝所有外部引用关系
    )?;
    assert_eq!(evidence.cell_reference, "A1");
    Ok(())
}
```

- **安全边界**：单部件解压上限 128 MiB，累计读取上限 512 MiB；自动检测并拒绝重复属性、宏代码（VBA）与未知外部数据链接。
- **并发与防覆盖**：支持通过 `edit_workbook_atomically_with_pre_publish` 在最终发布前校验源文件哈希，避免覆盖编辑期间发生的外部改动。
