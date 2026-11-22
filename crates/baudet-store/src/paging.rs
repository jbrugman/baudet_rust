//! Reading a trail one page at a time, oldest first, without decoding the whole file.
//!
//! This is `TranscriptStore.itemsWindow`, and the paging discipline that comes with it. A long
//! conversation must open on its tail: a session with two hundred rows should not decode two
//! hundred of them to draw the last ten, and it certainly should not decode them on the UI thread.
//!
//! The subtle part is the shape of the answer, so it is a type instead of a tuple:
//!
//!  * **items are oldest first, the cursor is not.** The window is *read* newest first and then
//!    reversed, because the newest rows are what you want and the cheapest to reach. `older_seq`
//!    is the lowest sequence actually present, which is the only boundary that can be handed back
//!    as the next `before_seq` without skipping or repeating a row.
//!  * **"more exists" is counted on stored rows, not on decoded ones.** An undecodable row is
//!    dropped — one stale card must not erase the conversation around it — but if the extra row
//!    proving that more precedes the window were only counted when it decodes, a single corrupt
//!    row would make the window over-read and the cursor under-count. Kotlin reads `limit + 1`
//!    rows for exactly this reason and counts them before decoding.
//!  * **`older_seq` is what is present, not what was requested.** When the newest row of a window
//!    fails to decode, the cursor names the row below it. Paging stays correct; the row is simply
//!    gone, which is the same tolerance the whole-file read gives.

use baudet_core::Item;

/// One stored row as a reader hands it over: its sequence number and its payload.
///
/// A named pair rather than a tuple so the decode callback below can take it by reference without
/// every caller spelling out `&(u64, T)`.
pub type Stored<T> = (u64, T);

/// One page of transcript rows, oldest first, as read by a paged window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemWindow {
    /// The rows of this window, oldest first.
    pub items: Vec<Item>,
    /// The lowest sequence present in [`ItemWindow::items`] — the boundary for the next older
    /// page — or `None` when the window is empty.
    pub older_seq: Option<u64>,
    /// Whether the store holds any row before this window.
    pub has_more_older: bool,
}

impl ItemWindow {
    /// An empty window: nothing read, nothing before it.
    pub fn empty() -> ItemWindow {
        ItemWindow { items: Vec::new(), older_seq: None, has_more_older: false }
    }

    /// Build a window from stored rows handed to it newest first, keeping at most `limit`.
    ///
    /// `rows` are `(seq, payload)` pairs, which is what a reader has before it decides what a
    /// payload means. `decode` is the caller's tolerance: it returns `None` for a row it cannot
    /// read, and this function still counts that row as present — both for the limit and for the
    /// "more precedes this" proof.
    pub fn from_newest_first<T>(
        rows: impl IntoIterator<Item = Stored<T>>,
        limit: usize,
        mut decode: impl FnMut(usize, &Stored<T>) -> Option<Item>,
    ) -> ItemWindow {
        let mut items: Vec<Item> = Vec::new();
        let mut older_seq: Option<u64> = None;
        // `take(limit + 1)`: the limit is a bowl and the extra row is the proof that something
        // precedes it. The enumeration hands the decoder this row's position in the window.
        let mut kept = rows.into_iter().take(limit + 1).peekable();
        for (slot, row) in kept.by_ref().take(limit).enumerate() {
            // Sequences descend by construction, so every row kept is lower than the last.
            older_seq = Some(row.0);
            if let Some(item) = decode(slot, &row) {
                items.push(item);
            }
        }
        // A row still waiting behind the bowl is the whole answer to "is there more before this?".
        items.reverse();
        ItemWindow { items, older_seq, has_more_older: kept.peek().is_some() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::{AgentMessageItem, ItemStatus};

    fn row(seq: u64) -> (u64, String) {
        (seq, format!("rij {seq}"))
    }

    /// De tolerantie van de lezer: `"kapot"` is een regel die niet meer decodeert.
    fn decoded(_slot: usize, row: &(u64, String)) -> Option<Item> {
        if row.1 == "kapot" {
            return None;
        }
        // Het id is het seq-nummer, zodat `ids` hier de cursor kan nalopen zonder dat de
        // payload-tekst en het id door elkaar gaan lopen.
        Some(Item::AgentMessage(AgentMessageItem {
            item_id: row.0.to_string(),
            text: row.1.clone(),
            status: ItemStatus::Completed,
        }))
    }

    fn ids(window: &ItemWindow) -> Vec<&str> {
        window.items.iter().map(Item::item_id).collect()
    }

    #[test]
    fn a_full_page_comes_back_oldest_first_with_the_cursor_below_it() {
        // Zeven opgeslagen rijen, de nieuwste eerst aangeboden, pagina-grootte 3.
        let rows: Vec<(u64, String)> = (0..7).rev().map(row).collect();
        let window = ItemWindow::from_newest_first(rows, 3, decoded);
        assert_eq!(ids(&window), ["4", "5", "6"], "gelezen nieuwste-eerst, teruggegeven oudste-eerst");
        assert_eq!(window.older_seq, Some(4), "het laagste nummer dat hier wérkelijk staat");
        assert!(window.has_more_older, "rij 3 bleef over, dus er gaat er nog een aan vooraf");
    }

    #[test]
    fn a_short_page_reports_no_boundary_and_no_more() {
        let rows = vec![row(1), row(0)];
        let window = ItemWindow::from_newest_first(rows, 10, decoded);
        assert_eq!(ids(&window), ["0", "1"]);
        assert_eq!(window.older_seq, Some(0));
        assert!(!window.has_more_older, "er was geen extra rij om af te keuren");
    }

    #[test]
    fn an_empty_store_yields_an_empty_window_without_a_cursor() {
        let window = ItemWindow::from_newest_first(Vec::<(u64, String)>::new(), 10, decoded);
        assert!(window.items.is_empty());
        assert_eq!(window.older_seq, None, "er is geen laagste nummer, dus geen volgend blad");
        assert!(!window.has_more_older);
    }

    #[test]
    fn a_corrupt_row_is_dropped_but_still_costs_a_slot_and_still_proves_more() {
        // Dit is de eigenschap die een naïeve implementatie verliest: als het bewijs van "er is
        // meer" alleen telt wanneer het decodeert, leest een venster te veel en telt de cursor te
        // weinig. De rij hier wordt géén item en gaat wél op in de limiet.
        let rows = vec![(9, "kapot".to_string()), (8, "ok".to_string()), (7, "meer".to_string())];
        let window = ItemWindow::from_newest_first(rows, 2, decoded);
        assert_eq!(ids(&window), ["8"], "rij 7 viel buiten de limiet van twee, rij 9 is onleesbaar");
        assert_eq!(window.older_seq, Some(8), "de kapotte rij is weg, dus het nummer eronder is de rand");
        assert!(window.has_more_older);
    }

    #[test]
    fn the_row_beyond_the_limit_proves_more_without_being_decoded() {
        // Een kapotte rij vóór de rand mag het bladeren niet vertonen als "klaar".
        let rows = vec![(5, "ok".to_string()), (4, "kapot".to_string())];
        let window = ItemWindow::from_newest_first(rows, 1, decoded);
        assert_eq!(ids(&window), ["5"]);
        assert_eq!(window.older_seq, Some(5));
        assert!(window.has_more_older, "onleesbaar is niet hetzelfde als afwezig");
    }

    #[test]
    fn paging_to_the_start_terminates_without_repeating_a_row() {
        // De cursor is exclusief: het volgende blad begint eronder. Negen rijen, telkens twee.
        let rows: Vec<(u64, String)> = (0..9).rev().map(row).collect();
        let mut seen: Vec<String> = Vec::new();
        let mut before: Option<u64> = None;
        let mut pages = 0;
        loop {
            let cut = match before {
                Some(cut) => rows.iter().filter(|(seq, _)| *seq < cut).cloned().collect(),
                None => rows.clone(),
            };
            let window = ItemWindow::from_newest_first(cut, 2, decoded);
            if window.items.is_empty() {
                break;
            }
            pages += 1;
            for id in ids(&window) {
                seen.push(id.to_string());
            }
            match window.older_seq {
                Some(cursor) if window.has_more_older => before = Some(cursor),
                _ => break,
            }
            assert!(pages < 20, "blind bladeren mag nooit eindloos worden");
        }
        seen.sort();
        assert_eq!(pages, 5, "negen rijen in bladen van twee zijn vijf bladen");
        assert_eq!(seen, (0..9).map(|seq| seq.to_string()).collect::<Vec<_>>(), "geen rij twee keer, geen rij gemist");
    }
}
