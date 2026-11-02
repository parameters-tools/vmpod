use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::orchestrator::{
    validate_name, validate_ram, validate_smp, validate_timeout, UpParams, VmOrchestrator,
};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "vmpod";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Tools catalog defined according to Model Context Protocol specification
fn get_tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "vmpod_spin",
                "description": "Spin up a hardware-accelerated Linux VM (QEMU+KVM) and wait with synchronous readiness probing until guest OS and SSH are 100% ready.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Unique VM instance name (1-64 chars [a-zA-Z0-9_.-]). Auto-generated if omitted."
                        },
                        "ram": {
                            "type": "string",
                            "description": "Guest RAM allocation (e.g. '512M', '2G', '4G', max 64GB). Default '2G'."
                        },
                        "smp": {
                            "type": "integer",
                            "description": "Guest vCPU core count (1-32). Default 2."
                        },
                        "ephemeral": {
                            "type": "boolean",
                            "description": "Auto-purge virtual disk and storage upon teardown. Default true."
                        },
                        "timeout": {
                            "type": "integer",
                            "description": "Max boot readiness wait time in seconds. Default 60."
                        }
                    }
                }
            },
            {
                "name": "vmpod_run",
                "description": "Execute a non-interactive shell command inside a running VM and return structured exit_code, stdout, stderr, and duration.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Target running VM instance name."
                        },
                        "command": {
                            "type": "string",
                            "description": "Shell command to execute inside the VM."
                        },
                        "workdir": {
                            "type": "string",
                            "description": "Working directory path inside guest VM (e.g. '/workspace')."
                        },
                        "env": {
                            "type": "object",
                            "description": "Key-value environment variables to export inside the guest VM.",
                            "additionalProperties": { "type": "string" }
                        },
                        "timeout": {
                            "type": "integer",
                            "description": "Execution timeout in seconds (default 60, max 1800). Auto-kills hung commands."
                        }
                    },
                    "required": ["name", "command"]
                }
            },
            {
                "name": "vmpod_write_file",
                "description": "Write a text or script file directly into the VM guest filesystem at the specified destination path.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Target running VM instance name."
                        },
                        "path": {
                            "type": "string",
                            "description": "Absolute destination path inside the guest VM (e.g. '/workspace/main.py')."
                        },
                        "content": {
                            "type": "string",
                            "description": "File text content to write."
                        }
                    },
                    "required": ["name", "path", "content"]
                }
            },
            {
                "name": "vmpod_read_file",
                "description": "Read file contents from inside the VM guest filesystem.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Target running VM instance name."
                        },
                        "path": {
                            "type": "string",
                            "description": "Absolute file path inside the guest VM to read."
                        }
                    },
                    "required": ["name", "path"]
                }
            },
            {
                "name": "vmpod_snapshot",
                "description": "Manage virtual disk snapshots (checkpoints) for a VM. Actions: 'create', 'list', 'delete'.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Target VM instance name."
                        },
                        "action": {
                            "type": "string",
                            "description": "Snapshot action to perform: 'create', 'list', or 'delete'.",
                            "enum": ["create", "list", "delete"]
                        },
                        "tag": {
                            "type": "string",
                            "description": "Snapshot name/tag (required for 'create' and 'delete')."
                        }
                    },
                    "required": ["name", "action"]
                }
            },
            {
                "name": "vmpod_rollback",
                "description": "Instantly revert a VM's virtual disk back to a named snapshot checkpoint.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Target VM instance name."
                        },
                        "tag": {
                            "type": "string",
                            "description": "Name or tag of the snapshot checkpoint to revert to."
                        }
                    },
                    "required": ["name", "tag"]
                }
            },
            {
                "name": "vmpod_status",
                "description": "Inspect the status, IP/port mappings, uptime, and resource usage of a VM instance.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Target VM instance name."
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "vmpod_list",
                "description": "List all active and stopped VM instances managed by vmpod.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "vmpod_down",
                "description": "Gracefully shut down a VM container and optionally purge its disk overlay.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Target VM instance name."
                        },
                        "purge": {
                            "type": "boolean",
                            "description": "Purge storage directory on host. Defaults to true if the VM was marked ephemeral."
                        }
                    },
                    "required": ["name"]
                }
            }
        ]
    })
}

/// Handle a single incoming JSON-RPC line
pub async fn handle_message(line: &str, orchestrator: &VmOrchestrator) -> Option<JsonRpcResponse> {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: None,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("Parse error: {}", e),
                    data: None,
                }),
            });
        }
    };

    let id = req.id.clone();

    match req.method.as_str() {
        "initialize" => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            })),
            error: None,
        }),

        "notifications/initialized" => {
            // Notifications do not receive a response
            None
        }

        "ping" => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!({})),
            error: None,
        }),

        "tools/list" => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(get_tools_list()),
            error: None,
        }),

        "tools/call" => {
            let params = req.params.unwrap_or_default();
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let tool_result = dispatch_tool(tool_name, arguments, orchestrator).await;

            match tool_result {
                Ok(content_text) => Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(json!({
                        "content": [
                            {
                                "type": "text",
                                "text": content_text
                            }
                        ],
                        "isError": false
                    })),
                    error: None,
                }),
                Err(err_msg) => Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(json!({
                        "content": [
                            {
                                "type": "text",
                                "text": format!("Error: {}", err_msg)
                            }
                        ],
                        "isError": true
                    })),
                    error: None,
                }),
            }
        }

        _ => {
            if id.is_some() {
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32601,
                        message: format!("Method not found: {}", req.method),
                        data: None,
                    }),
                })
            } else {
                None
            }
        }
    }
}

/// Dispatch tool call to the orchestrator with guardrail enforcement
async fn dispatch_tool(name: &str, args: Value, orchestrator: &VmOrchestrator) -> Result<String, String> {
    match name {
        "vmpod_spin" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    let id = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        % 100000;
                    format!("agent-{}", id)
                });

            let ram = args
                .get("ram")
                .and_then(|v| v.as_str())
                .unwrap_or("2G")
                .to_string();

            let smp = args
                .get("smp")
                .and_then(|v| v.as_u64())
                .unwrap_or(2) as u32;

            let ephemeral = args
                .get("ephemeral")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let timeout_secs = args
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(60);

            validate_name(&instance_name).map_err(|e| e.to_string())?;
            validate_smp(smp).map_err(|e| e.to_string())?;
            validate_ram(&ram).map_err(|e| e.to_string())?;
            validate_timeout(timeout_secs).map_err(|e| e.to_string())?;

            let params = UpParams {
                name: instance_name,
                port: None,
                ram,
                smp,
                storage: None,
                image: "vmpod/qemu-runner:latest".to_string(),
                ephemeral,
            };

            let res = orchestrator
                .spin(params, std::time::Duration::from_secs(timeout_secs))
                .await
                .map_err(|e| e.to_string())?;

            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "vmpod_run" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'name'".to_string())?;

            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'command'".to_string())?;

            let workdir = args.get("workdir").and_then(|v| v.as_str());

            let timeout_secs = args
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(60);

            validate_timeout(timeout_secs).map_err(|e| e.to_string())?;

            let mut env_vec = Vec::new();
            if let Some(env_obj) = args.get("env").and_then(|v| v.as_object()) {
                for (k, v) in env_obj {
                    if let Some(val_str) = v.as_str() {
                        env_vec.push(format!("{}={}", k, val_str));
                    }
                }
            }

            let res = orchestrator
                .run_command(
                    instance_name,
                    command,
                    workdir,
                    &env_vec,
                    std::time::Duration::from_secs(timeout_secs),
                )
                .await
                .map_err(|e| e.to_string())?;

            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "vmpod_write_file" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'name'".to_string())?;

            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'path'".to_string())?;

            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'content'".to_string())?;

            orchestrator
                .write_file(instance_name, path, content.as_bytes())
                .await
                .map_err(|e| e.to_string())?;

            Ok(json!({
                "status": "success",
                "instance": instance_name,
                "path": path,
                "bytes_written": content.len()
            })
            .to_string())
        }

        "vmpod_read_file" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'name'".to_string())?;

            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'path'".to_string())?;

            orchestrator
                .read_file(instance_name, path)
                .await
                .map_err(|e| e.to_string())
        }

        "vmpod_snapshot" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'name'".to_string())?;

            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("list");

            match action {
                "create" => {
                    let tag = args
                        .get("tag")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| "Missing required parameter 'tag' for snapshot create".to_string())?;

                    let res = orchestrator
                        .snapshot_create(instance_name, tag)
                        .await
                        .map_err(|e| e.to_string())?;

                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                }
                "list" => {
                    let res = orchestrator
                        .snapshot_list(instance_name)
                        .await
                        .map_err(|e| e.to_string())?;

                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                }
                "delete" => {
                    let tag = args
                        .get("tag")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| "Missing required parameter 'tag' for snapshot delete".to_string())?;

                    let res = orchestrator
                        .snapshot_delete(instance_name, tag)
                        .await
                        .map_err(|e| e.to_string())?;

                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                }
                _ => Err(format!(
                    "Invalid action '{}': must be 'create', 'list', or 'delete'",
                    action
                )),
            }
        }

        "vmpod_rollback" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'name'".to_string())?;

            let tag = args
                .get("tag")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'tag'".to_string())?;

            let res = orchestrator
                .snapshot_rollback(instance_name, tag)
                .await
                .map_err(|e| e.to_string())?;

            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "vmpod_status" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'name'".to_string())?;

            let res = orchestrator
                .status(instance_name)
                .await
                .map_err(|e| e.to_string())?;

            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "vmpod_list" => {
            let res = orchestrator
                .list_instances()
                .await
                .map_err(|e| e.to_string())?;

            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "vmpod_down" => {
            let instance_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter 'name'".to_string())?;

            let purge = args
                .get("purge")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let res = orchestrator
                .down(instance_name, purge)
                .await
                .map_err(|e| e.to_string())?;

            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        _ => Err(format!("Unknown tool: '{}'", name)),
    }
}

/// Run the MCP server over standard input/output (stdio JSON-RPC transport)
pub async fn run_mcp_server(orchestrator: Arc<VmOrchestrator>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    eprintln!("[vmpod-mcp] vmpod Model Context Protocol server v{} started on stdio.", SERVER_VERSION);
    eprintln!("[vmpod-mcp] Ready to accept JSON-RPC requests.");

    loop {
        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break, // EOF from client
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(resp) = handle_message(line, &orchestrator).await {
            let mut resp_str = serde_json::to_string(&resp)?;
            resp_str.push('\n');
            stdout.write_all(resp_str.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    eprintln!("[vmpod-mcp] Connection closed by client, server terminating.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_initialize() {
        let orch = VmOrchestrator::new().expect("Docker orchestrator init failed in test");
        let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
        let resp = handle_message(init_req, &orch).await.expect("No response");

        assert_eq!(resp.id, Some(json!(1)));
        let res = resp.result.expect("Missing result");
        assert_eq!(res["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(res["serverInfo"]["name"], SERVER_NAME);
    }

    #[tokio::test]
    async fn test_mcp_ping() {
        let orch = VmOrchestrator::new().expect("Docker orchestrator init failed in test");
        let ping_req = r#"{"jsonrpc":"2.0","id":"req-42","method":"ping"}"#;
        let resp = handle_message(ping_req, &orch).await.expect("No response");

        assert_eq!(resp.id, Some(json!("req-42")));
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_mcp_tools_list() {
        let orch = VmOrchestrator::new().expect("Docker orchestrator init failed in test");
        let list_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = handle_message(list_req, &orch).await.expect("No response");

        let res = resp.result.expect("Missing result");
        let tools = res["tools"].as_array().expect("Tools is not an array");
        assert_eq!(tools.len(), 9);

        let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(tool_names.contains(&"vmpod_spin"));
        assert!(tool_names.contains(&"vmpod_run"));
        assert!(tool_names.contains(&"vmpod_write_file"));
        assert!(tool_names.contains(&"vmpod_read_file"));
        assert!(tool_names.contains(&"vmpod_snapshot"));
        assert!(tool_names.contains(&"vmpod_rollback"));
        assert!(tool_names.contains(&"vmpod_status"));
        assert!(tool_names.contains(&"vmpod_list"));
        assert!(tool_names.contains(&"vmpod_down"));
    }

    #[tokio::test]
    async fn test_mcp_unknown_method() {
        let orch = VmOrchestrator::new().expect("Docker orchestrator init failed in test");
        let bad_req = r#"{"jsonrpc":"2.0","id":99,"method":"unknown_method"}"#;
        let resp = handle_message(bad_req, &orch).await.expect("No response");

        assert_eq!(resp.id, Some(json!(99)));
        let err = resp.error.expect("Missing error");
        assert_eq!(err.code, -32601);
    }

    #[tokio::test]
    async fn test_mcp_notification_no_response() {
        let orch = VmOrchestrator::new().expect("Docker orchestrator init failed in test");
        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = handle_message(notif, &orch).await;
        assert!(resp.is_none());
    }
}
