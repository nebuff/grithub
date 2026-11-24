//! The compact board model. Everything here is host-agnostic and serialisable,
//! because it is what gets cached between launches.

use serde::{Deserialize, Serialize};

use crate::queries::{ItemNode, ItemsData};

/// Hard cap on cards kept in memory and in the state file. A page is 100 items,
/// so five pages of history is plenty for a board read on a Kindle.
pub const MAX_CARDS: usize = 500;

/// Titles are clipped on the way in, not on the way out: the state file has a
/// 256 KiB budget shared with everything else.
pub const TITLE_LIMIT: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub number: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub project_id: String,
    pub project_title: String,
    pub field_name: String,
    pub columns: Vec<Column>,
    pub fetched_at: u64,
    pub truncated: bool,
}

impl Board {
    pub fn total_cards(&self) -> usize {
        self.columns.iter().map(|c| c.cards.len()).sum()
    }

    pub fn find_card(&self, id: &str) -> Option<(&Card, &Column)> {
        self.columns.iter().find_map(|column| {
            column
                .cards
                .iter()
                .find(|c| c.id == id)
                .map(|card| (card, column))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRef {
    pub id: String,
    pub title: String,
    pub number: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageStep {
    pub has_next: bool,
    pub cursor: Option<String>,
}

pub struct BoardBuilder {
    project_id: String,
    project_title: String,
    field_name: String,
    order: Vec<String>,
    cards: Vec<(Option<String>, Card)>,
    seen: usize,
    truncated: bool,
}

impl BoardBuilder {
    pub fn new(
        project_id: &str,
        project_title: &str,
        field_name: &str,
        options: &[String],
    ) -> Self {
        Self {
            project_id: project_id.to_owned(),
            project_title: project_title.to_owned(),
            field_name: field_name.to_owned(),
            order: options.to_vec(),
            cards: Vec::new(),
            seen: 0,
            truncated: false,
        }
    }

    pub fn is_full(&self) -> bool {
        self.cards.len() >= MAX_CARDS
    }

    pub fn seen(&self) -> usize {
        self.seen
    }

    pub fn push_page(&mut self, data: &ItemsData) -> PageStep {
        let items = match data.node.as_ref() {
            Some(node) => &node.items,
            None => return PageStep::default(),
        };

        for node in &items.nodes {
            self.seen += 1;
            if self.is_full() {
                self.truncated = true;
                continue;
            }
            let status = status_of(node, &self.field_name);
            self.cards.push((status, card_from(node)));
        }

        let more = items.page_info.has_next_page && !self.is_full();
        if items.page_info.has_next_page && self.is_full() {
            self.truncated = true;
        }

        PageStep {
            has_next: more,
            cursor: items.page_info.end_cursor.clone(),
        }
    }

    pub fn finish(self, now: u64) -> Board {
        let mut columns: Vec<Column> = self
            .order
            .iter()
            .map(|name| Column {
                name: name.clone(),
                cards: Vec::new(),
            })
            .collect();

        let mut without_status: Vec<Card> = Vec::new();

        for (status, card) in self.cards {
            match status {
                Some(name) => match columns.iter_mut().find(|c| c.name == name) {
                    Some(column) => column.cards.push(card),
                    None => columns.push(Column {
                        name,
                        cards: vec![card],
                    }),
                },
                None => without_status.push(card),
            }
        }

        if !without_status.is_empty() {
            columns.push(Column {
                name: "No status".to_owned(),
                cards: without_status,
            });
        }

        Board {
            project_id: self.project_id,
            project_title: self.project_title,
            field_name: self.field_name,
            columns,
            fetched_at: now,
            truncated: self.truncated,
        }
    }
}

fn status_of(node: &ItemNode, field: &str) -> Option<String> {
    node.field_values.as_ref()?.nodes.iter().find_map(|value| {
        let matches_field = value
            .field
            .as_ref()
            .map(|f| f.name == field)
            .unwrap_or(false);
        if !matches_field {
            return None;
        }
        let is_single_select = value.typename.as_deref() == Some("ProjectV2ItemFieldSingleSelectValue");
        if is_single_select {
            value.name.clone()
        } else {
            None
        }
    })
}

fn card_from(node: &ItemNode) -> Card {
    let (title, number) = match node.content.as_ref() {
        None => ("Restricted item".to_owned(), None),
        Some(content) => match content.typename.as_deref() {
            // Redacted items expose neither a title nor a number.
            Some("Redacted") => ("Restricted item".to_owned(), None),
            _ => (
                content.title.clone().unwrap_or_else(|| "Untitled".to_owned()),
                content.number,
            ),
        },
    };

    Card {
        id: node.id.clone(),
        title: clip(&title, TITLE_LIMIT),
        number,
    }
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(json: &str) -> ItemsData {
        serde_json::from_str(json).unwrap()
    }

    const PAGE_ONE: &str = r#"{"node":{"items":{"pageInfo":{"hasNextPage":true,"endCursor":"c1"},"nodes":[
        {"id":"i1","content":{"__typename":"Issue","number":12,"title":"Ship the board"},
         "fieldValues":{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"In Progress","field":{"name":"Status"}}]}},
        {"id":"i2","content":{"__typename":"DraftIssue","title":"Draft card"},"fieldValues":{"nodes":[]}}
    ]}}}"#;

    #[test]
    fn columns_follow_field_order_and_collect_statusless_cards() {
        let mut b = BoardBuilder::new(
            "P_1",
            "Roadmap",
            "Status",
            &["Todo".to_owned(), "In Progress".to_owned(), "Done".to_owned()],
        );
        let step = b.push_page(&page(PAGE_ONE));
        assert!(step.has_next);
        assert_eq!(step.cursor.as_deref(), Some("c1"));

        let board = b.finish(42);
        assert_eq!(
            board.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Todo", "In Progress", "Done", "No status"]
        );
        assert_eq!(board.columns[1].cards[0].title, "Ship the board");
        assert_eq!(board.columns[3].cards[0].id, "i2");
        assert_eq!(board.fetched_at, 42);
    }

    #[test]
    fn unknown_status_becomes_its_own_column_instead_of_vanishing() {
        let json = r#"{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"id":"i1","content":{"__typename":"Issue","number":1,"title":"x"},
             "fieldValues":{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Blocked","field":{"name":"Status"}}]}}
        ]}}}"#;
        let mut b = BoardBuilder::new("P", "T", "Status", &["Todo".to_owned()]);
        b.push_page(&page(json));
        let board = b.finish(0);
        assert_eq!(board.columns.len(), 2);
        assert_eq!(board.columns[1].name, "Blocked");
    }

    #[test]
    fn a_value_from_another_field_is_not_treated_as_status() {
        let json = r#"{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"id":"i1","content":{"__typename":"Issue","number":1,"title":"x"},
             "fieldValues":{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"High","field":{"name":"Priority"}}]}}
        ]}}}"#;
        let mut b = BoardBuilder::new("P", "T", "Status", &["Todo".to_owned()]);
        b.push_page(&page(json));
        let board = b.finish(0);
        assert_eq!(board.columns[1].name, "No status");
    }

    #[test]
    fn redacted_items_render_as_a_placeholder() {
        let json = r#"{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"id":"i9","content":{"__typename":"Redacted"},"fieldValues":null}
        ]}}}"#;
        let mut b = BoardBuilder::new("P", "T", "Status", &[]);
        b.push_page(&page(json));
        let board = b.finish(0);
        assert_eq!(board.columns[0].cards[0].title, "Restricted item");
        assert_eq!(board.columns[0].cards[0].number, None);
    }

    #[test]
    fn long_titles_are_clipped_on_the_way_in() {
        let long = "x".repeat(400);
        let json = format!(
            r#"{{"node":{{"items":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[
                {{"id":"i1","content":{{"__typename":"DraftIssue","title":"{long}"}},"fieldValues":null}}
            ]}}}}}}"#
        );
        let mut b = BoardBuilder::new("P", "T", "Status", &[]);
        b.push_page(&page(&json));
        let board = b.finish(0);
        let title = &board.columns[0].cards[0].title;
        assert_eq!(title.chars().count(), TITLE_LIMIT);
        assert!(title.ends_with('…'));
    }

    fn page_of(count: usize) -> String {
        let nodes: Vec<String> = (0..count)
            .map(|i| {
                format!(
                    r#"{{"id":"i{i}","content":{{"__typename":"Issue","number":{i},"title":"t"}},"fieldValues":null}}"#
                )
            })
            .collect();
        format!(
            r#"{{"node":{{"items":{{"pageInfo":{{"hasNextPage":true,"endCursor":"c"}},"nodes":[{}]}}}}}}"#,
            nodes.join(",")
        )
    }

    #[test]
    fn the_cursor_is_forwarded_while_pages_remain() {
        let mut b = BoardBuilder::new("P", "T", "Status", &[]);
        let step = b.push_page(&page(&page_of(110)));
        assert!(step.has_next);
        assert_eq!(step.cursor.as_deref(), Some("c"));
        assert_eq!(b.seen(), 110);
    }

    #[test]
    fn pagination_stops_once_the_card_cap_is_reached() {
        let mut b = BoardBuilder::new("P", "T", "Status", &[]);
        let step = b.push_page(&page(&page_of(MAX_CARDS + 1)));
        assert!(!step.has_next, "a full builder must not request another page");
        assert!(b.is_full());

        let board = b.finish(0);
        assert_eq!(board.total_cards(), MAX_CARDS);
        assert!(board.truncated);
    }
}
