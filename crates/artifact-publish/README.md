# suzushiro-artifact-publish

`suzushiro-artifact-publish` 基于 `suzushiro-controlled-root` 提供受控根目录内“新建且不覆盖已有文件”的通用产物发布协议：

- **发布契约**：临时路径与最终路径必须同处一个相对子目录下，写入前先核对最终目标路径不存在。
- **排他与流式写入**：通过 `create_new` 排他创建临时文件，在调用方闭包流式写入时施加字节上限，并同步计算 SHA-256。
- **持久化与原子链接**：缓冲区写入完成后显式调用 `sync_all` 刷盘，通过 `ControlledRoot::rename_new_file` 的硬链接加删除临时名称完成原子发布；若并发出现目标文件，则放弃发布并报错，绝对不覆盖已有竞争者。
- **失败安全回滚**：写入、持久化或发布任一步失败时，自动比对文件句柄身份回收本次创建的临时文件，若临时文件已被外部篡改替换则保留以便事后审计。

## 典型用法

```rust,no_run
use std::io::Write;
use std::path::Path;
use suzushiro_artifact_publish::{PublishNewError, publish_new_with};
use suzushiro_controlled_root::ControlledRoot;

fn main() -> Result<(), PublishNewError<std::io::Error>> {
    let root = ControlledRoot::open(Path::new("/srv/example"))?;
    let published = publish_new_with(
        &root,
        Path::new("data/.report.tmp"),
        Path::new("data/report.txt"),
        1024, // 允许的最大字节数
        |writer| writer.write_all(b"report"),
    )?;
    assert_eq!(published.size_bytes(), 6);
    Ok(())
}
```
