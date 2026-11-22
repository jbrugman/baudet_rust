//! De twee kanalen waaruit een beurt zijn calls haalt.
//!
//! Poort van `tool/PromptedCallExtractor.kt`. Eerste de native calls die de backend al terugbracht — die
//! winnen en staan vooraan — daarna de prompted lezer, zodat een model dat in tekst antwoordt in plaats
//! van in een native function_call tóch kan handelen. Een lokaal model krijgt één sterk merk te zien in
//! plaats van het vertrouwen dat het JSON uit proza laat, want die modellen gooien losse accolades rond.
//!
//! Proza rond een fence wordt genegeerd en een blok dat niet parseert wordt overgeslagen, niet gegooid:
//! één verprutste call degradeert naar "geen call" (het antwoord blijft staan) in plaats van de beurt te
//! laten falen. Een native argument-string die niet parseert wordt een leeg argumentobject, dus de tool
//! meldt de ontbrekende velden terug aan het model in plaats van te crashen.

use crate::tool::ToolCall;
use crate::turn_source::{CallExtractor, FENCE_TAG};
use baudet_core::protocol::Turn;
use serde_json::Value;

/// Alle ```` ```tag ````-blokken in `text`, in de volgorde van de origineler.
///
/// Met de hand gesucht en niet met een regex, om dezelfde reden als de origineler zijn slot doet: de
/// offline crate-boom van deze repo bevat geen regex-bouwer, en deze taal is zo klein dat een lus minder
/// risico draagt dan een dependency. Het risico van een eigen lus is dat hij de regex *vertaalt* in plaats
/// van hem na te bootsen, en juist daarom is dit het zoek-algoritme van ```` ```tag[ \t]*\n(.*?)``` ````
/// met `DOT_MATCHES_ALL`, niet een poging die te begrijpen: voor elke startpositie wordt de Entry
/// gecontroleerd en pas bij een mislukking één teken opgeschoven. Dat verklaart de eigenschappen die
/// een naïevere scanner verkeerd radt, en die hier gemeten zijn tegen de regex van de origineler:
///
///  * een header mag spaties en tabs dragen, maar géén `\r` — die hoort niet in de klasse vóór `\n`,
///    dus `` ```baudet-tool\r\n `` is geen blok;
///  * `` ``` `` zonder tag is geen blok, en ` ``baudet-tool ` (twee backticks) ook niet;
///  * bij een mislukte opening schuift **één** teken op, niet drie. In `` ```baudet ```baudet-tool `` kan
///    de match nog op de tweede backtick van de opening beginnen, dus het wél geldige blok daarna telt
///    mee; drie overslaan zou het verliezen.
///
/// Een opening zonder sluiting levert niets op: `(.*?)` vraagt om een sluitende fence, en de regex
/// zoekt dan gewoon elders verder. Vier backticks zijn géén langere fence: de regex vraagt nergens
/// om een vierde backtick, dus `` ```baudet-tool\n{}\n```` `` levert body `{}` en de vierde backtick is
/// gewoon proza (gemeten, en meegenomen in de differentiele test hieronder).
///
/// De indexering is byte-voor-byte en de guard staat vóór élke slice. Dat is geen stijl: de eerste
/// versie van deze lus indexeerde `text` met `start` vóórdat hij controleerde dat er drie backticks
/// stonden, en panicte daarmee op een meerbytes teken vóór een blok: een `é` (2 bytes) en dan een blok
/// gaf de regex het body `{}`, de scanner een paniek. Eén beurt met een accent was daarmee een crasht.
/// De vorm hieronder kan dieklasse paniek niet maken: hij leest alleen `bytes` en snijdt `text` uitsluitend op
/// posities die een `find` hem gaf, of die hij zelf met volledige fence-lengtes optelt.
///
/// Waarom dit de regex *navolgt* en niet alleen erop lijkt: de scanner is getest tegen
/// `Regex::captures_iter` van precies dat originele patroon: 400 000 gegenereerde inputten uit vierentwintig
/// fence-fragmenten (losse ` ``` `, losse newline en losse `\r\n`, CRLF-header, vier en twee backticks,
/// `é`, emoji en een nbsp ertussen) plus zestien handgevallen, zonder één verschil.
fn tagged_blocks(text: &str, tag: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::new();
    let mut at = 0usize;
    // Zelfde iteratie als de regex: op élke positie een Entry proberen, bij mislukking één teken op.
    // `at + 3` hoeft niet te worden afgetopt, want `entry_at` leest `bytes[at]` en stopt daar bij een
    // opening die te dicht bij het einde staat voor drie backticks.
    while at + 3 <= bytes.len() {
        match entry_at(text, bytes, at, tag) {
            // De body leent uit `text`, dus zijn beginindex is direct beschikbaar en daarmee het einde
            // van de sluitende fence: het volgende onderzoek begint erna, zodat een blok zijn eigen
            // sluiting niet nog eens als opening ziet.
            Some(body) => {
                let open = body.as_ptr() as usize - text.as_ptr() as usize;
                blocks.push(body.to_string());
                at = open + body.len() + 3;
            }
            None => at += 1,
        }
    }
    blocks
}

/// De Entry op deze positie: `Some(&body)` wanneer hier een blok begint, `None` wanneer de caller één
/// teken mag opschuiven. De body leent uit `text`, dus de caller houdt er zijn index aan over zonder
/// ergens een streep of som te hoeven gokken.
fn entry_at<'a>(text: &'a str, bytes: &[u8], at: usize, tag: &str) -> Option<&'a str> {
    if bytes[at] != b'`' || bytes[at + 1] != b'`' || bytes[at + 2] != b'`' {
        return None;
    }
    let after_open = &text[at + 3..];
    let line_end = after_open.find('\n')?;
    if after_open[..line_end].trim_end_matches([' ', '\t']) != tag {
        return None;
    }
    let body_start = at + 3 + line_end + 1;
    let close = text[body_start..].find("```")?;
    Some(&text[body_start..body_start + close])
}

/// De calls uit één prompted blok: één object, een array davon, of geen.
fn calls_from_body(body: &str) -> Vec<ToolCall> {
    let parsed: Value = match serde_json::from_str(body.trim()) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    match parsed {
        Value::Array(entries) => entries.iter().filter_map(call_from).collect(),
        object @ Value::Object(_) => call_from(&object).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Een call uit één JSON-waarde, of `None` zonder naam.
///
/// Zowel `tool` als `name` wordt geaccepteerd, en zowel `arguments` als `args`, precies zoals de
/// origineler: een chattaal-model kiest er een en een call die om één woordverschuiving wordt verworpen,
/// verliest een hele beurt werk.
fn call_from(element: &Value) -> Option<ToolCall> {
    let object = element.as_object()?;
    let name = object.get("tool").and_then(Value::as_str).or_else(|| object.get("name").and_then(Value::as_str))?;
    let arguments = object
        .get("arguments")
        .or_else(|| object.get("args"))
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some(ToolCall { tool: name.to_string(), arguments, call_id: None })
}

/// Een native argumentstring die niet parseert, wordt leeg: de tool meldt het ontbreken, de loop crasht niet.
fn native_arguments(raw: &str) -> Value {
    serde_json::from_str(raw.trim()).ok().filter(Value::is_object).unwrap_or_else(|| Value::Object(Default::default()))
}

/// Een extra tool — een MCP-tool — die het prompted-protocol moet aanbieden.
///
/// Poort van `CallExtractor.ExtraTool`. De invariant is dat de prompt exact de set van de registry noemt,
/// dus een tool die de loop heeft, moet hier ook staan. `parameters` is de JSON-schema van die tool als
/// tekst: een prompted (niet-native) backend krijgt geen schema uit de request-body en moet het uit de
/// prompt lezen om de tool correct aan te roepen.
#[derive(Debug, Clone)]
pub struct ExtraTool {
    pub name: String,
    pub description: String,
    /// De JSON-schema van de tool, als tekst, voor een backend die geen request-body-schema krijgt.
    pub parameters: String,
}

/// De instructies die het prompted-formaat vastleggen, woordelijk zo opgesteld dat ze alleen de tools
/// aanbieden die deze beurt werkelijk heeft.
///
/// Poort van `CallExtractor.protocolFor`. Bewust naast de parser gezet, zoals in de origineler: de prompt
/// en de lezer die hem leest, leven op één plek, zodat het contract dat ze delen niet uit elkaar kan lopen.
/// Altijd Engels en streng: een chattaal-model dat alleen wordt *uitgenodigd* een formaat te gebruiken,
/// verzint er zelf een, dus deze noemt de ene aanvaarde vorm en verbiedt elke andere.
///
/// Hij noemt alleen de ingeschakelde tools, en dat is de eigenschap die hieronder getest wordt: een protocol
/// dat `run_command` of `git` noemt terwijl de registry ze weglaat, leert het model een tool aan te roepen die
/// dan als onbekend faalt; en een protocol dat een wél aanwezige tool verzwijgt, laat het model niet weten dat
/// het mag bouwen, testen of git bekijken.
///
/// Een backend die native function-calling doet, negeert de fenced vorm maar heeft baat bij dezelfde regels
/// (eerst lezen, geen calls verzinnen) en bij dezelfde accurate tool-lijst, daarom hoort deze tekst ook daar
/// bij de systeemprompt.
pub fn protocol_for(enabled: &[&str], extra: &[ExtraTool]) -> String {
    let can_read = enabled.contains(&"read_file");
    let can_edit = enabled.contains(&"edit_file");
    let can_run = enabled.contains(&"run_command");
    let can_git = enabled.contains(&"git");
    // De ingebouwde tools volgen de registry in vaste volgorde, in plaats van "begin altijd met
    // read_file". De reden is de invariant zelf: noemt de registry `edit_file` zonder `read_file`, dan zou
    // een vast eerste element juist die ene tool verzwijgen — precies de richting die de spec van de
    // origineler verbiedt ("a tool the model has but never hears about"). `read_file` blijft vooraan staan
    // zodra de registry hem heeft, want dat is de volgorde van de origineler en van diens voorbeeld.
    let mut advertised: Vec<&str> = Vec::new();
    for name in ["read_file", "edit_file", "run_command", "git"] {
        if enabled.contains(&name) {
            advertised.push(name);
        }
    }
    // De extra tools wonen buiten de ingebouwde set, dus hun namen komen uit `extra` en niet uit
    // `advertised`: dat is de lijst die de opsomming oplevert.
    let mut out = String::new();
    out.push_str("You can act on the project only through the tools below, and only with the exact format shown. The only tools available to you are: ");
    let named: Vec<String> = advertised.iter().map(|name| format!("\"{name}\"")).chain(extra.iter().map(|tool| format!("\"{}\"", tool.name))).collect();
    out.push_str(&named.join(", "));
    out.push_str(". Do not describe a tool call or a patch in prose, and never invent another tool name or markup. To act, emit one fenced block tagged ");
    out.push_str(FENCE_TAG);
    out.push_str(" that contains a single JSON object, one object per call.\n\n");

    // Het voorbeeld blijft letterlijk dat van de origineler, maar het staat er alleen wanneer de registry
    // `read_file` ook werkelijk aanbiedt. De twee regels die eronder staan (`old_text kiezen uit wat
    // read_file teruggaf`), verwijzen naar dezelfde tool en hangen daarom aan dezelfde voorwaarde: een
    // prompt die een call-voorbeeld toont voor een tool die er niet is, leert het model een onbekende call.
    if can_read {
        out.push_str("Read a file before you change it or reason about it, so you work from the current text:\n");
        out.push_str("  {\"tool\": \"read_file\", \"arguments\": {\"file\": \"relative/path\"}}\n\n");
    }

    if can_edit {
        out.push_str("Change a file by replacing text you saw in that read:\n");
        out.push_str("  {\"tool\": \"edit_file\", \"arguments\": {\"file\": \"relative/path\", \"edits\": [{\"old_text\": \"...\", \"new_text\": \"...\"}]}}\n\n");
    }
    if can_run {
        out.push_str("Run a shell command in the project folder (the user approves it first), for example to build or test:\n");
        out.push_str("  {\"tool\": \"run_command\", \"arguments\": {\"command\": \"mvn -B test\"}}\n\n");
    }
    if can_git {
        out.push_str("Inspect the repository with a read-only git command such as status, diff or log:\n");
        out.push_str("  {\"tool\": \"git\", \"arguments\": {\"command\": \"status\"}}\n\n");
    }

    for tool in extra {
        out.push_str("You may also call an external MCP tool, named and shaped exactly as below; use its advertised name and an arguments object that matches its schema:\n");
        out.push_str(&format!("  {{\"tool\": \"{}\", \"arguments\": <object matching {}>}}\n", tool.name, tool.parameters));
        out.push_str(&format!("   Purpose: {}\n\n", tool.description));
    }
    out.push_str("Prose may come before or after a block. Each call returns an observation that says ok true or ok false; read it before acting again. On ok false, do not send the same call again unchanged; fix the arguments or answer without a block.");
    // De origineler kiest hier tussen twee zinnen en laat bij `false` de `edit_file`-regel weg, ook
    // wanneer die tool wél bestaat maar `read_file` niet. Met de vlaggen hierboven wordt de bijzin per tool
    // toegevoegd: elke uitspraak noemt alleen een tool die aangeboden is, en `edit_file` houdt zijn eigen
    // advies ongeacht of `read_file` meedoet. De volgorde van beide zinnen volgt de origineler.
    if can_read {
        out.push_str(" On ok true from read_file, choose old_text from the text it returned.");
    }
    if can_edit {
        out.push_str(" On ok true from edit_file, either answer or make the next call you still need.");
    }
    if !(can_read || can_edit) {
        out.push_str(" Otherwise either answer or make the next call you still need.");
    }
    out
}

/// De ingebouwde lezer: native calls eerst, dan de prompted fences.
pub struct PromptedCallExtractor;

impl CallExtractor for PromptedCallExtractor {
    fn extract(&self, turn: &Turn) -> Vec<ToolCall> {
        let mut calls: Vec<ToolCall> = turn
            .function_calls
            .iter()
            .enumerate()
            .map(|(index, call)| ToolCall {
                tool: call.name.clone(),
                arguments: native_arguments(&call.arguments),
                // Zelfde val-naam als de origineler: een call zonder backend-id verdient er één voor de
                // observatie, zodat het model iets heeft om naar te verwijzen.
                call_id: Some(call.call_id.clone().unwrap_or_else(|| format!("native-{index}"))),
            })
            .collect();

        calls.extend(tagged_blocks(&turn.text, FENCE_TAG).iter().flat_map(|body| calls_from_body(body)));
        calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::protocol::FunctionCall;
    use serde_json::json;

    /// Een beurt met alleen proza, de vorm waarin een prompted model antwoordt.
    fn prose(text: impl Into<String>) -> Turn {
        Turn::text(text)
    }

    /// Eén call in het veld, met de arguments zoals de lezer ze teruggevraagd heeft.
    fn call_of(calls: &[ToolCall], index: usize) -> (&str, &Value, Option<&str>) {
        (calls[index].tool.as_str(), &calls[index].arguments, calls[index].call_id.as_deref())
    }

    #[test]
    fn a_tagged_block_becomes_a_call() {
        let turn = prose(concat!(
            "Ik lees het eerst.\n",
            "```baudet-tool\n",
            "{\"tool\": \"read_file\", \"arguments\": {\"file\": \"a.txt\"}}\n",
            "```\n",
            "Daarna zie ik verder.",
        ));
        let calls = PromptedCallExtractor.extract(&turn);
        assert_eq!(calls.len(), 1);
        assert_eq!(call_of(&calls, 0), ("read_file", &json!({"file": "a.txt"}), None));
    }

    #[test]
    fn prose_around_a_block_is_ignored_and_several_blocks_keep_their_order() {
        let turn = prose(concat!(
            "```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"status\"}}\n```\n",
            "en dan\n",
            "```baudet-tool\n{\"name\": \"read_file\", \"args\": {\"file\": \"b.txt\"}}\n```\n",
        ));
        let calls = PromptedCallExtractor.extract(&turn);
        assert_eq!(calls.len(), 2, "naam-als-`name` en argumenten-als-`args`Tellers doen mee, zoals in de origineler");
        assert_eq!(calls[0].tool, "git");
        assert_eq!(call_of(&calls, 1), ("read_file", &json!({"file": "b.txt"}), None));
    }

    #[test]
    fn an_array_block_is_a_fan_out() {
        let turn = prose(concat!(
            "```baudet-tool\n",
            "[{\"tool\": \"read_file\", \"arguments\": {\"file\": \"a\"}}, {\"tool\": \"read_file\", \"arguments\": {\"file\": \"b\"}}]\n",
            "```\n",
        ));
        let calls = PromptedCallExtractor.extract(&turn);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].arg_str("file"), Some("b"));
    }

    #[test]
    fn a_block_that_will_not_parse_is_skipped_not_thrown() {
        // Eén verprutste call mag de beurt niet laten falen: het antwoord blijft staan.
        let broken = prose("```baudet-tool\n{\"tool\": \"read_file\", \n```\n");
        assert!(PromptedCallExtractor.extract(&broken).is_empty());

        // Een kapot blok vóór een goed blok mag dat goede blok niet opeeten.
        let mixed = prose(concat!(
            "```baudet-tool\n{kapot\n```\n",
            "```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"log\"}}\n```\n",
        ));
        let calls = PromptedCallExtractor.extract(&mixed);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arg_str("command"), Some("log"));
    }

    #[test]
    fn ordinary_code_is_not_a_call() {
        // De tag moet er staan; `DOT_MATCHES_ALL` maakt een greedy scan van het model geen excuse.
        let turn = prose("```rust\nlet x = \"{\\\"tool\\\": \\\"read_file\\\"}\";\n```\n");
        assert!(PromptedCallExtractor.extract(&turn).is_empty());
        // Een opening zonder sluiting bestaat niet voor de regex, dus niet voor ons.
        assert!(PromptedCallExtractor.extract(&prose("```baudet-tool\n{\"tool\": \"git\"}\n")).is_empty());
    }

    #[test]
    fn a_tag_with_trailing_spaces_is_not_a_block() {
        // Gemeten tegen de regex van de origineler: de header mag spaties en tabs dragen, maar `\r`
        // hoort niet in de klasse vóór de `\n`, dus een CRLF-header is géén blok.
        assert_eq!(tagged_blocks("```baudet-tool  \n{}\n```\n", FENCE_TAG), vec!["{}\n".to_string()]);
        assert_eq!(tagged_blocks("```baudet-tool\t\n{}\n```\n", FENCE_TAG), vec!["{}\n".to_string()]);
        assert!(tagged_blocks("```baudet-tool\r\n{}\n```\n", FENCE_TAG).is_empty(), "\\r staat vóór de \\n");
        // Noch een losse ``` zonder tag, nog ` ``tag ` met twee backticks is een blok.
        assert!(tagged_blocks("```\n{}\n```\n", FENCE_TAG).is_empty());
        assert!(tagged_blocks("``baudet-tool\n{}\n```\n", FENCE_TAG).is_empty());
    }

    #[test]
    fn a_near_miss_tag_cannot_hide_the_next_block() {
        // De regex begint op élke positie, dus in ` ```baudet ```baudet-tool ` kan de match nog op de
        // tweede backtick van de opening beginnen: het wél geldige blok daarna telt mee. Drie tekens
        // overslaan bij een mislukte opening zou het verliezen; één opschuiven, wat het algoritme doet, niet.
        let text = "```baudet ```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"show\"}}\n```\n";
        let calls = PromptedCallExtractor.extract(&prose(text));
        assert_eq!(calls.len(), 1, "het echte blok mag niet verloren gaan");
        assert_eq!(calls[0].arg_str("command"), Some("show"));
    }

    #[test]
    fn native_calls_come_first_and_win() {
        let mut turn =
            prose("```baudet-tool\n{\"tool\": \"read_file\", \"arguments\": {\"file\": \"uit de tekst\"}}\n```\n");
        turn.function_calls = vec![FunctionCall {
            call_id: Some("call_1".into()),
            name: "git".into(),
            arguments: "{\"command\": \"status\"}".into(),
        }];
        let calls = PromptedCallExtractor.extract(&turn);
        assert_eq!(calls.len(), 2, "beide kanalen worden geboden, native eerst");
        assert_eq!(call_of(&calls, 0), ("git", &json!({"command": "status"}), Some("call_1")));
        assert_eq!(calls[1].arg_str("file"), Some("uit de tekst"));
    }

    #[test]
    fn a_native_call_without_an_id_gets_a_stable_one() {
        let mut turn = prose("");
        turn.function_calls = vec![FunctionCall { call_id: None, name: "read_file".into(), arguments: "{}".into() }];
        let calls = PromptedCallExtractor.extract(&turn);
        assert_eq!(calls[0].call_id.as_deref(), Some("native-0"));
    }

    #[test]
    fn unparseable_native_arguments_become_an_empty_object() {
        // De tool meldt de ontbrekende velden aan het model; de loop crasht niet.
        let mut turn = prose("");
        turn.function_calls =
            vec![FunctionCall { call_id: None, name: "read_file".into(), arguments: "geen json".into() }];
        let calls = PromptedCallExtractor.extract(&turn);
        assert_eq!(calls[0].arguments, json!({}));
        // Een array als argumenten is wél JSON, maar geen object: hetzelfde lege resultaat.
        turn.function_calls[0].arguments = "[1,2]".into();
        assert_eq!(PromptedCallExtractor.extract(&turn)[0].arguments, json!({}));
    }

    #[test]
    fn prose_before_a_block_survives_the_scan() {
        // Gemeten tegen de regex: `"é```baudet-tool...` gaf de regex body `{}`, de eerste versie van
        // deze lus een paniek, want hij sneed `text` op `start` vóórdat hij de backticks controleerde en
        // `é` daar midden in een teken viel. Een accent in een modelantwoord mag geen crasht zijn.
        let accented = "\u{00e9}```baudet-tool\n{\"tool\": \"git\"}\n```\n";
        assert_eq!(PromptedCallExtractor.extract(&prose(accented)).len(), 1);
        // Of acht bytes vóór een blok: de openingspositie valt dan zelf in een meerbytes teken.
        let many = "\u{00e9}".repeat(4) + "```baudet-tool\n{\"tool\": \"git\"}\n```\n";
        assert_eq!(PromptedCallExtractor.extract(&prose(many)).len(), 1);
        // Een losse backtick en een tab, de randen van de byte-lus zelf.
        assert!(tagged_blocks("`", FENCE_TAG).is_empty());
        assert!(tagged_blocks("\t\t", FENCE_TAG).is_empty());
    }

    #[test]
    fn four_backticks_are_not_a_longer_fence() {
        // Gemeten: de regex vraagt nergens om een vierde backtick, dus de sluiting is de eerste drie en
        // de vierde is proza. Een scanner die "een fence sluit met evenveel backticks als hem opende"
        // navolgt, zou hier niets teruggeven.
        assert_eq!(tagged_blocks("```baudet-tool\n{}\n````", FENCE_TAG), vec!["{}\n".to_string()]);
        assert_eq!(tagged_blocks("```baudet-tool\n{}\n``````", FENCE_TAG), vec!["{}\n".to_string()]);
        // En andersom: vier backticks openden géén blok, maar de drie binnen hem zijn wel een opening.
        assert_eq!(tagged_blocks("````baudet-tool\n{}\n```", FENCE_TAG), vec!["{}\n".to_string()]);
    }

    #[test]
    fn a_block_without_a_name_is_not_a_call() {
        for body in ["{}", "{\"arguments\": {\"file\": \"a\"}}", "\"{\"tool\": \"read_file\"}\""] {
            let turn = prose(format!("```baudet-tool\n{body}\n```\n"));
            assert!(PromptedCallExtractor.extract(&turn).is_empty(), "{body}");
        }
    }

    #[test]
    fn the_protocol_names_exactly_the_enabled_tools() {
        // De kern van `protocolFor`: registry en prompt moeten dezelfde noemen. Vier vormen van de
        // ingebouwde set, want de UI zet de tools aan op de maprechten en niet op een schakelaar.
        let only_read = protocol_for(&["read_file"], &[]);
        assert!(only_read.contains("\"read_file\""), "{only_read}");
        for absent in ["edit_file", "run_command", "git"] {
            assert!(!only_read.contains(absent), "een tool die er niet is, mag niet aangeboden worden: {absent}");
        }
        // De sluitzin noemt alleen wat er is: bij alleen lezen is dat het read-advies van de origineler,
        // en de "Otherwise"-zin is er uitsluitend wanneer er géén van beide tools staat.
        assert!(only_read.ends_with("On ok true from read_file, choose old_text from the text it returned."));

        let editing = protocol_for(&["read_file", "edit_file"], &[]);
        assert!(editing.contains("\"edit_file\""));
        assert!(editing.contains("\"old_text\""));
        assert!(editing.ends_with("On ok true from edit_file, either answer or make the next call you still need."));
        assert!(!editing.contains("run_command"), "uitgeschreven rechten blijven weg uit de prompt");

        // De richting die de vorige versie doorliet: een registry die `edit_file` aanbiedt zonder
        // `read_file`. Een vast eerste element van de opsomming zou dán de ene tool verzwijgen die het
        // model wél heeft, en de bijzin zou een advies geven over een tool die niemand kan aanroepen.
        let edit_without_read = protocol_for(&["edit_file"], &[]);
        assert!(edit_without_read.contains("\"edit_file\""), "aanwezig blijft aangeboden: {edit_without_read}");
        assert!(!edit_without_read.contains("\"read_file\""), "afwezig blijft verzwegen: {edit_without_read}");
        assert!(!edit_without_read.contains("old_text from the text it returned"), "geen advies over een afwezige tool");
        assert!(edit_without_read.ends_with("On ok true from edit_file, either answer or make the next call you still need."));

        // Zonder élke tool blijft er een_protocol over dat niets belooft: de opsomming mag geen tool noemen
        // die de registry niet heeft, ook niet als de registry ledig is.
        let empty = protocol_for(&[], &[]);
        for tool in ["read_file", "edit_file", "run_command", "git"] {
            assert!(!empty.contains(&format!("\"{tool}\"")), "ledige registry adverteert niets: {tool}");
        }
        assert!(empty.contains(FENCE_TAG), "de vorm blijft, ook zonder tools");

        let full = protocol_for(&["read_file", "edit_file", "run_command", "git"], &[]);
        for tool in ["read_file", "edit_file", "run_command", "git"] {
            assert!(full.contains(&format!("\"{tool}\"")), "{tool} mist in {full}");
        }
        // De volgorde van de opsomming volgt de registry, en de opsomming is één regel: een model dat de
        // lijst parseert, mag niet twee interpretaties krijgen.
        let listing = full.split(". Do not describe").next().expect("één opsomming");
        assert!(listing.find("\"read_file\"") < listing.find("\"edit_file\""));
        assert!(listing.find("\"run_command\"") < listing.find("\"git\""));

        // De fenced vorm zelf: de tag en één object per call, met dezelfde letterlijke tag die de lezer hier
        // boven zoekt. Dwaalt die tag weg uit deze tekst, dan is de prompt een contract dat niemand leest.
        for text in [&only_read, &editing, &full] {
            assert!(text.contains(FENCE_TAG), "de prompt moet de tag noemen die deze module leest");
        }
        // Één voorbeeld per aangeboden tool, in de volgorde van de opsomming: het voorbeeld is wat een
        // prompted model kopieert, dus een ontbrekend voorbeeld is een tool die het nooit zal aanroepen.
        assert!(editing.contains("{\"tool\": \"edit_file\""));
        assert!(full.contains("{\"tool\": \"run_command\""));
        assert!(full.contains("{\"tool\": \"git\""));
    }

    #[test]
    fn an_extra_tool_is_advertised_with_its_schema() {
        let extra = ExtraTool {
            name: "mcp__time__now".to_string(),
            description: "Current time".to_string(),
            parameters: "{\"type\":\"object\"}".to_string(),
        };
        let text = protocol_for(&["read_file"], &[extra]);
        assert!(text.contains("\"mcp__time__now\""), "ook een extra tool moet geadverteerd worden: {text}");
        assert!(text.contains("Purpose: Current time"));
        assert!(text.contains("{\"type\":\"object\"}"), "een prompted backend leest de schema uit de prompt");
        // De ingebouwde regels blijven onveranderd staan naast de extra: de extra vervangt niets.
        assert!(text.contains("{\"tool\": \"read_file\""));
    }
}
