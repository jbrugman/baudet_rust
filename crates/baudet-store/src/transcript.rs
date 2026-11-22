//! Eén open gesprek op schijf: de lange-levende aanraking die `H2Store` met `db` is.
//!
//! `H2Store.kt:19-24` opent één `Connection` voor de hele levensduur van de app en doet er al
//! zijn `PreparedStatement`s op. De equivalent hiervan is niet het stateuteloze `Repository`
//! (dat per aanroep een `Rollout` opent en dus per beurt vier descriptors open-t) maar dit
//! object: één gesprek, één set open bestanden, zoveel beurten als de gebruiker wil.
//!
//! Twee bewust gekozen grenzen:
//!
//!  * **één gesprek per handvat.** De origineler is één `AppState`/één `session`, en meer
//!    live-gesprekken tegelijk staan hier expliciet als later (zie de turn-claim in
//!    `session.rs`). Dit object past precies bij die aannname; een handvat-per-sessie zou de
//!    eerste stap naar die volgende fase zijn en niet vooruitlopen op de huidige vraag.
//!  * **lezen van een *ouder* venster blijft de stateloze `Repository::items_window`.** Een
//!    scroll-om-hoger-lezing is een toevalslezing en mag geen open descriptor vergen; de
//!    oorspronkelijke lezer heeft er zelf ook geen voor nodig.

use crate::paging::ItemWindow;
use crate::records::{SessionDetail, SessionMeta};
use crate::repository::page;
use crate::rollout::Rollout;
use crate::session::Session;
use baudet_core::Item;
use std::path::Path;

/// Eén gesprek, open voor aanhangen en voor herschrijven van zijn kop.
///
/// Sluit de bestanden bij drop. Een `Rollout` is `Send` (inwendig geen delers), dus dit handvat
/// naar de worker-thread meenemen is veilig; het is bewust niet `Sync`, want er hoort één eigenaar
/// bij te horen.
#[derive(Debug)]
pub struct Transcript {
    rollout: Rollout,
}

impl Transcript {
/// Een bestaand gesprek openen, of een nieuw aanmaken als de map nog leeg is.
    ///
    /// `model`/`cwd` zijn de herkomst-regels voor een *nieuw* bestand en worden genegeerd als het
    /// gesprek er al staat: de identiteitsregel is write-once en mag niet van een tweede opening
    /// afhangen. `create` verzet zich tegen overschrijven van een bestaande sessie, dus deze route
    /// proberst eerst `open` en valt alleen terug op `create` als er werkelijk niets is.
    pub fn open_or_create(
        root: &Path,
        session_id: &str,
        model: Option<&str>,
        cwd: Option<&str>,
    ) -> std::io::Result<Transcript> {
        match Rollout::open(root, session_id) {
            Ok(rollout) => Ok(Transcript { rollout }),
            // `open` faalt op een sessie zonder identiteitsregel - dat is precies "nog niet
            // gemaakt". Een echte leesfout (map niet leesbaar) mag hier niet als "nieuw" lezen.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                let mut meta = SessionMeta::new(session_id);
                meta.model = model.map(str::to_owned);
                meta.cwd = cwd.map(str::to_owned);
                Ok(Transcript { rollout: Rollout::create(root, &meta)? })
            }
            Err(error) => Err(error),
        }
    }

    /// Het gesprek uit de schijf lezen in een verse staat, zonder iets gedeelds aan te raken
    /// (`AppState.buildSession`).
    ///
    /// De randen (`persisted_messages`, `persisted_items_tail_id`) worden uit de lezing gezet,
    /// zodat de eerstvolgende `persist` alléén de staart aanhangt die deze beurt toevoegt en niet
    /// elke rij herschrijft.
    pub fn build(&self, fallback_model: &str) -> std::io::Result<Session> {
        let detail = self.rollout.detail()?;
        let mut session = Session::new(
            self.rollout.session().session_id.clone(),
            detail
                .as_ref()
                .and_then(|d| d.model.clone())
                .or_else(|| self.rollout.session().model.clone())
                .unwrap_or_else(|| fallback_model.to_owned()),
        );
        session.previous_response_id = detail.as_ref().and_then(|d| d.prev_response_id.clone());
        session.messages = self.rollout.messages()?;
        session.summary = detail.as_ref().and_then(|d| d.summary.clone());
        let window = self.items_window(page::INITIAL, None)?;
        session.items = window.items;
        session.oldest_loaded_seq = if window.has_more_older { window.older_seq } else { None };
        session.persisted_messages = session.messages.len();
        session.persisted_items_tail_id = session.items.last().map(Item::item_id).map(String::from);
        Ok(session)
    }

    /// Aanhangen wat er nieuw is, en de kopregel actueel schrijven. De tail-only-regel staat bij
    /// `Repository::persist` en geldt hier hetzelfde; alleen de descriptor is nu gedeeld.
    pub fn persist(&mut self, session: &mut Session) -> std::io::Result<()> {
        if session.messages.len() > session.persisted_messages {
            for msg in &session.messages[session.persisted_messages..].to_vec() {
                self.rollout.append_msg(msg)?;
            }
            session.persisted_messages = session.messages.len();
        }
        let start = session.items.iter().rposition(|row| {
            session.persisted_items_tail_id.as_deref() == Some(row.item_id())
        });
        let new_items: &[Item] = match start {
            Some(index) => &session.items[index + 1..],
            None => &session.items[..],
        };
        if !new_items.is_empty() {
            for item in new_items {
                self.rollout.append_item(item)?;
            }
            session.persisted_items_tail_id =
                session.items.last().map(Item::item_id).map(String::from);
        }
        self.rollout.write_detail(&SessionDetail {
            session_id: session.session_id.clone(),
            model: Some(session.model.clone()),
            prev_response_id: session.previous_response_id.clone(),
            seq: session.messages.len() as u64,
            summary: session.summary.clone(),
        })?;
        self.rollout.flush()
    }

    /// Eén ouder venster, op hetzelfde open gesprek, zonder een tweede descriptor te openen.
    pub fn items_window(&self, limit: usize, before_seq: Option<u64>) -> std::io::Result<ItemWindow> {
        let rows = self.rollout.item_rows(Some(limit), before_seq)?;
        Ok(ItemWindow::from_newest_first(rows, limit, |_slot, row| Some(row.1.clone())))
    }

    /// De beurtental die op schijf staat, voor de index-titel (`last_seq`).
    pub fn message_count(&self) -> std::io::Result<usize> {
        Ok(self.rollout.messages()?.len())
    }
}
