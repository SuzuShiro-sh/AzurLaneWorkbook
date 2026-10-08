//! 在驻留实例目录内保存已验证的静态 RPC 结果，供不同宿主连接复用。

use std::path::Path;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use suzushiro_content_digest::sha256_sorted_json;

use super::super::protocol::{MAX_RESPONSE_BYTES, RpcOperation, RuntimeProtocolError};
use super::{AgentClient, RuntimeClientError};
use crate::adapters::file_snapshot::read_bounded_file_snapshot;
use crate::adapters::tool_root::ToolRoot;

const MAXIMUM_CACHE_FILE_BYTES: u64 = MAX_RESPONSE_BYTES as u64 + 1024;

fn decode_cached<R: DeserializeOwned>(bytes: &[u8]) -> Result<R, RuntimeClientError> {
    let mut document: serde_json::Value = serde_json::from_slice(bytes).map_err(cache_error)?;
    let body = document
        .get("result")
        .ok_or_else(|| cache_error("缓存缺少结果"))?;
    let digest = sha256_sorted_json(body).map_err(cache_error)?;
    if document["sha256"].as_str() != Some(digest.as_str())
        || body["complete"].as_bool() != Some(true)
    {
        return Err(cache_error("缓存内容不完整或摘要不符"));
    }
    serde_json::from_value(document["result"].take()).map_err(cache_error)
}

#[derive(Debug)]
pub(super) struct CatalogCache {
    root: ToolRoot,
    directory: std::path::PathBuf,
    pub(super) hits: u64,
    pub(super) misses: u64,
}

impl AgentClient {
    /// 缓存目录绑定已认证的 Agent 实例；动态快照不经过此入口。
    pub(crate) fn enable_catalog_cache(&mut self, root: ToolRoot, generation: u64) {
        self.catalog_cache = Some(CatalogCache {
            root,
            directory: Path::new("data/temp")
                .join(self.session_id.to_string())
                .join(format!("catalog-{generation}")),
            hits: 0,
            misses: 0,
        });
    }

    pub(crate) fn catalog_cache_counts(&self) -> (u64, u64) {
        self.catalog_cache
            .as_ref()
            .map_or((0, 0), |cache| (cache.hits, cache.misses))
    }

    pub(super) fn catalog_request<P, R, V>(
        &mut self,
        operation: RpcOperation,
        timeout_ms: u32,
        payload: P,
        validate: V,
    ) -> Result<R, RuntimeClientError>
    where
        P: Serialize,
        R: DeserializeOwned + Serialize,
        V: Fn(&R) -> Result<(), RuntimeProtocolError>,
    {
        if !self.usable {
            return Err(RuntimeClientError::SessionUnusable);
        }
        let key = sha256_sorted_json(&json!({"operation": operation, "payload": &payload}))
            .map_err(cache_error)?;
        let location = self.catalog_cache.as_ref().map(|cache| {
            (
                cache.root.clone(),
                cache.directory.join(format!("{key}.json")),
            )
        });
        if let Some((root, path)) = &location
            && root
                .as_path()
                .join(path)
                .try_exists()
                .map_err(cache_error)?
        {
            let cached: Result<R, RuntimeClientError> = (|| {
                let (bytes, _) = read_bounded_file_snapshot(root, path, MAXIMUM_CACHE_FILE_BYTES)
                    .map_err(cache_error)?
                    .into_parts();
                let result: R = decode_cached(&bytes)?;
                validate(&result)?;
                Ok(result)
            })();
            match cached {
                Ok(result) => {
                    self.catalog_cache.as_mut().expect("缓存已启用").hits += 1;
                    return Ok(result);
                }
                Err(error) => {
                    eprintln!("静态目录缓存 {} 无效，将重新读取: {error}", path.display());
                    root.remove_file_if_exists(path, None)
                        .map_err(cache_error)?;
                }
            }
        }
        let result = self.request(operation, timeout_ms, payload, &validate)?;
        if let Some(cache) = &mut self.catalog_cache {
            cache.misses += 1;
        }
        if let Some((root, path)) = location {
            let body = serde_json::to_value(&result).map_err(cache_error)?;
            // 不完整页面只能用于本次错误诊断，不能成为后续读取的缓存。
            if body.get("complete").and_then(|value| value.as_bool()) == Some(true) {
                let document = json!({"sha256": sha256_sorted_json(&body).map_err(cache_error)?, "result": body});
                let temporary = path.with_extension(format!(
                    "{}.tmp",
                    crate::adapters::device::session::SessionId::generate().map_err(cache_error)?
                ));
                suzushiro_artifact_publish::publish_new_with(
                    &root,
                    &temporary,
                    &path,
                    MAXIMUM_CACHE_FILE_BYTES,
                    |writer| serde_json::to_writer(writer, &document),
                )
                .map_err(|error| cache_error(format!("{error:?}")))?;
            }
        }
        Ok(result)
    }
}

fn cache_error(error: impl std::fmt::Display) -> RuntimeClientError {
    RuntimeProtocolError::new("catalog_cache_failed", error.to_string()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_result_requires_complete_content_and_matching_digest() {
        let body = json!({"complete": true, "source": {"module_sha256": "a"}, "records": [1, 2]});
        let mut document = json!({"sha256": sha256_sorted_json(&body).unwrap(), "result": body});
        assert_eq!(
            decode_cached::<serde_json::Value>(&serde_json::to_vec(&document).unwrap()).unwrap(),
            document["result"]
        );
        document["result"]["records"][0] = json!(9);
        assert!(
            decode_cached::<serde_json::Value>(&serde_json::to_vec(&document).unwrap()).is_err()
        );
        document["result"]["complete"] = json!(false);
        document["sha256"] = json!(sha256_sorted_json(&document["result"]).unwrap());
        assert!(
            decode_cached::<serde_json::Value>(&serde_json::to_vec(&document).unwrap()).is_err()
        );
        assert!(decode_cached::<serde_json::Value>(b"{").is_err());
    }
}
