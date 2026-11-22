# TODO — de volledige Kotlin-app naar Rust

Dit is de werklijst die elke beurt opnieuw wordt gelezen en bijgewerkt. Eén bron van de waarheid:
wat hier `[x]` staat, is gebouwd en getest; wat `[ ]` staat, bestaat niet of niet volledig. Als de
lijst en de code twisten, is de lijst fout en wordt híj gecorrigeerd, niet de code.

## De beslisregel (geldt bij elk item hieronder)

1. **Zeker?** Gewoon bouwen op `main`.
2. **Niet zeker, of een keuze die de richting koest?** Snapshot: commit + `git tag`. Dan een aparte
   `codex/*`-branch waarin de keuze staat, `main` onaangeroerd. In die branch: pas de README aan met
   het besluit en de reden, en bouw door. Zo is er altijd een punt om naar terug te keren en hoeft een
   omgegooide poging niets op te ruimen.
3. **Hoofdregel die deze hele port draagt:** een widget bestaat pas als het domein eronder bestaat.
   De UI is geen schil; hij is het venster op een domein. Eerst het domein (crate + tests), dan de
   UI die het toont. Dit is de reden waarom de eerste ronde widgets bouwde (folder-kiezer, tabbladen)
   die niets bedienden: het domein ontbrak. Die fout wordt niet herhaald.

## Hulpmiddel: embed-matchen van Kotlin naar Rust

Voor elk item hieronder geldt: **vind eerst de Kotlin-bron** vóórdat er iets in Rust wordt
uitgevonden. De Kotlin-app is de specificatie; geen Rust-domein wordt opnieuw ontworpen als de
origineler het al heeft. Voor het zoeken over de 32 Kotlin-domeinen heen is er een embedding:

```
python3 ~/.codex_local/scripts/emb.py "zoekterm"   # bge-m3, cross-lingual NL<->EN
```

Zoekvragen in het Nederlands vinden de Engelse Kotlin-symbolen (cos NL↔EN gemeten 0,976); een
ongerelateerde tekst blijft op ~0,54 vallen. Dus: een NL-omschrijving van een feature hieronder is
voldoende ingang om te vragen "waar staat dit in Kotlin". Na een match **altijd** de gevonden
`.kt`-bron lezen — de embedding wijst alleen de deur, de bron is het antwoord. Regels: embeddings
zijn geen chat-beurten en belasten het residentiemodel niet; wel nooit een sweep over meerdere
embed-modellen.

## Wat er al is (eerlijk, niet optisch)

- [x] `baudet-core` — events/reductie/rollout. Geen IO, geen UI.
- [x] `baudet-backend` — `Backend`-contract + `ScriptedBackend` + SSE + HTTP/SSE-transport (beide
      OpenAI-dialecten, streaming) + protocol + server-scan (detectie, no-stream, v1/models).
- [x] `baudet-settings` — settings-pad, schrijven, `GET /v1/models`. Leest geen UI.
- [x] `baudet-store` — rollouts op schijf + indexering.
- [x] `baudet-app` — proxy bovenop een backend: events, checkpoints, interrupts.
- [x] `baudet-ui` — transcript, streaming-rijen, composer, instelpaneel (endpoint/modellenlijst/
      redenereniveau/uitvoerplafond), server-detectie, folder-kiezer.
- [x] Toolrijen getoond als *regels* (wachttijd/details) — maar alleen als weergave van events; er
      draait géén tool achter (zie "tool-laag" hieronder).

## WAT ONTBREEKT — de daadwerkelijke rest (deze port is hier ~15-20%, niet de helft)

Deze lijst is gemeten tegen de origineler (~14.370 regels in 32 domeinen). Volgorde is de bedoelde
bouwvolgorde: domein eerst, UI daarna.

### A. Project- en permissiedomein  — crate `baudet-projects` (NU)
- [ ] `Project` (id, name, folderPath?, permissions-bitmask, archived, createdAt) — Kotlin `project/Project.kt`.
- [ ] `FolderPermission { Read=1, Write=2, Execute=4 }` — Kotlin `project/FolderPermission.kt`.
- [ ] `Permissions`-bitmaskhelpers (encode/decode/has/with) — Kotlin `project/Permissions.kt`.
- [ ] `ToolMode { Off, Ask, Auto }` — Kotlin `config/ToolMode.kt`.
- [ ] `ProjectIds`/`SessionIds`-generatie — Kotlin `project/ProjectIds.kt`, `session/SessionIds.kt`.
- [ ] ProjectStore (persist) — Kotlin `store/ProjectStore.kt`, `store/H2Store.kt`.

### B. Correcte vensterlay-out (na A)
- [ ] Doel, gemeten uit `Main.kt:161-170`: `[ ProjectSidebar | Chat/Settings ]  [ FileTree ]`.
      Linker split = sidebar + tabs; rechter pane = `FileTreePanel`.
- [ ] Bestandsboom van `panels::left` naar het **rechter** `FileTreePanel`; folder-kiezer mee naar rechts.
- [ ] Linker `ProjectSidebar`: projecten-boom met sessies eronder + `Unassigned`-knooppunt +
      Add-projectknop + read/write/execute-vakjes. Nu afwezig.
- [ ] Twee onafhankelijke toolbar-toggles (sidebar inklappen, bestandsboom inklappen) — Kotlin `Main.kt` + `SidebarToggleIcon.kt`.

### C. Sessies (na B)
- [ ] Meerdere sessies; sessie is niet één globe transcript. Kotlin `session/SessionState.kt`.
- [ ] Sessie-opslag + herstel (loadSession/startProjectChat). Kotlin `store/TranscriptStore.kt`, `store/SessionSummary.kt`.
- [ ] Sessies-nesting onder een project in de sidebar.

### D. Tool-laag: read / write / execute (na A, de kern van "de werking is anders")
- [ ] `Tool`/`ToolRegistry`/`ToolRunner`/`ToolLoop` — Kotlin `tool/Tool*.kt`, `tool/ToolLoop.kt`.
- [ ] `ReadFileTool` / `EditFileTool` / `ExecuteTool` + `PatchApplier` — Kotlin `tool/{ReadFileTool,EditFileTool,ExecuteTool,PatchApplier}.kt`.
- [ ] `FolderBoundary` (mag alleen binnen de map) + `ExecuteWorkspace` — Kotlin `tool/FolderBoundary.kt`, `tool/ExecuteWorkspace.kt`.
- [ ] `ToolGate` + ToolMode- afdwinging (Ask = preview vóór write) — Kotlin `app/ToolGate.kt`, `config/ToolMode.kt`.
- [ ] Rechten afgedwongen per tool: Read vandaag, Write/Execute conform de vlaggen — zie `Project.kt`-commentaar.
- [ ] Call-extractie uit de assistantstroom — Kotlin `tool/CallExtractor.kt`, `tool/PromptedCallExtractor.kt`.

### E. Overige Kotlin-domeinen die nog niet in Rust bestaan
- [ ] `git` — GitCommand/GitInspector/GitStatus/FileChange. Kotlin `git/`.
- [ ] `diff` — UnifiedDiff/LineDiff/DiffHighlight/NewFileDiff. Kotlin `diff/`.
- [ ] `fs` — FileLister/FileWatcher/FileEntry. Kotlin `fs/`.
- [ ] `mcp` — McpClient/JsonRpc/ToolAdapter/Servers. Kotlin `mcp/`.
- [ ] `session`-verdichting — Compactor/ContextTrimmer. Kotlin `session/`.
- [ ] `i18n` — Key/Language/Texts (NL/EN). Kotlin `i18n/`.
- [ ] `io` — ImageClipboard. Kotlin `io/ImageClipboard.kt`.
- [ ] `ui`-rest — Markdown/MarkdownHtml, About/Help, DiffViewDialog, ThinkingLayout. Kotlin `ui/`.

## Werkwijze per item
Voor elk `[ ]`-item: (1) Embed-zoek de Kotlin-deur, (2) lees die `.kt`-bron, (3) bouw crate/domein +
tests, (4) pas de UI aan, (5) vink hier af met de commit-hash, (6) commit. Twijfel over richting →
beslisregel hierboven (tag + `codex/*`-branch + README-besluit).
