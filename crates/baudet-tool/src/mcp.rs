use crate::tool::{Tool, ToolContext, ToolResult};
use baudet_core::{mcp::McpServerConfig, ImageItem, ItemStatus};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

// Bound the reader before allocating a complete server message.
const MAX_LINE: u64 = 16 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

struct Client {
    child: Child,
    send: mpsc::SyncSender<Value>,
    recv: mpsc::Receiver<Value>,
    next_id: u64,
    broken: bool,
}

impl Client {
    fn start(config: &McpServerConfig, cancelled: &AtomicBool) -> Result<Self, String> {
        let mut command = Command::new(&config.command);
        crate::process_runner::restrict_environment(&mut command);
        let mut child = command.args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let (send, outbound) = mpsc::sync_channel::<Value>(2);
        let (inbound, recv) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            for message in outbound {
                if serde_json::to_writer(&mut stdin, &message).is_err()
                    || stdin.write_all(b"\n").is_err()
                    || stdin.flush().is_err()
                {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut line = Vec::new();
            loop {
                line.clear();
                match (&mut stdout).take(MAX_LINE + 1).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(size) if size as u64 > MAX_LINE => break,
                    _ => {}
                }
                if let Ok(message) = serde_json::from_slice(&line) {
                    if inbound.send(message).is_err() {
                        break;
                    }
                }
            }
        });
        let mut client = Self { child, send, recv, next_id: 0, broken: false };
        let handshake = client.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "baudet", "version": env!("CARGO_PKG_VERSION") }
            }),
            CONNECT_TIMEOUT,
            cancelled,
        )?;
        if !matches!(
            handshake["protocolVersion"].as_str(),
            Some("2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25")
        ) {
            return Err("unsupported MCP protocol version".into());
        }
        client.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
        Ok(client)
    }

    fn write(&self, message: Value) -> Result<(), String> {
        self.send.try_send(message).map_err(|_| "MCP transport is unavailable".into())
    }

    // Discard the transport after timeout or cancellation so late replies cannot reach a later call.
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Value, String> {
        let result = self.exchange(method, params, timeout, cancelled);
        if result.is_err() && self.broken {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        result
    }

    fn exchange(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Value, String> {
        if self.broken || cancelled.load(Ordering::Relaxed) {
            return Err("MCP request cancelled or transport unavailable".into());
        }
        self.next_id += 1;
        let id = self.next_id;
        self.broken = true;
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let start = Instant::now();
        while !cancelled.load(Ordering::Relaxed) && start.elapsed() < timeout {
            let wait = timeout.saturating_sub(start.elapsed()).min(Duration::from_millis(50));
            match self.recv.recv_timeout(wait) {
                Ok(message) if message.get("method").is_some() => {
                    if let Some(id) = message.get("id") {
                        self.write(
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not supported"}}),
                        )?;
                    }
                }
                Ok(message) if message["jsonrpc"] == "2.0" && message["id"].as_u64() == Some(id) => {
                    self.broken = false;
                    return message
                        .get("result")
                        .cloned()
                        .ok_or_else(|| message["error"]["message"].as_str().unwrap_or("invalid MCP reply").to_owned());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("MCP server closed stdout".into()),
                _ => {}
            }
        }
        Err(if cancelled.load(Ordering::Relaxed) { "MCP request cancelled" } else { "MCP request timed out" }.into())
    }

    fn list_tools(&mut self, cancelled: &AtomicBool) -> Result<Vec<Value>, String> {
        let start = Instant::now();
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let params = cursor.as_ref().map_or_else(|| json!({}), |value| json!({"cursor":value}));
            let remaining = CONNECT_TIMEOUT.saturating_sub(start.elapsed());
            let result = self.request("tools/list", params, remaining, cancelled)?;
            tools.extend(result["tools"].as_array().ok_or("invalid MCP tool list")?.iter().cloned());
            cursor = result["nextCursor"].as_str().map(str::to_owned);
            let Some(next) = &cursor else { return Ok(tools) };
            if seen.len() >= 64 || !seen.insert(next.clone()) {
                return Err("MCP tool list exceeded its pagination limit".into());
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Connection {
    config: McpServerConfig,
    client: Arc<Mutex<Client>>,
    tools: Vec<Value>,
}

#[derive(Default)]
pub struct McpServers {
    connections: HashMap<String, Connection>,
}

impl McpServers {
    pub fn tools(
        &mut self,
        configs: &[McpServerConfig],
        cancelled: &Arc<AtomicBool>,
    ) -> Vec<Box<dyn Tool + Send + Sync>> {
        self.connections.retain(|id, conn| {
            configs.iter().any(|cfg| cfg.enabled && &cfg.id == id && *cfg == conn.config)
                && !conn.client.lock().unwrap().broken
        });
        let mut result: Vec<Box<dyn Tool + Send + Sync>> = Vec::new();
        let mut names = HashSet::new();
        for config in configs.iter().filter(|cfg| cfg.enabled && cfg.valid()) {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            if !self.connections.contains_key(&config.id) {
                let Ok(mut client) = Client::start(config, cancelled) else { continue };
                let Ok(tools) = client.list_tools(cancelled) else { continue };
                self.connections.insert(
                    config.id.clone(),
                    Connection { config: config.clone(), client: Arc::new(Mutex::new(client)), tools },
                );
            }
            let conn = &self.connections[&config.id];
            if conn.config != *config {
                continue;
            }
            for definition in &conn.tools {
                let Some(tool) = definition["name"].as_str().filter(|name| !name.is_empty()) else { continue };
                let name = format!("mcp__{}__{}", name_part(&config.id), name_part(tool));
                if name.len() > 64 || !names.insert(name.clone()) {
                    continue;
                }
                let description = definition["description"]
                    .as_str()
                    .filter(|description| !description.trim().is_empty())
                    .map_or_else(|| format!("MCP tool {tool} from {}", config.id), str::to_owned);
                result.push(Box::new(Adapter {
                    name,
                    tool: tool.to_owned(),
                    description,
                    server: config.id.clone(),
                    schema: definition["inputSchema"]
                        .as_object()
                        .map_or_else(|| json!({"type":"object"}), |schema| json!(schema)),
                    client: Arc::clone(&conn.client),
                    cancelled: Arc::clone(cancelled),
                    timeout: Duration::from_secs(config.timeout_seconds.filter(|value| *value > 0).unwrap_or(120)),
                }));
            }
        }
        result
    }
}

fn name_part(value: &str) -> String {
    value.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

struct Adapter {
    name: String,
    description: String,
    tool: String,
    server: String,
    schema: Value,
    client: Arc<Mutex<Client>>,
    cancelled: Arc<AtomicBool>,
    timeout: Duration,
}

impl Tool for Adapter {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult {
        if !(ctx.approve_mcp)(&self.server, &self.tool, args) {
            return ToolResult::fail("The user declined this MCP call.");
        }
        let result = self.client.lock().unwrap().request(
            "tools/call",
            json!({"name":self.tool,"arguments":args}),
            self.timeout,
            &self.cancelled,
        );
        match result {
            Ok(reply) => map_reply(&self.name, reply),
            Err(error) => ToolResult::fail(error),
        }
    }
}

fn map_reply(tool: &str, reply: Value) -> ToolResult {
    let ok = reply["isError"].as_bool() != Some(true);
    let mut result = ToolResult { ok, ..Default::default() };
    for part in reply["content"].as_array().into_iter().flatten() {
        match part["type"].as_str() {
            Some("text") => {
                if let Some(text) = part["text"].as_str() {
                    result.output.push_str(text);
                    result.output.push('\n');
                }
            }
            Some("image") => {
                if let (Some(mime), Some(data)) = (part["mimeType"].as_str(), part["data"].as_str()) {
                    result.images.push(ImageItem {
                        item_id: String::new(),
                        tool: tool.to_owned(),
                        mime_type: mime.to_owned(),
                        base64: data.to_owned(),
                        status: ItemStatus::Completed,
                    });
                }
            }
            _ => result.output.push_str("(Non-text MCP content omitted.)\n"),
        }
    }
    if !result.images.is_empty() {
        result.output.push_str(&format!("({} image(s) returned.)", result.images.len()));
    }
    if result.output.is_empty() {
        result.output = if ok { "(No output.)" } else { "The MCP server reported an error." }.into();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_server() {
        if std::env::var_os("BAUDET_MCP_TEST_SERVER").is_none() {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let message: Value = serde_json::from_str(&line).unwrap();
            let Some(id) = message.get("id") else { continue };
            let result = match message["method"].as_str().unwrap() {
                "initialize" => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}),
                "tools/list" => json!({"tools":[{"name":"echo", "inputSchema":{"type":"object"}}]}),
                "tools/call" if message["params"]["arguments"]["stall"] == true => continue,
                "tools/call" => json!({"content":[{"type":"text","text":message["params"]["arguments"]["text"]}]}),
                _ => break,
            };
            writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap();
            stdout.flush().unwrap();
        }
    }

    #[test]
    fn stdio_discovers_tools_requires_approval_and_reaps_a_timed_out_server() {
        let config = McpServerConfig {
            id: "fixture".into(),
            command: std::env::current_exe().unwrap().to_str().unwrap().into(),
            args: vec!["--exact".into(), "mcp::tests::fake_server".into(), "--nocapture".into()],
            env: [("BAUDET_MCP_TEST_SERVER".into(), "1".into())].into_iter().collect(),
            enabled: true,
            timeout_seconds: Some(1),
        };
        let running = Arc::new(AtomicBool::new(false));
        let mut manager = McpServers::default();
        let tools = manager.tools(&[config], &running);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "mcp__fixture__echo");
        let denied = tools[0].execute(&json!({"text":"denied"}), &ToolContext::default());
        assert!(!denied.ok);
        let approved = ToolContext {
            approve_mcp: Box::new(|server, tool, _| server == "fixture" && tool == "echo"),
            ..ToolContext::default()
        };
        assert_eq!(tools[0].execute(&json!({"text":"hello"}), &approved).output.trim(), "hello");
        let delayed = tools[0].execute(&json!({"stall":true}), &approved);
        assert!(!delayed.ok);
        assert!(delayed.output.contains("timed out"));
        drop(tools);
        manager.tools(&[], &running);
        assert!(manager.connections.is_empty());
    }
}
