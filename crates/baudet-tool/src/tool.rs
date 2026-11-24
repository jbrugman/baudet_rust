
use baudet_core::item::ItemKind;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub tool: String,
    pub arguments: Value,
    pub call_id: Option<String>,
}

impl ToolCall {
    pub fn arg_str(&self, key: &str) -> Option<&str> {
        self.arguments.get(key).and_then(Value::as_str)
    }
}

pub type Undo = Box<dyn Fn() + Send>;

#[derive(Default)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    pub images: Vec<baudet_core::ImageItem>,
    pub diff: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub cwd: Option<String>,
    pub undo: Option<Undo>,
}

impl std::fmt::Debug for ToolResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolResult")
            .field("ok", &self.ok)
            .field("output", &self.output)
            .field("diff", &self.diff)
            .field("exit_code", &self.exit_code)
            .field("timed_out", &self.timed_out)
            .field("duration_ms", &self.duration_ms)
            .field("cwd", &self.cwd)
            .field("undo", &self.undo.as_ref().map(|_| "<handgreep>"))
            .finish()
    }
}

impl PartialEq for ToolResult {
    fn eq(&self, other: &ToolResult) -> bool {
        self.ok == other.ok
            && self.output == other.output
            && self.diff == other.diff
            && self.exit_code == other.exit_code
            && self.timed_out == other.timed_out
            && self.duration_ms == other.duration_ms
            && self.cwd == other.cwd
            && self.images == other.images
    }
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> ToolResult {
        ToolResult { ok: true, output: output.into(), ..Default::default() }
    }

    pub fn fail(output: impl Into<String>) -> ToolResult {
        ToolResult { ok: false, output: output.into(), ..Default::default() }
    }
}

pub type Approval = Box<dyn Fn(&str, &str) -> bool + Send>;
pub type McpApproval = Box<dyn Fn(&str, &str, &Value) -> bool + Send>;
pub type CommandApproval = Box<dyn Fn(&str, &str, u32, &str) -> bool + Send>;

pub struct ToolContext {
    pub root: Option<PathBuf>,
    pub read_granted: bool,
    pub write_granted: bool,
    pub exec_granted: bool,
    pub approve: Approval,
    pub approve_mcp: McpApproval,
    pub approve_command: CommandApproval,
    pub auto_allow_provider: Box<dyn Fn() -> Vec<String> + Send>,
    pub auto_approve_safe: bool,
    pub trusted_commands_provider: Box<dyn Fn() -> Vec<String> + Send>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("root", &self.root)
            .field("read_granted", &self.read_granted)
            .field("write_granted", &self.write_granted)
            .field("exec_granted", &self.exec_granted)
            .field("auto_approve_safe", &self.auto_approve_safe)
            .finish_non_exhaustive()
    }
}

impl Default for ToolContext {
    fn default() -> ToolContext {
        ToolContext {
            root: None,
            read_granted: false,
            write_granted: false,
            exec_granted: false,
            approve: Box::new(|_, _| false),
            approve_mcp: Box::new(|_, _, _| false),
            approve_command: Box::new(|_, _, _, _| false),
            auto_allow_provider: Box::new(Vec::new),
            auto_approve_safe: false,
            trusted_commands_provider: Box::new(Vec::new),
        }
    }
}

impl ToolContext {
    pub fn auto_allow(&self) -> Vec<String> {
        (self.auto_allow_provider)()
    }

    pub fn trusted_commands(&self) -> Vec<String> {
        (self.trusted_commands_provider)()
    }

    pub fn for_folder(root: impl Into<PathBuf>, read: bool, write: bool, exec: bool) -> ToolContext {
        ToolContext {
            root: Some(root.into()),
            read_granted: read,
            write_granted: write,
            exec_granted: exec,
            ..Default::default()
        }
    }
}

pub trait Tool {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;

    fn item_kind(&self) -> ItemKind {
        ItemKind::ToolCall
    }

    fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
        false
    }

    fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool + Send + Sync>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Box<dyn Tool + Send + Sync>>) -> ToolRegistry {
        ToolRegistry { tools }
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn get(&self, name: &str) -> Option<&(dyn Tool + Send + Sync)> {
        self.tools.iter().find(|tool| tool.name() == name).map(Box::as_ref)
    }

    pub fn definitions(&self) -> Value {
        Value::Array(
            self.tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "name": tool.name(),
                        "description": tool.description(),
                        "parameters": tool.parameters(),
                    })
                })
                .collect(),
        )
    }
}

pub struct ToolRunner<'a> {
    registry: &'a ToolRegistry,
}

impl<'a> ToolRunner<'a> {
    pub fn new(registry: &'a ToolRegistry) -> ToolRunner<'a> {
        ToolRunner { registry }
    }

    pub fn run(&self, call: &ToolCall, ctx: &ToolContext) -> ToolResult {
        let Some(tool) = self.registry.get(&call.tool) else {
            return ToolResult::fail(format!("no tool named `{}` is available", call.tool));
        };
        tool.execute(&call.arguments, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct EchoTool {
        name: &'static str,
        parallel: bool,
    }

    impl Tool for EchoTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "echo the file argument"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {"file": {"type": "string"}}, "required": ["file"]})
        }
        fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
            self.parallel
        }
        fn execute(&self, args: &Value, _ctx: &ToolContext) -> ToolResult {
            match args.get("file").and_then(Value::as_str) {
                Some(file) => ToolResult::ok(format!("saw {file}")),
                None => ToolResult::fail("needs a file argument"),
            }
        }
    }

    fn registry() -> ToolRegistry {
        ToolRegistry::new(vec![
            Box::new(EchoTool { name: "read_file", parallel: true }),
            Box::new(EchoTool { name: "edit_file", parallel: false }),
        ])
    }

    #[test]
    fn the_registry_dispatches_by_name_and_misses_cleanly() {
        let reg = registry();
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("bestaan_niet").is_none());

        let runner = ToolRunner::new(&reg);
        let call = ToolCall { tool: "read_file".into(), arguments: json!({"file": "a.txt"}), call_id: None };
        let result = runner.run(&call, &ToolContext::default());
        assert!(result.ok);
        assert_eq!(result.output, "saw a.txt");

        let unknown = ToolCall { tool: "rm_rf".into(), arguments: json!({}), call_id: None };
        let refused = runner.run(&unknown, &ToolContext::default());
        assert!(!refused.ok);
        assert!(refused.output.contains("rm_rf"), "de naam moet erin staan: {}", refused.output);
    }

    #[test]
    fn the_tools_array_has_the_shape_the_responses_api_wants() {
        let definitions = registry().definitions();
        let array = definitions.as_array().expect("een array");
        assert_eq!(array.len(), 2);
        assert_eq!(array[0]["type"], "function");
        assert_eq!(array[0]["name"], "read_file");
        assert_eq!(array[0]["description"], "echo the file argument");
        assert_eq!(array[0]["parameters"]["required"][0], "file");
    }

    #[test]
    fn parallelism_is_per_call_and_refuses_by_default() {
        struct Quiet;
        impl Tool for Quiet {
            fn name(&self) -> &str {
                "quiet"
            }
            fn description(&self) -> &str {
                "d"
            }
            fn parameters(&self) -> Value {
                json!({"type": "object"})
            }
            fn execute(&self, _a: &Value, _c: &ToolContext) -> ToolResult {
                ToolResult::ok("ok")
            }
        }
        let ctx = ToolContext::default();
        assert!(!Quiet.can_run_parallel(&json!({}), &ctx), "default is serieel");
        assert!(
            registry().get("read_file").unwrap().can_run_parallel(&json!({}), &ctx),
            "lezen schrijft niets en wacht op niemand"
        );
        assert!(!registry().get("edit_file").unwrap().can_run_parallel(&json!({}), &ctx));
    }

    #[test]
    fn a_context_denies_everything_until_someone_grants_it() {
        let ctx = ToolContext::default();
        assert!(ctx.root.is_none());
        assert!(!ctx.read_granted && !ctx.write_granted && !ctx.exec_granted);
        assert!(!ctx.auto_approve_safe);
        assert!(!(ctx.approve)("a.txt", "diff"));
        assert!(!(ctx.approve_command)("rm -rf build", "/tmp", 30, ""));
        assert!(ctx.auto_allow().is_empty());
        assert!(ctx.trusted_commands().is_empty());

        let granted = ToolContext::for_folder("/tmp/x", true, true, false);
        assert_eq!(granted.root.as_deref(), Some(std::path::Path::new("/tmp/x")));
        assert!(granted.read_granted && granted.write_granted && !granted.exec_granted);
        assert!(!(granted.approve)("a.txt", "diff"));
    }

    #[test]
    fn an_argument_object_never_gets_a_guessed_value() {
        let call = ToolCall { tool: "read_file".into(), arguments: json!({"file": 42}), call_id: None };
        assert_eq!(call.arg_str("file"), None, "een getal is geen pad en mag niet zomaar text worden");
        let absent = ToolCall { tool: "read_file".into(), arguments: json!({}), call_id: None };
        assert_eq!(absent.arg_str("file"), None);
    }
}
