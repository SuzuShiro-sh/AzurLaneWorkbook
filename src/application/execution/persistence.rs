//! 定义配装执行历史、工作簿写回和完整执行流程的稳定应用层报告。

use serde::Serialize;

use super::super::{ExecutionReport, ExecutionReportStatus, WorkbookBackupReport};

/// 一次执行历史文件发布后的稳定证据摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionHistoryReport {
    message: &'static str,
    schema_version: u32,
    execution_schema_version: u32,
    plan_schema_version: u32,
    workbook_name: String,
    recorded_at_unix_millis: i64,
    relative_path: String,
    size_bytes: u64,
    file_sha256: String,
    target_fingerprint_sha256: String,
    plan_content_sha256: String,
    report_content_sha256: String,
    report_status: ExecutionReportStatus,
    may_have_writes: bool,
}

impl ExecutionHistoryReport {
    /// 汇总已经排他发布并重新核验的执行历史文件。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        schema_version: u32,
        execution_schema_version: u32,
        plan_schema_version: u32,
        workbook_name: String,
        recorded_at_unix_millis: i64,
        relative_path: String,
        size_bytes: u64,
        file_sha256: String,
        target_fingerprint_sha256: String,
        plan_content_sha256: String,
        report_content_sha256: String,
        report_status: ExecutionReportStatus,
        may_have_writes: bool,
    ) -> Self {
        Self {
            message: "配装执行记录已保存",
            schema_version,
            execution_schema_version,
            plan_schema_version,
            workbook_name,
            recorded_at_unix_millis,
            relative_path,
            size_bytes,
            file_sha256,
            target_fingerprint_sha256,
            plan_content_sha256,
            report_content_sha256,
            report_status,
            may_have_writes,
        }
    }

    /// 返回保存结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回执行历史 JSON 外壳版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回嵌套执行报告的契约版本。
    pub const fn execution_schema_version(&self) -> u32 {
        self.execution_schema_version
    }

    /// 返回被执行计划的契约版本。
    pub const fn plan_schema_version(&self) -> u32 {
        self.plan_schema_version
    }

    /// 返回被执行的工作簿文件名。
    pub fn workbook_name(&self) -> &str {
        &self.workbook_name
    }

    /// 返回执行结果开始发布时记录的 Unix 毫秒时间戳。
    pub const fn recorded_at_unix_millis(&self) -> i64 {
        self.recorded_at_unix_millis
    }

    /// 返回工具根目录内的历史文件相对路径。
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// 返回已发布 JSON 的 UTF-8 字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回已发布 JSON 文件字节的 SHA-256。
    pub fn file_sha256(&self) -> &str {
        &self.file_sha256
    }

    /// 返回不会暴露设备序列号或持有资产明细的目标指纹。
    pub fn target_fingerprint_sha256(&self) -> &str {
        &self.target_fingerprint_sha256
    }

    /// 返回被执行计划的内容摘要。
    pub fn plan_content_sha256(&self) -> &str {
        &self.plan_content_sha256
    }

    /// 返回完整执行报告的内容摘要。
    pub fn report_content_sha256(&self) -> &str {
        &self.report_content_sha256
    }

    /// 返回整份执行报告的聚合状态。
    pub const fn report_status(&self) -> ExecutionReportStatus {
        self.report_status
    }

    /// 返回本次执行是否存在任何已经发生或尚不能排除的写入。
    pub const fn may_have_writes(&self) -> bool {
        self.may_have_writes
    }
}

/// 执行结果工作表经过源摘要核对、包级校验和原子替换后的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionResultsWriteReport {
    message: &'static str,
    workbook_path: String,
    sheet_name: String,
    worksheet_part: String,
    table_part: String,
    row_count: usize,
    source_package_sha256: String,
    output_package_sha256: String,
    package_entry_count: usize,
    unchanged_entry_count: usize,
    changed_parts: Vec<String>,
    replacement_method: String,
    temporary_file_removed: bool,
}

impl ExecutionResultsWriteReport {
    /// 汇总定点写回范围、包级保真证据和最终替换机制。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        workbook_path: String,
        sheet_name: String,
        worksheet_part: String,
        table_part: String,
        row_count: usize,
        source_package_sha256: String,
        output_package_sha256: String,
        package_entry_count: usize,
        unchanged_entry_count: usize,
        changed_parts: Vec<String>,
        replacement_method: String,
        temporary_file_removed: bool,
    ) -> Self {
        Self {
            message: "执行结果已写回工作簿",
            workbook_path,
            sheet_name,
            worksheet_part,
            table_part,
            row_count,
            source_package_sha256,
            output_package_sha256,
            package_entry_count,
            unchanged_entry_count,
            changed_parts,
            replacement_method,
            temporary_file_removed,
        }
    }

    /// 返回写回结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回工具根目录内被原子更新的工作簿相对路径。
    pub fn workbook_path(&self) -> &str {
        &self.workbook_path
    }

    /// 返回写入的工作表显示名称。
    pub fn sheet_name(&self) -> &str {
        &self.sheet_name
    }

    /// 返回发生定点变化的 worksheet 部件。
    pub fn worksheet_part(&self) -> &str {
        &self.worksheet_part
    }

    /// 返回执行结果表格对应的 table 部件。
    pub fn table_part(&self) -> &str {
        &self.table_part
    }

    /// 返回写入执行结果工作表的数据行数。
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    /// 返回写回前经过备份身份约束的源包摘要。
    pub fn source_package_sha256(&self) -> &str {
        &self.source_package_sha256
    }

    /// 返回原子替换后工作簿包字节摘要。
    pub fn output_package_sha256(&self) -> &str {
        &self.output_package_sha256
    }

    /// 返回写回后包内的条目总数。
    pub const fn package_entry_count(&self) -> usize {
        self.package_entry_count
    }

    /// 返回逐字节保持不变的包条目数。
    pub const fn unchanged_entry_count(&self) -> usize {
        self.unchanged_entry_count
    }

    /// 返回实际发生变化且通过白名单核对的 OOXML 部件。
    pub fn changed_parts(&self) -> &[String] {
        &self.changed_parts
    }

    /// 返回当前平台采用的同目录原子替换机制。
    pub fn replacement_method(&self) -> &str {
        &self.replacement_method
    }

    /// 返回原子替换完成后临时文件是否已经移除。
    pub const fn temporary_file_removed(&self) -> bool {
        self.temporary_file_removed
    }
}

/// 一次工作簿计划从执行前备份到结果写回的完整证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookExecutionReport {
    message: &'static str,
    backup: WorkbookBackupReport,
    execution: ExecutionReport,
    history: ExecutionHistoryReport,
    execution_results: ExecutionResultsWriteReport,
}

impl WorkbookExecutionReport {
    /// 组合已经分别完成核验的备份、执行、历史和工作簿写回报告。
    pub(crate) fn new(
        backup: WorkbookBackupReport,
        execution: ExecutionReport,
        history: ExecutionHistoryReport,
        execution_results: ExecutionResultsWriteReport,
    ) -> Self {
        Self {
            message: "工作簿计划执行完成",
            backup,
            execution,
            history,
            execution_results,
        }
    }

    /// 返回完整流程结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回执行前不可覆盖备份的核验证据。
    pub const fn backup(&self) -> &WorkbookBackupReport {
        &self.backup
    }

    /// 返回游戏命令和回读闭环的完整报告。
    pub const fn execution(&self) -> &ExecutionReport {
        &self.execution
    }

    /// 返回不可覆盖执行历史的发布证据。
    pub const fn history(&self) -> &ExecutionHistoryReport {
        &self.history
    }

    /// 返回执行结果工作表原子写回的核验证据。
    pub const fn execution_results(&self) -> &ExecutionResultsWriteReport {
        &self.execution_results
    }
}
