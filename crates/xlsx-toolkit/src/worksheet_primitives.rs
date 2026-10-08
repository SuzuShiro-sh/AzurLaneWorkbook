//! 提供工作表坐标和 XML 限定名的无业务语义原语。

use rust_xlsxwriter::row_col_to_cell_absolute;

/// Excel 数据验证可覆盖的最后一个零基行号。
pub const MAX_VALIDATION_ROW: u32 = 1_048_575;

/// 返回从第二行延伸到 Excel 行上限的整列绝对引用范围。
pub fn column_read_only_range(column: u16) -> String {
    format!(
        "{}:{}",
        row_col_to_cell_absolute(1, column),
        row_col_to_cell_absolute(MAX_VALIDATION_ROW, column)
    )
}

/// 从 XML 限定名提取可选命名空间前缀。
pub fn namespace_prefix(name: &str) -> Option<&str> {
    name.rsplit_once(':').map(|(prefix, _)| prefix)
}

/// 使用来源元素的可选前缀建立同一命名空间下的限定名。
pub fn qualified_name(prefix: Option<&str>, local_name: &str) -> String {
    prefix.map_or_else(
        || local_name.to_owned(),
        |prefix| format!("{prefix}:{local_name}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{column_read_only_range, namespace_prefix, qualified_name};

    #[test]
    fn builds_the_complete_read_only_column_range() {
        assert_eq!(column_read_only_range(0), "$A$2:$A$1048576");
        assert_eq!(column_read_only_range(27), "$AB$2:$AB$1048576");
    }

    #[test]
    fn preserves_or_omits_the_source_namespace_prefix() {
        assert_eq!(namespace_prefix("x:c"), Some("x"));
        assert_eq!(namespace_prefix("c"), None);
        assert_eq!(qualified_name(Some("x"), "is"), "x:is");
        assert_eq!(qualified_name(None, "is"), "is");
    }
}
