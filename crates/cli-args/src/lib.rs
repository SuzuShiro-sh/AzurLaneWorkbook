//! 提供严格的成对命令行参数读取，并保留操作系统原生参数值。

use std::ffi::OsString;

/// 跳过程序名后，按“参数名、参数值”顺序消费命令行。
pub struct ArgumentCursor<I> {
    values: I,
}

impl<I> ArgumentCursor<I>
where
    I: Iterator<Item = OsString>,
{
    /// 建立游标并消费第一个程序名；空输入也会被视为没有后续参数。
    pub fn new(mut values: I) -> Self {
        let _program: Option<OsString> = values.next();
        Self { values }
    }

    /// 读取下一对参数；参数名必须是 Unicode，参数值保留操作系统原生编码。
    pub fn next_pair(&mut self, usage: &str) -> Result<Option<(String, OsString)>, String> {
        let Some(raw_name) = self.values.next() else {
            return Ok(None);
        };
        let name: String = raw_name
            .into_string()
            .map_err(|_| format!("参数名不是有效 Unicode。{usage}"))?;
        let value: OsString = self
            .values
            .next()
            .ok_or_else(|| format!("参数 {name} 缺少值。{usage}"))?;
        Ok(Some((name, value)))
    }
}

/// 只允许一个命令行选项成功赋值一次，重复时保留第一次的值。
pub fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("参数 {name} 重复"));
    }
    *slot = Some(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{ArgumentCursor, set_once};

    #[test]
    fn reads_pairs_in_order_and_preserves_native_values() {
        let mut cursor = ArgumentCursor::new(
            ["tool", "--output", "结果.xlsx", "--mode", "strict"]
                .map(OsString::from)
                .into_iter(),
        );

        assert_eq!(
            cursor.next_pair("用法").unwrap(),
            Some(("--output".to_owned(), OsString::from("结果.xlsx")))
        );
        assert_eq!(
            cursor.next_pair("用法").unwrap(),
            Some(("--mode".to_owned(), OsString::from("strict")))
        );
        assert_eq!(cursor.next_pair("用法").unwrap(), None);
    }

    #[test]
    fn reports_a_missing_value_with_the_argument_and_usage() {
        let mut cursor = ArgumentCursor::new(["tool", "--output"].map(OsString::from).into_iter());

        assert_eq!(
            cursor.next_pair("用法: tool --output FILE").unwrap_err(),
            "参数 --output 缺少值。用法: tool --output FILE"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_non_unicode_argument_name() {
        use std::os::unix::ffi::OsStringExt;

        let mut cursor = ArgumentCursor::new(
            [OsString::from("tool"), OsString::from_vec(vec![0xff])].into_iter(),
        );

        assert_eq!(
            cursor.next_pair("用法: tool").unwrap_err(),
            "参数名不是有效 Unicode。用法: tool"
        );
    }

    #[cfg(windows)]
    #[test]
    fn rejects_a_non_unicode_argument_name() {
        use std::os::windows::ffi::OsStringExt;

        let mut cursor = ArgumentCursor::new(
            [OsString::from("tool"), OsString::from_wide(&[0xd800])].into_iter(),
        );

        assert_eq!(
            cursor.next_pair("用法: tool").unwrap_err(),
            "参数名不是有效 Unicode。用法: tool"
        );
    }

    #[test]
    fn duplicate_assignment_keeps_the_first_value() {
        let mut slot = None;
        set_once(&mut slot, "first", "--mode").unwrap();

        assert_eq!(
            set_once(&mut slot, "second", "--mode").unwrap_err(),
            "参数 --mode 重复"
        );
        assert_eq!(slot, Some("first"));
    }
}
