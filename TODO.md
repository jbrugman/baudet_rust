# TODO — de volledige Kotlin-app naar Rust

Werklijst die elke beurt opnieuw wordt gelezen en bijgewerkt. Eén bron van de waarheid: `[x]` =
gebouwd én getest; `[ ]` = bestaat niet of niet volledig. Twistt de lijst met de code, dan wordt de
**lijst** gecorrigeerd, niet de code. De specificatie is de origineler: zijn `README.md`, `Docs/*.puml`
(en de `.kt`-bron zelf).

## Beslisregel (bij elk item hieronder)

1. **Zeker?** Gewoon bouwen op `main`.
2. **Niet zeker of richtingbepalend?** Snapshot (commit + `git tag`), nieuwe `codex/*`-branch,
   README-aanpassing met besluit + reden erbij, `main` onaangeroerd, doorbouwen. Altijd een punt om
   naar terug te keren; een omgegooide poging ruimt niets op.
3. **Dragende regel van deze port:** een widget bestaat pas als het domein eronder bestaat. Eerst het
   domein (crate + tests), dan de UI die het toont. De eerste ronde bouwde widgets die niets bedienden
   omdat het domein ontbrak — niet herhalen.

## Hoe een item wordt aangepakt

1. **Lees de source-first specificatie**: de relevante `Docs/*.puml` (architectuur) en de origineler
   `README.md` ("Working:"-lijst is de definitieve scope). Geen domein opnieuw ontwerpen.
2. **Embed-match** om de Kotlin-deuren te vinden (bge-m3-mlx-fp16, dim 1024, via `emb.py`). Dit is de
   werpvorm die ik eerder alleen noemde en niet gebruikte — hier staat hij daadwerkelijk:
   ```
   cd ../baudet
   # embedding per batch van main-.kt + de NL-werkvraag, cosine-ranking:
   python3 ~/.codex_local/scripts/emb.py "<NL-omschrijving>" -   # + per-feature-teksten
   ```
   Gemeten cos: mcp→McpJsonRpc 0.72, diff→UnifiedDiff 0.70, read→ReadFileTool 0.64,
   execute→SafeCommands 0.62, context→ContextStrategy 0.62, i18n→Language 0.61. Match **wijst** de
   deur; de `.kt`-bron is het antwoord. Embeddings zijn géén chat-beurten; geen sweep over meerdere
   embed-modellen.
3. Bouw crate/domein + tests → pas UI aan → vink hier af met commit-hash.

## Architectuur van de origineler (uit Docs/components.puml)

Lagen en wie wie aanroept — de port moet deze grenzen respecteren:

- **UI-laag**: Main, ChatPanel, ProjectSidebar, FileTreePanel, ModelsPanel, SettingsPanel, DiffViewDialog.
- **Orchestrator** (worker-thread):AppState — send/compact/cancelTurn, poorten + ceilings, gatewayFactory.
- **API-laag**: ResponsesClient → Http (POST /responses, SSE) → SseFramer → TurnAccumulator→Turn;
  CancellationToken (socket-registry); ModelWindows; ModelGateway/ResponsesGateway (seam).
- **Tool-laag**: ToolLoop, ResponsesTurnSource (+trim), ToolRegistry, CallExtractor/PromptedCallExtractor,
  ToolContext (rechten/approvals/allow-lijsten), FolderBoundary, SafeCommands/CommandSignature,
  ExecuteWorkspace, tools: ReadFile/EditFile/Execute/Git.
- **Context-laag**: SessionState, ContextTrimmer, Transcript (scrub/split/fit), Compactor/CompactionPlan.
- **Domain**: AppConfig, Project+FolderPermission, ContextStrategy/ToolMode.
- **Persistence**: H2Store → ProjectStore/SettingsStore/TranscriptStore.

Besluiten die meekomen: `previous_response_id` alleen bij Stop-at-limit + optie; store=responses;
tolerante eventnaam-census (onbekende event nooit fataal); redeneer-tokens apart; client-side
compaction (geen /compact-endpoint); toolloop runt onafhankelijke **reads parallel (pool ≤2)**,
writes/gepinde calls blijven serieel.

## A. Persistentielaag — JSON-catalogus boven rollouts (branch `codex/store-backend`)
- [x] Persistentiekeuze: JSON-catalogus voor de project-/sessie-index; rollout blijft JSONL. Geen rusqlite of sled, omdat H2 JVM-only is en de offline crate-boom geen SQLite-bouw toelaat. Besluit en grens: README.
- [x] Vervanging van de drie H2-interface-taken: SettingsStore blijft `baudet-settings`; TranscriptStore blijft `baudet-store`; ProjectStore komt in `baudet-projects::catalog` (projects + sessie-index) uit `store/{ProjectStore,SettingsStore,TranscriptStore,H2Store}.kt`.
- [x] Data-dir-deel: catalogus gebruikt `baudet_store::data_dir()`, met `BAUDET_HOME`-override. Een latere `--home`-UI-optie resteert als UI-aansluiting.

## B. Project- en permissiedomein — crate `baudet-projects`
- [x] `Project { id, name, folderPath?, permissions-bitmask, archived, createdAt }` — `project/Project.kt`.
- [x] `FolderPermission { Read=1, Write=2, Execute=4 }` — `project/FolderPermission.kt`.
- [x] `Permissions`-bitmask (encode/decode/has/with) — `project/Permissions.kt`.
- [x] `ProjectIds`/`SessionIds` — `project/ProjectIds.kt`, `session/SessionIds.kt`: `p-`/`s-` plus acht hex-tekens.
- [x] Sidemap-project-/sessieindex met actieve-volgorde, cascade, titeloverride, afgeleide titel (eerste gebruikersregel, cap 48), meest-recent-eerst en atomair schrijven. Tests: 10 groen; workspace: 144 groen.
- [x] `ToolMode { Off, Ask, Auto }` — `config/ToolMode.kt` naar `baudet-settings::config::ToolMode`: default `Ask` (`AppConfig.kt:86`), claimvolgorde Off|Ask|Auto behouden omdat die in de origineler de combopositie drijft (`SettingsPanel.kt:263,371`), JSON-waarden smallcase `off|ask|auto`. `Settings.tool_mode` bestaat en overleeft het bestand; een bestand zónder dat veld leest als `Ask`, nooit als `Off`. De UI-keuzelijst en de poort zelf staan bewust nog niet: zonder `ToolGate`/tool-lus (item E/F) bedient zo'n keuzelijst niets, en dat is de dragende regel van deze port. `ContextStrategy` blijft bij item F. Tests: 3 in `config.rs` (default+veld, older-bestand, woord/index) — workspace 234 groen. commit `f4afe1c`.
- [ ] `ToolGate`-afdwinging van ToolMode — sluit aan bij item E (`.kt: AppState.kt:739` `Off → Disabled`, `951` `Auto → goedkeuring overgeslagen`).
- [x] UI-koppeling van deze catalogus naar de linkersidebar (item C). commit `af9b336`.
- Embed-match: "rechten lezen schrijven uitvoeren" → Permissions.kt 0.57, FolderPermission 0.53.

## C. Correcte vensterlay-out (na B)
- [x] Lay-out uit `Main.kt:150-170`: `[ ProjectSidebar | Chat/Settings ]  [ FileTree ]`. Sidebar links, bestandsboom rechts; README-besluit "projecten links". commit `af9b336`.
- [x] Linker `ProjectSidebar`: projecten-boom + sessies eronder + `Unassigned` + Add-project + read/write/execute-vakjes + contextmenu (hernoemen, map koppelen/ontkoppelen, archiveren, verwijderen met cascade + bevestigingsriem). Domein in `sidebar.rs`, tekening in `panels::project_sidebar`, 12 testen in `tests/project_tree.rs`. `ui/ProjectSidebar.kt` (embed-match 0.53). commit `af9b336`.
- [x] Twee onafhankelijke toolbar-toggles: sidebar in/uit (west) en bestandsboom in/uit (oost) — `sidebarToggle` + de NORTH-toolbar in `Main.kt:176-184,247-269`. De toggle is een eigen `Widget` met het id van de aanroeper, geplaatst met `Ui::put` op een eigen vierkant op de rand van de kop (niet als knop in de rij — een wikkel daar erft de volle root-breedte en wordt onbereikbaar, gemeten). Verbergen tekent het paneel gewoon niet; `PanelState` onthoudt de breedte, dus hij keert er exact op terug. Tests: 4 in `tests/panel_toggles.rs` (klik → vlag om + breedte behouden, weer klik → terug op dezelfde breedte). commit `254a951`.
- [~] Git-kleuren in de bestandsboom: de boom **links** kleurt nu bestandsrijen met de rgb-waarden van de origineler (`BaudetPalette.kt:65-67`: added/modified/renamed groen 46,160,67 · deleted rood 210,70,70 · untracked blauw 130,170,220), opgezocht per repository-relatief pad uit `probe_changes`; mappen blijven kleurloos zoals in `FileTreePanel.colorFor`. De rechter `changes`-lijst schakelt van de diff-inkten naar dezelfde tabel, zodat een pad overal één kleur leest. **Restant:** dubbelklik = patch-venster (`DiffViewDialog`) staat nog als item G (`git/` + `diff/`); unit-tests in `panels::tests` (kleurentabel, `relative_to`, `FileStatus`). commit `254a951`.
- Splitsing van C: de **sessie-lading** (`OpenSession` → transcript uit de rollout) staat als item D; `SidebarIntent::OpenSession` onthoudt nu alleen het gekozen sessie-id.

## D. Sessies (na C)
- [ ] Meerdere sessies, niet één globe transcript — `session/SessionState.kt`.
- [ ] Sessie-opslag + herstel (loadSession/startProjectChat), lazy paginering — `store/TranscriptStore.kt`, `store/SessionSummary.kt`.

## E. Tool-laag read/write/execute (na B; de kern van "de werking is anders")
- [x] Veiligheidsbasis van de laag, crate `baudet-tool`: `safe_commands` (read-only-belofte + gezins-teken,
      volle tabel inclusief blocklist, git-subcommando's, cluster-vlagherkenning en de
      end-of-options-telling), `command_signature` (wat één "altijd" pinnt), `folder_boundary` (symlink-
      opgeloste grens, zeven tests waarvan drie echte symlinks naar buiten) en `folder_listing` (de hint:
      diepte 3, cap 50, geen dot-mappen). 35 tests in `crates/baudet-tool`. **Gemeten fout tijdens het
      porteren:** `is_safe("cargo test")` is `false` — `cargo`/`mvn` staan in de origineler op geen van
      beide lijsten, ze krijgen alléén een gezins-teken via `trusted_key`; de test eiste het verkeerde
      gedrag, niet de code. Zelfde les bij de diepte-cap: die is relatief aan de root, een `tempdir` woont
      zelf al op niveau vier, dus de eerste test bewees niets. commit `cea2f33`.
- [x] `Tool` + `ToolRegistry` + `ToolRunner` — `tool/Tool.kt`, `tool/ToolRegistry.kt`, `tool/ToolRunner.kt`.
      Poort-getrouw: één registratie per naam met verdringsmelding, unknown name wordt een fout-resultaat
      voor het model in plaats van een panic, en `ToolRunner` scheidt uitvoeren van kennen zodat de loop
      later alleen de ronde-regels hoeft toe te voegen.
- [x] `ToolLoop` (max iteraties, reads parallel ≤2 via `can_run_parallel`, writes serieel) — `tool/ToolLoop.kt` (ziet tool-loop.puml). `crates/baudet-tool/src/tool_loop.rs` (182 testen in de crate, workspace 383 groen). Een async bestuurder over de drie bestaande naden (TurnSource, CallExtractor, ToolRunner) die zelf niets persisteert: hij geeft `messages`/`undos` terug en de aanroeper beslist. Gemeten eigenschappen, niet aangenomen: **serialiteit** — een gemengde batch blijft serieel, dus een write racet nooit en een goedkeuringskaart blijft live opkomen; **fan-out-piek 2** — twee draden meldden elkaar via een poort `peak()==2`, en `MAX_PARALLEL_TOOL_CALLS=2` begrenst ook een batch van vijf (een kom, geen wachtrij-verlies); **modelvolgorde** — kaarten en observaties volgen de callvolgorde van het model, ook bij parallel, zodat een fan-out voor de lezer niet van een seriele run te onderscheiden is; **pool-overflow**; **observatie-cap** `MAX_OBSERVATION_CHARS=32_000`; **`MAX_BLANK_TURNS=2`** voor een lege redeneerbeurt (de nudge steekt alleen in de werkende kop, nooit in het transcript); **max-iteraties-budget** met één afrekenbeurt in proza, zodat de persoon niet zonder antwoord zit; **context-plafond vóór het veroordeelde verzoek** — een kop die al te lang is, kost nul beurten, nul stappen en nul regels op het scherm. Twee besluiten. (1) De pool is `thread::scope` rond een `SharedContext`-wrapper met `unsafe impl Send/Sync`, want `ToolContext` is wél `Send` maar níét `Sync` en de tools lezen er hun rechten uit. (2) De plafond-schatting wordt gemeten door de stub-bron zélf (`estimates`), niet narekend in de test — de eerste versie beweerde één betaalbare beurt en was damit fout; gemeten: 5, dan 44 van plafond 60, en pas op 82 stapt de lus uit. Resteert (nieuwe haken hieronder): de instelling `max_tool_iterations` zélf en de UI-draden. commit `a85291f`.
- [ ] `max_tool_iterations` als instelling — `config/AppConfig.kt:88` (default 8) bestaat in `baudet-settings::config::Settings` NIET: de lus neemt het getal aan via `ToolLoop::new(.., max_iterations)`, dus er is nu geen dragende default van 8 en geen veld in het bestand. Te doen: veld + default 8 + een oud bestand leest als 8 + de instellingskaart die het schrijft.
- [ ] De lus aan de UI — `app/AppState.kt:926-1022`: `maxIterations` uit de settings doorgeven, `previous_response_id` uit de sessie, en de goedkeuringen van `ToolContext` op echte egui-kaarten. Zover is de tool-rij in de UI nog sierraad: er draait niets achter.

- [x] Call-extractie: native + fenced JSON — `tool/CallExtractor.kt`, `tool/PromptedCallExtractor.kt`.
      `crates/baudet-tool/src/call_extractor.rs` (13 testen): native calls eerst en vooraan, daarna de
      ```` ```baudet-tool ````-blokken; `tool`/`name` en `arguments`/`args` doen allebei mee, een blok dat
      niet parseert wordt overgeslagen in plaats van gegooid, en een native argumentstring die niet parseert
      wordt een leeg object zodat de tool de ontbrekende velden terugmeldt in plaats van te crashen.
      **De scanner is met de hand gesucht omdat de offline crate-boom geen regex kent, en dat is de plek
      waar een port stil verkeerds kan zijn; daarom is hij differentieel getest tegen `Regex::captures_iter`
      van exact het Kotlin-patroon: 400 000 gegenereerde inputten uit 22 fence-fragmenten plus 16
      handgevallen, nul verschillen.** Drie eigenschappen die een naïevere lus verkeerd radt, allemaal
      gemeten: bij een mislukte opening schuift de regex **één** teken op (dus ` ```baudet ```baudet-tool `
      levert het blok daarna, drie overslaan verliest het); een header mag spaties en tabs maar géén `\r`;
      en vier backticks zijn géén langere fence (`...\n```` ` levert body `{}`). Twee eigen gebreken
      tijdens dat werk, beide vastgelegd als test: pointer-aritmetiek gaf een overflow-crash op elke
      niet-ASCII body, en `text` op `start` snijden **vóór**dat de backticks gecontroleerd zijn, panicte op
      een accent vóór een blok — één beurt met een `é` was daarmee een crasht. De lus indexeert nu bytes en
      guardt vóór élke slice. Crate 157 groen, workspace 397 groen. commit `ebd7ff1`.
- [x] `TurnSource`-naad — `tool/TurnSource.kt`. `crates/baudet-tool/src/turn_source.rs` (2 testen):
      `next_turn(TurnRequest) -> TurnFuture`, met de zes argumenten van de origineler als één struct omdat
      vier van de zeven aanroepplekken daar `..default..` laten en Rust die defaults niet heeft. De bron
      geeft een boxed future (`Send`) terug, want een echte bron leest een socket; de loop blijft daardoor
      runtime-vrij. Gemeten bij het schrijven: een `FnMut`-haak leent niet, dus een implementatie moet
      `mut request` overnemen — dat staat nu bij het veld in plaats van als verveling in een test.
- [x] `ToolContext` (rechten, approvals, allow-lijsten) — `tool/ToolContext.kt`. Besluit: de goedkeuringen
      zijn callbacks (`Approval`, `CommandApproval`) en geen UI-type, zodat een tool vrij blijft van egui en
      de UI op háár thread kan vragen terwijl de tool blokkeert. De twee allow-lijsten zijn _providers_ en
      geen kopie: een pin uit ronde twee moet dezelfde beurt al gelden, anders wordt de persoon over het
      commando gevraagd dat hij zojuist vastlegde. Rechten staan uit, dus een context die niets mag, is de
      default en niet een uitzondering.
- [x] `FolderBoundary` (buiten de map verboden) — `tool/FolderBoundary.kt`. `crates/baudet-tool/src/folder_boundary.rs` (8 testen): `real_within` lost beide kanten op en onthoudt het oordeel wanneer één kant faalt — root ontbreekt of een kapotte link — en `resolve_file_within` weigert in dezelfde model-gerichte formulering voor beide bestands-tools. De lexicale `normalize()` (de JDK-`normalize()`: `..`-segmenten op de top blijven staan en worden daarboven af gewezen, de conservatieve richting) gaat aan de symlink-controle vooraf. Beide aanroep-poorten staan er en zijn poort-getrouw: `read_file.rs` en `edit_file.rs` nemen `resolveFileWithin` over, en `edit_file.rs` vraagt vóór het schrijven opnieuw `real_within`, zoals `EditFileTool.kt:113` — de schrijver mag nooit op een oud oordeel schrijven. Bewezen in de testen: een link die/binen de map woont maar buitenwijst, doorstaat élke lexicale check en wordt door deze module alsnog geweigerd. commit `a85291f`.
- [x] `ReadFileTool` — `tool/ReadFileTool.kt` (embed 0.64). Poort-getrouw en gemeten: `MAX_FILE_BYTES`
      1 000 000 met de weigeringstekst die "976 KB read limit" noemt, `MAX_CHARS` 60 000 in **karakters**
      (een test met 60 001 × `é` bewijst dat het geen bytes zijn), binaire weigering op byte 0, een leeg
      bestand is succes, en een ontbrekend pad hint naar de mapinhoude via `folder_listing`. Weigeringsformat
      `read_file: <reden>`, zodat het model uit de tekst alleen al weet wat het anders moet doen.
- [x] `EditFileTool` + `PatchApplier` + `Edit` + de diff-laag — `tool/{EditFileTool,PatchApplier,Edit,PatchOutcome}.kt`,
      `diff/{LineDiff,UnifiedDiff,DiffLineKind}.kt`. Ask toont de diff-preview vóór élke write. Twee dingen
      opgemeten in plaats van aangenomen, en beide keerden mijn test: `"c\n"` zijn bij `split('\n')` **twee**
      regels, dus de diff draagt een lege slot-regel mee en die wordt `" \n"` in de render — gladstrijsen zou
      de output onherkenbaar maken voor een externe kijker; en een afwezig bestand valt al weg in
      `resolve_file_within`, dat de map-opsomming zelf meldt, zodat de "could not be read"-tak alleen een
      bestand treft dat wél bestaat. "no change" en "declined" dragen géén `edit_file:`-prefix, want de
      origineler bouwt die `ToolResult` rechtstreeks. De tijdvolgorde is de veiligheid: lezen → patchen in het
      geheugen → vragen → **opnieuw lezen** → pas schrijven; die tweede lezing vangt een handmatige wijziging
      tussen preview en klik. Een gelande write draagt zijn `Undo`-handgreep mee, zodat `LoopOutcome.undos`
      straks de hele beurt terug kan draaien. `Tool` kreeg `item_kind` (edit = `FileChange`), zodat de loop
      nooit op tool-*naam* hoeft te matchen.
      Besluit: `ToolResult` verloor `Clone` en kreeg `Debug`/`PartialEq` met de hand, want een sluiting is noch
      kopieer- noch vergelijkbaar en `Debug` toont alleen of er een handgreep is — dezelfde keuze die
      `ToolContext` al voor zijn callbacks maakte. commit `2660bf8`.
- [x] `ExecuteTool` + `SafeCommands` + `CommandSignature` + `ExecuteWorkspace` + `ProcessRunner` — shell in map, TMPDIR, read-only auto / altijd-vraag, family-pin. `tool/{ExecuteTool,SafeCommands,CommandSignature,ExecuteWorkspace,ProcessRunner}.kt` (embed 0.62). `process_runner.rs` (15 testen: dubbeldradige drain, head+tail-cap van 16 KiB, begrensde join), `execute_workspace.rs` (7 testen: TMPDIR-canoniceer, schrijfbaarheid, `within_allowed`) en `execute_tool.rs` (14 testen: rechten, drie pre-goedkeuringsroutes,decline draait niets, exit-code en beide streams, timeout zonder fantoom-code, TMPDIR-zetting, eigen script-opruiming). De environment gaat mee via `ProcessRunner::run_with`, de environment-haak die `ExecuteTool.kt:109` ook gebruikt; `set_var` op dit proces zou de parallelle threads van dezelfde beurt racen. Crate 116 groen, workspace 350 groen. commit `f6a8d37`.
- [x] `GitTool` — lezen standaard; schrijft verbs (add/commit/checkout/stash/push) opt-in + Write-recht + goedkeuring; force-push/rewrite geblokkeerd. `tool/GitTool.kt` + `git/`. `git_command.rs` (14 testen: de negen lees- en vijf schrijf-verbs van de origineler, álle globale vlaggen (`-C`, `--git-dir`, `--work-tree`, `-c`, `--exec-path`) geweigerd vóór de processtart zodat een call het project nooit verlaat, `push` zonder één vlag dus onmogelijke force, `stash` beperkt tot zijn vijf echte subcommando's, `summary` als één leesbare regel voor de goedkeuringsdialoog) en `git_tool.rs` (12 testen: lezen vraagt nooit, schrijven alleen bij Write-recht én setting, de weigeringstekst draagt zoals `GitTool.kt:78` zijn eigen `git:`-prefix en niet de lokale `fail()`-omslag, een niet-nul exit is model-gerichte feedback, een eerdere pin voor dezelfde samenvatting slaat de vraag over). Twee gemeten gedragissen-corriges op mijn eerste tests, niet op de code: `log --oneline` wordt **geweigerd** — de poort kent geen per-subcommando-vlaggen, de standaardvlaggen zitten al in `base` — en de "Allowed:"-lijst is letterlijk de advertentievolgorde `status, diff, diff_staged, log, show, branch, ls_files, rev_parse, blame`. Besluit: `GitWrites::{Disabled, Enabled}` in plaats van de Kotlin-bool `writesAllowed`, want drie routes (advertentie, weigering, uitvoering) delen dat besluit en `writes_allowed` naast `write_granted` is precies de verwarring die deze naamgeving voorkomt. `item_kind` blijft `ToolCall`: de origineler draagt geen `itemKind` mee. Crate 142 groen, workspace 376 groen. commit `c5f8241`.
- [x] `ToolGate` (Active/Disabled/NoProject/NoFolder/NoWrite) + ToolMode-afdwinging — `app/ToolGate.kt`, turn-decision.puml. `crates/baudet-projects/src/tool_gate.rs` (6 testen): `decide` als zuivere voorrang in dezelfde volgorde als `AppState.kt:739-744`, `gate_for_session` als catalogusroute (`AppState.kt:738`), `is_active` = `toolsActive()` (`AppState.kt:767`) en `warns` = de amber van `ChatPanel.kt:437-442` — **alleen** `NoFolder` en `NoWrite`, want `NoProject` en `Disabled` zijn bewuste keuzes en blijven stil. Extra `Catalog::project(id)`: de sidebar zocht vijf keer lineair in `list_projects()` (elk een volledige lijstkopie) en de poort vraagt per beurt, vóór élke toolcall. Twee besluiten. (1) `tools_on` is een bool en niet de `ToolMode` van de settings-crate: die richting is er wél, de omgekeerde zou deze crate aan één vergelijking hangen, en `AppState.kt:739` vraagt daar ook maar één ding (`!= Off`); de settings-crate is daarom **alleen dev-dependency**, voor één test die bewijst dat de echte default `Ask` is en dus `Active` geeft bij een map met Write. (2) Een sessie die niet in de index staat geeft `NoProject`, niet een foutmelding, want deze catalogus is de index en niet de eigendom — het gesprek woont in een rollout, terwijl de origineler haar project in het geheugen draagt en dus nooit mist. Beide aannames zijn gemeten vóórdat ze in een comment kwamen (`set_folder` trimt een blanco pad zelf weg; default is `Ask`). Wat nog resteert: de amber-tekst zelf is item H en de afdwinging gebeurt in `ToolLoop`, dat deze poort als ingang neemt. Crate 11 groen, workspace 382 groen. commit `a78482e`.

## F. Context- en verdichtingslaag (na D)
- [ ] `ContextStrategy` + `ContextTrimmer` (systeemprompt leidt, bound vóór vertrek) — `config/ContextStrategy.kt`, `session/ContextTrimmer.kt` (embed 0.62).
- [ ] `Transcript` scrub/split/fit — `tool/Transcript.kt`.
- [ ] `Compactor`/`CompactionPlan`/`CompactionOutcome` (vouwen over meerdere calls, eerlijk bij niets) — `session/Compactor*.kt` + compaction-fold.puml.

## G. Git- en diff-laag (nodig voor C + E)
- [~] `GitCommand` is omgezet naar `git_command.rs` (zie item E). **Restant:** `GitInspector`/`GitStatus`/`FileChange` — `git/`.
- [ ] `UnifiedDiff`/`LineDiff`/`DiffHighlight`/`DiffLineKind` + `NewFileDiff` — `diff/` (embed 0.70) + `ui/NewFileDiff.kt`.
- [ ] `DiffViewDialog` (gekleurde git patch) — `ui/DiffViewDialog.kt`.
- [ ] Dubbelklik in de linker bestandsboom opent die patch voor één pad (kleuren staan er al; de dialoog is dit venster) — sluit het restant van item C af.

## H. i18n (na C)
- [ ] `Key`/`Language`/`Texts` NL/EN/FR/DE, runtime wissel, EN default — `i18n/` (embed 0.61). Lokaal-honesty-note reger.

## I. MCP client (later)
- [ ] stdio-only McpClient/JsonRpc/ToolAdapter/Servers/Config; per-server always-allow + timeout — `mcp/` (embed 0.72). (HTTP/SSE + Baudet-MCP-server bestaan in de origineler NIET — niet overbouwen.)

## J. UI-rest (loopt parallel met domeinen)
- [ ] Markdown/MarkdownHtml ( fenced, koppen, bold/italic, inline code, wrap-optsie) — `ui/Markdown*.kt`.
- [ ] ThinkingLayout (reasoning-dock boven de input, display-only) — `ui/ThinkingLayout.kt`.
- [ ] About/Help-vensters + helpMenu — `ui/{About,Help}.kt`.
- [ ] ImageClipboard: ctrl+V = beeld als OpenAI image-part; tool-beeld inline met Save-strip — `io/ImageClipboard.kt`.

## Bronnen die niet meegaan (uit origineler README "Not there yet")
- Geen in-app text editor.
- Geen echte **concurrente** gesprekken (twee live-sessies tegelijk); de turn-claim is al een teller.
- Geen native GraalVM-build (Rust is hier juist native — dit punt verdwijnt).

## Wat er al is in Rust
- [x] `baudet-core` (events/reductie/rollout), `baudet-backend` (Backend + Scripted + SSE + HTTP/SSE
      beide dialecten + streaming + protocol + server-scan), `baudet-settings` (pad + schrijven +
      `GET /v1/models`), `baudet-store` (rollouts + index), `baudet-app` (proxy), `baudet-ui`
      (transcript/streaming/composer/instellingen/detectie/folder-kiezer).
- [x] Toolrijen als **regels** (weergave), maar géén tool draait erachter → item E.

## Werkwijze per item
(1) Lees `.puml`/README-spec, (2) embed-match de Kotlin-deur, (3) lees die `.kt`, (4) domein+tests,
(5) UI, (6) vink af met commit-hash, (7) commit. Twijfel over richting → beslisregel hierboven.
