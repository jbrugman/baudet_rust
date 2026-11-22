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
- [ ] `ToolLoop` (max iteraties = `maxToolIterations` 8, reads parallel ≤2 via `can_run_parallel`, writes serieel) — `tool/ToolLoop.kt` (ziet tool-loop.puml). Vereist een turn-bron die nog niet bestaat.
- [ ] Call-extractie: native + fenced JSON — `tool/CallExtractor.kt`, `tool/PromptedCallExtractor.kt`.
- [x] `ToolContext` (rechten, approvals, allow-lijsten) — `tool/ToolContext.kt`. Besluit: de goedkeuringen
      zijn callbacks (`Approval`, `CommandApproval`) en geen UI-type, zodat een tool vrij blijft van egui en
      de UI op háár thread kan vragen terwijl de tool blokkeert. De twee allow-lijsten zijn _providers_ en
      geen kopie: een pin uit ronde twee moet dezelfde beurt al gelden, anders wordt de persoon over het
      commando gevraagd dat hij zojuist vastlegde. Rechten staan uit, dus een context die niets mag, is de
      default en niet een uitzondering.
- [ ] `FolderBoundary` (buiten de map verboden) — `tool/FolderBoundary.kt`.
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
      `ToolContext` al voor zijn callbacks maakte. commit `TODO-HASH`.
- [ ] `ExecuteTool` + `SafeCommands` + `CommandSignature` + `ExecuteWorkspace` + `ProcessRunner` — shell in map, TMPDIR, read-only auto / altijd-vraag, family-pin. `tool/{ExecuteTool,SafeCommands,CommandSignature,ExecuteWorkspace,ProcessRunner}.kt` (embed 0.62).
- [ ] `GitTool` — lezen standaard; schrijft verbs (add/commit/checkout/stash/push) opt-in + Write-recht + goedkeuring; force-push/rewrite geblokkeerd. `tool/GitTool.kt` + `git/`.
- [ ] `ToolGate` (Active/Disabled/NoProject/NoFolder/NoWrite) + ToolMode-afdwinging — `app/ToolGate.kt`, turn-decision.puml.

## F. Context- en verdichtingslaag (na D)
- [ ] `ContextStrategy` + `ContextTrimmer` (systeemprompt leidt, bound vóór vertrek) — `config/ContextStrategy.kt`, `session/ContextTrimmer.kt` (embed 0.62).
- [ ] `Transcript` scrub/split/fit — `tool/Transcript.kt`.
- [ ] `Compactor`/`CompactionPlan`/`CompactionOutcome` (vouwen over meerdere calls, eerlijk bij niets) — `session/Compactor*.kt` + compaction-fold.puml.

## G. Git- en diff-laag (nodig voor C + E)
- [ ] `GitCommand`/`GitInspector`/`GitStatus`/`FileChange` — `git/`.
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
