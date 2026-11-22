//! De flankerende panelen: links projecten en sessies, rechts de map en daaronder de wijzigingen.
//!
//! Dit volgt `Main.kt:161-170`: `[ProjectSidebar | Chat/Settings] | FileTreePanel`. De mapstate
//! `Workspace` heet historisch zo omdat ze het werkgebond aanwijst; ze staat niet meer in een eigen
//! linkerstrook. De linkerboom is en blijft de project-/sessieboom, terwijl de rechterstrook laat
//! zien wat er in de map staat en wat git ervan zegt.
//!
//! Beide rechts lezen hun feiten uit `baudet-core` en bezitten niets van het transcript. Git is al
//! een bron van waarheid voor "wat er veranderde", dus de lijst toont gemeten paden. Een onleesbare
//! map of een map zonder repository zegt dat met woorden en verzint geen rijen.
//!
//! Refresh happens when a folder becomes the subject -- on opening one and on switching to another
//! -- and never on a timer, bounded by the probe's own budget ([`baudet_core::GIT_PROBE_TIMEOUT`] per
//! git call, two calls per refresh). Polling on a timer would make the panel's cost a function of
//! frame rate, which is how an inspector ends up costing more than the transcript it sits beside.
//! The consequence is stated rather than hidden: edit a file outside the app and the list stays as
//! it was until the folder is chosen again.

use crate::layout::{self};
use crate::palette;
use crate::sidebar::{Confirm, NameAsk, PermissionState, Selection, SidebarModel, UNASSIGNED};
use baudet_core::{probe_changes, probe_git, ChangeKind, ChangedFile, GitMeta};
use egui::{Color32, Panel, ScrollArea, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Names that are build output rather than source. Measured on a workspace whose crate had been
/// built, `target` alone held more entries than every source directory combined.
const IGNORED_DIRS: &[&str] = &["target", "node_modules", "build", "dist", "out"];

/// `Key.FileTreeNoFolder`: de selectie heeft geen map, dus er is niets te tonen.
pub const NO_FOLDER: &str = "Select a project with a linked folder to see its files.";

/// `Key.FileTreeNoRead`: er is wél een map, maar deze app mag er niet in lezen.
pub const NO_READ: &str = "Read access is not granted for this project's folder.";

/// Directories descended per listing. A panel four rows deep is already showing more than a person
/// scans, and an unbounded walk of a large checkout is work nobody asked for.
const MAX_DEPTH: usize = 4;

/// Entries kept per directory. When a listing is cut, the panel says so: a silent truncation reads
/// as "these are all the files", which is a false statement about someone's folder.
const MAX_ENTRIES_PER_DIR: usize = 200;

/// Leading characters of a directory kept when a name has to be cut. The tail is what identifies a
/// file, so it survives and the head gives way.
const KEEP_TAIL: usize = 22;

/// De mapachtergrond van het rechterpaneel: wat er staat en of git erover kon praten.
///
/// Whether a directory is open is *not* stored here. egui keeps that state per widget identity, and
/// a second copy in this struct would be a second answer to the same question — the kind that ends
/// up disagreeing with the arrow the user just clicked.
#[derive(Default)]
pub struct Workspace {
    /// The folder under inspection. Empty means nothing has been chosen yet.
    cwd: String,
    /// Waarom er niets staat, in de woorden van de referentie-app. Een lege `cwd` kan twee dingen
    /// betekenen — er is geen project gekozen, of het project heeft wel een map maar die mag hier
    /// niet gelezen worden — en die twee verdienen verschillende tekst: `Main.kt:156` koppelt de
    /// boom aan de selectie en `FileTreePanel.showProject` kiest tussen die twee meldingen. Eén
    /// "no folder linked" voor beide zou een rechtenontzegging verbergen als een afwezigheid, en
    /// iemand dan op de verkeerde plek gaan zoeken (bij de ingest, niet bij de maprechten).
    message: Option<&'static str>,
    /// Of namen met een punt mee mogen in de boom: het vinkje `Hidden files` van
    /// `FileTreePanel.kt:97`. De origineler herbouwt de hele boom bij elke wijziging van dat
    /// vinkje (`hiddenBox.addActionListener { rebuild() }`); in immediate mode is dit veld zelf
    /// de herbouw, want de volgende pass filtert ermee.
    show_hidden: bool,

    /// Whether the current folder has been probed. Opening a folder and switching to another are
    /// one event -- "this folder is now the subject" -- so the caller refreshes on the first folder
    /// for the same reason it refreshes on the second, and `Workspace` stays the only owner of
    /// which folder that is.
    probed: bool,
}

/// One directory level, as read from disk.
pub struct DirListing {
    /// Directories, folders-first order already applied.
    pub dirs: Vec<PathBuf>,
    /// Files, in the same order.
    pub files: Vec<PathBuf>,
    /// Whether this level alone was cut at [`MAX_ENTRIES_PER_DIR`].
    pub truncated: bool,
}

impl DirListing {
    /// A level that could not be read: no entries, and not claiming to be empty.
    fn unreadable() -> DirListing {
        DirListing { dirs: Vec::new(), files: Vec::new(), truncated: false }
    }
}

impl Workspace {
    /// Point the panel at `path`. Always replaces the previous folder, including when `path` cannot
    /// be read: keeping the old listing would describe a folder the panel no longer looks at.
    pub fn open(&mut self, path: &str) {
        self.cwd = path.trim().to_string();
        self.probed = false;
        // Wie zelf een map noemt, hoeft geen uitleg meer waarom er niets stond.
        self.message = None;
    }

    /// Laat de boom een reden tonen in plaats van een map: `FileTreePanel.showProject:211-219`.
    ///
    /// Onvoorwaardelijk, net als de origineler. `ProjectSidebar.kt:187` geeft bij élke echte
    /// selectie door — de enige wacht is zijn `refreshing`-vlag tijdens een heropbouw — en
    /// `showProject` toon daarna wat dat project heeft: zijn map, of de reden dat er geen is.
    /// Een dedupe op project-id zou dat breken: een map die zojuist aan het al geselecteerde
    /// project is gekoppeld, moet meteen tonen.
    pub fn show_message(&mut self, text: &'static str) {
        self.cwd.clear();
        self.probed = false;
        self.message = Some(text);
    }

    /// De uitleg die de boom moet tonen, of `None` wanneer er een map staat.
    pub fn message(&self) -> Option<&'static str> {
        self.message
    }

    /// Of het vinkje `Hidden files` aan staat.
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Zet het vinkje. De origineler herbouwt de hele tree bij zo'n klik; hier is de boom een
    /// immediate-mode lijst, dus het vlaggen is de herbouw.
    pub fn set_show_hidden(&mut self, show: bool) {
        self.show_hidden = show;
    }

    /// The folder being shown, or `None` when nothing has been chosen.
    pub fn cwd(&self) -> Option<&str> {
        (!self.cwd.is_empty()).then_some(self.cwd.as_str())
    }

    /// Read one directory. Called as the tree is painted, for each level that is open, so closing a
    /// directory also stops reading it. The answer carries whether *that level* was cut; `&mut self`
    /// is a formality of the paint borrow and no part of a listing depends on it, which is why a
    /// level cannot leave an alarm behind for a folder that was never cut.
    pub fn tree_at(&mut self, dir: &Path) -> DirListing {
        let Ok(read_dir) = std::fs::read_dir(dir) else { return DirListing::unreadable() };
        let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
        for entry in read_dir.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            // A symlink is listed and never descended: `node_modules` trees and self-referential
            // links loop, and a walk that loops during a paint pass is a hung window.
            let is_dir = file_type.is_dir() && !file_type.is_symlink();
            let name = entry.file_name().to_string_lossy().into_owned();
            // Twee filters, twee betekenis. Een punt-naam is een *voorkeur* van de persoon —
            // `FileLister.kt:20` (`if (!showHidden && name.startsWith(".")) continue`) en het
            // vinkje erboven. De bouw-uitgang-filter is dat niet: `FileTreePanel`-boom toont die
            // nóóit, want `target` en `node_modules` zijn geen projectbron, en een boom die die
            // toont zodra iemand verborgen bestanden wil zien, is een boom die het paneel vult
            // met duizend rijen die niemand zocht.
            if (!self.show_hidden && name.starts_with('.')) || (is_dir && IGNORED_DIRS.contains(&name.as_str())) {
                continue;
            }
            names.push((name, entry.path(), is_dir));
        }
        // Folders first, then case-insensitive by name: the order a file manager uses and the one a
        // person scanning this panel expects.
        names.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        let truncated = names.len() > MAX_ENTRIES_PER_DIR;
        if truncated {
            names.truncate(MAX_ENTRIES_PER_DIR);
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for (_, path, is_dir) in names {
            if is_dir {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
        DirListing { dirs, files, truncated }
    }
}

/// The right panel's view of the repository in the folder.
#[derive(Default)]
pub struct Changes {
    /// The repository's own facts, or `None` when the folder holds none.
    meta: Option<GitMeta>,
    paths: Vec<ChangedFile>,
    /// Whether the probe could not be answered. That is a different sentence from "nothing
    /// changed", and the panel says them as two sentences.
    unavailable: bool,
}

impl Changes {
    /// Ask git twice about `path` and keep its answers.
    pub fn refresh(&mut self, path: Option<&str>) {
        self.meta = None;
        self.paths.clear();
        self.unavailable = false;
        let Some(cwd) = path else { return };
        let Some(meta) = probe_git(cwd) else { return };
        match probe_changes(cwd) {
            Some(paths) => {
                self.meta = Some(meta);
                self.paths = paths;
            }
            // A repository whose status git would not answer. Saying so beats an empty list, which
            // would read as "the tree is clean".
            None => self.unavailable = true,
        }
    }

    /// The repository facts, for the left panel's branch line.
    pub fn meta(&self) -> Option<&GitMeta> {
        self.meta.as_ref()
    }

    /// The paths git reported, for the left panel to colour its file rows with. A read-only view:
    /// the tree never mutates a status, it only asks which ink a path wears.
    pub fn paths(&self) -> &[ChangedFile] {
        &self.paths
    }
}

/// Het vooraantal van de rechterfiletree: map kiezen, paden, branch.
///
/// De functie heet niet `left`: de layoutopdracht van de origineler plaatst deze inhoud rechts.
/// Returns whether the caller should ask git about the folder now: true for a folder that has not
/// been probed yet and for every switch, false while the same folder stays the subject. A folder
/// that cannot be read asks once as well, because "no repository" is git's honest answer there and
/// not one the panel may invent.
pub fn file_tree_controls(
    ui: &mut Ui,
    workspace: &mut Workspace,
    draft: &mut String,
    meta: Option<&GitMeta>,
    changes: &[ChangedFile],
) -> bool {
    // De draft komt van de aanroeper (`TranscriptApp::folder_draft`) en niet uit een lokaal
    // `String::new()`. Dat laatste was de bug: `singleline(&mut draft)` haalt zijn tekst uit de
    // meegegeven string (`TextEditState` bewaart alleen cursor/offset), dus een verse lokale lege
    // string wist elke toetsaanslag vóór de klik in de volgende frame — de rij stond dan leeg en
    // `requested` werd nooit gezet. Hier blijft `draft` leeg zolang er niets getypt of gekozen is,
    // maar overleeft hij wél wat er wél getypt is. Het is nog steeds geen tweede display van `cwd`
    // (daarvoor is de folder-regel hieronder); het is alleen de invoer, en die leeft buiten de paint.
    let mut requested: Option<String> = None;
    // Geen eigen `Panel`: de mapkinderen delen de rechterstrook met `changes_body`, zodat egui
    // hier één flank overhoudt, zoals de Kotlin `SplitPane`.
    {
        let root = crate::app::root_ui_id(ui.ctx());
        ui.strong("Files");
        // De knoppen-rij van de origineler: `[Refresh] [vinkje Hidden files]`
        // (`FileTreePanel.kt:126-131`, met de woorden uit `Key.kt:375` en `:380`).
        // Een eigen rij en niet aangeplakt bij de map-rij hieronder: de checkbox is 18 pt hoog
        // en gemeten 86,7 pt breed, de `Refresh`-knop 53 pt. Bij 20 pt veldhoogte zou die
        // rij op de paneelvloer van 180 pt het pad op nul terugbrengen, en afgeknepen tekst is
        // precies de silent failure die `tests/folder_row.rs` bestrijdt. De origineler kent die
        // scheiding ook: zijn knoppen-rij staat bóven de kaarten met de boom, niet erin.
        let refresh_clicked = ui
            .scope_builder(
                egui::UiBuilder::new().id(root.with((TREE_REFRESH, "refresh"))).sense(egui::Sense::click()),
                |ui| {
                    ui.add(egui::Label::new("Refresh").selectable(false));
                },
            )
            .response
            .clicked();
        // Het vinkje naast de knop, niet op eigen reinforcement: het bedient dezelfde boom.
        let mut hidden = workspace.show_hidden();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut hidden, "Hidden files").changed() {
                workspace.set_show_hidden(hidden);
            }
        });
        // Verversen heeft pas zin als er een map staat — ``showMessage`` zet de knop in de
        // origineler ook op `isEnabled = false` (`FileTreePanel.kt:229`). Weigeren in plaats van
        // verbergen: een adres dat verdwijnt, kan een test niet meer vinden.
        if refresh_clicked && workspace.cwd().is_some() {
            requested = workspace.cwd().map(str::to_string);
        }
        // Eén rij: het veld zo breed als overblijft, de knoppen ernaast. Het veld blijft de plek om
        // te typen — een dialoog die een pad kiest, is een toevoeging en geen vervanging, want een
        // pad dat iemand uit het hoofd typt, is sneller dan drie klikken door een boom.
        //
        // De breedtes worden hierbuiten bepaald en niet door egui verdeeld. Meten eerst (zie
        // tests/folder_row.rs): `desired_width(f32::INFINITY)` maakt het veld zo breed als de
        // beschikbare ruimte — in een root-UI 10 000 pt — en `Ui::put` knipt die.rect niet af, dus
        // elke knop ernaast verdween voorbij de rand: stil, zonder foutmelding, en op een plaatje niet
        // te onderscheiden van "erbij vergeten". Uitsluitend `add_sized` met een eindige breedte houdt
        // de rij binnen de rechterstrook, en dan sluit die per constructie aan op de rand.
        let row_width = ui.available_width();
        let spacing = ui.spacing().item_spacing.x;
        // De twee knoppen meten 64,3 en 40,2 pt (gemeten in tests/folder_row.rs); 64,4 en 40,2 is
        // naar boven afgerond, dus de rest die het veld krijgt is nooit te ruim en de rij nooit te vol.
        let buttons_width = 64.4 + 40.2 + spacing * 2.0;
        let field_width = (row_width - buttons_width).max(0.0);
        let mut field: Option<egui::Response> = None;
        let mut browse = false;
        let mut open_clicked = false;
        // Het veld krijgt een expliciet root-adres (`UiBuilder::id`-wikkel) en niet alleen een
        // `.id_salt`, omdat `.id_salt` relatief adresseert aan de ouder-`Ui` — en die schuift mee met
        // de paneel-kolom, dus is het adres van buiten niet te reconstrueren (idem `TABSTRIP`). De
        // wikkel draagt click-sense, maar de tekst-invoer leeft op het veld erin (Sense::focus), niet
        // op de wikkel; de wikkel bestaat zodat een test dit veld kan *vinden* om erin te typen.
        ui.horizontal(|ui| {
            // `with_main_wrap(false)`: zonder die regel wrapt de tweede knop naar een eigen rij zodra
            // het veld op 0 staat, en dan valt de rij alsnog buiten de rand (gemeten bij 130 pt).
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                field = Some(ui.add_sized(
                    [field_width, 20.0],
                    egui::TextEdit::singleline(draft).id_salt(FOLDER_FIELD).hint_text("folder path"),
                ));
                // De native kiezer van het besturingsselt, niet een eigen boom in een venster: mappen
                // tonen, snelkoppelingen herkennen en de voorkeur van iemand vinden, is het werk van
                // de zoeker die er al is.
                if ui.small_button(BROWSE_LABEL).clicked() {
                    browse = true;
                }
                // `Open` in een expliciet-id wikkel, niet als kale `small_button`: diens auto-adres
                // is van buiten niet te reconstrueren en de knop verdween daar eerder. De klik leeft
                // op de wikkel (click-sense), de tekst is alleen het label — exact het patroon van
                // de tabstrip en de modelknop.
                open_clicked = ui
                    .scope_builder(
                        egui::UiBuilder::new().id(root.with((FOLDER_OPEN, "open"))).sense(egui::Sense::click()),
                        |ui| {
                            ui.add(egui::Label::new("Open").selectable(false));
                        },
                    )
                    .response
                    .clicked();
            });
        });
        if browse {
            // De kiezer vult de draft, hij opent niet. Daardoor blijft `requested` hieronder de enige
            // deur naar `Workspace::cwd`, en een gekozen pad doorloopt hetzelfde commit-pad als een
            // getypt pad — inclusief het verversen van de boom en de git-vraag.
            if let Some(chosen) = pick_folder() {
                *draft = chosen;
            }
        }
        // Commit via de knop óf via Enter in het veld. De oude eis `field.lost_focus() && enter` was
        // de tweede doodsteek: Enter drukt men in *terwijl* het veld focus heeft, en dan is
        // `lost_focus()` per definitie onwaar — de commitslag kon dus ook via het toetsenbord nooit
        // vallen. De juiste vraag is "stond er iets in het veld toen de Enter of de knop kwam". Een
        // leeg veld betekent nog steeds "houd wat open is", dus een dwaal-Enter wist de paneel niet.
        let enter_in_field =
            field.is_some_and(|field| field.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (open_clicked || enter_in_field) && !draft.trim().is_empty() {
            requested = Some(draft.trim().to_string());
            // Een geopend pad hoeft niet in het veld te blijven staan: de folder-regel hieronder toont
            // wat er open is, en een lege draft na het openen is geen tweede display van cwd.
            draft.clear();
        }
        ui.separator();
        let chosen = workspace.cwd().map(|s| s.to_string());
        // The git statuses this folder was asked about, indexed once per paint so a file row is a
        // lookup and not a scan of every changed path. Built from the folder that is about to be
        // shown, because its paths are repository-relative to exactly that folder.
        let status = FileStatus::build(chosen.as_deref(), changes);
        match &chosen {
            // De reden van de leegte, niet de afwezigheid: `FileTreePanel` wisselt tussen de boom en
            // een melding (`cards`/`showMessage`), en die melding zegt wélk geval het is — geen map
            // gekoppeld, of geen leestoegang. Zonder die tekst is "mag niet lezen" niet te
            // onderscheiden van "vergeten te koppelen", en dat zijn twee verschillende plekken om te
            // zoeken. Langere zinnen wrappen hier, dus de breedte gaat mee met de strook.
            None => {
                let text = workspace.message().unwrap_or(NO_FOLDER);
                ui.label(dim(text));
            }
            Some(cwd) => {
                let root = PathBuf::from(cwd);
                ui.label(folder_name(cwd)).on_hover_text(cwd.clone());
                // De branch-regel blijft: de origineler heeft er geen kop-regel voor, maar hij is
                // hier tóch nuttig, en valt buiten de vergelijking met `Key.kt`. Wat wél weg is:
                // `"no repository"` onder de mapnaam. `FileTreePanel.kt:272-276` zegt dat in de
                // changes-kop, en die zegt het nu ook — tweemaal dezelfde zin in één strook.
                if let Some(meta) = meta {
                    branch_line(ui, meta);
                }
                ui.separator();
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if !root.is_dir() {
                        // `showMessage(FileTreeNoRead)` (`FileTreePanel.kt:212-218`): een pad dat
                        // niet te lezen is, krijgt dezelfde reden als een map zonder Lezen.
                        ui.label(dim(NO_READ));
                        return;
                    }
                    // `default_open`, because `begin` loads the state through
                    // `CollapsingState::load_with_default_open` (collapsing_header.rs:540): the
                    // chosen folder is open on the first paint and closed from the second onward
                    // only if the user said so. A panel that lists its own name and nothing else
                    // reads as an empty folder rather than as a collapsed one.
                    egui::CollapsingHeader::new(folder_name(cwd))
                        .id_salt(cwd.clone())
                        .default_open(true)
                        .show(ui, |ui| tree(ui, workspace, &root, 1, &status, Some(&root)));
                });
            }
        }
    }
    if requested.is_some() {
        // `open` already cleared `probed`, so the answer is read from the state it just left rather
        // than recomputed from the field: the panel cannot claim a refresh the state disagrees with.
        workspace.open(requested.as_deref().unwrap().trim());
    }
    if workspace.cwd().is_some() && !workspace.probed {
        workspace.probed = true;
        return true;
    }
    false
}

// ---------------------------------------------------------------------------------------------
// De linkersidebar: projecten, hun sessies en de maprechten van wat is gekozen.
// ---------------------------------------------------------------------------------------------

/// Het zout van de `Add project`-knop. Samen met `root_ui_id` is dat adres van buiten te vinden en
/// te klikken, zoals `TABSTRIP` en `FOLDER_OPEN`: een kale `small_button` in een `SidePanel`-kolom
/// adresseert relatief aan die kolom en verdween daar eerder voorbij de rand — dezelfde clase als de
/// tabstrip en de `Open`-knop al hadden.
///
/// Het contextmenu heeft geen vast adres: de items zitten in een popup op de muis en hun handeling
/// bereikt het model via `MenuAction`, niet via een klik op een bekend id.
pub const SIDEBAR_ADD: &str = "sidebar-add-project";

/// De brief onder de boom heet daar `Folder permissions` (`Key.kt:321`), en `createTitledBorder`
/// zet die titel bóven het vakje (`ProjectSidebar.kt:99`). Daaronder staat eerst de map zelf:
/// het label `folder:` (`Key.kt:319`) naast zijn waarde, en die waarde is
/// `no folder linked` zolang er geen gekoppeld is (`Key.kt:320`).
pub const PERMISSIONS_TITLE: &str = "Folder permissions";
pub const FOLDER_LABEL: &str = "folder:";
pub const FOLDER_NONE: &str = "no folder linked";

/// Wat er na een Paint te doen is. De sidebar is de enige die deze intenties *verzint*; wie ze
/// uitvoert, beslist de aanroeper. zo blijft de UI-knoop klein en wordt de sessie-lading uit item D
/// later één plaats in de lay-out, niet een zoektocht door de paint-pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarIntent {
    /// Er is een sessie gekozen of aangemaakt. Item D maakt hier van het id een geladen gesprek; nu
    /// is het alleen de deur die openstaat — de sidebar doet niet alsof er al iets achter ligt.
    OpenSession(String),
    /// De chat moet een nieuw gesprek in dit project beginnen (`None` = de inbox).
    StartChat(Option<String>),
    /// Er is iets weggehaald; wat er in het midden stond, is mogelijk niet meer waar.
    Deleted,
}

/// De linkersidebar. Geeft de intenties terug die deze pass heeft opgeleverd. De catalogus zelf is
/// dan al bijgeschreven door `SidebarModel`, dus een intentie is nooit een verzoek om op te slaan.
pub fn project_sidebar(ui: &mut Ui, model: &mut SidebarModel, width: f32) -> Vec<SidebarIntent> {
    let mut intents = Vec::new();
    let frame = Panel::left("projects")
        .resizable(true)
        // De volgorde is geen smuk: egui klemt `outer_size_range.min` op de default
        // (`panel.rs:396` `clamp_range`), dus een `default_size` onder de vloer zou die vloer
        // stiljes halveren. Eerst de vloer, dan de maat.
        .min_size(190.0)
        .default_size(width)
        // 190 pt blijft de vloer, en de reden is dezelfde: de breedste riem van het paneel — het
        // rechtenvakje met zijn titel plus de drie woorden van de disk-rij — is samen ~165 pt,
        // plus de margering van de `Frame`. Een onthouden stand smaller dan dat bestaat niet, want
        // `worth_remembering` laat een balk op de vloer als inklapping vallen.
        .frame(egui::Frame::side_top_panel(ui.style()));
    frame.show(ui, |ui| {
        // --- bovenrij: één knop, één handeling per klik. ---
        let root = crate::app::root_ui_id(ui.ctx());
        if ui
            .scope_builder(
                egui::UiBuilder::new().id(root.with((SIDEBAR_ADD, "add"))).sense(egui::Sense::click()),
                |ui| {
                    ui.add(egui::Label::new("Add project").selectable(false));
                },
            )
            .response
            .clicked()
        {
            // De vraag verschijnt in het paneel zelf, niet in een zwevend modaal venster: de
            // origineler gebruikt `JOptionPane`, maar een los venster boven de sidebar zou precies
            // de klacht terughalen die de settings-overlay hier al had.
            model.cancel_confirm();
            model.start_name(NameAsk::NewProject, None);
        }

        ui.separator();

        // --- de boom: projecten met hun sessies, dan de inbox. ---
        ScrollArea::vertical().id_salt("project-tree").auto_shrink([false, false]).show(ui, |ui| {
            let projects = model.catalog().list_projects();
            for project in &projects {
                let sessions = model.catalog().sessions_in(Some(&project.id));
                let response = egui::CollapsingHeader::new(project_label(&project.name, project.archived))
                    .id_salt(project.id.clone())
                    .default_open(true)
                    .show(ui, |ui| {
                        for session in &sessions {
                            session_row(ui, model, Some(project.id.clone()), session, &mut intents);
                        }
                        // Geen "no conversations"-riem: `ProjectSidebar.kt:137` voegt alleen
                        // de bestaande sessies toe, dus een project zonder gesprek is daar leeg.

                    });
                if response.header_response.clicked() {
                    model.select_project(Some(&project.id));
                    intents.push(SidebarIntent::StartChat(Some(project.id.clone())));
                }
                row_menu(ui, &response.header_response, model, Some(project.id.clone()), None, &mut intents);
            }

            let inbox = model.catalog().sessions_in(None);
            // `.selected(..)` bestaat op `CollapsingHeader` niet in deze egui-versie (0.36), dus de
            // inbox-markering steekt in een dik lettertype: het is een markeerstijl, geen nabouwsel
            // van een selectie-api die er niet is.
            let inbox_label = if model.selection() == Some(&Selection::Unassigned) {
                egui::RichText::new(UNASSIGNED).strong()
            } else {
                egui::RichText::new(UNASSIGNED)
            };
            let response =
                egui::CollapsingHeader::new(inbox_label).id_salt(UNASSIGNED).default_open(true).show(ui, |ui| {
                    for session in &inbox {
                        session_row(ui, model, None, session, &mut intents);
                    }
                    // Idem de inbox: `ProjectSidebar.kt:144-145` kent geen lege-riem.

                });
            if response.header_response.clicked() {
                model.select_project(None);
                intents.push(SidebarIntent::StartChat(None));
            }
            row_menu(ui, &response.header_response, model, None, None, &mut intents);
        });

        ui.separator();

        // --- onderaan: de map van het gekozen project en wat de schijf ervan toestaat. ---
        match model.permission_state() {
            None => {
                // Geen selectie is geen map: dezelfde waarde als hierboven, met hetzelfde label
                // (`ProjectSidebar.kt:250-252` — `updateDetail` vult `FolderNone` en zet de
                // vinkjes uit), en niet een vierde zin die ik bedacht heb.
                ui.horizontal(|ui| {
                    ui.label(FOLDER_LABEL);
                    ui.label(dim(FOLDER_NONE));
                });
            }
            Some(state) => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.strong(PERMISSIONS_TITLE);
                    // Twee cellen zoals de origineler ze tekent (`ProjectSidebar.kt:96-97`:
                    // `BorderLayout.WEST` het label, `CENTER` de waarde), hier in één
                    // horizontale riem. Eén geformatteerde zin maakte van `map:` en zijn waarde
                    // één tekst en gaf daarmee twee woorden waar de bron er twee heeft.
                    ui.horizontal(|ui| {
                        ui.label(FOLDER_LABEL);
                        ui.label(state.folder.clone().unwrap_or_else(|| FOLDER_NONE.to_string()));
                    });
                    if let Some(capability) = &state.disk_capability {
                        ui.label(dim(capability.clone()));
                    }
                    permission_boxes(ui, &state, model);
                });
            }
        }

        // --- de tekst-riem en de bevestiging: een incident, geen vast onderdeel van de lay-out. ---
        ask_and_confirm(ui, model, &mut intents);
    });
    intents
}

/// Eén sessie-riem. Een klik kiest en vraagt om opening; het menu vraagt om handelen.
fn session_row(
    ui: &mut Ui,
    model: &mut SidebarModel,
    project: Option<String>,
    session: &baudet_projects::SessionSummary,
    intents: &mut Vec<SidebarIntent>,
) {
    let selected = model.selected_session_id() == Some(session.id.as_str());
    // `SelectableLabel` bestaat in deze egui-build niet meer; `Button::selected` is wat erover is
    // gegaan (widgets/button.rs:270) en doet hetzelfde: een riem met de selectie-inkt. `frame(false)`
    // houdt het een lijst-riem in plaats van een knop, want dit is een keuze uit een boom en geen
    // handeling op zichzelf.
    // `push_id` en niet `.id_salt`: een `Button` heeft in deze build geen id-zout (alleen
    // `CollapsingHeader` en `TextEdit` dragen die), dus het adres komt van de wikkel eromheen —
    // dezelfde techniek als de tabstrip, alleen nu binnen de boom. `push_id` neemt in 0.36 de
    // sluiting direct aan en draagt het antwoord ervan terug in een `InnerResponse`.
    let row = ui
        .push_id(session.id.clone(), |ui| {
            let row =
                ui.add(egui::Button::new(truncate_head(&session.title, KEEP_TAIL)).frame(false).selected(selected));
            row.on_hover_text(session.title.clone())
        })
        .inner;
    if row.clicked() {
        model.select_session(&session.id);
        intents.push(SidebarIntent::OpenSession(session.id.clone()));
    }
    row_menu(ui, &row, model, project, Some(session.id.clone()), intents);
}

/// De drie vakjes. Ze zijn alleen aanraakbaar als er een map is: `add_enabled_ui(false)` tekent ze
/// in de inactieve inkt en maakt ze onklikbaar, dus "er is geen map" is aan de widgets zelf te zien
/// en niet alleen aan het label erboven.
fn permission_boxes(ui: &mut Ui, state: &PermissionState, model: &mut SidebarModel) {
    let mut read = state.read;
    let mut write = state.write;
    let mut execute = state.execute;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(state.checkbox_enabled, |ui| {
            ui.checkbox(&mut read, "Read");
            ui.checkbox(&mut write, "Write");
            ui.checkbox(&mut execute, "Execute");
        });
    });
    if (read, write, execute) != (state.read, state.write, state.execute) {
        // Alleen bijschrijven als er iets veranderde: de catalogus schrijft per oproep weg, en een
        // vinkje dat niets verrandert, mag geen schijf-aktie zijn.
        let _ = model.set_permissions(read, write, execute);
    }
}

/// De vraag-om-naam-riem en de bevestigingsriem, onder alles. Beide zijn dezelfde riem-vorm: een
/// regel tekst, een of twee knoppen, en het paneel weet wat er stond.
fn ask_and_confirm(ui: &mut Ui, model: &mut SidebarModel, intents: &mut Vec<SidebarIntent>) {
    if let Some(ask) = model.ask_name.clone() {
        ui.separator();
        ui.label(ask.prompt());
        let field = ui.add(egui::TextEdit::singleline(&mut model.name_entry).hint_text("naam").desired_width(140.0));
        let submit = field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            if (ui.button("OK").clicked() || submit) && model.answer_name().is_some() {
                // De intentie gaat alleen uit als de vraag iets opleverde: een lege naam bij een nieuw
                // project is geen wijziging, dus het middenpaneel hoeft dan niets te laten vallen.
                intents.push(SidebarIntent::Deleted);
            }
            if ui.button("Annuleer").clicked() {
                model.cancel_name();
            }
        });
        return;
    }
    if let Some(confirm) = model.confirm.clone() {
        ui.separator();
        ui.label(confirm.prompt(model));
        ui.horizontal(|ui| {
            if ui.button("Verwijder").clicked() && model.answer_confirm(true).is_some() {
                intents.push(SidebarIntent::Deleted);
            }
            if ui.button("Annuleer").clicked() {
                model.cancel_confirm();
            }
        });
    }
}

/// Een projectriem met hetzelfde archief-achtervoegsel als `ProjectNode.toString()` in de
/// origineler, zodat een gearchiveerd project niet voor een actief project doorgaat.
fn project_label(name: &str, archived: bool) -> String {
    if archived {
        format!("{name} (archived)")
    } else {
        name.to_string()
    }
}

/// Het contextmenu van één riem: rechtermuisklik zet het op de muis, zoals `maybePopup` in de
/// origineler het op de klikplaats neerzet.
///
/// De klik kiest de riem *zacht*: de selectie volgt mee, maar er gaat geen intentie uit. Iemand die
/// een knoop rechts aanklikt om erover te handelen, vraagt niet om erheen te reizen. Dat is de
/// `refreshing`-wacht uit de origineler, alleen nu in types.
///
/// Twee egui-eigenschappen dragen die wacht mee: `context_menu(..)` sluit het menu zelf zodra de
/// riem links wordt geklikt (containers/popup.rs:248), en `show` vraagt een `FnOnce`, dus de
/// handeling mag hier niet plaatsvinden terwijl het model al door de boom-lus in bruikleen is. Daarom
/// verzamelt de sluiting slechts één handeling en voert die ná de pass uit, als de lezing van de catalogus
/// vrij is.
fn row_menu(
    _ui: &mut Ui,
    row: &egui::Response,
    model: &mut SidebarModel,
    project: Option<String>,
    session: Option<String>,
    intents: &mut Vec<SidebarIntent>,
) {
    // Eén lezing van het menu, dan één handeling. `Popup::show` vraagt een `FnOnce` en mag het model
    // niet muteren zolang de rijen eromheen het lezen, dus de sluiting geeft terug wat er gekozen is.
    // Eén handeling per pass. `Popup::show` vraagt een `FnOnce` en de rijen van de boom lenen het
    // model al, dus de sluiting is pure keuze (`menu_item` raakt het model niet) en de handeling
    // gebeurt hieronder, ná de pass. De popup geeft zijn antwoord in een `Option<InnerResponse>`
    // terug, dus `and_then` plukt de keuze uit de wikkel; een lokaal `picked`-variabel zou de
    // sluiting een tweede mutabele leen geven.
    //
    // De aanroep staat in élke pass, niet alleen bij de secundaire klik, en die volgorde is geen
    // bijzaak: `context_menu` produceert per pass het commando voor dit popup-state (popup.rs:248-258)
    // — `Bool(true)` bij een secundaire klik, `Bool(false)` bij een linkerklik — en `show` is de
    // enige plek waar dat commando wordt uitgevoerd (popup.rs:517-529). Sla je die aanroep over bij
    // een linkerklik, dan sluit het menu niet. Verder telt één knoop per viewport
    // (memory/mod.rs:1112-1115): een secundaire klik op een andere riem vervangt het menu, en een
    // pass waarin geen enkele riem het state levend houdt, ruimt de pop-up weg
    // (`open_this_frame` is dan niet meer gezet, memory/mod.rs:806-819).
    let picked = egui::Popup::context_menu(row)
        .show(|ui: &mut Ui| menu_item(ui, model, project.as_deref(), session.as_deref()))
        .and_then(|response| response.inner);
    match (&project, &session) {
        (Some(project), None) => model.select_project(Some(project)),
        (_, Some(session)) => model.select_session(session),
        (None, None) => model.select_project(None),
    }
    match picked {
        Some(MenuAction::NewSession) => {
            if let Ok(created) = model.add_session(project.as_deref()) {
                intents.push(SidebarIntent::OpenSession(created.id));
            }
        }
        Some(MenuAction::Open) => {
            if let Some(session) = &session {
                model.select_session(session);
                intents.push(SidebarIntent::OpenSession(session.clone()));
            }
        }
        Some(MenuAction::Rename) => match (&project, &session) {
            (_, Some(session)) => {
                let title = model.session_title(session);
                model.start_name(NameAsk::RenameSession(title), Some(session.clone()));
            }
            (Some(project), None) => {
                let name = model.project_name(project).unwrap_or_default();
                model.start_name(NameAsk::RenameProject(name), Some(project.clone()));
            }
            (None, None) => {}
        },
        Some(MenuAction::ToggleArchive) => {
            if let Some(project) = &project {
                let archived = model.project_archived(project).unwrap_or(false);
                let _ = model.set_archived(project, !archived);
            }
        }
        Some(MenuAction::LinkFolder) => {
            if let (Some(project), Some(chosen)) = (&project, pick_folder()) {
                let _ = model.link_folder(project, Some(&chosen));
            }
        }
        Some(MenuAction::UnlinkFolder) => {
            if let Some(project) = &project {
                let _ = model.link_folder(project, None);
            }
        }
        Some(MenuAction::DeleteProject) => {
            if let Some(project) = &project {
                let count = model.session_count(project);
                model.start_confirm(Confirm::DeleteProject(project.clone(), count));
            }
        }
        Some(MenuAction::DeleteSession) => {
            if let Some(session) = &session {
                model.start_confirm(Confirm::DeleteSession(session.clone()));
            }
        }
        None => {}
    }
}

/// Wat er in het menu stond. Eén handeling per pass: een menu dat twee dingen tegelijk zou doen,
/// zou in dezelfde pass het model wijzigen dat de rijen eromheen net lazen.
enum MenuAction {
    NewSession,
    Open,
    Rename,
    ToggleArchive,
    LinkFolder,
    UnlinkFolder,
    DeleteProject,
    DeleteSession,
}

/// Het menu van één riem, teruggevend wat er gekozen is. De drie knoopvormen — project, sessie,
/// inbox — vragen om verschillende handelingen, dus die splitsing staat hier en niet in de rijen.
///
/// Bewust zonder `model`-argument: de sluiting die dit aanroept, leent het sidebar-model al voor de
/// boom eromheen. Wat hier staat, is pure keuze — geen enkele handeling, geen enkel schrijven.
fn menu_item(
    ui: &mut Ui,
    model: &SidebarModel,
    project: Option<&str>,
    session: Option<&str>,
) -> Option<MenuAction> {
    match (project, session) {
        (Some(project), None) => {
            if ui.button("New session").clicked() {
                return Some(MenuAction::NewSession);
            }
            if ui.button("Rename").clicked() {
                return Some(MenuAction::Rename);
            }
            // Twee woorden, geen derde bedacht: `Key.kt:296/297` kiest archiveer- of
            // dearchiveer-woorden naargelang de staat, precies zoals `ProjectSidebar.kt:222`.
            let archived = model.project_archived(project).unwrap_or(false);
            let archive_label = if archived { "Unarchive project" } else { "Archive project" };
            if ui.button(archive_label).clicked() {
                return Some(MenuAction::ToggleArchive);
            }
            if ui.button("Link folder\u{2026}").clicked() {
                return Some(MenuAction::LinkFolder);
            }
            // `if (p.folderPath != null)` (`ProjectSidebar.kt:226`): loskoppelen heeft geen
            // betekenis zonder map, dus het item bestaat dan niet \u2014 het is niet inactief.
            if model.project_folder(project).is_some() && ui.button("Unlink folder").clicked() {
                return Some(MenuAction::UnlinkFolder);
            }
            ui.separator();
            if ui.button("Delete project").clicked() {
                return Some(MenuAction::DeleteProject);
            }
            None
        }
        (_, Some(_)) => {
            if ui.button("Open").clicked() {
                return Some(MenuAction::Open);
            }
            if ui.button("Rename").clicked() {
                return Some(MenuAction::Rename);
            }
            if ui.button("Delete conversation").clicked() {
                return Some(MenuAction::DeleteSession);
            }
            None
        }
        (None, None) => {
            if ui.button("New session").clicked() {
                return Some(MenuAction::NewSession);
            }
            None
        }
    }
}

/// Het label op de kiezer. Als constante, zodat tests/folder_row.rs dezelfde tekst tekent als de
/// paneel: de breedtes in die rij zijn erop gemeten, en een label dat ergens stillen verandert,
/// verandert die maten.
pub const BROWSE_LABEL: &str = "Browse\u{2026}";

/// Het zout van het mappadveld. Een `.id_salt` met een vaste waarde houdt het veldadres stabiel
/// binnen de kolom; het is géén van buiten reconstrueerbaar root-adres (het veld zit in de
/// paneel-kolom). Een test vindt het veld via `root_ui_id` plus deze keten niet — typen gebeurt door
/// het veld te focusen bij de klik op `Open` of via `InteractRequest`; dit zout bestaat voor de
/// stabiliteit zélf (geen toevallige auto-id die meeschuift als er bovenin iets bij komt).
pub const FOLDER_FIELD: &str = "folder-path-field";

/// Het root-adres van de `Open`-knop, als wikkel met click-sense (zie de call site). `root_ui_id`
/// plus dit zout is wél van buiten te vinden en te klikken, zoals `TABSTRIP`.
pub const FOLDER_OPEN: &str = "folder-open";

/// Het adres van de `Refresh`-knop in de rechterkop: `Key.FileTreeRefreshButton`
/// (`Key.kt:375`). Een kale knop in de kolom van het paneel adresseert relatief aan die kolom
/// en is van buiten niet te reconstrueren — hetzelfde bezwijken als de tabstrip en `Open`
/// al hadden — dus ook hier een wikkel op een wortel-adres.
pub const TREE_REFRESH: &str = "file-tree-refresh";

/// Vraag het besturingsselt om een map en geef het gekozen pad terug.
///
/// `Option` en geen paniek: iemand die de dialoog wegklikt, heeft niets gekozen, en dat is geen
/// fout. Losgetrokken van de rij erboven, zodat datgene wat er ná een keuze gebeurt — het pad door
/// hetzelfde commit-pad als een getypt pad sturen — te lezen is zonder een dialoog te openen.
///
/// An unnamed function rather than a method on `Workspace`, because it changes nothing the panel
/// owns: the chosen path re-enters as a draft, and `Workspace::cwd` still has exactly one writer.
fn pick_folder() -> Option<String> {
    rfd::FileDialog::new().set_title("Choose a folder").pick_folder().map(|path| path.display().to_string())
}

/// Het "Copy path"-menu dat de origineler op élke riem van beide lijsten zet.
///
/// `FileTreePanel.kt:510-539`: `maybePopup` voor een boomknoop en `maybePopupRows` voor een
/// git-rij monden allebei uit in `copyPathPopup` — één popup, één item, en dat item draagt
/// `Texts[Key.CopyPath]` (`Key.kt:390`). Twee regels uit die bron zijn hier overgenomen en niet
/// verzonnen: `item.isEnabled = path != null` ("it copies nothing rather than an empty string
/// when there is no path", `:532`) en de tekst is `displayPath(path)`, dus relatief aan de
/// projectmap (`:540-544`, de map zelf als `"."`).
///
/// De handeling gebeurt buiten de sluiting: `Popup::show` vraagt een `FnOnce` en de rijen die het
/// menu dragen, lenen intussen `workspace` en `changes`. Daarom geeft dit alleen terug wat er
/// werd gekozen — dezelfde wacht als `row_menu` in de linkersidebar.
fn copy_path_menu(row: &egui::Response, root: Option<&Path>, path: Option<&Path>) -> Option<String> {
    let text = crate::clipboard::display_path(root, path);
    let enabled = crate::clipboard::copyable(path);
    let chosen = egui::Popup::context_menu(row)
        .show(|ui: &mut Ui| {
            let item =
                ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(crate::clipboard::COPY_PATH).weak()));
            item.clicked().then_some(())
        })
        .and_then(|response| response.inner);
    chosen.map(|()| text)
}

/// Paint one directory level, descending through the headers the user has opened.
///
/// De retourwaarde is nieuw ten opzichte van de origineler en verandert niets aan wat er staat:
/// een `JTree` deelt zijn klik met het menu via een listener, maar in onmiddellijke modus moet de
/// tekening zelf zeggen wat er geklik is. Alleen de klik van deze niveau's bestandsrijen komt
/// terug; een geopende map geeft de aanvraag van zijn eigen rijen door.
fn tree(
    ui: &mut Ui,
    workspace: &mut Workspace,
    dir: &Path,
    depth: usize,
    status: &FileStatus,
    root: Option<&Path>,
) -> Option<String> {
    let listing = workspace.tree_at(dir);
    let mut copied = None;
    for path in &listing.files {
        if let Some(text) = file_row(ui, path, status.get(path), root) {
            copied = Some(text);
        }
    }
    // Said by the level that was cut, so the claim and the rows it describes are one unit: a folder
    // that fits never inherits another folder's warning.
    if listing.truncated {
        ui.label(dim(format!("… cut at {MAX_ENTRIES_PER_DIR} entries")));
    }
    for path in &listing.dirs {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        egui::CollapsingHeader::new(truncate_head(&name, KEEP_TAIL)).id_salt(path.display().to_string()).show(
            ui,
            |ui| {
                if depth >= MAX_DEPTH {
                    ui.label(dim("…"));
                } else {
                    if let Some(text) = tree(ui, workspace, path, depth + 1, status, root) {
                        copied = Some(text);
                    }
                }
            },
        );
    }
    if listing.files.is_empty() && listing.dirs.is_empty() {
        // "empty folder" (`Key.kt:388`), niet "empty": de origineler vult er een
        // kind-riem mee zodat een map als dicht- en niet als leeg-lerend hoeft te lezen.
        ui.label(dim("empty folder"));
    }
    copied
}

/// One file in the tree. A path git named wears git's colour -- the same three values as the changed
/// list on the right, so a modified, added or new file reads identically on both sides -- while a
/// path git left alone stays the panel's own dim. Dirs are never coloured here, as in the original:
/// git reports states for files, and a coloured folder would claim a change it cannot name.
fn file_row(ui: &mut Ui, path: &Path, change: Option<ChangeKind>, root: Option<&Path>) -> Option<String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let colour = change.map_or(palette::DIM, ink_for);
    // De wikkel is de volle breedte, zoals `BaudetTree.getScrollableTracksViewportWidth` de bron
    // zijn rijen laat volgen (`FileTreePanel.kt:554`): een rechtermuisklik naast de naam opent bij
    // de origineler óók het menu, en een klik die alleen op de letters telt, zou dat verliezen.
    let mut copied = None;
    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::hover()), |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(truncate_head(&name, KEEP_TAIL)).color(colour))
                    .selectable(false),
            )
            .on_hover_text(path.display().to_string());
        });
        // Het vierkant van de wikkel is de rij, niet de letters: `ui.max_rect()` is de volle
        // breedte die de wikkel inneemt, en geeft zo de klikplaats die `JTree` heeft omdat zijn
        // rijen de viewport-breedte volgen (`getScrollableTracksViewportWidth`, `FileTreePanel.kt:554`).
        let row = ui.interact(ui.max_rect(), ui.id().with(("file-row", path)), egui::Sense::hover());
        if let Some(text) = copy_path_menu(&row, root, Some(path)) {
            copied = Some(text);
        }
    });
    copied
}

/// De strook die de changes-lijst onder de boom krijgt: de hoogte van de greep erboven plus de
/// lijst, met de greep op de rand van de twee.
///
/// Bestaat, omdat de origineler hier een `JSplitPane` neerzet en deze flank tot nu toe alles in
/// één kolom duwde: de band had toen een hoogte die niemand kon veranderen. De maten zijn niet
/// eigen — `CHANGES_HEIGHT` en `CHANGES_MIN` komen uit `FileTreePanel.kt:629`, en wat hier staat
/// is only the strip's own reach, die de bron niet laat zien omdat een screenshot geen greep is.
fn changes_band(
    ui: &mut Ui,
    changes: &Changes,
    chosen: Option<&str>,
    dividers: &mut layout::Dividers,
) -> Option<String> {
    let column = ui.available_height();
    let stored = if dividers.files > 0 { dividers.files as f32 } else { layout::CHANGES_HEIGHT };
    // De vloer van de origineler, met de kop van de lijst erbij: `MIN_FILES_VISIBLE` is wat er van
    // de band zichtbaar moet blijven, en de kop is geen deel van die rijen.
    let header = ui.text_style_height(&egui::TextStyle::Heading) + ui.spacing().item_spacing.y;
    let floor = layout::CHANGES_MIN + header;
    let mut band = stored.clamp(floor, column.max(floor));
    let handle = ui.available_rect_before_wrap();
    let strip = egui::Rect::from_min_size(handle.min, egui::vec2(handle.width(), 6.0));
    let grip = ui.interact(strip, egui::Id::new("files-split-handle"), egui::Sense::drag());
    if grip.dragged() {
        // Omlaag slepen maakt de band lager: de band is wat ónder de greep ligt.
        band = (band - grip.drag_delta().y).clamp(floor, column.max(floor));
        dividers.files = band as i32;
        ui.ctx().request_repaint();
    }
    let hot = grip.hovered() || grip.dragged();
    ui.painter().hline(
        strip.x_range(),
        strip.center().y,
        egui::Stroke::new(if hot { 2.0 } else { 1.0 }, palette::BORDER),
    );
    // De band is wat er onder de greep overblijft. `allocate_ui_with_layout` geeft die strook een
    // eigen vierkant binnen dezelfde flank, zodat de kolom één paneel blijft — de bron heeft ook
    // één `SplitPane` in de rechterhelft en geen derde paneel naast de boom.
    let height = layout::band_height(column, band);
    let mut copied = None;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_max_height(height);
            copied = changes_body(ui, changes, chosen);
        },
    );
    copied
}

/// Het achterantwoord van de rechterfiletree: git-wijzigingen onder de mappenboom.
fn changes_body(ui: &mut Ui, changes: &Changes, chosen: Option<&str>) -> Option<String> {
    ui.separator();
    // De kop-rij van de origineler is één regel met één zin, geen kop plus een los aantal:
    // `FileTreePanel.kt:272-276` zet `changesHeader.text` op de notitie van git, het
    // schoon-cijfer, of `changes ({0})` (`Key.kt:391/394/395`). Zolang er geen map is, gaat die
    // regel naar `" "` (`FileTreePanel.kt:226`): de oorspronkelijke reden staat al in de
    // boom-kaart erboven, en de changes-kaart is leeg — twee maal dezelfde zin op elkaars
    // hoogte zou er een derde van maken.
    ui.strong(if chosen.is_none() {
        " ".to_string()
    } else if changes.unavailable {
        "git did not answer".to_string()
    } else if changes.meta.is_none() {
        "not a git repository".to_string()
    } else if changes.paths.is_empty() {
        "no changes".to_string()
    } else {
        format!("changes ({})", changes.paths.len())
    });
    let mut copied = None;
    // `chosen` is hier de projectmap: `maybePopupRows` relativizeert tegen `rootPath` en lost het
    // git-pad daar binnen op (`FileTreePanel.kt:525-527`), precies wat `display_path` doet.
    let root = chosen.map(Path::new);
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if chosen.is_none() || changes.unavailable || changes.meta.is_none() || changes.paths.is_empty() {
            return;
        }
        for file in &changes.paths {
            if let Some(text) = change_row(ui, file, root) {
                copied = Some(text);
            }
        }
    });
    copied
}

/// De rechterflank zoals `Main.kt:170` hem aan het venster vraagt: eerst de map, dan de wijzigingen.
pub fn file_tree(
    ui: &mut Ui,
    workspace: &mut Workspace,
    draft: &mut String,
    changes: &Changes,
    dividers: &mut layout::Dividers,
    width: f32,
) -> bool {
    let frame = Panel::right("filetree")
        .resizable(true)
        // Zelfde regel als links: vloer eerst, want `default_size` klemt hem.
        // 180 pt is de ondergrens die de gemeten folderrij per constructie aankan
        // (tests/folder_row.rs).
        .min_size(180.0)
        // De strook was 280 pt, uit mijn hoofd. De origineler meet de bestandsboom op het
        // verschil tussen het venster en `rightDivider` (`Main.kt:171`, val 720), en in een venster
        // van 1040 pt is dat verschil 320 pt — dus dat is de fabrieksbreedte hier.
        .default_size(width)
        .frame(egui::Frame::side_top_panel(ui.style()));
    let chosen = workspace.cwd().map(|s| s.to_string());
    let mut refresh: Option<bool> = None;
    // De boom en de lijst kunnen allebei een kopieeractie opleveren; één pass hoeft er maar één te
    // doen, want de bron laat één klik één popup openen en die sluit bij de volgende.
    let mut copied: Option<String> = None;
    frame.show(ui, |ui| {
        refresh = Some(file_tree_controls(ui, workspace, draft, changes.meta(), changes.paths()));
        // De verdeeling die de origineler met een `JSplitPane` tekent: boom boven, changes eronder,
        // en tussen die twee een greep die iemand kan pakken (`FileTreePanel.kt:103-113`).
        copied = changes_band(ui, changes, chosen.as_deref(), dividers);
    });
    // De kopie zelf is het gevolg: `copyPathPopup` zet het klembord en zegt niets (`:533-538`), en
    // er is in de origineler geen statusregel voor. Een "copied …"-tekst zou gedrag zijn dat de
    // bron niet heeft, dus een mislukt klembord blijft hier ook stil — het pad staat immers wél
    // in de popup-tekst die iemand al zag. Wat wél blijft, is dat de poging plaatsvindt.
    if let Some(text) = copied {
        let _ = crate::clipboard::copy(&text);
    }
    refresh.unwrap_or(false)
}

/// One path git reports: its state, its name, and git's own counts.
fn change_row(ui: &mut Ui, file: &ChangedFile, root: Option<&Path>) -> Option<String> {
    let ink = ink_for(file.change);
    let body_ink = if file.change.exists_on_disk() { palette::TEXT } else { palette::DIM };
    let mut copied = None;
    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::hover()), |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(format!("{:4}", file.change.word())).color(ink).size(11.0))
                    .selectable(false),
            )
            .on_hover_text(&file.path);
            ui.add(
                egui::Label::new(egui::RichText::new(truncate_head(&file.name(), KEEP_TAIL)).color(body_ink).strong())
                    .selectable(false),
            )
            .on_hover_text(&file.path);
            if let Some(old) = &file.old_path {
                let old =
                    Path::new(old).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| old.clone());
                ui.label(dim(truncate_head(&format!("← {old}"), KEEP_TAIL)));
            }
            // Git's numbers or nothing. A path without counts shows no figures, because `+0 −0` would
            // claim a measured zero on a binary path or in a repository without a commit.
            if let (Some(a), Some(d)) = (file.additions, file.deletions) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if d > 0 {
                        ui.label(egui::RichText::new(format!("−{d}")).color(palette::DIFF_REMOVED).size(11.0));
                    }
                    if a > 0 {
                        ui.label(egui::RichText::new(format!("+{a}")).color(palette::DIFF_ADDED).size(11.0));
                    }
                });
            }
        });
    // Zelfde volle-breedte-regel als de boom, en dezelfde `displayPath`: git noemt een
    // repository-relatief pad, dus de riem toont het relatieve en kopiéért het relatieve
    // (`maybePopupRows` lost `row.relPath` op in `rootPath`, `FileTreePanel.kt:525-527`).
        let git_path = root.map(|root| root.join(&file.path));
        let row = ui.interact(ui.max_rect(), ui.id().with(("change-row", &file.path)), egui::Sense::hover());
        if let Some(text) = copy_path_menu(&row, root, git_path.as_deref()) {
            copied = Some(text);
        }
    });
    copied
}

/// The branch line, in the words the probe actually managed to produce.
fn branch_line(ui: &mut Ui, meta: &GitMeta) {
    match (&meta.branch, &meta.commit_hash) {
        (Some(branch), Some(hash)) => {
            ui.label(format!("{branch}@{}", short(hash)));
        }
        (Some(branch), None) => {
            // An unborn HEAD: measured, `rev-parse HEAD` fails there while `--is-inside-work-tree`
            // still answers `true`, so the branch is real and the commit is not.
            ui.label(format!("{branch} (no commits yet)"));
        }
        (None, Some(hash)) => {
            ui.label(format!("detached@{}", short(hash)));
        }
        (None, None) => {
            ui.label(dim("no branch"));
        }
    }
    if let Some(url) = &meta.repository_url {
        ui.label(dim(truncate_head(url, KEEP_TAIL))).on_hover_text(url.clone());
    }
}

/// Git's state as ink, over the Kotlin table (`FileTreePanel.colorFor`): added/modified/renamed
/// share the added green, a deletion reads red, an untracked path reads blue. It colours the changed
/// list on the right and the file rows in the tree on the left with the same three values, so a path
/// reads one colour wherever it is shown.
///
/// The states git names beyond those three (`Copied`, `Removed`, `Unmerged`) follow the same reading
/// as their nearest kin; `Other` stays dim, because an unnamed porcelain state is not something this
/// panel may colour as if it knew what it is.
fn ink_for(change: ChangeKind) -> Color32 {
    match change {
        ChangeKind::Added | ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::Copied => palette::GIT_ADDED,
        ChangeKind::Deleted | ChangeKind::Removed => palette::GIT_DELETED,
        ChangeKind::Untracked => palette::GIT_UNTRACKED,
        ChangeKind::Unmerged | ChangeKind::Other => palette::DIM,
    }
}

/// The name a `ChangedFile` reaches from an absolute path in the tree, or `None` for a path outside
/// the folder git was asked about. Git reports repository-relative paths, so matching the left
/// panel's absolute rows to its statuses means cutting the repository root off the front -- the
/// same string both sides share; a path that is not below it is not one git named.
fn relative_to(root: &str, path: &Path) -> Option<String> {
    path.strip_prefix(root).ok().map(|rel| rel.to_string_lossy().into_owned())
}

/// The status of a path in the tree, looked up by the repository-relative name git uses. The tree
/// walks a directory at a time and asks per file; a map built once per paint keeps that a lookup
/// rather than a scan of every changed path.
struct FileStatus {
    root: Option<String>,
    statuses: HashMap<String, ChangeKind>,
}

impl FileStatus {
    /// Index the changed paths by the name git gave them, remembering the folder they are relative
    /// to. Without a folder (nothing open) the index stays empty and every lookup reads untracked-
    /// free: the tree then colours nothing, which is honest when git was never asked.
    fn build(cwd: Option<&str>, files: &[ChangedFile]) -> Self {
        let mut statuses = HashMap::with_capacity(files.len());
        for file in files {
            statuses.insert(file.path.clone(), file.change);
        }
        Self { root: cwd.filter(|_| !statuses.is_empty()).map(str::to_owned), statuses }
    }

    /// The kind for one absolute path in the tree, or `None` when git named no change there.
    fn get(&self, path: &Path) -> Option<ChangeKind> {
        let rel = relative_to(self.root.as_deref()?, path)?;
        self.statuses.get(&rel).copied()
    }
}

// ---------------------------------------------------------------------------------------------
// De twee pane-toggles in de kop.
// ---------------------------------------------------------------------------------------------

/// Het zout van de twee pane-toggles, zodat een test ze op adres terugvindt (idem `SIDEBAR_ADD`,
/// `TABSTRIP`, `FOLDER_OPEN`): een expliciet id registreert zijn klik op exact dát adres.
pub const PANEL_TOGGLES: &str = "panel-toggles";

/// De afmeting van een toggle-knop. De kopstrook is één rij hoog en deze knop vult die; de breedte
/// staat los van de inhoud, want de knop wordt met `Ui::put` op een vaste plek gezet en mag de
/// breedte van de strook niet mee naar zich toe trekken.
pub const TOGGLE_SIZE: f32 = 26.0;

/// Het vierkant waarin een toggle thuishoort, gegeven de strook van de kop en of hij aan de west-
/// of de oost-rand hoort. Puur rekenwerk op de strook die de kop aanlevert, dus apart te testen:
/// west ankert op `band.min.x`, oost op `band.max.x - TOGGLE_SIZE`, beide op de bovenrand. Ze raken
/// elkaar alleen als de strook smaller is dan twee knoppen -- op een normale breedte niet.
pub fn toggle_rect(band: egui::Rect, right_side: bool) -> egui::Rect {
    let height = band.height().min(TOGGLE_SIZE);
    let x = if right_side { band.max.x - TOGGLE_SIZE } else { band.min.x };
    let min = egui::pos2(x, band.min.y);
    egui::Rect::from_min_size(min, egui::vec2(TOGGLE_SIZE, height))
}

/// Het teken van een paneel-toggle: de zijde die hij verbergt en weer tevoorschijn haalt, als een
/// dik blok aan die rand. Een geopend paneel wijst naar buiten (klik om te verbergen), een gesloten
/// paneel naar binnen (klik om terug te halen), exact de betekenis van `sidebarToggle` in
/// `Main.kt:247-269`.
fn toggle_glyph(right_side: bool, shown: bool) -> &'static str {
    match (right_side, shown) {
        (false, true) => "\u{25E7}",  // ◧ blok links: sidebar open
        (false, false) => "\u{25E5}", // ◥ blok rechts: sidebar weg
        (true, true) => "\u{25E5}",   // ◥ blok rechts: bestandboom open
        (true, false) => "\u{25E7}",  // ◧ blok links: bestandboom weg
    }
}

/// Een paneel-toggle. Het is een eigen `Widget` en geen `Button`, en hij krijgt zijn `Id` van de
/// aanroeper: zo registreert hij zijn klik op het adres dat de aanroeper construeert, en pas een test
/// hem vinden en klikken zonder de kop-lay-out na te bouwen.
pub struct Toggle {
    id: egui::Id,
    right_side: bool,
    shown: bool,
}

impl Toggle {
    /// `id` is het root-adres waarop deze klik geland moet worden; de aanroeper zet er de `Id` neer
    /// die hij zelf weer kan reconstrueren (via `root_ui_id(..).with((PANEL_TOGGLES, ...))`).
    pub fn new(id: egui::Id, right_side: bool, shown: bool) -> Self {
        Self { id, right_side, shown }
    }
}

impl egui::Widget for Toggle {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let rect = ui.max_rect();
        let response = ui.interact(rect, self.id, egui::Sense::click());
        let colour = if response.hovered() { palette::TEXT } else { palette::DIM };
        let glyph = toggle_glyph(self.right_side, self.shown);
        let layout = egui::Layout::centered_and_justified(egui::Direction::TopDown);
        ui.with_layout(layout, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(glyph).color(colour).size(16.0)).selectable(false));
        });
        response
    }
}

fn dim(text: impl Into<String>) -> egui::WidgetText {
    egui::RichText::new(text.into()).color(palette::DIM).into()
}

fn folder_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

fn short(hash: &str) -> String {
    hash.chars().take(7).collect()
}

/// Cut a string to `keep_tail` characters, keeping its end. A path is identified by its last
/// characters, so the head is what may go; the marker says the cut happened.
fn truncate_head(text: &str, keep_tail: usize) -> String {
    let count = text.chars().count();
    if count <= keep_tail {
        return text.to_string();
    }
    let skip = count - keep_tail;
    let tail: String = text.chars().skip(skip).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The last component of a path, as the tests name entries. Inside the test module on purpose:
    /// the panel has its own `folder_name` for this, and a second copy outside the tests would be
    /// dead code in every build that ships.
    fn name_of(path: &Path) -> String {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// De drie git-kleuren van de origineler (`BaudetPalette.kt:65-67`) moeten ook hier de drie
    /// rgb-waarden dragen, niet de diff-inkten. Een kleurentabel die stil meeverandert naar de
    /// diff-palet, zou de boom en de lijst in één klap ander kleuren laten lezen dan de app die ze
    /// nabouwen; deze assert houdt de rgb-waarden vast zoals de origineler ze noemt.
    #[test]
    fn git_kleuren_zijn_de_waarden_van_de_origineler() {
        assert_eq!(palette::GIT_ADDED, Color32::from_rgb(46, 160, 67));
        assert_eq!(palette::GIT_DELETED, Color32::from_rgb(210, 70, 70));
        assert_eq!(palette::GIT_UNTRACKED, Color32::from_rgb(130, 170, 220));
    }

    /// De tabel van `FileTreePanel.colorFor`: added/modified/renamed delen het groen, deleted is
    /// rood, untracked is blauw. Beyond the original's three states the panel still has to say
    /// something: a copy reads as work (green), a removal as subtraction (red), and a state git
    /// would not name stays dim rather than inventing a colour.
    #[test]
    fn git_staten_lezen_als_de_kleurentabel() {
        assert_eq!(ink_for(ChangeKind::Added), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Modified), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Renamed), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Copied), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Deleted), palette::GIT_DELETED);
        assert_eq!(ink_for(ChangeKind::Removed), palette::GIT_DELETED);
        assert_eq!(ink_for(ChangeKind::Untracked), palette::GIT_UNTRACKED);
        assert_eq!(ink_for(ChangeKind::Unmerged), palette::DIM);
        assert_eq!(ink_for(ChangeKind::Other), palette::DIM);
    }

    /// Git spellt een gewijzigd pad repository-relatief; de boom loopt absolute paftjes af. De
    /// verbinding tussen beide is precies de voorvoegsel-correctie, en een pad buiten de map mag er
    /// geen zijn: die zou een status lenen van een ongerelateerd pad met dezelfde laatste naam.
    #[test]
    fn een_pad_krijgt_zijn_relatieve_naam_of_geen() {
        assert_eq!(relative_to("/repo", Path::new("/repo/src/app.rs")).as_deref(), Some("src/app.rs"));
        assert_eq!(relative_to("/repo", Path::new("/repo")).as_deref(), Some(""));
        // Buiten de repository: geen relatieve naam, dus geen kleur, geen geleende status.
        assert_eq!(relative_to("/repo", Path::new("/other/src/app.rs")), None);
    }

    /// De index zoekt op de naam die git gaf, en één map-raak buiten de root lost niets op. Dit is
    /// wat `file_row` vraagt per rij van de boom.
    #[test]
    fn de_status_index_zoekt_op_git_naam() {
        let files = vec![ChangedFile {
            path: "src/app.rs".to_string(),
            additions: None,
            deletions: None,
            change: ChangeKind::Modified,
            old_path: None,
        }];
        let status = FileStatus::build(Some("/repo"), &files);
        assert_eq!(status.get(Path::new("/repo/src/app.rs")), Some(ChangeKind::Modified));
        // Een pad dat git niet noemde, en een pad buiten de root, zijn allebei "geen kleur".
        assert_eq!(status.get(Path::new("/repo/src/other.rs")), None);
        assert_eq!(status.get(Path::new("/elsewhere/src/app.rs")), None);
    }

    #[test]
    fn a_name_that_fits_is_left_alone() {
        assert_eq!(truncate_head("main.rs", KEEP_TAIL), "main.rs");
    }

    /// The expected strings here are written out rather than recomputed from the function, so a
    /// change to `truncate_head` has to disagree with an independently stated fact.
    #[test]
    fn a_long_path_keeps_its_tail_and_says_so() {
        // The input counts 27 characters, so a tail of 22 starts five in: the cut eats "crate".
        assert_eq!(truncate_head("crates/baudet-ui/src/app.rs", KEEP_TAIL), "…s/baudet-ui/src/app.rs");
        assert_eq!(truncate_head("crates/baudet-core/src/context.rs", 12), "…c/context.rs");
    }

    /// The accent in the middle of the name is the point: counting bytes would cut one character
    /// short per accent and can land the cut inside a two-byte sequence.
    #[test]
    fn the_cut_counts_characters_not_bytes() {
        assert_eq!(truncate_head("naam_met_éééën_tot_lang_pad.rs", 8), "…g_pad.rs");
    }

    /// A listing is read per level while the tree is painted, so nothing the panel showed for the
    /// previous folder can survive into the next one: there is no second listing to go stale. The
    /// numbers below are the contents of the two folders this test builds, written out rather than
    /// derived from `tree_at`.
    #[test]
    fn a_folder_without_a_path_has_nothing_to_show() {
        let mut workspace = Workspace::default();
        assert_eq!(workspace.cwd(), None);
        let missing = workspace.tree_at(Path::new("/no/such/folder/at/all"));
        assert!(missing.dirs.is_empty() && missing.files.is_empty(), "nothing chosen means nothing listed");
        assert!(!missing.truncated, "an unreadable level does not claim to have been cut");
        let changes = Changes::default();
        assert!(changes.meta().is_none());
        assert!(!changes.unavailable);
    }

    /// Moving the panel to a folder that is not there has to lose the previous folder's rows, and
    /// the panel must know which folder it is pointing at even then. A name the caller has already
    /// given up on cannot be resurrected by a later listing.
    #[test]
    fn moving_to_an_absent_folder_leaves_no_stale_listing() {
        let mut workspace = Workspace::default();
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("bron.rs"), "x\n").unwrap();
        workspace.open(&dir.path().to_string_lossy());
        let here = workspace.tree_at(dir.path());
        assert_eq!(here.files.len(), 1, "the real folder lists the file written into it");
        assert_eq!(here.dirs.len(), 0);

        let gone = dir.path().join("verwijderd");
        workspace.open(&gone.to_string_lossy());
        assert_eq!(workspace.cwd(), Some(gone.to_string_lossy().as_ref()));
        let there = workspace.tree_at(gone.as_path());
        assert!(there.files.is_empty() && there.dirs.is_empty(), "a folder that does not exist has no tree");
    }

    #[test]
    fn build_output_and_dotfiles_are_not_offerings_to_the_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("target").join("artifact"), "x").unwrap();
        // `main.rs` sits at the level the test reads, because `tree_at` gives one level and a file
        // one directory down is another level's business. That is what makes this a test of the
        // filter: the ordinary file and `.hidden` are the same kind of entry at the same level, and
        // only one of them may reach the listing.
        std::fs::write(dir.path().join("main.rs"), "x\n").unwrap();
        std::fs::write(dir.path().join(".hidden"), "x").unwrap();
        let mut workspace = Workspace::default();
        let listing = workspace.tree_at(dir.path());
        let folders: Vec<String> = listing.dirs.iter().map(|p| name_of(p.as_path())).collect();
        let files: Vec<String> = listing.files.iter().map(|p| name_of(p.as_path())).collect();
        // A level lists what is in it, folders and files apart. `target` is build output and the two
        // dot-prefixed names are not source, so none of the three may appear in either list.
        assert_eq!(folders, vec!["src".to_string()], "source folders show, build output does not");
        assert_eq!(files, vec!["main.rs".to_string()], "the files of the level show, dot files do not");
    }

    /// De koppeling van `Main.kt:156`: de boom is een lezing van de linkerselectie, dus
    /// De koppeling van `Main.kt:156`: de boom is een lezing van de linkerselectie, dus
    /// `apply_sidebar_intents` eindigt met wat `showProject` zou doen. Die drieling — map
    /// tonen, 'geen map', of 'geen leestoegang' — leeft hier in `show_message`/`open` en is
    /// hier bewezen in plaats van in pixels nagemeten.
    #[test]
    fn een_selectie_zonder_map_verduistert_de_boom_met_zijn_reden() {
        let mut workspace = Workspace::default();
        workspace.show_message(NO_FOLDER);
        assert_eq!(workspace.cwd(), None, "een melding laat de map van een eerder project staan");
        assert_eq!(workspace.message(), Some(NO_FOLDER), "de boom zegt dat er geen map is");
    }

    /// De selectie wint van de handmatige kiezer, zoals de origineler doet: `showProject` laadt
    /// wat het gekozen project heeft, ook al bladerde de persoon daarvoor zelf elders. Dit is
    /// precies het geval dat een dedupe op project-id stil zou laten falen: de map die zojuist
    /// aan het al geselecteerde project is gekoppeld, moet meteen tonen.
    #[test]
    fn een_selectie_vervangt_de_handmatig_gekozen_map() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut workspace = Workspace::default();
        workspace.open(&dir.path().to_string_lossy());
        workspace.show_message(NO_FOLDER);
        assert_eq!(workspace.cwd(), None, "het project zonder map vervangt de gebladerde map");
        assert_eq!(workspace.message(), Some(NO_FOLDER));
        workspace.open(&dir.path().to_string_lossy());
        assert_eq!(workspace.cwd(), Some(dir.path().to_string_lossy().as_ref()), "de nieuwe koppeling toont");
        assert_eq!(workspace.message(), None, "met een map is er niets meer te zeggen");
    }

    /// Twee opeenvolgende redenen: de jongste is wat de boom zegt, want de selectie is de bron.
    #[test]
    fn de_laatste_selectie_bepaalt_de_reden() {
        let mut workspace = Workspace::default();
        workspace.show_message(NO_READ);
        assert_eq!(workspace.message(), Some(NO_READ), "geen leestoegang is een eigen reden");
        workspace.show_message(NO_FOLDER);
        assert_eq!(workspace.message(), Some(NO_FOLDER), "de jongste selectie is wat er staat");
    }

    /// Zelf een map noemen is geen projectgebeurtenis, maar wist wel de reden die er stond.
    #[test]
    fn een_geopende_map_houdt_geen_reden_meer_over() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut workspace = Workspace::default();
        workspace.show_message(NO_FOLDER);
        workspace.open(&dir.path().to_string_lossy());
        assert_eq!(workspace.message(), None, "wie zelf een map kiest, hoeft geen uitleg meer");
        assert_eq!(workspace.cwd(), Some(dir.path().to_string_lossy().as_ref()));
    }
    /// Het vinkje `Hidden files` uit `FileTreePanel.kt:97`: de punt-namen gaan mee, de
    /// bouw-uitgang niet. Dat onderscheid is de hele inhoud van het item — `FileLister.kt:20
    /// negeert alleen het punt-deel`, en de `IGNORED_DIRS`-filter kent de origineler niet in
    /// die lijst maar in zijn eigen boom, waar hij ook met het vinkje aan blijft gelden.

    /// De brief onder de boom is geen vrije tekst. Drie woorden staan vast in de bron en
    /// bestaan hier uit dezelfde constanten: `Folder permissions` (`Key.kt:321`), het
    /// `folder:`-label (`Key.kt:319`) en `no folder linked` (`Key.kt:320`). Een test die ze
    /// vergelijkt met een in de test herhaalde zin, bewijst niets; hij vergelijkt ze met de
    /// regels uit `Key.kt` die erboven staan, en met de selectieregel van de bron.
    #[test]
    fn de_brief_onder_de_boom_gebruikt_de_woorden_uit_key_kt() {
        assert_eq!(PERMISSIONS_TITLE, "Folder permissions");
        assert_eq!(FOLDER_LABEL, "folder:");
        assert_eq!(FOLDER_NONE, "no folder linked");
        assert_eq!(UNASSIGNED, "Unassigned");
    }

    /// Zonder selectie tekent de bron zijn map-riem met `FolderNone` en zet hij de vinkjes uit
    /// (`ProjectSidebar.kt:249-253`). De waarden hier komen uit `permission_state()`, dus als die
    /// ophoudt `None` te geven boven een lege selectie — of zijn vinkjes aan laat — faalt dit.
    #[test]
    fn zonder_selectie_is_er_geen_map_en_zijn_de_vinkjes_uit() {
        let model = SidebarModel::open_default();
        assert!(model.selected_project().is_none(), "een vers model claimt een selectie");
        assert!(model.permission_state().is_none(), "zonder selectie tekent de bron geen rechten-vak");
    }

    /// Met een map worden de vinkjes aanraakbaar en krijgt de schijf zijn eigen regel
    /// (`ProjectSidebar.kt:254-256`: `setPermChecks(p.permissions, enabled = folderPath != null)`
    /// plus `capLabel.text = diskCapability(..)`). Zonder map moet die schijf-regel er niet staan:
    /// `capLabel.text = " "` (`:251`), en "op schijf: leesbaar=nee" zou een meting claimen van
    /// een map die er niet is.
    #[test]
    fn een_map_geeft_de_vinkjes_en_zijn_schijf_regel() {
        let dir = std::env::temp_dir();
        let path = dir.to_str().expect("temp-dir is geldig UTF-8");
        let mut model = SidebarModel::open_default();
        let project = model.add_project("woorden").expect("project aangemaakt");
        model.select_project(Some(&project.id));
        model.link_folder(&project.id, Some(path)).expect("map gekoppeld");
        let state = model.permission_state().expect("met selectie tekent de bron het vak");
        assert_eq!(state.folder.as_deref(), Some(path));
        assert!(state.checkbox_enabled, "met een map staan de vinkjes niet uit");
        assert!(state.disk_capability.is_some(), "met een map hoort de schijf-regel er te staan");
    }

    #[test]
    fn verborgen_stukken_mogen_mee_na_het_vinkje() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".github")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join(".env"), "x").unwrap();
        std::fs::write(dir.path().join("main.rs"), "x\n").unwrap();
        let mut workspace = Workspace::default();
        assert!(!workspace.show_hidden(), "de origineler start zonder vinkje (`JCheckBox` aan)");
        let stil = workspace.tree_at(dir.path());
        assert_eq!(stil.files.iter().map(|p| name_of(p)).collect::<Vec<_>>(), vec!["main.rs".to_string()]);
        // Aan: de punt-namen komen erbij, `target` blijft buiten de boot.
        workspace.set_show_hidden(true);
        let open = workspace.tree_at(dir.path());
        assert_eq!(open.files.iter().map(|p| name_of(p)).collect::<Vec<_>>(), vec![".env".to_string(), "main.rs".to_string()]);
        assert_eq!(open.dirs.iter().map(|p| name_of(p)).collect::<Vec<_>>(), vec![".github".to_string()], "`target` is geen verborgen naam, maar bouw-uitgang");
    }
    #[test]
    fn folders_are_listed_before_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("aaa.txt"), "x\n").unwrap();
        std::fs::create_dir_all(dir.path().join("zzz")).unwrap();
        let mut workspace = Workspace::default();
        let listing = workspace.tree_at(dir.path());
        assert_eq!(listing.dirs.iter().map(|p| name_of(p.as_path())).collect::<Vec<_>>(), vec!["zzz".to_string()]);
        assert_eq!(listing.files.iter().map(|p| name_of(p.as_path())).collect::<Vec<_>>(), vec!["aaa.txt".to_string()]);
        // Folders lead because they are asked about first and files are never interleaved: the panel
        // paints the whole of `dirs` before the whole of `files`.
        let paint_order: Vec<String> =
            listing.dirs.iter().chain(listing.files.iter()).map(|p| name_of(p.as_path())).collect();
        assert_eq!(paint_order, vec!["zzz".to_string(), "aaa.txt".to_string()], "directories lead");
    }
}
