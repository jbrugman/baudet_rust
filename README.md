# Baudet

Baudet is een lichte desktopapp voor gesprekken met een AI-model en werken aan code.
De app maakt via HTTP verbinding met een modelserver op je eigen computer of op een
externe server. Je model draait op die server. Baudet ondersteunt de OpenAI-compatibele
Responses API en Chat Completions API.

- **Chat** met streaming, Markdown, klembordafbeeldingen en lokaal bewaarde gesprekken.
  **Stop** breekt een lopende aanvraag af. 
  **Compact** vat oudere modelcontext samen en laat het zichtbare gesprek intact.
- **Projecten** koppelen gesprekken aan een lokale map. Je bepaalt per project of tools
  mogen lezen, schrijven en commando’s uitvoeren.
- **Git en MCP** bieden een bestandsboom met Git-status, een diffviewer en ondersteuning
  voor lokale MCP-servers via stdio.

## Snel starten

Download een pakket onder **Artifacts** bij een geslaagde run van
[Build desktop packages](.github/workflows/build-desktop.yml).

| Platform | Pakket | Openen |
|---|---|---|
| macOS | `Baudet-macOS-universal.dmg` voor Apple Silicon en Intel | Open de DMG en sleep `Baudet.app` naar Programma’s. Zie [macOS ondertekenen](#macos-ondertekenen) voor distributie via Gatekeeper. |
| Windows x64 | `Baudet-windows-x64.zip` | Pak het archief uit en open `Baudet.exe`. |
| Ubuntu 24.04 x64 | `baudet_<versie>_amd64.deb` | Installeer het gedownloade bestand met `sudo apt install ./baudet_0.1.0_amd64.deb`, met de versie uit je download. |

Met Rust en de [bouwvereisten hieronder](#zelf-bouwen) kun je ook meteen de demo openen.
De demo gebruikt vooraf opgenomen antwoorden en heeft geen modelserver nodig.

```sh
cargo run --locked -p baudet-ui -- --demo
```

### Een modelserver verbinden

1. Start je modelserver of zorg dat de externe server bereikbaar is.
2. Open **Instellingen** en vul de API-basis-URL in. De standaard is
   `http://127.0.0.1:8000/v1`.
3. Haal de modellen op, kies een model en klik op **Scan endpoint** om het protocol vast
   te stellen. Voor een beveiligd endpoint kun je een sleutel invullen of een
   omgevingsvariabele gebruiken.
4. Klik op **Opslaan** en open **Chat**. De opgeslagen verbinding geldt vanaf de
   volgende beurt.

**Ophalen**, **Scan endpoint** en **Chat** gebruiken een ingevulde sleutel eerst.
Bij een leeg sleutelveld leest Baudet de ingestelde omgevingsvariabele, standaard
`OMLX_API_KEY`. De omgevingswaarde wordt niet opgeslagen.

Heb je de variabele in je Terminal geëxporteerd, start Baudet dan vanuit diezelfde
Terminal. Voor een macOS-app in Programma’s kan dat met
`/Applications/Baudet.app/Contents/MacOS/baudet-ui`. Een start vanuit Finder erft
de variabelen van je Terminal niet.

Bij **reasoning-effort** laat **Default** de server kiezen. Met **Uit** vraag je de
server om redenering uit te schakelen. Bij een gekozen niveau vraagt Baudet redenering
aan. Vink voor een compatibele server zoals oMLX **Gebruik de thinking-schakelaar van
de server** aan om redenering expliciet aan of uit te zetten bij een gemaakte keuze.

### Datamap

Baudet bewaart instellingen, projecten en gesprekken lokaal. Met `--home <map>` kies je
een andere datamap voor die start. Die optie heeft voorrang op `BAUDET_HOME`.

| Platform | Standaardmap |
|---|---|
| macOS | `~/Library/Application Support/Baudet` |
| Windows | `%APPDATA%\Baudet` |
| Linux | `$XDG_DATA_HOME/baudet`, of `~/.local/share/baudet` |

## Zelf bouwen

Installeer rustup. [rust-toolchain.toml](rust-toolchain.toml) legt de Rust-versie vast.
Draai het script op het bijbehorende platform. De uitvoer staat standaard in `target/dist`.

| Platform | Vereisten | Commando |
|---|---|---|
| macOS | Xcode Command Line Tools | `bash packaging/build-macos.sh` |
| Windows x64 | PowerShell 7 en Visual Studio Build Tools met **Desktop development with C++** | `pwsh -NoProfile -File packaging/build-windows.ps1` |
| Ubuntu 24.04 x64 | De bouwpakketten hieronder | `bash packaging/build-linux.sh` |

```sh
sudo apt update
sudo apt install build-essential dpkg-dev pkg-config libwayland-dev libxkbcommon-dev
bash packaging/build-linux.sh
```

De macOS-app gebruikt systeemframeworks. Windows levert de systeem-DLL’s en de executable
linkt de Visual C++-runtime statisch. Voor het Ubuntu-pakket installeert `apt` de benodigde
gedeelde bibliotheken. Het Windows-archief bevat een lijst van de gebruikte DLL’s.

GitHub Actions draait dezelfde scripts bij pull requests, pushes naar `main`, tags die
met `v` beginnen en handmatige runs.

### macOS ondertekenen

De CI-build gebruikt een ad-hoc-handtekening. Voor verspreiding via Gatekeeper heb je een
**Developer ID Application**-certificaat en Apple-notarizatie nodig. Bouw eerst lokaal met
`bash packaging/build-macos.sh`. Gebruik daarna `Baudet.app` uit `target/dist`.

Zoek je certificaat met `security find-identity -v -p codesigning` en vervang de
voorbeeldwaarden hieronder. `notarytool store-credentials` vraagt om een app-specifiek
wachtwoord.

```sh
identity='Developer ID Application: Jouw naam (JOUWTEAMID)'
xcrun notarytool store-credentials Baudet-notary --apple-id 'jij@example.com' --team-id JOUWTEAMID

codesign --force --options runtime --timestamp --sign "$identity" target/dist/Baudet.app
codesign --verify --strict --verbose=2 target/dist/Baudet.app

(
  staging="$(mktemp -d)"
  trap 'rm -rf "$staging"' EXIT
  ditto target/dist/Baudet.app "$staging/Baudet.app"
  ln -s /Applications "$staging/Applications"
  hdiutil create -ov -volname Baudet -format UDZO -srcfolder "$staging" \
    target/dist/Baudet-macOS-universal-signed.dmg
)

codesign --timestamp --sign "$identity" target/dist/Baudet-macOS-universal-signed.dmg
xcrun notarytool submit target/dist/Baudet-macOS-universal-signed.dmg --keychain-profile Baudet-notary --wait
xcrun stapler staple target/dist/Baudet-macOS-universal-signed.dmg
xcrun stapler validate target/dist/Baudet-macOS-universal-signed.dmg
```

Zie [Apple’s handleiding voor macOS-distributie](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution)
en [notarizatie met notarytool](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

## Ontwikkelen

```sh
cargo run --locked -p baudet-ui
cargo test --workspace --locked
cargo clippy --locked -p baudet-ui
```

`baudet-ui` bevat de egui-interface en `baudet-app` beheert gesprekken en beurten.
`baudet-backend` verzorgt het HTTP-transport. Opslag, instellingen, projectrechten en
tools hebben eigen crates. De huidige prioriteiten staan in [TODO.md](TODO.md).

## Licentie

Baudet is open source onder de [MIT-licentie](LICENSE). De distributiepakketten bevatten
ook de [licenties van de ingebouwde lettertypen](packaging/FONT-LICENSES.txt).
