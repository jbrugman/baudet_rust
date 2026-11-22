# Baudet (Rust)

Groen opgezette Rust-versie van de Baudet-app, met de Kotlin-app als voorbeeld. De UI is fase
één; de backend komt later. Crastructuur: `baudet-ui` (egui/eframe), `baudet-settings`,
`baudet-store`, `baudet-backend`. Geen enkele crate kent de UI.


De **specificatie** van deze port is de origineler, niet een nieuw ontwerp: zijn `README.md`
("Working:"-lijst) en de vijf architectuurdiagrammen in `../baudet/Docs/*.puml` (components,
sse-transport, turn-decision, tool-loop, compaction-fold). Elke feature wordt daar gelezen vóórdat er
iets in Rust wordt uitgevonden; `TODO.md` koppelt elk item aan de gevonden `.kt`-deur via embed-match.

De volledige werklijst staat in [`TODO.md`](TODO.md): elk domein van de origineler met `[x]`/`[ ]`,
de beslisregel bij twijfel, en hoe de embed-match Kotlin en Rust aan elkaar koppelt. Die lijst is
de bron van de waarheid over wat er is en wat er nog moet; deze README gaat over *hoe*.

De front-end toont wat er onder zit, en dat is minder dan het doel: transcript met streaming-regels,
composer, en het ingestpaneel met endpoint, modellenlijst, redenereniveau en uitvoerplafond. **Nog
niet** het project-/sessiedomein, de read/write/execute-rechten, de linker `ProjectSidebar`, of de
bestandsboom in het **rechter** `FileTreePanel` zoals `Main.kt:161-170` die legt. Zie `TODO.md`
voor de volledige stand en de bouwvolgorde. De lay-out is hier eerder onjuist begonnen (boom links
in plaats van rechts, sidebar helemaal afwezig); dat wordt in item A/B van `TODO.md` rechtgezet.
`--screenshot` bewijst de staat van een sessie zonder muis.
`ScriptedBackend` voert beurtstromen uit voor een venster zonder server; `--live` stuurt de beurt
naar het opgeslagen endpoint via `ResponsesBackend`, die beide OpenAI-dialecten spreekt en wiens
dialect uit een server-scan komt. Het transport bestaat dus; wat er nu bovenop gebeurt, is hier
onder.

## Bouwen en draaien

De toolchain staat vast in `rust-toolchain.toml` (1.95.0, met `clippy` en `rustfmt`). Een `cargo`
van vóór 1.82 kan het transieveboom van egui/eframe 0.36 niet eens parseren (edition 2024), dus
gebruik de gepinde toolchain — `rustup` pakt hem automatisch zodra `rust-toolchain.toml` hier staat;
de machine-standaard blijft 1.82 en geen ander project verandert daardoor.

```sh
cargo run -p baudet-ui          # de app, met de demo-gesprek (geen server nodig)
cargo test --workspace          # alle crates, inclusief de UI-tests (vensters getekend via run_ui)
cargo clippy -p baudet-ui       # schone lint voor iets groen te noemen
```

Twee bouwprofielen, twee binaire — ze lopen uiteen, dus let op welk profiel je aanroept:

| profiel | commando | binair | doel |
|---|---|---|---|
| debug (default) | `cargo build -p baudet-ui` | `target/debug/baudet-ui` | ontwikkelen en testen; snelle compile |
| release | `cargo build --release -p baudet-ui` | `target/release/baudet-ui` | de vlotte versie om te tonen |

Een nieuwe bronwijziging bouwt alleen het profiel dat je aanroept; wie beide gebruikt, bouwt beide
(`--release` vergeet de debug niet, en omgekeerd). Er is geen `.app`-bundle — het binaire is een
los staand executable dat je rechtstreeks start of met `cargo run`.

De opties op de commandoregel staan volledig in `baudet-ui --help`; de gebruikelijke: `--screenshot
<file>` tekent en sluit (headless bewijs, geen focusdiefstal), `--workspace <folder>` opent een map bij
start, `--settings` opent het ingesttabblad, `--fetch-models` vraagt de endpoint na zijn modellen,
`--live` stuurt beurten naar het opgeslagen endpoint in plaats van de scriptspeler, en `--discover`
noemt de plaatselijke servers die antwoorden.

## Besluit: de fetch-sleutel komt uit de omgeving, en "geland" is niet "geslaagd"

Twee gaten in het ingestpaneel, beide gemeten tegen de echte servers (oMLX-gateway met
sleutel, LM Studio zonder), vastgelegd in tag `snapshot-2026-11-21-fetchkey`:

1. **Een listing stuurde alleen het getypte veld mee.** Het label van het veld belooft
   "leeg = lees uit de omgeving", en de Kotlin-referentie maakt dat waar
   (`AppConfig.kt:204-205`: typwaarde wint, anders de genoemde variabele). Onze app had die
   resolutie alléén in `Settings::resolved_key()` en raakte die bij een listing nooit, dus
   élke gateway met een sleutel antwoordde 401 totdat de sleutel met de hand getypt en
   opgeslagen werd — de weg die het paneel zelf afraadt. De fetch lost de sleutel nu op via
   `draft()` + `resolved_key()`, dus één waarheid voor knop en veld.

2. **"Is de listing geland?" werd beantwoord met "is de lijst actueel?"** — en een gefaalde
   listing kan die tweede vraag per definitie niet met ja beantwoorden (`fetched_for` blijft
   `None`, bewust: een geblonken endpoint mag een werkende lijst niet leegmaken). Een
   run die op het antwoord wachtte, draaide daarom zijn eigen plafond uit en tekende de status
   van de fetch die hij zélf had aangevraagd: precies het bewijs dat hij moest vastleggen.
   Daarvoor staat nu een eigen, klein signaal: `listing_answered`, gezet bij élk listingantwoord
   en gereset zodra het adres verandert.

Waarom dit een besluit is en geen bijklus: het raakt het credentialsmodel (niets wordt op
schijf gezet wat de omgeving kan geven) en de definitie van "actueel" in de picker. Beide zijn
gedrag dat de gebruiker ziet, niet inwendige netheid.

## Werkwijze bij een vastloper of een onzekere keuze

**Zeker?** Bouw door op `main`. **Niet zeker, of een keuze die de richting koest?** Maak van de
huidige staat een snapshot (commit + `git tag`), dan een nieuwe `codex/*`-branch waarin je doorwerkt,
`main` onaangeroerd, met een README-aanpassing die het besluit en de reden noemt. Dan hoef je niet te
worden afgeremd: er is altijd een punt om naar terug te keren en een omgegooide poging ruimt niets op.
De volledige lijst en deze regel staan in [`TODO.md`](TODO.md).

## Wat er nu is, en wat er als next komt

Zes crates, de UI kent alleen het `Backend`-contract:

| crate | taak |
|---|---|
| `baudet-core` | events/reductie/rollout. Geen IO, geen UI. |
| `baudet-backend` | het `Backend`-contract + `ScriptedBackend` + `sse.rs` (parser) + `transport.rs` (`ResponsesBackend`: HTTP/SSE, beide dialecten, streaming) + `protocol.rs` (lichamen en de server-scan). |
| `baudet-settings` | settings-pad, schrijven, en `GET /v1/models`. Leest geen UI. |
| `baudet-store` | rollouts op schijf en hun indexeringslaag. |
| `baudet-app` | de proxy bovenop een backend: events, checkpoints, interrupts. |
| `baudet-ui` | egui/eframe: frames, rijen, panelen, het ingestpaneel. |

Bewezen gedrag van de frontend: chat over een script, plan/details, toolrijen met wachttijd en
details, interrupt/resume met toetsenbord en met Esc, Shift+Enter, scroll-vastzetten bijknepen,
transcript opschonen, rijselectie, workspace-tree met lazy vouwen en verversen, changed-files met
kopieerbare pakketcodes, settings-venster incl. model-ophalen.

## Wat er sinds de transport is bijgekomen, en wat er toen twee keer misging

De transportlaag (`transport.rs`: HTTP, chunked SSE, beide dialecten, streaming) is er, en
vervangt daarmee wat er eerder ontbrak. Daarbij zijn twee fouten gevonden, beide gemeten tegen een
levend venster of een naspeelde stroom, niet bedacht:

1. **Een beurt sloot zijn eigen regel nooit.** `transport.rs` bouwde de sluit-events en gooide ze
   weg (`let _ = state.close_open_rows(...)`). Het effect was in het venster zichtbaar: de kop zei
   `idle` terwijl het antwoord eeuwig op `working` bleef staan — rij en kop lezen dezelfde beurt via
   twee deuren en één van die deuren kreeg nooit een event. Nu gaan die events het kanaal in. Het
   bewijs dat dit geen kosmetische ingreep is: het nieuwe `baudet-backend/tests/stream_closure.rs`
   (2 tests) speelt een complete chat-stroom na: allebei falen zodra deze fix wordt weggehaald (gemeten met
   `git stash`), allebei slagen wanneer hij er staat.
2. **Documentatie beloofde een exit die er niet was.** De `SHOT_FRAMES`-doc zei dat een
   `--screenshot`-run op het tijdsplafond "non-zero" eindigt. Gemeten: `RETOUR=0`, en géén enkele
   test claimde die exit. Alleen de doc is gecorrigeerd naar wat de code werkelijk doet (het venster
   wordt zoals het is weggeschreven, met de melding op stderr); er is geen gedrag ingebouwd dat
   niemand had gevraagd.

Ook mee in deze ronde, elk met eigen test: het voltooide gesprek reist mee in elke aanvraag
(`completed_messages` + 6 tests in `baudet-app/tests/history.rs`); de usage-chunk van een
chat-stroom — `choices: []` met de aantallen ernaast — wordt niet meer weggegooid maar geteld; en
elke beurt noemt het model en het redenereniveau uit het ingestpaneel, zonder dat de persoon die
keuze nog eens hoeft te typen. `--live` blijft een expliciete vlag: het scripted venster is en
blijft de standaard.

Bij de live-run is de shot-variatie verklaard, met een meting erbij en niet met een modifier.
Een `--screenshot`-run vraagt de demo-vraag en tekent zodra de beurt gezet is; `SHOT_FRAMES`
(600 frames) is daar het plafond voor. Gemeten in drie runs met dezelfde instellingen: 73 KB,
141 KB en 241 KB, dus de uitkomst hangt aan de frame-snelheid van de machine en niet aan de
server. Snel getekend past een hele beurt onder het plafond en tekent het venster een voltooid
beeld, met het echte antwoord van de LM Studio-gemma via de transport in de kop `streaming`; traag
getekend viel het plafond vóór het eerste token en blijft de vraag zonder antwoord staan, met de
"not settled"-melding erbij. Die melding is dus het eerlijke gevolg van een plafond dat lager ligt
dan één beurt op een lokaal model — gemeten: dezelfde vraag met het plafond uit de instellingen
(512 tokens) doet er op die gemma ~22 s over en bereikt `finish: length` — en niet van een kapotte
streaming, want de streaming bereikte het venster zichtbaar.

Twee gedragingen eraan vast zijn bewust niet aangeraakt: het plafond zelf, want het bestaat om een
stroom te snoeren die nooit sluit en niet om een traag model te straffen, en de plaats van de
settle-controle, die vóór `paint` staat en dus de snapshot van de vorige frame beoordeelt. Wie een
volledig beeld wil, kiest een snellere machine of server. De volgorde omdraaien kan ook, maar dat
is een UI-gedragswijziging die een eigen besluit en een eigen meting verdient.

### De sweep vindt de server die hier draait

`--discover` loopt de loopback-kandidaten af en noemt de eerste die een beurt antwoordt, met per
kandidaat wat er gezien is. Het is geen tweede oordeelslogica: per adres draait dezelfde
`protocol::scan` die het instellingenpaneel ook gebruikt, dus een sweep kan niet tot een ander
antwoord komen dan de knop daar.

De volgorde is niet gratis gekozen. **Eerst een socket, dan pas een verzoek**: acht poorten vragen
— open en gesloten — kostte gemeten 0,00 s, dezelfde vraag over HTTP kostte 36 s voor achttien
poorten, omdat een gesloten poort daar de hele watchdog van de client opeist. En de sweep **stopt
bij de eerste die antwoordt**, want elke probe noemt een model dat een server kan moeten inladen;
kandidaten na de winnaar kosten niets. De sweep op deze machine: 1,6 s.

Daarbij twee dingen die een meeting waren, geen aanname:

- **Een listing is geen chatmodellijst.** LM Studio noemt naast zijn gemma
  `text-embedding-nomic-embed-text-v1.5`, de gateway noemt `bge-m3-mlx-fp16`,
  `chatterbox-multilingual-v3` en `whisper-large-*`. Een probe die zo'n id noemt, kost een verzoek
  dat niet kan antwoorden — en op een server die modellen inlaadt, een lading die het resident model
  verdringt. `scan` noemt daarom het eerste *aannemelijke* model in plaats van het eerste. De test
  staat op de id (`likely_chat_model`), want dat is alles wat een listing geeft; een model dat hij
  niet herkent, blijft gewoon in de plukker staan, hij krijgt alleen de probe niet.
- **Een statuscode noemt ook het verkeerde antwoord de goede kant op.** De gateway antwoordt
  `404 {"error": …}` zowel op een pad dat hij niet dient als op een model dat hij niet heeft, op
  `/responses` als op `/chat/completions`; LM Studio antwoordt op een nepmodel op een pad dat hij
  wél dient met `200` en het antwoord van *zijn eigen* model. Een status-test zou de gateway een
  afwijzing van het dialect noemen en bij de ander het verkeerde model noemen.

De sweep staat bewust op een vlag en niet bij elke start: live op deze machine kostte hij 5,69 s
(vandaag 1,6 s, omdat de winnaar na één gesloten poort al gevonden wordt), en een venster dat
seconden wacht voordat het verschijnt, oogt kapot.

Waar hij **niet** naar zoekt, is een besluit. `127.0.0.1:8000` is de default van de referentie-app
en van `Settings::default`, maar de gateway die hier antwoordt, is op dat adres níét te bereiken —
gemeten: loopback `8000` verwerpt, `192.168.1.2:8000` antwoordt. Een sweep die dat overslaat, ziet de
server waar deze ontwikkeling tegen draait, als afwezig. Over een subnet poorten gaan aftasten is een
portscan van andermans netwerk en niet wat "vind mijn server" betekent, dus zo'n server blijft één
getypt adres in het paneel verwijderd. De 36 s hierboven is trouwens betaald op precies zo'n
niet-loopback gateway, wat de socket-poort verdedigt.

### Klussen die nog open staan

1. **Echte beurt tonen** — de app-toepassing van de transport: een live-venster dat een server
   kiest, scant, en antwoordt zonder dat er iets aan de code gesleuteld hoeft te worden. Nu de sweep
   er is, is wat resteert het knelpunt in het paneel zelf: een discovery-knop vraagt een vijfde
   `SettingsEvent`-eis en een sweep van seconden in een venster dat al open is.

### Het modelvenster staat in de plukker

`Model::context_window` werd al gelezen door de parser en door verder niemand. De consumer is er nu en is bewust
klein: `row_for` in `baudet-ui/src/settings.rs` plakt het venster achter de naam, via
`Model::window_label`, en de combo box blijft de **id** dragen — een suffix bereikt het
instellingenbestand dus niet, wat door een eigen test wordt afgedwongen.

De wording is de moeite waard, want de getallen die servers publiceren zijn machten van twee.
Gemeten op de gateway, die voor elk model `max_model_len` noemt: 262144 wordt `256k`, 128000
wordt `128k`, 1048576 wordt `1m`, 8194 wordt `8k`; `window_label` rondt naar de dichtstbijzijnde
macht van twee, zodat 8194 niet als een rauwe telling leest. Een model zonder vensterveld —
gemeten: LM Studio, dat in het geheel geen lengteveld teruggeeft — krijgt zijn kale naam, dus de
plukker krijgt geen lege kolom en verzint geen plafond.

Twee dingen zijn bewust níet gedaan. Het venster wordt niet overgenomen als eigen instelling: de
server mag zelf beslissen wat hij aanvaardt, en `context_length_exceeded` bereikt ons nu al als
fout van de server zelf. En de keus van het model wordt op id onthouden in plaats van op het label,
want een rij die "Qwen … · 256k" leest matcht een kale id niet — anders zou de plukker "choose…"
tonen boven een keuze die gewoon gezet is.

## Bouten

`OMLX_API_KEY` (of de variabele die het ingestpaneel noemt) voor een gateway die erom vraagt;
`BAUDET_HOME` wijst de data-map aan. `cargo run -p baudet-ui` toont de app.
