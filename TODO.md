# TODO

## Werkregel

Lokale modellen blijven van de frontend af. Geef ze geen opdrachten die bestanden onder
`crates/baudet-ui` wijzigen.

## Release en praktijktest

- [ ] Draai de pakketjobs voor macOS, Linux en Windows in GitHub Actions. Start de gebouwde app op elk platform. Controleer op Windows x64 ook of alle DLL's uit de zip worden geladen.
- [ ] Test één sessie met een echte modelserver. **Stop** onderbreekt de stream en bewaart het deelantwoord. **Compact** meldt ook dat er niets te compacteren viel. Na het opslaan van een ander endpoint gaat de volgende beurt naar die server.
- [ ] Voer vanuit de app een echte MCP-aanroep uit en controleer de hele route van toolselectie en goedkeuring tot het resultaat in de chat.

## Betrouwbaarheid

- [ ] Open het venster direct, ook bij een ontbrekend protocol of een onbereikbare server. Laat **Scan endpoint** de serververkenning starten.
- [ ] Toon een afgebroken stream als onvolledig en voorkom dat de tool-lus op basis van een onvolledige beurt doorgaat.
- [ ] Laat een falende parallelle tool de beurt afronden met een foutmelding. Laat de compiler de threadveiligheid van gedeelde callbacks controleren.
- [ ] Rapporteer fouten bij het wachten op een proces als uitvoerfout. Maak mislukte MCP-serverstarts zichtbaar bij de betreffende server.

## Werkruimterechten

- [ ] Voeg toestemming toe voor extra mappen en netwerktoegang vanuit shellcommando's, eenmalig of per project. Controleer de grenzen ook vanuit de verpakte macOS- en Linux-app.
- [ ] Implementeer en verifieer de Windows-werkruimtemodus voordat die zonder bevestiging shellcommando's uitvoert.
- [ ] Laat Git- en bestandstools de projectrechten zelf afdwingen. Controleer ook dat bestandswijzigingen binnen de mapgrens blijven als een pad tijdens de bewerking verandert.
- [ ] Controleer welke omgevingsvariabelen Git, MCP en de overige commandoroutes erven. Geef app-geheimen alleen door als ze expliciet voor die integratie zijn ingesteld.
- [ ] Vraag een expliciet protocol voor onbeveiligde endpoints. Behoud de mogelijkheid om zelf een HTTP-adres op het lokale netwerk op te geven.

## Onderhoud

- [ ] Schrijf instellingen via een uniek tijdelijk bestand dat vanaf de eerste byte privé is. Test gelijktijdige schrijfacties.
- [ ] Test `--send` en `--fetch-models` ook buiten screenshotmodus en herstel waar nodig de koppeling met de app.
- [ ] Werk de resterende onvertaalde meldingen bij en controleer de Enter-afhandeling in de instellingen en zijpanelen.
- [ ] Werk bestaande formatterings- en Clippy-meldingen weg en voeg beide checks toe aan CI.
