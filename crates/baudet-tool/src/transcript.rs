//! Haalt een lopend transcript op voor één backend-opdracht.
//!
//! Poort van `tool/Transcript.kt`. Het nieuwste bericht is de huidige invoer; wat ervoor wordt
//! nagespeeld, wordt gezuiverd zodat de besturingstokens die een chat-model soms lekt uit de
//! geschiedenis niet naboubaar zijn en nooit op het scherm belanden. Elke eerdere beurt
//! gaat mee, assistent-beurten incluis, zodat een run zijn werkcontext houdt wanneer
//! `previous_response_id` uit staat; scrubbing dekt ook assistent-tekst, dus naspelen kan de
//! lekkage die dit stopt niet terugvoeren.
//!
//! Model-tekst blijft Engels, ook de nota die `fit` vooraan zet: die gaat het verzoek in, ongeacht
//! de UI-taal — precies zoals in de origineler.

use baudet_core::Msg;
use baudet_store::window::{self, ContextStrategy};

/// Plafond op de tekst die als eerdere beurten meegaat, in karakters (~3 karakters per token, dus
/// ongeveer zestienduizend tokens). Bewust ver onder een venster van 128k: de instructies, de
/// tools, de huidige invoer en het budget van het antwoord zelf hebben allemaal nog ruimte nodig,
/// en een lokale backend rekent zijn chat-template daarbovenop.
const MAX_REPLAY_CHARS: usize = 48_000;

/// Haalt gelekte besturingstokens uit willekeurige tekst; laat gewone prosa heel.
///
/// Het patroon is `<|` + één of meer tekens behalve `|` en `>` + `|>`, exact zoals het regex-literal
/// in `Transcript.kt`. Het staat hier als scannen en niet als regex, om twee redenen: de token-literal
/// hoeft zo nooit als geheel in deze bron voor te komen — de tekens worden hier apart genoemd — en een
/// vijfde dependency voor één eliminatie is het hier niet waard. De scanner volgt die klassen
/// woordelijk: de klasse sluit `|` en `>` uit, maar géén `<`, dus `<|a|b|>` is één token terwijl
/// `<|a|` zonder sluiting heel blijft en als tekst leest.
pub fn scrub(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    let mut copied = 0; // tot hier is `text` al overgenomen
    while index < bytes.len() {
        if bytes[index] == b'<' && text[index..].starts_with("<|") {
            let after_open = index + 2;
            let mut scan = after_open;
            // `[^|>]+`: geen pijpe en geen punt, maar wél een `<` doorlatend, en minstens één teken.
            // Een `<` stopt hier bewust niet: `Transcript.kt` wijst alleen die twee tekens af, dus
            // `<|a|b|>` is één token en `<|a|b>c` is er geen.
            while scan < bytes.len() && bytes[scan] != b'|' && bytes[scan] != b'>' {
                scan += 1;
            }
            if scan > after_open && text[scan..].starts_with("|>") {
                out.push_str(&text[copied..index]);
                index = scan + 2;
                copied = index;
                continue;
            }
        }
        index += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// Knipt de eerdere beurten die in één opdracht passen, oudste eerst, met een marker voor wat eruit
/// moest. Zonder dit groeit een tool-run met één observatie per ronde en stuurt die observaties elke
/// iteratie opnieuw, dus sterft een lange run halverwege met een contextfout verliest de gebruiker
/// de run op het duurste moment. De oudste eerst weg houdt wat het model nodig heeft om nu te
/// handelen: zijn eigen laatste stappen en de observaties die die opleverden. Het bewaarde deel is
/// altijd een aaneengesloten staart, dus de run leest nog steeds in volgorde.
pub fn fit(prior: &[Msg], max_chars: usize) -> Vec<Msg> {
    if max_chars == 0 {
        return prior.to_vec();
    }
    let mut used = 0usize;
    let mut first = prior.len();
    while first > 0 {
        let msg = &prior[first - 1];
        // Dezelfde per-bericht-toeslag als `SessionState.estimateTokens`, zodat de twee mekaar blijven
        // treffen; `cost_chars` is precies die som.
        let cost = baudet_store::cost_chars(msg);
        if used + cost > max_chars {
            break;
        }
        used += cost;
        first -= 1;
    }
    let mut kept = prior[first..].to_vec();
    if first == 0 {
        return kept;
    }
    // Model-tekst, dus Engels wat de UI ook spreekt: hij gaat het verzoek in. De woordkeuze is die van
    // `Transcript.kt:112-114`, bewust niet de nota uit `baudet-store::history`: twee lagen die één tekst
    // beweren te bezitten, is hoe die tekst later uit mekaar loopt.
    let note = format!(
        "[{first} earlier messages were dropped from this request to keep it inside the model's \
         context window. The oldest context is gone; the turns below are intact.]"
    );
    kept.insert(0, Msg::user(note));
    kept
}

/// De huidige invoer voor één backend-opdracht.
///
/// Zonder server-sessie is het nieuwste bericht alleen de invoer en speelt de rest na. Met een
/// server-sessie houdt de backend alles tot de laatste assistent-beurt al, dus is de invoer elk
/// bericht dat de lus sindsdien toevoegde — meestal verschillende tool-observaties uit één
/// modelbeurt — samengevoegd zodat er geen enkel verdwijnt; vóór elke assistent-beurt staat het
/// nieuwste alleen.
pub fn current_input(history: &[Msg], under_server_session: bool) -> String {
    if !under_server_session {
        return history.last().map(|msg| scrub(&msg.content)).unwrap_or_default();
    }
    let last_assistant = history.iter().rposition(|msg| msg.role == "assistant");
    let tail: &[Msg] = match last_assistant {
        Some(index) => &history[index + 1..],
        None if !history.is_empty() => &history[history.len() - 1..],
        None => &[],
    };
    let joined = tail
        .iter()
        .map(|msg| scrub(&msg.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.trim().is_empty() {
        return history.last().map(|msg| scrub(&msg.content)).unwrap_or_default();
    }
    joined
}

/// Splitsing voor een venster dat de aanroeper al begrensde. Dezelfde scrubbing als [`for_call`] maar
/// zonder lengte-klem, want `baudet_store::window` sneed de lijst al op de maat die dit model voor
/// deze strategie draagt; hier opnieuw op de vaste replay-klem zetten zou dat ongedaan maken en de
/// geldende grens terugbrengen tot een gok in plaats van het venster.
pub fn split_for_window(history: &[Msg]) -> (Vec<Msg>, String) {
    let user_text = history.last().map(|msg| scrub(&msg.content)).unwrap_or_default();
    let prior = history[..history.len().saturating_sub(1)]
        .iter()
        .map(|msg| scrubbed(msg))
        .collect();
    (prior, user_text)
}

/// @return de gesanitiseerde eerdere berichten om na te spelen, en de huidige gebruikersinvoer.
pub fn for_call(history: &[Msg]) -> (Vec<Msg>, String) {
    let user_text = history.last().map(|msg| scrub(&msg.content)).unwrap_or_default();
    let prior = history[..history.len().saturating_sub(1)]
        .iter()
        .map(|msg| scrubbed(msg))
        .collect::<Vec<_>>();
    (fit(&prior, MAX_REPLAY_CHARS), user_text)
}

/// Splitst een lopend tool-loop-transcript voor één verzoek, begrensd door het contextbeleid van de
/// aanroeper zodat dezelfde maat die de beurt opende élke latere opdracht begrenst en niet een vast
/// plafond. Een tool-loop voegt per ronde één of meer observaties toe, dus zonder dit zouden de latere
/// verzoeken terugvallen op de vaste klem van [`for_call`] en het budget dat de persoon instelde
/// negeren. Onder [`ContextStrategy::StopAtLimit`] gaat het transcript heel en geldt de vaste klem
/// nog, met `under_server_session` als beslissing over de huidige invoer; onder een
/// inkortingsstrategie wordt het venster eerst door `baudet_store::window` begrensd en daarna zonder
/// die klem gesplitst, dus het inkorten beslist de omvang. Het nieuwste bericht is de huidige invoer
/// telkens als de lus client-specs napeelt, en dat doet hij onder een inkortingsstrategie.
pub fn for_loop(
    history: &[Msg],
    strategy: ContextStrategy,
    budget_tokens: usize,
    under_server_session: bool,
    keep_recent: usize,
) -> (Vec<Msg>, String) {
    if strategy == ContextStrategy::StopAtLimit {
        let (prior, _) = for_call(history);
        return (prior, current_input(history, under_server_session));
    }
    split_for_window(&window::bound(history, strategy, budget_tokens, keep_recent))
}

/// Een bericht met schone inhoud; alles wat er buiten de inhoud staat blijft staan zoals het was.
fn scrubbed(msg: &Msg) -> Msg {
    let mut copy = msg.clone();
    copy.content = scrub(&msg.content);
    copy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(role: &str, content: &str) -> Msg {
        Msg::new(role, content)
    }

    /// Het literal dat de origineler uit codepunten opbouwt; hier alleen in testen, nooit in de bron.
    fn token(name: &str) -> String {
        format!("<|{}|>", name)
    }

    #[test]
    fn an_im_start_never_survives_and_prose_is_untouched() {
        let leaked = format!("voor {} na", token("im_start"));
        assert_eq!(scrub(&leaked), "voor  na");
        assert_eq!(scrub(&token("endoftext")), "");
        // Meerdere tokens, ook opeenvolgend.
        assert_eq!(scrub(&format!("{}{}x", token("a"), token("b"))), "x");
        // Gewone tekst mag geen krul krijgen, ook niet als ze tekens bevat die erop lijken.
        for prose in ["a < b and c > d", "x | y", "a <| b", "<a|b>", "price <|a|", "a|>b", "", "   "] {
            assert_eq!(scrub(prose), prose, "{prose:?} is geen token");
        }
    }

    #[test]
    fn the_sweeper_meets_the_reference_pattern_on_every_border_case() {
        // De waarheid is gemeten, niet bedacht: `re.sub` met hetzelfde letterlijke klas-literal als
        // `Transcript.kt`; de verwachtingen hieronder zijn die meting. Pas ze alleen aan als het
        // literal zelf verandert, niet omdat een test rood staat. Twee van deze gevallen stonden er
        // eerst verkeerd en waren alleen door meten te zien: een losse `>` ná een token valt buiten
        // de match en blijft staan, en de klasse `[^<>]` wijst een regeleinde niet af, dus een token
        // mag over een regel lopen.
        let cases = [
            ("<|a|>", ""),
            ("<|a|>|>", "|>"),
            ("<|a|>>", ">"),
            ("<<|a|>", "<"),
            ("<|a|b|>", "<|a|b|>"),
            ("<||>", "<||>"),
            ("<|>", "<|>"),
            ("<|a|", "<|a|"),
            ("a|b|>", "a|b|>"),
            ("<|a\nb|>", ""),
            ("<|a|>x<|b|>", "x"),
            ("<|went|> bewaard, <|leeft|> mee", " bewaard,  mee"),
        ];
        for (input, expected) in cases {
            assert_eq!(scrub(input), expected, "scrub({input:?})");
        }
    }

    #[test]
    fn a_broken_token_stays_text_instead_of_getting_eaten() {
        // Waar de eerste versie van deze scanner van afweek: `<|a|` zonder sluiting en `<||>` met lege
        // inhoud mogen niet verdwijnen, want dat is tekst die een persoon schreef. De vaste klas van
        // de origineler wijst allebei af; een scanner die dat "grootmoedig" soepeler leest, slikt in.
        assert_eq!(scrub("<|a|"), "<|a|");
        assert_eq!(scrub("<||>"), "<||>");
        assert_eq!(scrub("<|>"), "<|>");
        assert_eq!(scrub("<|a|>rest"), "rest");
        assert_eq!(scrub("<|a|>|>"), "|>");
    }

    #[test]
    fn fit_keeps_a_contiguous_tail_and_names_what_went() {
        // Elke post kost 100 + 4 + 8 = 112 karakters. De getallen hieronder zijn niet narekend maar
        // afgeleid uit wat `fit` zelf teruggeeft: de noot noemt het aantal dat wegviel, dus de test
        // leest dat getal uit en controleert de som. Eerder stond hier een verzonnen "3 + 1" met een
        // valse uitleg erbij, en dat viel alleen te zien door te meten.
        let prior: Vec<Msg> = (0..10).map(|_| chat("user", &"y".repeat(100))).collect();
        let kept = fit(&prior, 350);

        let dropped = note_count(&kept).expect("er viel iets weg, dus er staat een noot");
        let sources = kept.len() - 1;
        assert_eq!(dropped + sources, 10, "niets verdwijnt dubbel, de staart is aaneengesloten");
        // Drie bronnen van 112 plus de noot (165) samen 501: de noot telt niet mee bij het kiezen
        // van de staart, dus de som mag de cap overschrijden. De getallen zijn gemeten, niet bedacht.
        assert_eq!(sources, 3);
        assert_eq!(baudet_store::cost_chars(&kept[0]), 165);
        assert_eq!(sources * 112, 336);
        assert!(kept[1..].iter().all(|m| m.content == "y".repeat(100)));
        assert_eq!(kept[0].role, "user");
        assert!(kept[0].content.contains("The oldest context is gone; the turns below are intact."),
            "de noot is de tekst van `Transcript.kt:102-103`, niet een eigen verzonnen");

        // Krap bij één bron: de noot moet naast die bron passen, anders valt alles weg en is er geen
        // noot nodig — precies het gedrag dat een rechte `<=`-som niet geeft.
        // Eén bron (112) past bij een cap van 112 naast de noot: de noot plus die ene bron, geen
        // meer. Bij een kleinere cap was er alleen de noot; dit is de grens waarop een bron nóg past.
        let tight = fit(&prior, 112);
        assert_eq!(tight.len(), 2, "noot plus één bron");
        assert_eq!(note_count(&tight), Some(9));
        assert_eq!(baudet_store::cost_chars(&tight[0]), 165);

        // Ruim: heel de lijst, in volgorde, zonder noot.
        let whole = fit(&prior, 10_000);
        assert_eq!(whole.len(), 10);
        assert!(note_count(&whole).is_none());
        // 0 is geen limiet maar "uit": ook een transcript van 400k karakters blijft dan heel.
        assert_eq!(fit(&prior, 0).len(), 10);
        assert!(fit(&[], 50).is_empty());
    }

    /// Het getal uit de noot, of `None` als er geen noot voorop staat.
    fn note_count(msgs: &[Msg]) -> Option<usize> {
        let head = msgs.first()?;
        let rest = head.content.strip_prefix("[")?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }

    #[test]
    fn the_current_input_replays_client_side_and_joins_server_side() {
        let history = vec![chat("user", "vraag"), chat("assistant", "antwoord"), chat("tool", "obs")];
        // Client: alleen het nieuwste, de rest speelt na.
        assert_eq!(current_input(&history, false), "obs");
        // Server: alles sinds de laatste assistent-beurt, meegegeven zodat er niets wegvalt.
        let with_two = vec![
            chat("user", "vraag"),
            chat("assistant", "antwoord"),
            chat("tool", "obs1"),
            chat("tool", "obs2"),
        ];
        assert_eq!(current_input(&with_two, true), "obs1\n\nobs2");
        // Vóór elke assistent-beurt staat het nieuwste alleen.
        assert_eq!(current_input(&[chat("user", "alle")], true), "alle");
        // Een lege staart mag niet terugvallen op niets: het nieuwste bericht staat dan klaar.
        let blank = vec![chat("user", "vraag"), chat("assistant", "antwoord"), chat("tool", "   ")];
        assert_eq!(current_input(&blank, true), "   ");
        assert_eq!(current_input(&[], true), "");
        assert_eq!(current_input(&[], false), "");
    }

    #[test]
    fn scrubbing_reaches_assistant_turns_too_and_the_last_message_stands_apart() {
        let history = vec![
            chat("assistant", &format!("a {} b", token("im_end"))),
            chat("user", &format!("verder {}", token("x"))),
        ];
        let (prior, input) = for_call(&history);
        assert_eq!(prior.len(), 1);
        assert_eq!(prior[0].content, "a  b", "assistent-tekst wordt gezuiverd, niet gespaard");
        assert_eq!(input, "verder ", "de huidige invoer is schoon en staat buiten de replay");
        assert_eq!(prior[0].role, "assistant", "het recht blijft staan, alleen de inhoud schuift");
        // Een lege geschiedenis is geen paniek maar een leeg verzoek.
        let (prior, input) = for_call(&[]);
        assert!(prior.is_empty());
        assert_eq!(input, "");
    }

    #[test]
    fn for_loop_under_stop_at_limit_keeps_the_clamp_and_a_trimming_strategy_does_not() {
        // Twee berichten van 30 000 karakters: samen voorbij de replay-klem van 48 000.
        let big = "z".repeat(30_000);
        let history = vec![chat("user", &big), chat("assistant", &big), chat("user", "nu")];
        let (prior, input) = for_loop(&history, ContextStrategy::StopAtLimit, 1_000_000, false, usize::MAX);
        // Budget 1 000 000 maar de vaste replay-klem van 48 000 snijdt erin: het eerste bericht valt
        // af, de noot en het tweede blijven. De klem geldt dus, niet het budget dat de aanroeper meegaf.
        assert_eq!(prior.len(), 2, "noot plus het tweede bericht blijven");
        assert_eq!(input, "nu");
        // Zelfde geschiedenis, inkortingsstrategie zonder budget: het venster beslist, hier niets.
        let (prior, _) = for_loop(&history, ContextStrategy::RecentMessages, 0, false, usize::MAX);
        assert_eq!(prior.len(), 2, "geen budget betekent hier niet de vaste klem");
        // Met een klein budget knipt het venster zélf, en de klem komt er niet bovenop.
        let (prior, _) = for_loop(&history, ContextStrategy::RecentMessages, 1, false, usize::MAX);
        assert!(prior.len() <= 2);
    }

    #[test]
    fn for_loop_under_stop_at_limit_asks_the_server_for_the_input() {
        // De enige plek waar de strategie de bron van de invoer omschakelt: onder StopAtLimit beslist
        // `under_server_session`, onder een inkortingsstrategie is het altijd het nieuwste bericht.
        let history = vec![chat("user", "v"), chat("assistant", "a"), chat("tool", "obs1"), chat("tool", "obs2")];
        let (_, input) = for_loop(&history, ContextStrategy::StopAtLimit, 0, true, usize::MAX);
        assert_eq!(input, "obs1\n\nobs2");
        let (_, input) = for_loop(&history, ContextStrategy::RecentMessages, 0, true, usize::MAX);
        assert_eq!(input, "obs2", "client-naspelen kent de server niet");
    }
}
