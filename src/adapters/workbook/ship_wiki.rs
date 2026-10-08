//! 由舰船显示名派生碧蓝航线 Wiki 超链接，并识别允许写入工作簿的关系目标。

use super::package::PackageRelationship;

const WIKI_PAGE_PREFIX: &str = "https://wiki.biligame.com/blhx/";
const HYPERLINK_RELATIONSHIP_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
const MAX_EXCEL_URL_CHARS: usize = 2_080;

/// 将舰船名称编码为图鉴页地址；空名称或含控制字符时不生成链接。
pub(crate) fn ship_wiki_url(name: &str) -> Option<String> {
    let title = wiki_title(name)?;
    let url = format!("{WIKI_PAGE_PREFIX}{title}");
    if url.chars().count() > MAX_EXCEL_URL_CHARS {
        return None;
    }
    Some(url)
}

/// 把名称单元格上的图鉴地址还原成写入时的原名。不能从显示名猜测。
pub(crate) fn original_name_from_wiki_url(url: &str) -> Option<String> {
    if !is_ship_wiki_url(url) {
        return None;
    }
    let title = url.strip_prefix(WIKI_PAGE_PREFIX)?;
    let mut decoded = String::new();
    let mut characters = title.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '%' {
            let hex: String = characters.by_ref().take(2).collect();
            if hex.chars().count() != 2 {
                return None;
            }
            match u8::from_str_radix(&hex, 16).ok() {
                Some(b'/') => decoded.push('/'),
                Some(b'?') => decoded.push('?'),
                Some(b'#') => decoded.push('#'),
                Some(b'\\') => decoded.push('\\'),
                Some(b'%') => decoded.push('%'),
                _ => return None,
            }
        } else if character == '_' {
            decoded.push(' ');
        } else {
            decoded.push(character);
        }
    }
    if ship_wiki_url(&decoded).as_deref() != Some(url) {
        return None;
    }
    Some(decoded)
}

/// 仅允许指向图鉴页的 Excel 超链接关系，其它外部目标仍拒绝。
pub(crate) fn is_ship_wiki_hyperlink(relationship: &PackageRelationship) -> bool {
    relationship.external
        && relationship.relationship_type == HYPERLINK_RELATIONSHIP_TYPE
        && is_ship_wiki_url(&relationship.target)
}

fn is_ship_wiki_url(target: &str) -> bool {
    let Some(title) = target.strip_prefix(WIKI_PAGE_PREFIX) else {
        return false;
    };
    !title.is_empty()
        && !title.contains(['/', '?', '#', '\\'])
        && title.chars().all(|character| !character.is_control())
}

fn wiki_title(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    let mut title = String::new();
    for character in trimmed.chars() {
        match character {
            ' ' => title.push('_'),
            '/' => title.push_str("%2F"),
            '?' => title.push_str("%3F"),
            '#' => title.push_str("%23"),
            '\\' => title.push_str("%5C"),
            '%' => title.push_str("%25"),
            value => title.push(value),
        }
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::{
        HYPERLINK_RELATIONSHIP_TYPE, PackageRelationship, is_ship_wiki_hyperlink, ship_wiki_url,
    };

    #[test]
    fn encodes_media_wiki_titles_without_leaving_the_wiki_host() {
        assert_eq!(
            ship_wiki_url("大凤(μ兵装)").as_deref(),
            Some("https://wiki.biligame.com/blhx/大凤(μ兵装)")
        );
        assert_eq!(
            ship_wiki_url(" 测试 舰船 ").as_deref(),
            Some("https://wiki.biligame.com/blhx/测试_舰船")
        );
        assert_eq!(
            ship_wiki_url("a/b?c#d\\e%f").as_deref(),
            Some("https://wiki.biligame.com/blhx/a%2Fb%3Fc%23d%5Ce%25f")
        );
        assert_eq!(ship_wiki_url("").as_deref(), None);
        assert_eq!(ship_wiki_url(" \t ").as_deref(), None);
        assert_eq!(ship_wiki_url("换行\n名称").as_deref(), None);
        let encoded = ship_wiki_url("a/b?c#d\\e%f").unwrap();
        assert_eq!(
            super::original_name_from_wiki_url(&encoded).as_deref(),
            Some("a/b?c#d\\e%f")
        );
        assert_eq!(
            super::original_name_from_wiki_url("https://wiki.biligame.com/blhx/测试_舰船")
                .as_deref(),
            Some("测试 舰船")
        );
        assert_eq!(
            super::original_name_from_wiki_url("https://example.invalid/测试").as_deref(),
            None
        );
    }

    #[test]
    fn accepts_only_wiki_hyperlink_relationships() {
        assert!(is_ship_wiki_hyperlink(&relationship(
            HYPERLINK_RELATIONSHIP_TYPE,
            "https://wiki.biligame.com/blhx/测试舰船",
            true,
        )));
        assert!(!is_ship_wiki_hyperlink(&relationship(
            HYPERLINK_RELATIONSHIP_TYPE,
            "https://example.invalid/layout.xlsx",
            true,
        )));
        assert!(!is_ship_wiki_hyperlink(&relationship(
            HYPERLINK_RELATIONSHIP_TYPE,
            "https://wiki.biligame.com/blhx/测试/其它",
            true,
        )));
        assert!(!is_ship_wiki_hyperlink(&relationship(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject",
            "https://wiki.biligame.com/blhx/测试舰船",
            true,
        )));
        assert!(!is_ship_wiki_hyperlink(&relationship(
            HYPERLINK_RELATIONSHIP_TYPE,
            "https://wiki.biligame.com/blhx/测试舰船",
            false,
        )));
    }

    fn relationship(relationship_type: &str, target: &str, external: bool) -> PackageRelationship {
        PackageRelationship {
            id: "rId1".to_owned(),
            relationship_type: relationship_type.to_owned(),
            target: target.to_owned(),
            external,
        }
    }
}
