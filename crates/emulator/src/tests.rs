use super::registry::{LDPLAYER, MUMU};
use super::*;
#[test]
fn selection_namespace_keeps_providers_distinct() {
    assert!(split_selection("0").is_err());
    assert_eq!(split_selection("mumu12:0").unwrap(), ("mumu12", "0"));
    assert!(split_selection("mumu:0").is_err());
    assert_eq!(split_selection("ldplayer:0").unwrap(), ("ldplayer", "0"));
    assert!(split_selection("other:0").is_err());
    assert!(split_selection("ldplayer:0:1").is_err());
}
#[test]
fn root_command_quotes_a_single_remote_program() {
    let args = LDPLAYER.root_arguments("0", "printf '%s' \"a b\"; exit 7");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], "shell");
    assert!(args[1].starts_with("su -c '"));
    assert!(args[1].contains("'\"'\"'"));
    assert!(args[1].contains("exit 7"));
    let args = MUMU.root_arguments("3", "id");
    assert_eq!(&args[..4], &["sh", "-v", "3", "-c"]);
}

#[test]
fn registry_has_unique_ids_and_manager_names() {
    let mut ids = std::collections::BTreeSet::new();
    let mut names = std::collections::BTreeSet::new();
    for adapter in ADAPTERS {
        assert!(ids.insert(adapter.id()));
        for name in adapter.manager_names() {
            assert!(names.insert(name.to_ascii_lowercase()));
        }
        assert!(!adapter.launch_arguments("0").is_empty());
    }
}
