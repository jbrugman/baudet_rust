# Baudet (Rust)

Groen opgezette Rust-versie van de Baudet-app, met de Kotlin-app als voorbeeld. De UI is fase
één; de backend komt later. Crastructuur: `baudet-ui` (egui/eframe), `baudet-settings`,
`baudet-store`, `baudet-backend`. Geen enkele crate kent de UI.

## Stand van zaken

De front-end is representatief: transcript met streaming-regels, linker pane (project, tree),
rechter pane (changed files), composer, en het ingestpaneel met endpoint, modellenlijst,
redenereniveau en uitvoerplafond. `--screenshot` bewijst de staat van een sessie zonder muis.
`ScriptedBackend` voert beurtstromen uit; de echte transportlaag ontbreekt nog.

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
| `baudet-backend` | het `Backend`-contract + `ScriptedBackend` + `sse.rs` (parser, nog geen transport). |
| `baudet-settings` | settings-pad, schrijven, en `GET /v1/models`. Leest geen UI. |
| `baudet-store` | rollouts op schijf en hun indexeringslaag. |
| `baudet-app` | de proxy bovenop een backend: events, checkpoints, interrupts. |
| `baudet-ui` | egui/eframe: frames, rijen, panelen, het ingestpaneel. |

Bewezen gedrag van de frontend: chat over een script, plan/details, toolrijen met wachttijd en
details, interrupt/resume met toetsenbord en met Esc, Shift+Enter, scroll-vastzetten bijknepen,
transcript opschonen, rijselectie, workspace-tree met lazy vouwen en verversen, changed-files met
kopieerbare pakketcodes, settings-venster incl. model-ophalen.

### Klussen die nog open staan

1. **Streaming-transport** (#B). Er is geen enkel netwerkverzoek op de chat. `sse.rs` parseert
   alleen; de HTTP/SSE-laag ontbreekt. Detectie van het protocol hoort bij een server-scan (die
   nooit `/chat/completions` of `/responses` raakt, alleen `GET /v1/models`) — het doel is lokaal,
   daar draaien meer servers (oMLX, LM Studio, Jan, Ollama).
2. **Model en reasoning naar de aanvraag** (#2). `SendRequest` heeft géén model-veld, dus de
   keuzes uit het ingestpaneel bereiken de backend nog niet. Kan pas na 1, want
   `ScriptedBackend` negeert het request.
3. **`max_model_len` als alias** in `baudet-settings::provider::Entry`. De gateway-venstermaten
   (262144 voor Qwen, 128000 voor DeepSeek) gaan nu verloren; de parser kent alleen
   `context_length`/`max_context_length`.
4. **Echte beurt tonen** — de app-toepassing van 1 en 2.

## Bouten

`OMLX_API_KEY` (of de variabele die het ingestpaneel noemt) voor een gateway die erom vraagt;
`BAUDET_HOME` wijst de data-map aan. `cargo run -p baudet-ui` toont de app.
