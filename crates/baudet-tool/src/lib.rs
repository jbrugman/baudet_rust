//! Wat de assistent in een projectmap mag doen, en wie daarover beslist.
//!
//! Poort van het `tool`-pakket van de origineler. De dragende regel van deze port — een widget bestaat
//! pas als het domein eronder bestaat — geldt hier het hardst: de toolknoppen in de UI zijn vandaag
//! sierraad, omdat geen van hen erachter iets draait. Deze crate begint bij de bodem van de dependency-graaf,
//! bij het onderdeel waar een fout de duurste is: de vraag welk commando zonder mens mag draaien
//! ([`safe_commands`]). Daarna pas de rest van de laag.
//!
//! Zowel deze laag als de API-laag blijft model-tekst in het Engels: descriptions en tool-uitvoer gaan
//! rechtstreeks naar het model, ongeacht de UI-taal, precies zoals in de origineler.

pub mod command_signature;
pub mod folder_boundary;
pub mod folder_listing;
pub mod safe_commands;
