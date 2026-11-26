
use baudet_core::Item;

pub type Stored<T> = (u64, T);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemWindow {
    pub items: Vec<Item>,
    pub older_seq: Option<u64>,
    pub has_more_older: bool,
}

impl ItemWindow {
    pub fn empty() -> ItemWindow {
        ItemWindow { items: Vec::new(), older_seq: None, has_more_older: false }
    }

    // Count stored rows before decoding. Skipped records still advance the paging cursor.
    pub fn from_newest_first<T>(
        rows: impl IntoIterator<Item = Stored<T>>,
        limit: usize,
        mut decode: impl FnMut(usize, &Stored<T>) -> Option<Item>,
    ) -> ItemWindow {
        let mut items: Vec<Item> = Vec::new();
        let mut older_seq: Option<u64> = None;
        let mut kept = rows.into_iter().take(limit + 1).peekable();
        for (slot, row) in kept.by_ref().take(limit).enumerate() {
            older_seq = Some(row.0);
            if let Some(item) = decode(slot, &row) {
                items.push(item);
            }
        }
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

    fn decoded(_slot: usize, row: &(u64, String)) -> Option<Item> {
        if row.1 == "kapot" {
            return None;
        }
        Some(Item::AgentMessage(AgentMessageItem {
            created_at_ms: None,
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
        let rows = vec![(9, "kapot".to_string()), (8, "ok".to_string()), (7, "meer".to_string())];
        let window = ItemWindow::from_newest_first(rows, 2, decoded);
        assert_eq!(ids(&window), ["8"], "rij 7 viel buiten de limiet van twee, rij 9 is onleesbaar");
        assert_eq!(window.older_seq, Some(8), "de kapotte rij is weg, dus het nummer eronder is de rand");
        assert!(window.has_more_older);
    }

    #[test]
    fn the_row_beyond_the_limit_proves_more_without_being_decoded() {
        let rows = vec![(5, "ok".to_string()), (4, "kapot".to_string())];
        let window = ItemWindow::from_newest_first(rows, 1, decoded);
        assert_eq!(ids(&window), ["5"]);
        assert_eq!(window.older_seq, Some(5));
        assert!(window.has_more_older, "onleesbaar is niet hetzelfde als afwezig");
    }

    #[test]
    fn paging_to_the_start_terminates_without_repeating_a_row() {
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
