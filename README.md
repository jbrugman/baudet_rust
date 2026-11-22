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

De front-end is de lay-out van de origineler: `[ ProjectSidebar | Chat/Settings ] | FileTree`
(`Main.kt:161-170`). Links de projectenboom met zijn sessies en `Unassigned`, de mapkoppeling en de
read/write/execute-vakjes; rechts boven de bestandsboom en daaronder de changed-files; in het midden
de tabs. Inclusief transcript met streaming-regels, composer, plan/details, toolrijen,
interrupt/resume, en het instellingpaneel met endpoint, modelplukker, redenereniveau en
uitvoerplafond.

**Wat er nog niet aan hangt, is de werking eronder, niet de lay-out.** De tool-rijen zijn sierraad
tot item E de draad naar `ToolLoop` legt; er draait nog één transcript in plaats van meerdere
sessies (item D.2); er is nog geen verdichting (F), geen diff-dialoog (G) en de UI is Engelstalig
terwijl de origineler vier talen draagt (H). `TODO.md` is de bron van de waarheid over die stand.
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

## Besluit: de projectindex is geen transcript, en krijgt daarom geen SQL

De Kotlin-persistentielaag plaatst projecten, sessie-index en berichtinhoud in één H2-database. Rust
 heeft die keuze niet: H2 is JVM-only en een offline gemaakte crate-boom bevat geen rusqlite. De
rolloutlaag bewijst bovendien dat platte JSON de gespreksgeschiedenis al duurzaam maakt. Deze port
splitst de twee taken daarom expliciet:

* `baudet-store` blijft één sessie per map, append-only, met de bericht- en itemregels;
* `baudet-projects` krijgt alleen de catalogus: projecten en de metadata waarmee de sidebar hun
  sessies toont;
* de transcript-inhoud wordt niet in de catalogus gekopieerd. Een projectverwijdering mag de
  bijbehorende rollout-mappen niet onherkenbaar achterlaten, maar de index is geen tweede bron van
  gespreksgeschiedenis.

Dat is smaller dan één SQL-database en past bij de bestaande rustige gelaagdheid: de catalogus is
een herstelbare index, de rollout is de bron van waarheid van de beurt. De catalogus schrijft
atomair via een tijdelijk bestand plus rename, waardoor een afgebroken opslag het vorige complete
bestand heel laat. Voor de lokale id's geldt dezelfde vorm als Kotlin (`p-`/`s-` plus acht tekens);
ze worden gegenereerd uit een monotone tijd en een procesloper, zonder cryptografisch
random-dependency: de eis is lokale uniciteit, niet een niet-raadbare identifier.

## Besluit: projecten links, bestandsboom rechts — zoals `Main.kt` tekent

Eerdere rondes zetten de workspace-/bestandsboom in de linkerkolom en lieten de projectsidebar
ontbreken. Dat is een lay-out die de origineler niet heeft, en het verdween daardoor zichtbaar
gedrag: projecten toevoegen, sessies eronder, de inbox en de drie maprechten. `Main.kt:150-178`
tekent expliciet

```
[ ProjectSidebar | tabs ]  | FileTree
```

dus deze port draait de siden terug: `panels::project_sidebar` als `Panel::left("projects")`
(280 px, min 190 px), de bestandsboom naar rechts. De twee toolbar-toggles uit `Main.kt:176-184` staan
er nu ook, in de kop op de west- en oost-rand — hieronder onder "twee pane-toggles".

Het domein onder die sidebar staat in `sidebar.rs` en is bewust vrij van egui: het is een model met
publieke methoden (`add_project`, `set_permissions`, `answer_confirm`, …) dat de catalogus uit
`baudet-projects` doorgeeft. `panels.rs` tekent alleen wat erachter bestaat — de dragende regel in
[`TODO.md`](TODO.md): een widget bestaat pas als het domein eronder bestaat. De testen in
`tests/project_tree.rs` draaien tegen een echte egui-pass in een tijdelijke `BAUDET_HOME`, dus de
knop is vindbaar via zijn adres in plaats van via een aanname over het scherm.

## Besluit: de teksten zitten in een tabel, niet in bundles

De origineler kent één enum `Key` met vier woorden per sleutel (`i18n/Key.kt`) en één `Texts`
dat daaruit leest (`i18n/Texts.kt`). Die vorm is overgenomen: `baudet-i18n` draagt 264 sleutels
in-en-de-taal, met namen uit `Key.kt` zodat een `.kt`-regel en een `.rs`-regel elkaar zonder
woordenboek vinden. Bewust géén `.ftl`/`.po`-bundles: een typfout in een bundel is een
runtime-verrassing, een typfout in de tabel is een compileerfout, en een ontbrekende vertaling
bestaat in deze opbouw niet — de compiler laat een onvolledige tabel niet toe.

Twee regels die het verschil maken:

* **Een adres draagt geen taal.** `id_salt`-waarden en de id van een knop blijven letterlijk
  staan (`UNASSIGNED` is het adres van de inbox, `Key::InboxNode` is wat er op de riem staat).
  Zit de vertaling in het adres, dan staat een knoopje na een taalwissel "open" als een ander
  knoopje en is wat iemand openzette kwijt.
* **De taal is één waarde voor het hele proces.** `thread_local` leek handig en is meetbaar fout:
  de werker-draad die een beurt bouwt, de test-draad die een bevestiging leest en de paint-draad
  zijn drie draden, en per draad zou elke in een andere taal antwoorden. Nu een `AtomicU8`,
  gezet uit het ingestellingenbestand bij het openen (`AppState.kt:195`) en bij het opslaan
  (`Main.kt:74-82`, want `TipSave`: "The language switch is applied here too.").

## Besluit: twee pane-toggles, en git-kleuren in beide bomen

**Twee toggles.** `Main.kt` zet in zijn NORTH-toolbar één knop op `BorderLayout.WEST` (de sidebar) en
één op `BorderLayout.EAST` (de bestandsboom), elk met de herinnerde-breedte van `sidebarToggle`
(`Main.kt:247-269`). In Rust is dat een eigen `Widget` in `panels.rs` met het `Id` van de aanroeper,
geplaatst met `Ui::put` op een eigen vierkant op de rand van de kopstrook. Waarom niet als knop in de
koprij: een wikkel daar erft de volle root-breedte mee, dus valt zijn klikadres ná de kop en wordt hij
onbereikbaar — gemeten, met een proef-knop die in elke pass `hovered=false` hield. `Ui::put` geeft hem
een eigen rect op de rand, en dát adres (`root_ui_id(..).with((PANEL_TOGGLES, ..))`) vindt een test.
Verbergen tekent het paneel gewoon niet; `PanelState` is persistent en onthoudt de breedte, dus de
terugkeer is precies dezelfde breedte. Vier testen in `tests/panel_toggles.rs` bewijzen dit met een
klik, niet met een aanname.

**Git-kleuren.** De boom links en de lijst rechts lezen dezelfde drie rgb-waarden van de origineler
(`BaudetPalette.kt:65-67`): added/modified/renamed groen (46,160,67), deleted rood (210,70,70),
untracked blauw (130,170,220). De boom zoekt een status per repository-relatief pad (`probe_changes`);
mappen blijven kleurloos, want git geeft een toestand voor bestanden, niet voor een map — precies
`FileTreePanel.colorFor`. Bewust géén hergebruik van de diff-inkten (`DIFF_ADDED`/`DIFF_REMOVED`): die
zijn uit een diff-venster gemeten en horen bij een diff, niet bij deze statuslijst. De tabel staat
vastgelegd in `panels::tests`, zodat een stil meeveranderende kleur hier struikelt.

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
transcript opschonen, rijselectie, de linkerprojectbalk met add/hernoemen/map koppelen/archiveren/
verwijderen, de foldertoepasser, de rechter workspace-tree met lazy vouwen en verversen, changed-files
met kopieerbare pakketcodes, settings in het middenpane, model-ophalen uit een server-sweep, twee
pane-toggles met herinnerde breedte, en git-kleuren in zowel de rechter boom als de changed-files-lijst.

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
