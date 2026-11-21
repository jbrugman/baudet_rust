# Baudet (Rust)

Groen opgezette Rust-versie van de Baudet-app, met de Kotlin-app als voorbeeld. De UI is fase
één; de backend komt later. Crastructuur: `baudet-ui` (egui/eframe), `baudet-settings`,
`baudet-store`, `baudet-backend`. Geen enkele crate kent de UI.

## Stand van zaken

De front-end is representatief: transcript met streaming-regels, linker pane (project, tree),
rechter pane (changed files), composer, en het ingestpaneel met endpoint, modellenlijst,
redenereniveau en uitvoerplafond. `--screenshot` bewijst de staat van een sessie zonder muis.
`ScriptedBackend` voert beurtstromen uit voor een venster zonder server; `--live` stuurt de beurt
naar het opgeslagen endpoint via `ResponsesBackend`, die beide OpenAI-dialecten spreekt en wiens
dialect uit een server-scan komt. Het transport bestaat dus; wat er nu bovenop gebeurt, is hier
onder.

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

## Werkwijze bij een vastloper

Werkstaat vastleggen (commit + tag), dan een `codex/*`-branch voor het vervolg en `main`
onaangeroerd, met het besluit hierboven. Zo is er altijd een punt om naar terug te keren en
hoeft een omgegooide poging niets op te ruimen.

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

### Klussen die nog open staan

1. **Het modelvenster wordt gelezen en nergens gebruikt.** Gemeten op de gateway: elk model noemt
   daar `max_model_len` (Qwen 262144, DeepSeek 128000) en `Entry` leest die spelling — naast
   `context_length` en `max_context_length` — al, met een test die de alias afdwingt. Wat wél ontbreekt,
   is de consument: `Model::context_window` staat in de plukker maar wordt door geen enkele rij of
   label gelezen, dus de persoon ziet nergens hoe groot een venster is. Een getal kopiëren naar de
   eigen instelling is een ander gesprek en is bewust niet gedaan: de server mag zelf beslissen wat
   hij aanvaardt, en `context_length_exceeded` bereikt ons nu al als fout van de server zelf.
2. **Echte beurt tonen** — de app-toepassing van de transport: een live-venster dat een server
   kiest, scant, en antwoordt zonder dat er iets aan de code gesleuteld hoeft te worden.

## Bouten

`OMLX_API_KEY` (of de variabele die het ingestpaneel noemt) voor een gateway die erom vraagt;
`BAUDET_HOME` wijst de data-map aan. `cargo run -p baudet-ui` toont de app.
