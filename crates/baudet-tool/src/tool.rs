//! Eén capaciteit die aan het model aangeboden kan worden, en wat daarvoor nodig is.
//!
//! Poort van `tool/Tool.kt`, `tool/ToolContext.kt`, `tool/ToolCall.kt` en `tool/ToolResult.kt`. De grens uit
//! de origineler is hier de reden van bestaan: een onderhandeling met de mens loopt uitsluitend via
//! [`ToolContext`], nooit door in de UI te grijpen — vandaar dat deze crate zonder egui compileert.
//!
//! Model-gerichte tekst (`description`, `output`) blijft Engels, ongeacht de UI-taal, precies zoals daar.

use serde_json::Value;
use std::path::PathBuf;

/// Eén tool waar het model in een beurt om vroeg. `arguments` is het ruwe JSON-object dat de handler
/// verbruikt, dus er wordt hier niets ingeraad: een ontbrekend of fout getyld veld blijft een tool-probleem
/// dat het model ziet en kan herstellen, in plaats van een parse-crash. De vorm is transport-onafhankelijk —
/// prompted-JSON en een native function_call reduceren allebei naar een naam plus een argumentobject.
/// `call_id` draagt het backend-id van een native call, zodat diens observatie eraan terug te koppelen is.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub tool: String,
    pub arguments: Value,
    pub call_id: Option<String>,
}

impl ToolCall {
    /// De kale string-veldwaarde van een argument, of `None` wanneer hij ontbreekt of geen string is. De
    /// De `str`-hulp van de API-laag in de origineler, die geen argument ooit inraadt.
    pub fn arg_str(&self, key: &str) -> Option<&str> {
        self.arguments.get(key).and_then(Value::as_str)
    }
}

/// Wat een tool-run teruggeeft. `output` gaat naar het model; `diff` is de mens-gerichte unified preview
/// naast de goedkeuringsvraag en bestaat alleen wanneer er een is.
///
/// De proces-velden zijn voor de mens, niet voor het model: een run die "exit code: 2" al in `output`
/// spellt, zou in een kop alleen maar herhalen, en een kop uit proza ineenraadseld is een kop die raadt.
/// `exit_code` blijft precies `None` wanneer er geen code is produced — een timeout, of een commando dat
/// nooit startte — en dat is het onderscheid waar deze app voor exists.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    /// De unified preview voor de mens, alleen bij een schrijfbare tool met iets te tonen.
    pub diff: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub cwd: Option<String>,
}

impl ToolResult {
    /// Een geslaagde run met model-tekst.
    pub fn ok(output: impl Into<String>) -> ToolResult {
        ToolResult { ok: true, output: output.into(), ..Default::default() }
    }

    /// Een mislukte run. In de origineler is een geweigerde of mislukte tool feedback voor het model, dus
    /// geen exception: de loop leeft door en het model kan het opnieuw proberen.
    pub fn fail(output: impl Into<String>) -> ToolResult {
        ToolResult { ok: false, output: output.into(), ..Default::default() }
    }
}

/// Hoe de UI een schrijfwijziging vraagt: (pad, uitvoerbare samenvatting) en `true` voor accepteren.
pub type Approval = Box<dyn Fn(&str, &str) -> bool + Send>;
/// Hoe de UI een commando vraagt: (signatuur, volledige opdracht, aantal keer dat al gevraagd is).
pub type CommandApproval = Box<dyn Fn(&str, &str, u32) -> bool + Send>;

/// Wat een tool nodig heeft om te handelen: de projectmap waarop hij begrensd is, mogen er geschreven en
/// gedraaid worden, en hoe hij de mens moet vragen vóór er iets landt.
///
/// De goedkeuringen zijn gewone callbacks, zodat een tool vrij blijft van de UI: de UI beslist hóe ze vraagt
/// en draait dat op háár thread, terwijl de tool op het antwoord blokkeert. Een `None`-root of een false-recht
/// is een reden te weigeren, welke de tool zélf controleert. Rechten staan uit en beide goedkeuringen
/// onthouden, dus een context die nooit leest, draait of schrijft hoeft van geen van deze iets mee te geven.
pub struct ToolContext {
    pub root: Option<PathBuf>,
    pub read_granted: bool,
    pub write_granted: bool,
    pub exec_granted: bool,
    /// Vraag vóór een schrijfwijziging landt; `true` betekent accepteren. Een edit biedt alleen accepteren
    /// of weigeren — er is geen per-bestand "altijd" op te pinnen — dus de boolean is gewoon het antwoord.
    pub approve: Approval,
    /// Vraag vóór een commando draait. Een commando vraagt í always, ook bij `Auto`, want een subproces valt
    /// niet binnen de mapgrens te vangen. Drie antwoorden zoals in de origineler: eenmaal draaien, pinnen en
    /// draaien, of weigeren.
    pub approve_command: CommandApproval,
    /// Commando-pins die de persoon altijd toestaat: een commando whose signature hierin staat, slaat de
    /// vraag over. Alleen een commando dat [`crate::safe_commands`] bewezen read-only kan heten, heeft een
    /// herbruikbare signatuur; alles anders pint op zijn hele tekst.
    ///
    /// Een provider in plaats van de set zelf, want een tool-loop draait vele rondes op één context terwijl
    /// "altijd" halverwege één van die rondes wordt geklikt. Als een kopie was meegegeven, zou een pin uit
    /// ronde twee pas de volgende beurt bestaan en zou de persoon juist over het commando worden gevraagd dat
    /// hij zojuist vastlegde.
    pub auto_allow_provider: Box<dyn Fn() -> Vec<String> + Send>,
    /// Wanneer waar, slaat een door [`crate::safe_commands`] als read-only herkend commando de vraag over.
    pub auto_approve_safe: bool,
    /// Commando-families die de persoon vertrouwt; zie [`crate::safe_commands::trusted_key`].
    pub trusted_commands_provider: Box<dyn Fn() -> Vec<String> + Send>,
}

impl std::fmt::Debug for ToolContext {
    /// De callbacks zijn niet debugbaar en hun rechten wél: dat zijn de feiten die een test of een log wil
    /// zien, dus die zijn het wat deze impl toont.
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
    /// Niets mag, alles vraagt, en dus is dit de vorm die een test gebruikt wanneer hij alleen de
    /// mapgrens wil controleren. De origineler bereikt hetzelfde met default-argumenten.
    fn default() -> ToolContext {
        ToolContext {
            root: None,
            read_granted: false,
            write_granted: false,
            exec_granted: false,
            approve: Box::new(|_, _| false),
            approve_command: Box::new(|_, _, _| false),
            auto_allow_provider: Box::new(Vec::new),
            auto_approve_safe: false,
            trusted_commands_provider: Box::new(Vec::new),
        }
    }
}

impl ToolContext {
    /// De pins die de persoon altijd toestaat, opgevraagd op dit moment (zie het veld).
    pub fn auto_allow(&self) -> Vec<String> {
        (self.auto_allow_provider)()
    }

    /// De vertrouwde commando-families, opgevraagd op dit moment (zie het veld).
    pub fn trusted_commands(&self) -> Vec<String> {
        (self.trusted_commands_provider)()
    }

    /// Een context scoped aan één map, met de rechten die de persoon gaf. De callbacks blijven onthouden.
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

/// Eén capaciteit die aan het model aangeboden kan worden. `description` en `parameters` zijn
/// model-gericht en blijven Engels; `parameters` is een JSON-Schema-object, de vorm die de Responses API
/// voor een function tool wil. [`Tool::execute`] is vrij van UI: elke goedkeuring loopt via [`ToolContext`].
pub trait Tool {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;

    /// Of deze call naast zijn buren in dezelfde batch mag draaien. Een tool zegt alleen ja wanneer de call
    /// niets schrijft én op niemand wacht, zodat een fan-out nooit een write kan racen en nooit aan een
    /// goedkeuring kan hangen. Default weigert, dus een nieuwe tool blijft serieel tot hij bewijst dat hij
    /// geen neveneffecten heeft; de loop fan-out alleen wanneer élke call in een batch ja zegt.
    fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
        false
    }

    fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult;
}

/// De tools die in een beurt aan staan. [`ToolRegistry::definitions`] tekent het `tools`-array dat de
/// Responses API verwacht; [`ToolRegistry::get`] stuurt een model-aanvraag terug naar zijn handler. Los van
/// elke enkele tool gehouden, zodat de loop één set ziet ongeacht hoeveel er zijn aangesloten, en een onbekende
/// naam een nette miss is in plaats van een crash.
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

    /// Het `tools`-veld voor een Responses-request: één function-object per tool.
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

/// Sturt één [`ToolCall`] naar zijn handler. De twee dingen die kunnen gooien — een onbekende naam en een
/// handler die gooit — worden omgezet in een model-gericht [`ToolResult`], want het model moet een mislukte
/// tool als bruikbare feedback zien, niet als een gecrashte loop.
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

    /// Een tool die alles onthoudt wat hem gevraagd wordt, zodat de registry en de runner getest kunnen
    /// worden zonder een van de echte tools te hoeven aansluiten.
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

        // Een onbekende naam is feedback voor het model, geen crash.
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
        // De default van de trait weigert, want een nieuwe tool is side-effect-vrij tot hij het bewijst.
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
        assert!(!(ctx.approve_command)("rm -rf build", "/tmp", 30));
        assert!(ctx.auto_allow().is_empty());
        assert!(ctx.trusted_commands().is_empty());

        let granted = ToolContext::for_folder("/tmp/x", true, true, false);
        assert_eq!(granted.root.as_deref(), Some(std::path::Path::new("/tmp/x")));
        assert!(granted.read_granted && granted.write_granted && !granted.exec_granted);
        // Rechten verlopen niet door de callbacks heen: "altijd" blijft een eigen beslissing.
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
