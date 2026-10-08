//! 在受控目录中串行分配短文件名；锁保持到调用方完成发布。

use super::tool_root::ToolRoot;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) struct NumberedDirectory {
    root: ToolRoot,
    directory: PathBuf,
    _lock: File,
}

impl NumberedDirectory {
    pub(crate) fn open(root: &ToolRoot, directory: &Path) -> io::Result<Self> {
        root.ensure_directory(directory).map_err(io::Error::other)?;
        let lock_relative = directory.with_extension("sequence.lock");
        let lock = root.lock_file(&lock_relative, std::time::Duration::from_secs(5))?;
        Ok(Self {
            root: root.clone(),
            directory: directory.to_owned(),
            _lock: lock,
        })
    }

    /// 同目录的类型共用编号；调用方继续通过既有排他写入接口发布。
    pub(crate) fn next_path(
        &self,
        kind: &str,
        extension: &str,
        number_first: bool,
    ) -> io::Result<PathBuf> {
        if ![kind, extension]
            .iter()
            .all(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_lowercase()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "文件类型和扩展名必须是小写字母",
            ));
        }
        let files = self
            .root
            .list_direct_files(&self.directory)
            .map_err(io::Error::other)?;
        let maximum = files
            .iter()
            .filter_map(|(path, _)| {
                let name = path.file_name()?.to_str()?;
                if number_first {
                    numbered_name(name).map(|(number, _)| number)
                } else {
                    let (stem, _) = name.rsplit_once('.')?;
                    let (_, number) = stem.rsplit_once('-')?;
                    parse_number(number)
                }
            })
            .max()
            .unwrap_or(0);
        let number = maximum
            .checked_add(1)
            .ok_or_else(|| io::Error::other("文件编号已达到上限"))?;
        let name = if number_first {
            format!("{number:03}-{kind}.{extension}")
        } else {
            format!("{kind}-{number:03}.{extension}")
        };
        Ok(self.directory.join(name))
    }
}

pub(crate) fn numbered_name(name: &str) -> Option<(u64, &str)> {
    let (number, rest) = name.split_once('-')?;
    Some((parse_number(number)?, rest))
}

fn parse_number(text: &str) -> Option<u64> {
    if text.len() < 3 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = text.parse::<u64>().ok()?;
    (number != 0 && format!("{number:03}") == text).then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn numbers_are_canonical_and_grow_past_three_digits() {
        assert_eq!(numbered_name("001-app.log"), Some((1, "app.log")));
        assert_eq!(numbered_name("1000-exec.json"), Some((1000, "exec.json")));
        for name in ["000-app.log", "01-app.log", "0001-app.log", "-1-app.log"] {
            assert!(numbered_name(name).is_none());
        }
    }

    #[test]
    fn concurrent_publishers_share_directory_numbers_without_overwriting() {
        let base = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .unwrap();
        let path = PathBuf::from(base)
            .join("suzushiro/scratch/azlw-file-numbers")
            .join(
                suzushiro_session_core::SessionId::generate()
                    .unwrap()
                    .to_string(),
            );
        fs::create_dir_all(&path).unwrap();
        let root = ToolRoot::open(&path).unwrap();
        root.ensure_directory(Path::new("data/history")).unwrap();
        fs::write(path.join("data/history/999-check.json"), b"existing").unwrap();
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let root = root.clone();
                std::thread::spawn(move || {
                    let sequence =
                        NumberedDirectory::open(&root, Path::new("data/history")).unwrap();
                    let relative = sequence
                        .next_path(if index % 2 == 0 { "check" } else { "exec" }, "json", true)
                        .unwrap();
                    let file = root.prepare_new_file(&relative).unwrap();
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(file)
                        .unwrap();
                    numbered_name(relative.file_name().unwrap().to_str().unwrap())
                        .unwrap()
                        .0
                })
            })
            .collect();
        let mut numbers: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        numbers.sort_unstable();
        assert_eq!(numbers, (1000..1008).collect::<Vec<_>>());
        assert_eq!(
            fs::read(path.join("data/history/999-check.json")).unwrap(),
            b"existing"
        );
    }
}
