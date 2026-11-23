# TODO

- [ ] **Distributie verifiëren.** Draai de macOS-, Windows- en Ubuntu-jobs op GitHub Actions.
  Controleer op Windows x64 of `Baudet.exe` uit het zipbestand start en de DLL-controle slaagt.
- [ ] **Praktijktest met een echte modelserver.** Test in één gebruikerssessie of **Stop**
  een stream afbreekt en het deelantwoord bewaart, of **Compact** zijn uitkomst toont,
  en of na het opslaan van een ander endpoint de volgende beurt daar aankomt.

## Werkregel

Lokale modellen blijven van de frontend af. Geef ze geen opdrachten die bestanden onder
`crates/baudet-ui` wijzigen.
