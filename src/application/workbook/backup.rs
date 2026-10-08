//! 定义执行前原工作簿备份完成后可供用例、命令行和日志消费的证据。

use serde::Serialize;

/// 一份原工作簿经过不可覆盖发布和逐字节重读后形成的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookBackupReport {
    message: &'static str,
    source_path: String,
    backup_path: String,
    backed_up_at_unix_millis: i64,
    size_bytes: u64,
    source_package_sha256: String,
    backup_package_sha256: String,
}

impl WorkbookBackupReport {
    /// 汇总源工作簿和已经重读核验的备份身份。
    pub(crate) fn new(
        source_path: String,
        backup_path: String,
        backed_up_at_unix_millis: i64,
        size_bytes: u64,
        source_package_sha256: String,
        backup_package_sha256: String,
    ) -> Self {
        Self {
            message: "原工作簿备份完成",
            source_path,
            backup_path,
            backed_up_at_unix_millis,
            size_bytes,
            source_package_sha256,
            backup_package_sha256,
        }
    }

    /// 返回面向用户的备份结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回工具根目录内的源工作簿相对路径。
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// 返回工具根目录内的不可覆盖备份相对路径。
    pub fn backup_path(&self) -> &str {
        &self.backup_path
    }

    /// 返回开始建立该备份时记录的 Unix 毫秒时间戳。
    pub const fn backed_up_at_unix_millis(&self) -> i64 {
        self.backed_up_at_unix_millis
    }

    /// 返回源工作簿和备份共同具有的包字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回固定句柄读取到的源 XLSX 包字节 SHA-256。
    pub fn source_package_sha256(&self) -> &str {
        &self.source_package_sha256
    }

    /// 返回最终备份重新打开后取得的 XLSX 包字节 SHA-256。
    pub fn backup_package_sha256(&self) -> &str {
        &self.backup_package_sha256
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::WorkbookBackupReport;

    #[test]
    fn serializes_all_audit_fields_with_stable_names() {
        let report = WorkbookBackupReport::new(
            "data/workbooks/plan.xlsx".to_owned(),
            "data/backups/workbook-backup.xlsx".to_owned(),
            1_700_000_000_123,
            42,
            "a".repeat(64),
            "a".repeat(64),
        );

        assert_eq!(
            serde_json::to_value(report).unwrap(),
            json!({
                "message": "原工作簿备份完成",
                "source_path": "data/workbooks/plan.xlsx",
                "backup_path": "data/backups/workbook-backup.xlsx",
                "backed_up_at_unix_millis": 1_700_000_000_123_i64,
                "size_bytes": 42,
                "source_package_sha256": "a".repeat(64),
                "backup_package_sha256": "a".repeat(64),
            })
        );
    }
}
