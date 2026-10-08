//! 在被占用的业务队列前取消写请求，验证它不会进入应用服务。

use super::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[test]
fn tool_guidance_preserves_schemas_and_explains_action_fields() {
    let tools = registry::tools();
    assert_eq!(tools.len(), 39);
    let actions = &tools
        .iter()
        .find(|t| t.name == "equipment_actions_check")
        .unwrap()
        .input_schema["properties"]["actions"];
    for variant in actions["items"]["oneOf"].as_array().unwrap() {
        for (name, property) in variant["properties"].as_object().unwrap() {
            assert!(
                property["description"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
                "{name}: {property}"
            );
        }
    }
    let tool = tools.iter().find(|t| t.name == "workbook_execute").unwrap();
    let error =
        dispatch::validate_arguments(tool, json!({"workbook":"plan.xlsx"}).as_object().unwrap())
            .unwrap_err();
    assert!(
        error.contains("workbook_execute") && error.contains("plan_hash") && error.contains("检查"),
        "{error}"
    );
}

#[test]
fn queued_cancellation_never_starts_a_write_operation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root=std::env::temp_dir().join(format!("azlw-mcp-cancel-{}",std::process::id()));
        assert!(!root.exists());
        let gate=Arc::new(tokio::sync::Mutex::new(()));
        let guard=gate.clone().lock_owned().await;
        let server=McpServer { root:root.clone(), gate, tools:registry::tools(), tasks:Arc::new(tasks::Tasks::new(root.clone())) };
        let (client,transport)=tokio::io::duplex(65536);
        let server_task=tokio::spawn(async move { server.serve(transport).await.unwrap().waiting().await.unwrap(); });
        let (read,mut write)=tokio::io::split(client);
        let mut read=BufReader::new(read);
        let initialize=json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cancel-test","version":"1"}}});
        write.write_all(format!("{initialize}\n").as_bytes()).await.unwrap();
        let mut line=String::new();
        tokio::time::timeout(std::time::Duration::from_secs(5),read.read_line(&mut line)).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"],1);
        for message in [
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"preferences_update","arguments":{"original":{},"preferences":{}},"_meta":{"progressToken":"must-not-start"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"cancel queued write"}}),
            json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        ] { write.write_all(format!("{message}\n").as_bytes()).await.unwrap(); }
        line.clear();
        tokio::time::timeout(std::time::Duration::from_secs(5),read.read_line(&mut line)).await.unwrap().unwrap();
        let response:Value=serde_json::from_str(&line).unwrap();
        // SDK 丢弃已取消请求的响应；ping 确认取消通知已经被处理。
        assert_eq!(response["id"],3,"{response}");
        drop(guard);
        let next=json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"settings_get","arguments":{}}});
        write.write_all(format!("{next}\n").as_bytes()).await.unwrap();
        line.clear();
        tokio::time::timeout(std::time::Duration::from_secs(5),read.read_line(&mut line)).await.unwrap().unwrap();
        let response:Value=serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"],4,"取消的请求不得发出业务开始进度: {response}");
        assert!(!root.exists());
        drop(write);
        drop(read);
        tokio::time::timeout(std::time::Duration::from_secs(5),server_task).await.unwrap().unwrap();
    });
}
