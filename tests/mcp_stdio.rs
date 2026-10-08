//! 用正式可执行文件验证 MCP 握手、工具调用、设置保存及 stdout 协议隔离。

mod common;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

struct Client {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
    id: u64,
    notifications: Vec<Value>,
}
impl Client {
    fn start(executable: &std::path::Path) -> Self {
        let mut child = Command::new(executable)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                let message: Value = serde_json::from_str(&line)
                    .expect("stdout 每行必须是 JSON-RPC，不允许普通日志");
                if send.send(message).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            input,
            output,
            id: 0,
            notifications: Vec::new(),
        };
        let result = client.request("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"integration-test","version":"1"}}));
        assert_eq!(result["result"]["serverInfo"]["name"], "AzurLaneWorkbook");
        let instructions = result["result"]["instructions"].as_str().unwrap();
        assert!(
            instructions.contains("request_id")
                && instructions.contains("result_get")
                && instructions.contains("interrupted")
        );
        client.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        client
    }
    fn send(&mut self, message: Value) {
        writeln!(self.input.as_mut().unwrap(), "{message}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let message = self
                .output
                .recv_timeout(Duration::from_secs(30))
                .expect("MCP 响应超时或协议输出无效");
            if message["id"] == id {
                return message;
            }
            self.notifications.push(message);
        }
    }
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({"name":name,"arguments":arguments,"_meta":{"progressToken":"test-progress"}}),
        )
    }
    fn finish(mut self) {
        self.input.take();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "MCP 关闭 stdin 后未结束"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

#[test]
fn long_task_results_are_recoverable_and_submissions_are_idempotent() {
    let directory = common::TestDirectory::new("azlw-mcp-tests", "tasks");
    let executable = common::prepare_install(directory.path());
    std::fs::create_dir_all(common::resource_root(directory.path()).join("data/workbooks"))
        .unwrap();
    let mut client = Client::start(&executable);
    let arguments = json!({"request_id":"missing-workbook","workbook":"missing.xlsx"});
    let submission = client.call("acquisition_update", arguments.clone());
    let id = submission["result"]["structuredContent"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let lookup = json!({"task_id":id});
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let completed = loop {
        let response = client.call("task_get", lookup.clone());
        let task = response["result"]["structuredContent"].clone();
        if task["state"] == "failed" {
            break task;
        }
        assert!(std::time::Instant::now() < deadline, "{response}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(completed["result"]["status"], "failed");
    let result_id = completed["result"]["details"]["result_id"].clone();
    let details = client.call(
        "result_get",
        json!({"result_id": result_id,"field":"error.message"}),
    );
    assert_eq!(
        details["result"]["structuredContent"]["value"],
        completed["result"]["error"]["message"]
    );
    let invalid_field = client.call(
        "result_get",
        json!({"result_id":result_id,"field":"nonexistent"}),
    );
    assert_eq!(invalid_field["error"]["code"], -32602);
    let message = invalid_field["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("result_get")
            && message.contains("field=nonexistent")
            && message.contains(result_id.as_str().unwrap()),
        "{message}"
    );
    let evidence =
        serde_json::to_vec(&json!({"result":{"execution":{"steps":(0..24).collect::<Vec<_>>()}}}))
            .unwrap();
    let evidence_id = suzushiro_content_digest::sha256_bytes(&evidence);
    let evidence_path = common::resource_root(directory.path())
        .join("data/mcp-results")
        .join(format!("{evidence_id}.json"));
    std::fs::write(&evidence_path, &evidence).unwrap();
    let page = client.call(
        "result_get",
        json!({"result_id":evidence_id,"field":"result.execution.steps","offset":20,"limit":10}),
    );
    assert_eq!(
        page["result"]["structuredContent"]["entries"],
        json!([20, 21, 22, 23])
    );
    assert_eq!(page["result"]["structuredContent"]["total"], 24);
    assert!(page["result"]["structuredContent"]["next_offset"].is_null());
    std::fs::write(&evidence_path, b"{}").unwrap();
    assert_eq!(
        client.call("result_get", json!({"result_id":evidence_id}))["error"]["code"],
        -32603
    );
    assert!(
        completed["result"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing.xlsx"),
        "{completed}"
    );
    client.finish();
    let mut reopened = Client::start(&executable);
    assert_eq!(
        reopened.call(
            "result_get",
            json!({"result_id":result_id,"field":"error.message"})
        )["result"]["structuredContent"],
        details["result"]["structuredContent"]
    );
    assert_eq!(
        reopened.call("task_get", json!({"request_id":"missing-workbook"}))["result"]["structuredContent"],
        completed
    );
    assert_eq!(
        reopened.call("acquisition_update", arguments)["result"]["structuredContent"],
        completed
    );
    assert_eq!(
        reopened.call("task_cancel", lookup)["result"]["structuredContent"],
        completed
    );
    for (name, args) in [
        (
            "acquisition_update",
            json!({"request_id":"missing-workbook","workbook":"other.xlsx"}),
        ),
        ("acquisition_update", json!({"workbook":"missing.xlsx"})),
        ("task_get", json!({})),
        ("task_get", json!({"task_id":"../settings.json"})),
    ] {
        let response = reopened.call(name, args);
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
    reopened.finish();
}

#[test]
fn stdio_tools_validate_inputs_and_share_settings_services() {
    let directory = common::TestDirectory::new("azlw-mcp-tests", "stdio");
    let executable = common::prepare_install(directory.path());
    let mut client = Client::start(&executable);
    let tools = client.request("tools/list", json!({}));
    let tools = tools["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 39);
    let names = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names.len(), 39);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
    for (name, arguments) in [
        ("equipment_actions_apply", json!({"actions":[]})),
        ("workbook_execute", json!({"workbook":"x.xlsx"})),
        (
            "workbook_execute",
            json!({"workbook":"x.xlsx","plan_hash":"invalid"}),
        ),
        ("settings_get", json!({"unexpected":true})),
        ("not_a_tool", json!({})),
    ] {
        let response = client.call(name, arguments);
        assert_eq!(response["error"]["code"], -32602, "{response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains(name),
            "{response}"
        );
    }
    // 嵌套动作的类型错误必须指出数组位置，且保留反序列化的原始原因。
    let malformed = client.call(
        "equipment_actions_check",
        json!({"actions":[{"action":"unequip","target":{"ship_id":"wrong","slot_index":1}}]}),
    );
    let error = &malformed["result"]["structuredContent"]["error"];
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("equipment_actions_check")
            && message.contains("actions[0]")
            && message.contains("invalid type"),
        "{malformed}"
    );
    assert!(error["causes"].is_array());
    let settings = client.call("settings_get", json!({}));
    assert_eq!(
        settings["result"]["structuredContent"]["status"], "ok",
        "{settings}"
    );
    assert!(
        client
            .notifications
            .iter()
            .any(|message| message["method"] == "notifications/progress"
                && message["params"]["progressToken"] == "test-progress")
    );
    let original =
        client.call("preferences_get", json!({}))["result"]["structuredContent"]["result"].clone();
    let mut preferences = original.clone();
    preferences["detailed_diagnostics"] =
        json!(!original["detailed_diagnostics"].as_bool().unwrap());
    let saved = client.call(
        "preferences_update",
        json!({"original":original,"preferences":preferences}),
    );
    assert_eq!(
        saved["result"]["structuredContent"]["result"], preferences,
        "{saved}"
    );
    let conflict = client.call(
        "preferences_update",
        json!({"original":original,"preferences":original}),
    );
    assert_eq!(conflict["result"]["isError"], true, "{conflict}");
    let read = client.call("preferences_get", json!({}));
    assert_eq!(read["result"]["structuredContent"]["result"], preferences);
    for (name, args) in [
        ("ships", json!({"full":true,"fields":["name"]})),
        ("catalog_skills", json!({"ids":[]})),
        ("logs_show", json!({"filename":"../settings.json"})),
    ] {
        let response = client.call(name, args);
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
    let logs = client.call("logs_list", json!({}));
    assert_eq!(
        logs["result"]["structuredContent"]["status"], "ok",
        "{logs}"
    );
    client.finish();
    let output = Command::new(executable)
        .args(["settings", "preferences"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let persisted: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(persisted, preferences);
}
