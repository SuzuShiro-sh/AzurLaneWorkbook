//! 提取 BWiki 舰娘图鉴的信息表，不从台词、说明正文或活动名称推断获取途径。

use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Debug)]
pub(super) enum ParsedPage {
    Ready(String),
    Missing,
    Partial { summary: String, detail: String },
    Failed(PageIssue),
}

#[derive(Debug)]
pub(super) enum PageIssue {
    Message(String),
    Api { code: String, detail: String },
}

impl PageIssue {
    pub(super) fn detail(&self) -> &str {
        match self {
            Self::Message(message) => message,
            Self::Api { detail, .. } => detail,
        }
    }

    pub(super) fn api_code(&self) -> Option<&str> {
        match self {
            Self::Api { code, .. } => Some(code),
            Self::Message(_) => None,
        }
    }
}

pub(super) fn parse_response(bytes: &[u8]) -> ParsedPage {
    match response_html(bytes) {
        Ok(Some(html)) => parse_html(&html),
        Ok(None) => ParsedPage::Missing,
        Err(issue) => ParsedPage::Failed(issue),
    }
}

fn response_html(bytes: &[u8]) -> Result<Option<String>, PageIssue> {
    let response: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| PageIssue::Message(format!("BWiki JSON: {error}")))?;
    if let Some(error) = response.get("error") {
        if error["code"].as_str() == Some("missingtitle") {
            return Ok(None);
        }
        return Err(PageIssue::Api {
            code: error["code"].as_str().unwrap_or("").to_owned(),
            detail: format!("BWiki API: {error}"),
        });
    }
    let html = response
        .pointer("/parse/text/*")
        .and_then(|value| value.as_str())
        .ok_or_else(|| PageIssue::Message("BWiki 响应缺少页面正文".to_owned()))?;
    Ok(Some(html.to_owned()))
}

fn parse_html(html: &str) -> ParsedPage {
    let mut entries = Vec::new();
    let mut recognized = false;
    for row in html.split("</tr>") {
        let row = row.rsplit("<tr").next().unwrap_or(row);
        let Some((_, first)) = row.split_once("<td") else {
            continue;
        };
        let Some((_, first)) = first.split_once('>') else {
            continue;
        };
        let Some((header, rest)) = first.split_once("</td>") else {
            continue;
        };
        // 表头的悬浮说明属于解释文字，不能混入类别名。
        let header = header.split("</b>").next().unwrap_or(header);
        let label = match text(header) {
            Ok(value) => value
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '；')
                .collect::<String>(),
            Err(error) => return partial_or_failed(entries, error),
        };
        let category = match label.as_str() {
            "建造时间" => "建造",
            "普通掉落点" => "普通掉落",
            "档案掉落点" => "档案掉落",
            "活动掉落点" => "活动掉落（历史记录，开放情况以游戏为准）",
            "其他途径" => "其他途径",
            _ => continue,
        };
        recognized = true;
        if row.split('>').next().is_some_and(|opening| {
            opening.contains("display:none") || opening.contains("display: none")
        }) {
            continue;
        }
        let Some((_, cell)) = rest.split_once("<td") else {
            return partial_or_failed(entries, format!("BWiki {label}缺少内容单元格"));
        };
        let Some((_, cell)) = cell.split_once('>') else {
            return partial_or_failed(entries, format!("BWiki {label}内容格式异常"));
        };
        let Some((cell, _)) = cell.split_once("</td>") else {
            return partial_or_failed(entries, format!("BWiki {label}内容未闭合"));
        };
        let value = match text(cell) {
            Ok(value) => value,
            Err(error) => return partial_or_failed(entries, error),
        };
        if !value.is_empty() {
            let entry = format!("{category}：{value}");
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    }
    if !recognized {
        return ParsedPage::Failed(PageIssue::Message(
            "BWiki 页面未找到舰娘图鉴获取途径表".to_owned(),
        ));
    }
    match finish_summary(entries) {
        Ok(summary) => ParsedPage::Ready(summary),
        Err(message) => ParsedPage::Failed(PageIssue::Message(message)),
    }
}

fn partial_or_failed(entries: Vec<String>, detail: String) -> ParsedPage {
    if entries.is_empty() {
        return ParsedPage::Failed(PageIssue::Message(detail));
    }
    match finish_summary(entries) {
        Ok(summary) => ParsedPage::Partial { summary, detail },
        Err(message) => ParsedPage::Failed(PageIssue::Message(message)),
    }
}

fn finish_summary(entries: Vec<String>) -> Result<String, String> {
    let mut summary = if entries.is_empty() {
        "BWiki 未记录获取途径".to_owned()
    } else {
        entries.join("\n")
    };
    if summary.contains("活动") || summary.contains("限定") || summary.contains("限时") {
        summary.push_str("\n活动及限时途径以游戏当前开放情况为准");
    }
    if summary.encode_utf16().count() > 28_000 {
        return Err("BWiki 获取途径超出单元格展示上限".to_owned());
    }
    Ok(summary)
}

fn text(html: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(html);
    reader.config_mut().check_end_names = false;
    reader.config_mut().allow_unmatched_ends = true;
    let mut output = String::new();
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("BWiki 表格 HTML: {e}"))?
        {
            Event::Eof => break,
            Event::Text(value) => output.push_str(&value.decode().map_err(|e| e.to_string())?),
            Event::GeneralRef(value) => {
                if let Some(character) = value.resolve_char_ref().map_err(|e| e.to_string())? {
                    output.push(character);
                } else {
                    let entity = value.decode().map_err(|e| e.to_string())?;
                    output.push_str(
                        quick_xml::escape::resolve_html5_entity(&entity)
                            .ok_or_else(|| format!("BWiki HTML 未知实体 {entity}"))?,
                    );
                }
            }
            Event::Start(tag) | Event::Empty(tag)
                if matches!(tag.name().as_ref(), b"br" | b"li") =>
            {
                output.push('；')
            }
            _ => {}
        }
    }
    Ok(output
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches([' ', '；'])
        .to_owned())
}

#[cfg(test)]
impl ParsedPage {
    fn ready(self) -> String {
        match self {
            Self::Ready(summary) => summary,
            other => panic!("期望完整解析结果: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_historical_drops_and_ignores_tooltips_and_dialogue() {
        let html = r#"<tr><td><b><span>建造</span><span>时间</span></b></td><td><a>00:27:00（轻型池）</a></td></tr>
        <tr><td><b>普通掉落点</b></td><td><a>2-1</a>、<a>2-2</a></td></tr>
        <tr><td><b>活动<br/>掉落点</b><span>首次和复刻可能不同</span></td><td>活动A：B3<br>活动B：D3</td></tr>
        <tr><td><b>其他途径</b></td><td>兑换&amp;奖励&#x3000;港区</td></tr>
        <tr style="display:none;"><td><b>档案掉落点</b></td><td>不展示</td></tr>
        <tr><td>获取台词</td><td>我是标枪</td></tr>"#;
        assert_eq!(
            parse_html(html).ready(),
            "建造：00:27:00（轻型池）\n普通掉落：2-1、2-2\n活动掉落（历史记录，开放情况以游戏为准）：活动A：B3；活动B：D3\n其他途径：兑换&奖励 港区\n活动及限时途径以游戏当前开放情况为准"
        );
        assert!(matches!(
            parse_html("<p>普通页面</p>"),
            ParsedPage::Failed(_)
        ));
        match parse_response(b"not json") {
            ParsedPage::Failed(issue) => assert!(issue.detail().contains("BWiki JSON")),
            other => panic!("坏 JSON 应是页面失败: {other:?}"),
        }
        assert!(matches!(
            parse_response(br#"{"error":{"code":"missingtitle"}}"#),
            ParsedPage::Missing
        ));
        assert!(matches!(
            parse_response(br#"{"parse":{}}"#,),
            ParsedPage::Failed(PageIssue::Message(message)) if message == "BWiki 响应缺少页面正文"
        ));
        assert_eq!(
            parse_html("<tr><td><b>其他途径</b></td><td></td></tr>").ready(),
            "BWiki 未记录获取途径"
        );
        match parse_html(
            "<tr><td><b>建造时间</b></td><td>00:27:00</td></tr><tr><td><b>其他途径</b></td><td",
        ) {
            ParsedPage::Partial { summary, detail } => {
                assert_eq!(summary, "建造：00:27:00");
                assert!(detail.contains("其他途径"));
            }
            other => panic!("已解析字段应保留，坏字段单独标为不完整: {other:?}"),
        }
        match parse_response(br#"{"error":{"code":"ratelimited","info":"slow down"}}"#) {
            ParsedPage::Failed(PageIssue::Api { code, detail }) => {
                assert_eq!(code, "ratelimited");
                assert!(detail.contains("BWiki API"));
            }
            other => panic!("服务端限流应保留错误码: {other:?}"),
        }
    }
}
