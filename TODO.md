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
- [ ] `ToolMode { Off, Ask, Auto }` — `config/ToolMode.kt`; `ContextStrategy` is reeds in `baudet-store::window`, maar moet bij item F aan de settings-/tool-gate gekoppeld worden.
- [ ] UI-koppeling van deze catalogus naar de linkersidebar (item C).
- Embed-match: "rechten lezen schrijven uitvoeren" → Permissions.kt 0.57, FolderPermission 0.53.

## C. Correcte vensterlay-out (na B)
- [ ] Doel uit `Main.kt:161-170`: `[ ProjectSidebar | Chat/Settings ]  [ FileTree ]`; twee onafhankelijke toolbar-toggles.
- [ ] Bestandsboom van `panels::left` naar **rechter** `FileTreePanel` + git-kleuren (groen gewijzigd, blauw untracked, rood deleted), dubbelklik = patch. `ui/FileTreePanel.kt`.
- [ ] Linker `ProjectSidebar`: projecten-boom + sessies eronder + `Unassigned` + Add-project + read/write/execute-vakjes + contextmenu. `ui/ProjectSidebar.kt` (embed-match 0.53).

## D. Sessies (na C)
- [ ] Meerdere sessies, niet één globe transcript — `session/SessionState.kt`.
- [ ] Sessie-opslag + herstel (loadSession/startProjectChat), lazy paginering — `store/TranscriptStore.kt`, `store/SessionSummary.kt`.

## E. Tool-laag read/write/execute (na B; de kern van "de werking is anders")
- [ ] `Tool`/`ToolRegistry`/`ToolRunner`/`ToolLoop` (max iteraties, reads parallel ≤2, writes serieel) — `tool/Tool*.kt`, `tool/ToolLoop.kt` (ziet tool-loop.puml).
- [ ] Call-extractie: native + fenced JSON — `tool/CallExtractor.kt`, `tool/PromptedCallExtractor.kt`.
- [ ] `ToolContext` (rechten, approvals, allow-lijsten) — `tool/ToolContext.kt`.
- [ ] `FolderBoundary` (buiten de map verboden) — `tool/FolderBoundary.kt`.
- [ ] `ReadFileTool` — `tool/ReadFileTool.kt` (embed 0.64).
- [ ] `EditFileTool` + `PatchApplier` + `Edit` — `tool/{EditFileTool,PatchApplier,Edit}.kt`; Ask toont diff-preview (turn-decision.puml).
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
