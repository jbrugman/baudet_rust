//! De enige bron van de read-only-belofte voor commando-goedkeuring.
//!
//! Poort van `tool/SafeCommands.kt`. Het onderscheid dat deze module maakt, is de reden dat hij exists:
//! `"echo ok | tee build.log"` en `"echo ok | tee ~/.ssh/authorized_keys"` zijn dezelfde buis met een
//! ander slotwoord, en een generieke "allow-list van programma's" zou na één goedkeuring beide dekken.
//! Daarom gelden hier twee onafhankelijke Fragen, die bewust niet samenvallen:
//!
//! * [`is_safe`] — mag dit zonder mens draaien? Alleen één simpel, bewezen read-only commando. Een keten
//!   krijgt hier geen ja, hoe ongevaarlijk zijn delen ook zijn.
//! * [`trusted_key`] — waar mag één "altijd" op? Hier mag een keten wél, maar het gezin is maar zo veilig
//!   als zijn zwakste segment, dus één `rm`- of `--output`-segment kost het hele teken.
//!
//! Dit verbreedt niet wat een commando kan aanraken: de mapgrens en de tijdelijke-map-omheining rond de
//! run blijven staan zoals ze zijn. En de poort blijft uit tot de persoon `auto_approve_safe` zet.
//!
//! De onveilige-tekens worden op de **ruwe** string gelezen bij [`is_safe`] — `grep 'a; rm -rf /'` is een grep
//! met een bedoelde quote en wordt daarom geweigerd. Citaat-bewustheid behoort tot [`trusted_key`], waar
//! de mens garant staat voor het programma en een quote geen shell-woord meer is.

use std::collections::HashSet;

/// Shell-tekens die kunnen ketenen, omleiden, substitueren of groeperen: niets daarvan mag automatisch
/// draaien. Een quote staat er bewust niet in, want een quoted read-only grep is precies de read die deze
/// poort hoort te vertrouwen.
const UNSAFE_CHARS: [char; 17] = [';', '&', '|', '<', '>', '$', '(', ')', '{', '}', '\\', '`', '*', '?', '!', '#', '~'];
/// Twee regeleindes behoren ook tot deUnsafe-tekens; als `char`-array apart omdat ze in de oorspronkelijke
/// list achteraan staan en hier in de tekst van [`UNSAFE_CHARS`] meelezen als losse regels.
const NEWLINE_UNSAFE: [char; 2] = ['\n', '\r'];

/// Tekens die nooit een gezins-teken mogen verdienen, zelfs niet voor één segment van een keten:
/// omleiding, substitutie, glob, accolade, regeleinde (een tweede commando) of commentaar. In tegenstelling
/// tot [`UNSAFE_CHARS`] laten de segment-scheiders `;`, `|` en `&` hier weg, juist omdat [`trusted_key`]
/// een keten van anders veilige segmenten een gezin gunt.
const NON_SEGMENT_UNSAFE: [char; 14] = ['<', '>', '$', '(', ')', '{', '}', '\\', '`', '*', '?', '!', '#', '~'];

/// De tekens die bash binnen dubbele quotes nog steeds expanteert: substitutie, backtick, backslash en de
/// history-vlag. Een enkele quote is volledig literaal en heeft geen van deze nodig.
const DQ_ACTIVE: [char; 4] = ['$', '`', '\\', '!'];

/// Het woord-equivalent van `hasUnsafeOutsideQuotes`: bekijkt `s` en beslist of er een teken van
/// `unsafe` staat waar bash het als syntax zou lezen.
///
/// Een enkele-quote-lus wordt overgeslagen (bash leest hem literaal); binnen dubbele quotes tellen alleen de
/// tekens die bash daar nog expanteert, met backslash als escape van de volgende. Een teken binnen geen
/// quote is syntax en dus verboden. Een niet-gesloten quote is onveilig — de conservatieve richting voor
/// een poort waar een onjuist "veilig"-oordeel de gevaarlijke fout is.
fn has_unsafe_outside_quotes(s: &str, unsafe_chars: &[char]) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if c == '\\' {
                    i += 2;
                    continue;
                } else if c == '"' {
                    quote = None;
                } else if DQ_ACTIVE.contains(&c) && unsafe_chars.contains(&c) {
                    return true;
                }
            }
            _ => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if unsafe_chars.contains(&c) || NEWLINE_UNSAFE.contains(&c) {
                    return true;
                }
            }
        }
        i += 1;
    }
    quote.is_some()
}

/// De programma's die kunnen lezen. Elk wordt daarna vrijgesproken of veroordeeld door zijn vlaggen in
/// [`deny_flags`]. Een set in plaats van de lineaire lijst van de origineler, want deze vraag wordt per
/// commando vaker gesteld dan dat hij wordt gebouwd; de inhoud is woord-voor-woord dezelfde 21 namen.
fn read_only_programs() -> &'static HashSet<&'static str> {
    static SET: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        [
            "ls", "pwd", "cat", "head", "tail", "wc", "grep", "rg", "find", "sort", "uniq", "echo", "date", "uname",
            "whoami", "df", "du", "file", "stat", "tree", "which",
        ]
        .into_iter()
        .collect()
    })
}

/// Vlaggen die een nominaal read-only programma schrijf- of exec-macht geven, per programma. Zonder deze
/// tabel zou een naam op de read-only-lijst in zijn geheel vertrouwd worden, terwijl `sort -o` schrijft, `rg --pre`
/// een filter-programma draait, `find -delete` wist en `date -s` de klok zet.
fn deny_flags(program: &str) -> Option<&'static [&'static str]> {
    match program {
        "sort" => Some(&["-o", "--output", "--compress-program"]),
        "rg" => Some(&["--pre", "--hostname-bin"]),
        "find" => Some(&["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fls", "-fprintf"]),
        "date" => Some(&["-s", "--set"]),
        _ => None,
    }
}

/// git-subcommando's die de repository alleen inspecteren. Alles anders wordt geweigerd: commit, push,
/// checkout, stash, clean en fetch schrijven allemaal, en een poort die zonder mens draait kan dat niet per
/// aanroep afwegen.
fn git_read_only_subcommands() -> &'static [&'static str] {
    &["status", "log", "diff", "show", "blame", "shortlog", "ls-files", "ls-tree", "rev-parse", "describe"]
}

/// git-vlaggen die de read-only-belofte breken: ze schrijven een bestand, draaien een ander programma of
/// wijzen git naar een andere repository. Als prefix vergeleken, want git accepteert een waarde aan de vlag
/// geplakt, dus `--output=name` en `-c core.pager=rm` zijn dezelfde hazard als de kale vlag.
fn git_escape_flags() -> &'static [&'static str] {
    &[
        "--output",
        "--ext-diff",
        "--paginate",
        "-P",
        "-c",
        "--exec-path",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--super-prefix",
    ]
}

/// Programma's die nooit gezinsvertrouwen kunnen verdienen, want één aanroep ervan kan willekeurige code
/// draaien, data vernietigen of van de machine ontsnappen. Een naam hier maakt [`trusted_key`] op `null`,
/// dus zelfs als een slechte string in de vertrouwde lijst wordt getypt kan hij nooit aan een echte run
/// matchen; "altijd" valt dan terug op de letterlijke pin.
fn is_blocked_program(program: &str) -> bool {
    const BLOCKED: [&str; 74] = [
        // shells en interpreters: één argument is willekeurige code
        "sh",
        "bash",
        "zsh",
        "dash",
        "ksh",
        "fish",
        "csh",
        "tcsh",
        "python",
        "python2",
        "python3",
        "pypy",
        "node",
        "deno",
        "bun",
        "ts-node",
        "ruby",
        "perl",
        "php",
        "lua",
        "tclsh",
        "expect",
        "osascript",
        "awk",
        "gawk",
        "sed",
        // directe vernietiging of rechtsverandering
        "rm",
        "rmdir",
        "mv",
        "cp",
        "dd",
        "mkfs",
        "shred",
        "truncate",
        "chmod",
        "chown",
        "chgrp",
        // privilege-, proces- en systeemcontrole
        "sudo",
        "su",
        "doas",
        "kill",
        "killall",
        "pkill",
        "reboot",
        "shutdown",
        "halt",
        "poweroff",
        "mount",
        "umount",
        "launchctl",
        "systemctl",
        "service",
        // netwerk: halen-en-draaien en bediening op afstand
        "curl",
        "wget",
        "nc",
        "ncat",
        "socat",
        "ssh",
        "scp",
        "sftp",
        "rsync",
        // schrijvers en exec-trampolines: tee/install/patch/touch/mkdir/ln plaatsen of herschrijven bestanden
        // waar hun argument wijst (de mapgrens heint een execute niet), en xargs/env bouwen een commandoregel
        // uit hun input. Een gezins-teken op de kale naam zou dus `echo key | tee ~/.ssh/authorized_keys`
        // dekken na één keer `echo ok | tee build.log`.
        "tee",
        "install",
        "patch",
        "touch",
        "mkdir",
        "ln",
        "xargs",
        "env",
        // archief- en compressieschrijvers: één "altijd" op een onschuldige pack mag niet later laten schrijven
        // waar ook maar de output-wijzer heen wijst.
        "tar",
        "gzip",
        "bzip2",
        "zip",
        "cpio",
    ];
    BLOCKED.contains(&program)
}

/// git-subcommando's die werk kunnen weggooien of history herschrijven, dus nooit gezinsvertrouwen
/// verdienen: één klik op een ongerelevante git-opdracht mag niet later een clean, reset of checkout het
/// werkterrein onbeheerd laten wissen. Elk ander subcommando, inclusief de schrijvende die een ontwikkelaar
/// met opzet herhaalt (add, commit, push, branch), mag een gezin worden zodra de persoon daarom vraagt.
fn git_unsafe_trust_subcommands() -> &'static [&'static str] {
    &["clean", "reset", "checkout", "restore", "switch", "rebase", "filter-branch"]
}

/// Relatieve build-wrappers die gezinsvertrouwen bij naam mogen verdienen. De eigen `./mvnw`/`./gradlew`-
/// launcher van een project is de gewoonlijke manier om te rebouwen, dus één "altijd" moet de volgende
/// build-variant dekken — maar alleen deze twee namen, zodat een geplante of hernoemde executable niet de
/// identiteit van een wrapper kan lenen.
fn is_build_wrapper(program: &str) -> bool {
    matches!(program, "mvnw" | "gradlew")
}

/// Het teken dat de programma-actually ontvangt, met de shell-quotes verwijderd voor de deny-vergelijking.
/// De shell haalt quotes weg vóórdat het programma start, dus een deny-vlag in quotes bereikt het programma
/// nog steeds kaal; een vergelijking op de gequote tekst alleen zou die laten glippen.
fn strip_quotes(token: &str) -> String {
    token.chars().filter(|c| *c != '\'' && *c != '"').collect()
}

/// Of `token` aan `program` een schrijf- of exec-vlag geeft. Een lange vlag wordt als prefix vergeleken, dus
/// zowel `--output bestand` als `--output=bestand` wordt gevangen. Een korte vlag wordt gevangen zowel op
/// zichzelf als gebundeld in een cluster, want `sort -no` zet zijn schrijvende `-o`; een dubbele-streep-token
/// wordt nooit per letter afgezocht, dus een onschuldige lange optie valt nooit op een gedeelde letter.
fn carries_write_or_exec_flag(program: &str, token: &str) -> bool {
    let Some(deny) = deny_flags(program) else { return false };
    let bare = strip_quotes(token);
    if !bare.starts_with('-') {
        return false;
    }
    deny.iter().any(|flag| {
        if flag.len() == 2 {
            !bare.starts_with("--") && bare.len() > 1 && bare[1..].contains(&flag[1..])
        } else {
            bare.starts_with(flag)
        }
    })
}

/// Hoeveel operanden `args` aan het programma meegeven, met eerbiediging van de end-of-options-marker. Vóór
/// een eenzame `--` is een token met een streep een optie; van die `--` af is élk token een operand, want
/// dat is wat het programma dan leest. Een naïefellen op een leidende streep liet `uniq -- in.txt -out.txt`
/// als één operand zien terwijl de shell uniq twee bestanden geeft en het de tweede schrijft.
fn count_positional_operands(args: &[String]) -> usize {
    let mut operands = 0usize;
    let mut options_ended = false;
    for token in args {
        let bare = strip_quotes(token);
        if options_ended {
            operands += 1;
        } else if bare == "--" {
            options_ended = true;
        } else if !bare.starts_with('-') {
            operands += 1;
        }
    }
    operands
}

/// Of `program` via een positionele operand schrijft in plaats van via een vlag. `uniq` is de enige naam op
/// de read-only-lijst die een uitvoerbestand als tweede positionele argument neemt.
fn writes_to_positional_output(program: &str, args: &[String]) -> bool {
    program == "uniq" && count_positional_operands(args) >= 2
}

/// Waar `tokens` een git-oproep is die alleen inspecteert: een bekende read-only subopdracht en geen
/// ontsnap-vlag. Een kaal `git` toont usage; er is niets toe te staan en niets te vrezen, maar het is ook
/// geen inspectie, dus het houdt zijn vraag zoals elk commando dat deze poort niet kent.
fn git_is_read_only(tokens: &[&str]) -> bool {
    let Some(subcommand) = tokens.get(1) else { return false };
    if !git_read_only_subcommands().contains(subcommand) {
        return false;
    }
    tokens.iter().all(|token| {
        let bare = strip_quotes(token);
        !git_escape_flags().iter().any(|flag| bare.starts_with(flag))
    })
}

/// True alleen voor één simpel commando waarvan het programma een kale bekende read-only naam is, zonder
/// shell-syntax, zonder de schrijf- of exec-vlaggen van dat programma en zonder positionele uitvoer. Een
/// programma als pad wordt meteen geweigerd: alleen de kale naam wordt vertrouwd, dus een geplante binary
/// kan niet de identiteit van een veilig commando lenen en in zijn plaats automatisch draaien.
pub fn is_safe(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }
    // De read-only-poort leest de ruwe string, dus een metateken verborgen in een quote wordt hier nog
    // steeds geweigerd: `grep 'a; rm'` wordt nooit automatisch gedraaid.
    if trimmed.chars().any(|c| UNSAFE_CHARS.contains(&c) || NEWLINE_UNSAFE.contains(&c)) {
        return false;
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let program = tokens[0];
    if program.contains('/') {
        return false;
    }
    if program == "git" {
        return git_is_read_only(&tokens);
    }
    if !read_only_programs().contains(program) {
        return false;
    }
    let args: Vec<String> = tokens[1..].iter().map(|t| t.to_string()).collect();
    if tokens[1..].iter().any(|t| carries_write_or_exec_flag(program, t)) {
        return false;
    }
    !writes_to_positional_output(program, &args)
}

/// Of een programma dat aan [`trusted_key`] wordt gegeven gezinsvertrouwen mag verdienen, bij naam alleen.
/// Een kale naam wordt beoordeeld door de blocklist. Een relatieve wrapper uit één segment wordt pas
/// toegelaten als zijn basisnaam een bekende launcher is; een map-pad, een parent-hop, een absoluut pad of
/// enige andere wrapper wordt geweigerd.
fn is_family_trusted_program(program: &str) -> bool {
    if !program.contains('/') {
        return true;
    }
    program.strip_prefix("./").is_some_and(|name| !name.contains('/') && is_build_wrapper(name))
}

/// Het gezins-teken voor één segment dat al vrij is van scheiders, of `None` wanneer een van de eisen faalt.
/// Dit is de enkel-commando-regel die [`trusted_key`] op élk segment van een keten toepast, dus een keten
/// wordt alleen vertrouwd wanneer elk deel het apart ook zou worden.
fn single_family_key(segment: &str) -> Option<String> {
    if segment.trim().is_empty() {
        return None;
    }
    let tokens: Vec<&str> = segment.split_whitespace().collect();
    let program = tokens[0];
    if !is_family_trusted_program(program) || is_blocked_program(program) {
        return None;
    }
    if program == "git" {
        let subcommand = *tokens.get(1)?;
        if git_unsafe_trust_subcommands().contains(&subcommand) {
            return None;
        }
        let escapes = tokens.iter().any(|token| {
            let bare = strip_quotes(token);
            git_escape_flags().iter().any(|flag| bare.starts_with(flag))
        });
        return if escapes { None } else { Some(format!("git {subcommand}")) };
    }
    let args: Vec<String> = tokens[1..].iter().map(|t| t.to_string()).collect();
    if tokens[1..].iter().any(|t| carries_write_or_exec_flag(program, t)) {
        return None;
    }
    if writes_to_positional_output(program, &args) {
        return None;
    }
    Some(program.to_string())
}

/// `s` splitsen in segmenten en de operatoren ertussen, waarbij de shell-scheiders `&&`, `||`, `|`, `;` en
/// `&` alleen buiten elke quote worden herkend, zodat een scheider binnen een quoted argument (een commit-
/// bericht met een `&&`) niet voor een keten aanziet. `None` voor een vorm waarvoor geen gezin bestaat: een
/// onbalansquote, een leeg segment (een leidende, trailing of dubbele scheider) of een operator-run die niet
/// exact één canonieke operator is. De operatoren blijven bewaard, zodat het gezin de vorm van de keten
/// onthoudt en `git diff | tail` nooit op `git diff ; tail` matcht.
fn split_top_level(s: &str) -> Option<(Vec<String>, Vec<String>)> {
    let chars: Vec<char> = s.chars().collect();
    let mut segments: Vec<String> = Vec::new();
    let mut operators: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            current.push(c);
            i += 1;
            continue;
        }
        if c == ';' || c == '|' || c == '&' {
            let seg = current.trim().to_string();
            if seg.is_empty() {
                return None;
            }
            segments.push(seg);
            current.clear();
            let mut j = i;
            while j < chars.len() && chars[j] == c {
                j += 1;
            }
            let run = j - i;
            let op = match (c, run) {
                (';', 1) => ";",
                ('|', 1) => "|",
                ('|', 2) => "||",
                ('&', 1) => "&",
                ('&', 2) => "&&",
                _ => return None,
            };
            operators.push(op.to_string());
            i = j;
            continue;
        }
        current.push(c);
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    let last = current.trim().to_string();
    if last.is_empty() {
        return None;
    }
    segments.push(last);
    Some((segments, operators))
}

/// Het gezins-teken waar één "altijd" vertrouwen aan mag geven, of `None` wanneer de vorm van het commando
/// gezinsvertrouwen onveilig maakt. spaties en tabs worden gevouwen zodat het opgeslagen teken en het
/// gecontroleerde teken overeenkomen ook al verschilt de spacing, terwijl een regeleinde blijft zoals het is
/// en geweigerd wordt: door een shell gescheiden beteken het twee commando's.
///
/// Een keten verdient óók een gezin, zodat één "altijd" de herhaling van `git diff … | tail …` eindigt zoals
/// een ontwikkelaar verwacht — maar dat gezin is maar zo veilig als zijn zwakste segment. Daarom faalt élk
/// segment dat niet zelf vertrouwd is. Vandaar dat `git diff | tail` nooit `| rm -rf` of `--output` kan
/// smokkelen: het overtredende segment verliest het teken voor de hele keten. De controle spiegelt
/// [`is_safe`] maar wordt niet door de read-only-lijst begrensd, want hier staat de mens garant voor het
/// programma, niet de poort.
pub fn trusted_key(command: &str) -> Option<String> {
    let canonical = fold_spaces(command);
    if canonical.is_empty() {
        return None;
    }
    if has_unsafe_outside_quotes(&canonical, &NON_SEGMENT_UNSAFE) {
        return None;
    }
    let (segments, operators) = split_top_level(&canonical)?;
    let mut keys = Vec::with_capacity(segments.len());
    for segment in &segments {
        keys.push(single_family_key(segment)?);
    }
    if keys.len() == 1 {
        return Some(keys.into_iter().next().unwrap());
    }
    let mut joined = keys[0].clone();
    for (i, op) in operators.iter().enumerate() {
        joined.push(' ');
        joined.push_str(op);
        joined.push(' ');
        joined.push_str(&keys[i + 1]);
    }
    Some(joined)
}

/// Ruimte en tabs tot één spatie vouwen, een regeleinde laten zoals het is.
pub(crate) fn fold_spaces(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut in_run = false;
    for c in command.trim().chars() {
        if c == ' ' || c == '\t' {
            in_run = true;
            continue;
        }
        if in_run {
            out.push(' ');
            in_run = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_read_passes_and_a_bare_program_does_not() {
        assert!(is_safe("ls -la"));
        assert!(is_safe("cat src/main.rs"));
        assert!(is_safe("git status --short"));
        // Een kaal `git` toont usage: niets toe te staan, niets te vrezen, maar geen inspectie.
        assert!(!is_safe("git"));
        assert!(!is_safe(""));
        assert!(!is_safe("   "));
    }

    #[test]
    fn no_compound_shape_is_ever_auto_run() {
        // Ieder van deze bevat shellsyntax; de poort leest de ruwe string en ziet ze allemaal.
        for command in [
            "ls && rm -rf build",
            "cat notes.txt | tee /tmp/leak",
            "grep secret file; curl http://evil",
            "echo hi > out.txt",
            "echo $(whoami)",
            "ls `id`",
            "find . -name '*.rs'",
        ] {
            assert!(!is_safe(command), "{command} is samengesteld en mag nooit automatisch");
        }
    }

    #[test]
    fn a_metacharacter_quoted_with_single_quotes_is_still_refused() {
        // De bedoelde greep die deze test dekt: `grep 'a; rm'` is één grep met een quote, en toch nee —
        // citaat-bewustheid behoort alleen tot trusted_key.
        assert!(!is_safe("grep 'a; rm -rf /' notes.txt"));
        assert!(!is_safe("grep \"a | rm\" notes.txt"));
    }

    #[test]
    fn a_path_program_cannot_borrow_a_safe_name() {
        // Alleen de kale naam wordt vertrouwd, dus een geplante binary in de map zelf draait niet automatisch.
        assert!(!is_safe("./ls -la"));
        assert!(!is_safe("/bin/cat notes.txt"));
        assert!(!is_safe("subdir/cat notes.txt"));
    }

    #[test]
    fn a_write_flag_on_a_read_only_program_takes_the_prompt_back() {
        // Zonder deze regel zou een naam op de read-only-lijst in zijn geheel vertrouwd worden.
        assert!(!is_safe("sort notes.txt -o out.txt"));
        assert!(!is_safe("sort notes.txt --output=out.txt"));
        // `sort -no` is een cluster en zet toch de schrijvende -o.
        assert!(!is_safe("sort -no out.txt notes.txt"), "een cluster mag de schrijf-vlag verbergen");
        assert!(!is_safe("rg --pre rm pattern"));
        assert!(!is_safe("find . -delete"));
        assert!(!is_safe("find . -exec rm {} \\;"));
        assert!(!is_safe("date -s 2026-01-01"));
        // En de kale read-only-vorm van dezelfde programma's blijft gewoon door.
        assert!(is_safe("sort notes.txt"));
        assert!(is_safe("rg pattern src"));
    }

    #[test]
    fn uniq_writes_when_it_gets_two_operands() {
        assert!(is_safe("uniq notes.txt"));
        assert!(!is_safe("uniq in.txt out.txt"));
        // De val die de end-of-options-telling sluit: na `--` telt een token met een streep ook als operand.
        assert!(
            !is_safe("uniq -- in.txt -out.txt"),
            "de shell geeft uniq hier twee bestanden en hij schrijft de tweede"
        );
    }

    #[test]
    fn git_is_read_only_per_subcommand_not_per_program_name() {
        for sub in ["status", "log", "diff", "show", "blame", "ls-files", "rev-parse"] {
            assert!(is_safe(&format!("git {sub} --porcelain")));
        }
        for sub in ["commit", "push", "checkout", "stash", "clean", "fetch", "reset"] {
            assert!(!is_safe(&format!("git {sub}")), "git {sub} schrijft");
        }
    }

    #[test]
    fn a_git_escape_flag_breaks_the_read_only_promise_even_quoted() {
        // De shell haalt quotes weg vóórdat git start, dus een ontsnap-vlag in quotes is dezelfde hazard.
        assert!(!is_safe("git diff --output=/tmp/patch"));
        assert!(!is_safe("git diff '--output=/tmp/patch'"));
        assert!(!is_safe("git log -c core.pager=rm"));
        assert!(!is_safe("git status --git-dir /elsewhere"));
        // Een quoted bestandsargument blijft wél gewoon een read.
        assert!(is_safe("git diff 'src/main.rs'"));
    }

    #[test]
    fn a_denied_flag_inside_quotes_reaches_the_program_bare() {
        assert!(!is_safe("sort notes.txt '-o' out.txt"), "quotes verdwijnen vóór het programma start");
    }

    #[test]
    fn a_family_key_covers_a_chain_only_when_every_segment_is_trusted() {
        // Het voordeel dat een ontwikkelaar voelt: één "altijd" op deze pipeline dekt elke herformulering.
        assert_eq!(trusted_key("git diff main | tail -5").as_deref(), Some("git diff | tail"));
        // … en de smokkel die dezelfde regel moet sluiten: één kwade segment kost het hele teken.
        assert_eq!(trusted_key("git diff main | rm -rf build"), None);
        assert_eq!(trusted_key("git diff --output=/tmp/x | tail -5"), None);
        assert_eq!(trusted_key("git status && git clean -fd"), None);
        // git die werk kan weggooien krijgt nooit een gezin, ook al is het een verder onschuldige keten.
        assert_eq!(trusted_key("git reset HEAD~1"), None);
        assert_eq!(trusted_key("git checkout main"), None);
    }

    #[test]
    fn a_family_key_records_the_shape_of_the_chain() {
        // `git diff | tail` mag niet op `git diff ; tail` matchen: de operator maakt deel uit van het teken.
        assert_eq!(trusted_key("git diff | tail").as_deref(), Some("git diff | tail"));
        assert_eq!(trusted_key("git diff ; tail").as_deref(), Some("git diff ; tail"));
        assert_ne!(trusted_key("git diff | tail"), trusted_key("git diff ; tail"));
    }

    #[test]
    fn a_malformed_chain_gets_no_family_key() {
        assert_eq!(trusted_key("git diff |"), None, "een lege segment is een vormfout");
        assert_eq!(trusted_key("| git diff"), None);
        assert_eq!(trusted_key("git diff ||| tail"), None, "drie pijpen zijn geen operator");
        assert_eq!(trusted_key("git diff 'main"), None, "een onbalansquote");
        assert_eq!(trusted_key("echo-regel\nrm -rf build"), None, "een regeleinde is een tweede commando");
    }

    #[test]
    fn a_commit_message_holding_an_operator_is_not_a_chain() {
        // De scheider binnen een quoted argument is tekst, geen keten: één segment dus, één teken.
        assert_eq!(trusted_key("git commit -m 'fix a && b'").as_deref(), Some("git commit"));
    }

    #[test]
    fn spacing_never_changes_a_key_and_a_shell_never_earns_one() {
        assert_eq!(trusted_key("git    diff\tmain"), trusted_key("git diff main"));
        // Blokkeerde programma's: zelfs als iemand het teken typt, matcht het nooit aan een echte run.
        for program in ["bash", "python3", "rm", "curl", "sudo", "tee", "tar", "xargs", "sed"] {
            assert_eq!(trusted_key(&format!("{program} something")), None, "{program} vertrouwt nooit");
        }
    }

    #[test]
    fn a_known_build_wrapper_earns_a_family_and_a_renamed_one_does_not() {
        assert_eq!(trusted_key("./mvnw -q package").as_deref(), Some("./mvnw"));
        assert_eq!(trusted_key("./gradlew test").as_deref(), Some("./gradlew"));
        assert_eq!(trusted_key("./mvnw-evil package"), None, "enige andere wrapper leent de identiteit niet");
        assert_eq!(trusted_key("tools/mvnw package"), None, "een map-pad is geen wrapper");
        assert_eq!(trusted_key("./mvnw").as_deref(), Some("./mvnw"), "de launcher alleen mag ook");
    }

    #[test]
    fn a_bare_program_family_key_needs_no_read_only_prove() {
        // Hier staat de mens garant, niet de poort: mvn staat op geen read-only-lijst en krijgt tóch een
        // gezin, want precies die herhaling is de nag die de ontwikkelaar voelt.
        assert_eq!(trusted_key("mvn -q -DskipTests package").as_deref(), Some("mvn"));
        assert_eq!(trusted_key("cargo test --workspace").as_deref(), Some("cargo"));
    }

    #[test]
    fn is_safe_and_trusted_key_stay_two_independent_questions() {
        // Een keten krijgt een gezin maar draait nooit zonder mens; een simpelere read krijgt beide.
        assert!(trusted_key("git diff | tail").is_some());
        assert!(!is_safe("git diff | tail"), "automatisch draaien blijft één simpel commando");
        // `cargo` en `mvn` staan in de origineler op géén van beide lijsten: ze bouwen en schrijven, dus
        // ze zijn niet read-only en draaien nooit zonder mens. Ze krijgen wél een gezin, want daar staat
        // de mens zelf garant. Dat is precies het verschil tussen de twee vragen.
        assert!(!is_safe("cargo test"), "bouwen is geen read");
        assert!(!is_safe("mvn -q package"), "bouwen is geen read");
        assert_eq!(trusted_key("cargo test").as_deref(), Some("cargo"));
    }
}
