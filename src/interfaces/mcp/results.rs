//! 执行回执保留完整证据，默认只返回决策所需摘要；详情通过受控文件分页读取。

use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::Path,
};
use suzushiro_content_digest::{is_canonical_sha256, sha256_bytes};
use suzushiro_controlled_root::ControlledRoot;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const DIRECTORY: &str = "data/mcp-results";
const MAX_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn needs_summary(name: &str) -> bool {
    matches!(
        name,
        "equipment_actions_apply"
            | "workbook_execute"
            | "workbook_check_save"
            | "workbook_generate"
            | "acquisition_update"
    )
}

pub(super) fn snapshot_notice() -> Value {
    json!({"is_live":false,"message":"工作簿是读取时的快照，直接游戏操作不会同步已有文件。实时装备请用 equipment 查询；刷新表格请调用 workbook_generate 生成新工作簿，原文件的计划不会被覆盖。","live_tool":"equipment","refresh_tool":"workbook_generate","refresh_arguments":{"request_id":"新的唯一请求编号"}})
}

/// 同时保留业务状态和证据位置，持久化失败不得伪装成业务尚未执行。
pub(super) fn summarize(root: &Path, name: &str, mut output: Value) -> Value {
    let mut summary = compact(&output);
    match persist(root, &output) {
        Ok(reference) => {
            summary["details"] = reference;
        }
        Err(error) => {
            eprintln!("MCP {name} 完整结果保存失败: {error}");
            output["evidence_error"] = json!(error.to_string());
            output["response_mode"] = json!("full_due_to_evidence_failure");
            return output;
        }
    }
    summary["response_mode"] = json!("summary");
    summary
}

fn compact(value: &Value) -> Value {
    match value {
        Value::Array(items) => {
            let outcomes =
                !items.is_empty() && items.iter().all(|item| item.get("outcome").is_some());
            if outcomes {
                let mut counts = BTreeMap::<String, usize>::new();
                for item in items {
                    let outcome = &item["outcome"];
                    let name = outcome
                        .as_str()
                        .or_else(|| {
                            outcome
                                .as_object()
                                .and_then(|o| o.keys().next().map(String::as_str))
                        })
                        .unwrap_or("unknown");
                    *counts.entry(name.into()).or_default() += 1;
                }
                return json!({"count":items.len(),"outcomes":counts});
            }
            value.clone()
        }
        Value::Object(object) => {
            let mut result = serde_json::Map::new();
            for (key, value) in object {
                match key.as_str() {
                    "steps" if value.is_array() => {
                        let items = value.as_array().map(Vec::as_slice).unwrap_or_default();
                        let mut counts = BTreeMap::<String, usize>::new();
                        for item in items {
                            if let Some(status) = item.get("status").and_then(Value::as_str) {
                                *counts.entry(status.into()).or_default() += 1;
                            }
                        }
                        result.insert("step_count".into(), json!(items.len()));
                        if !counts.is_empty() {
                            result.insert("step_status_counts".into(), json!(counts));
                        }
                    }
                    "ship_acquisition" if value.is_object() => {
                        result.insert(
                            "ship_acquisition_count".into(),
                            json!(value.as_object().map_or(0, |v| v.len())),
                        );
                    }
                    "warnings" | "issues" => {
                        if let Some(items) = value.as_array() {
                            result.insert(format!("{key}_count"), json!(items.len()));
                            result.insert(
                                key.clone(),
                                Value::Array(items.iter().take(5).cloned().collect()),
                            );
                        } else {
                            result.insert(key.clone(), compact(value));
                        }
                    }
                    _ => {
                        result.insert(key.clone(), compact(value));
                    }
                }
            }
            Value::Object(result)
        }
        _ => value.clone(),
    }
}

fn persist(path: &Path, output: &Value) -> Result<Value> {
    let root = ControlledRoot::open(path)?;
    root.ensure_directory(Path::new(DIRECTORY))?;
    let bytes = serde_json::to_vec(output)?;
    let id = sha256_bytes(&bytes);
    let relative = Path::new(DIRECTORY).join(format!("{id}.json"));
    let _lock = root.lock_file(
        Path::new("data/mcp-results/publish.lock"),
        std::time::Duration::from_secs(2),
    )?;
    if !root.as_path().join(&relative).try_exists()? {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce)?;
        let temporary =
            Path::new(DIRECTORY).join(format!(".{id}-{}.tmp", u64::from_le_bytes(nonce)));
        suzushiro_artifact_publish::publish_new_with(
            &root,
            &temporary,
            &relative,
            MAX_BYTES,
            |writer| writer.write_all(&bytes),
        )
        .map_err(|error| io::Error::other(format!("发布 MCP 结果证据失败: {error:?}")))?;
    }
    Ok(
        json!({"result_id":id,"relative_path":relative.to_string_lossy().replace('\\',"/"),"tool":"result_get","arguments":{"result_id":id},"size_bytes":bytes.len()}),
    )
}

pub(super) fn read(
    root: &Path,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
    let id = args
        .get("result_id")
        .and_then(Value::as_str)
        .filter(|id| is_canonical_sha256(id))
        .ok_or_else(|| {
            rmcp::ErrorData::invalid_params("result_get 的 result_id 必须复制操作回执 details.result_id 的完整 64 位小写十六进制摘要；不是 task_id", None)
        })?;
    let field = args.get("field").and_then(Value::as_str);
    let integer = |key: &str, default| -> std::result::Result<usize, rmcp::ErrorData> {
        args.get(key).map_or(Ok(default), |value| {
            value
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| {
                    rmcp::ErrorData::invalid_params(format!("result_get 的 {key} 必须为非负 JSON 整数，不是字符串；offset 默认 0，limit 默认 20"), None)
                })
        })
    };
    let offset = integer("offset", 0)?;
    let limit = integer("limit", 20)?;
    if limit == 0 || limit > 100 {
        return Err(rmcp::ErrorData::invalid_params(
            "result_get 的 limit 必须介于 1 与 100；省略时默认 20",
            None,
        ));
    }
    let load = || -> Result<Value> {
        let root = ControlledRoot::open(root)?;
        let path = root.existing_file(&Path::new(DIRECTORY).join(format!("{id}.json")))?;
        let file = File::open(&path)?;
        root.ensure_open_file_matches(&file, &path)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::other("结果超过 64 MiB").into());
        }
        if sha256_bytes(&bytes) != id {
            return Err(io::Error::other("结果证据摘要不一致").into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    };
    let value = load().map_err(|e| {
        rmcp::ErrorData::internal_error(format!("result_get 读取证据失败：result_id={id}，文件={}；原因：{e}。请确认 result_id 来自同一安装目录的回执；该错误不代表原操作未执行，不要重新执行原操作来重建证据", root.join(DIRECTORY).join(format!("{id}.json")).display()), None)
    })?;
    let selected = match field {
        None => {
            if args.contains_key("offset") || args.contains_key("limit") {
                return Err(rmcp::ErrorData::invalid_params(
                    "result_get 使用 offset/limit 时必须指定数组 field；先仅传 result_id 查看 summary 和 fields，再选择实际存在的数组字段",
                    None,
                ));
            }
            return bounded_result(
                json!({"result_id":id,"summary":compact(&value),"fields":value.as_object().map(|o|o.keys().collect::<Vec<_>>()),"usage":"提供 field 点号路径读取对象；数组使用 offset/limit 分页，例如 result.execution.steps"}),
            );
        }
        Some(field) => field
            .split('.')
            .try_fold(
                &value,
                |v, key| if key.is_empty() { None } else { v.get(key) },
            )
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    format!("result_get 的 field={field} 在 result_id={id} 中不存在；省略 field 查看 summary 和 fields。路径使用对象字段名，不支持数组下标；数组请选到数组字段后用 offset/limit 分页"),
                    None,
                )
            })?,
    };
    let result = if let Some(items) = selected.as_array() {
        let end = offset.saturating_add(limit).min(items.len());
        json!({"result_id":id,"field":field,"total":items.len(),"offset":offset,"entries":items.get(offset..end).unwrap_or_default(),"next_offset":(end<items.len()).then_some(end)})
    } else {
        if offset != 0 || args.contains_key("limit") {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "result_get 的 field={} 不是数组；请删除 offset/limit 读取 value，或改选数组字段",
                    field.unwrap_or_default()
                ),
                None,
            ));
        }
        json!({"result_id":id,"field":field,"value":selected})
    };
    bounded_result(result)
}

fn bounded_result(result: Value) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
    if serde_json::to_vec(&result)
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
        .len()
        > 48 * 1024
    {
        return Err(rmcp::ErrorData::invalid_params(
            "result_get 所选结果超过 48 KiB；对象请用 field 选择更深的字段，数组请减小 limit。此错误只影响证据读取，不需要重跑原操作",
            None,
        ));
    }
    Ok(CallToolResult::structured(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_keeps_effects_resources_and_failures_without_duplicate_steps() {
        let value = json!({"status":"incomplete","result":{"plan_hash":"hash","check":{"warnings":["warning"],"plan":{"steps":vec![json!({"a":1});24],"resource_delta":{"coins":-10}}},"execution":{"steps":[{"status":"succeeded"},{"status":"unknown"}],"may_have_writes":true,"stop_reason":"unknown","final_verification_summary":"still unknown"},"cleanup":{"status":"failed","error":{"message":"cleanup"}}}});
        let summary = compact(&value);
        assert_eq!(summary["status"], "incomplete");
        assert_eq!(summary["result"]["execution"]["step_count"], 2);
        assert_eq!(
            summary["result"]["execution"]["step_status_counts"]["unknown"],
            1
        );
        assert_eq!(
            summary["result"]["check"]["plan"]["resource_delta"]["coins"],
            -10
        );
        assert_eq!(summary["result"]["cleanup"]["error"]["message"], "cleanup");
        assert!(summary["result"]["execution"].get("steps").is_none());
        assert!(
            compact(&json!([{"outcome":"cached"},{"outcome":{"failed":{"detail":"HTTP567"}}}]))["outcomes"]
                ["failed"]
                == 1
        );
    }
}
