# suzushiro-controlled-root

`suzushiro-controlled-root` 提供严格限制在特定根目录边界内的安全文件系统操作抽象，用于防止路径遍历与恶意文件注入：

- **根边界规范化**：将目标目录路径转换为唯一标准根，所有操作仅接受纯相对路径。
- **路径边界拦截**：拒绝绝对路径、父级跳转（`..`）、符号链接、Windows 重解析点、NTFS 备用数据流（`:`）及保留设备名（如 `CON`、`NUL`）。
- **文件身份双重核验**：打开文件后，通过系统级文件元数据（Windows 下的卷序列号与 File ID，Unix 下的设备号与 inode）核对文件句柄是否指向预期路径；这不是路径操作的原子隔离，调用方仍需控制目录写入者与文件内容修改。
- **硬链接原子重命名**：`rename_new_file` 通过硬链接（Hard Link）实现新文件发布，确保在目标已存在时不发生覆盖，发布成功后再清理临时文件。
- **安全删除**：`remove_file_if_exists` 传入预期文件句柄时会比对底层文件身份，拒绝删除身份不符的文件；不传句柄仅适用于调用方独占管理的路径。

## 典型用法

```rust,no_run
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use suzushiro_controlled_root::ControlledRoot;

let root = ControlledRoot::open(Path::new("/srv/example"))?;
root.ensure_directory(Path::new("data/output"))?;

let temporary_relative = Path::new("data/output/.report.tmp");
let temporary = root.prepare_new_file(temporary_relative)?;

// 排他创建临时文件
let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;

// 核对打开的句柄身份与路径是否一致
root.ensure_open_file_matches(&file, &temporary)?;

file.write_all(b"report")?;
file.sync_all()?;

// 通过硬链接原子建立正式文件并移除临时名称
root.rename_new_file(&file, temporary_relative, Path::new("data/output/report.json"))?;
drop(file);
# Ok::<(), Box<dyn std::error::Error>>(())
```
