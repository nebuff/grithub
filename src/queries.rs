//! GraphQL documents and their response shapes.
//!
//! Only what the board needs is selected: `__typename` on content is mandatory
//! because a draft issue has no number and a redacted item has no title at all.

use serde::Deserialize;

pub const PROJECTS: &str = r#"
query ViewerProjects {
  viewer {
    login
    projectsV2(first: 20) {
      nodes { id title number }
      pageInfo { hasNextPage endCursor }
    }
  }
  rateLimit { remaining resetAt }
}
"#;

pub const FIELDS: &str = r#"
query ProjectFields($id: ID!) {
  node(id: $id) {
    ... on ProjectV2 {
      title
      fields(first: 50) {
        nodes {
          __typename
          name
          ... on ProjectV2SingleSelectField { options { name } }
        }
      }
    }
  }
}
"#;

pub const ITEMS: &str = r#"
query ProjectItems($id: ID!, $first: Int!, $after: String) {
  node(id: $id) {
    ... on ProjectV2 {
      items(first: $first, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          content {
            __typename
            ... on Issue { number title }
            ... on PullRequest { number title }
            ... on DraftIssue { title }
          }
          fieldValues(first: 20) {
            nodes {
              __typename
              ... on ProjectV2ItemFieldSingleSelectValue {
                name
                field { ... on ProjectV2SingleSelectField { name } }
              }
            }
          }
        }
      }
    }
  }
}
"#;

#[derive(Debug, Deserialize)]
pub struct ProjectsData {
    pub viewer: Viewer,
}

#[derive(Debug, Deserialize)]
pub struct Viewer {
    pub login: String,
    #[serde(rename = "projectsV2")]
    pub projects: ProjectConnection,
}

#[derive(Debug, Deserialize)]
pub struct ProjectConnection {
    pub nodes: Vec<ProjectNode>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectNode {
    pub id: String,
    pub title: String,
    pub number: i64,
}

#[derive(Debug, Deserialize)]
pub struct FieldsData {
    pub node: Option<ProjectFieldNode>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectFieldNode {
    pub title: Option<String>,
    pub fields: FieldConnection,
}

#[derive(Debug, Deserialize)]
pub struct FieldConnection {
    pub nodes: Vec<FieldNode>,
}

#[derive(Debug, Deserialize)]
pub struct FieldNode {
    #[serde(rename = "__typename")]
    pub typename: String,
    pub name: String,
    #[serde(default)]
    pub options: Option<Vec<OptionNode>>,
}

#[derive(Debug, Deserialize)]
pub struct OptionNode {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemsData {
    pub node: Option<ItemsNode>,
}

#[derive(Debug, Deserialize)]
pub struct ItemsNode {
    pub items: ItemConnection,
}

#[derive(Debug, Deserialize)]
pub struct ItemConnection {
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
    pub nodes: Vec<ItemNode>,
}

#[derive(Debug, Deserialize)]
pub struct PageInfo {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ItemNode {
    pub id: String,
    pub content: Option<ContentNode>,
    #[serde(rename = "fieldValues")]
    pub field_values: Option<FieldValueConnection>,
}

#[derive(Debug, Deserialize)]
pub struct ContentNode {
    #[serde(rename = "__typename")]
    pub typename: Option<String>,
    pub title: Option<String>,
    pub number: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct FieldValueConnection {
    pub nodes: Vec<FieldValueNode>,
}

#[derive(Debug, Deserialize)]
pub struct FieldValueNode {
    #[serde(rename = "__typename")]
    pub typename: Option<String>,
    pub name: Option<String>,
    pub field: Option<FieldRef>,
}

#[derive(Debug, Deserialize)]
pub struct FieldRef {
    pub name: String,
}

/// Choose the field that becomes the board's columns: `Status` if present,
/// otherwise the first single-select field, otherwise nothing.
pub fn pick_board_field(nodes: &[FieldNode]) -> Option<(String, Vec<String>)> {
    let single_select: Vec<&FieldNode> = nodes
        .iter()
        .filter(|n| n.typename == "ProjectV2SingleSelectField")
        .collect();

    let chosen = single_select
        .iter()
        .find(|n| n.name.eq_ignore_ascii_case("status"))
        .or_else(|| single_select.first())?;

    let options = chosen
        .options
        .as_ref()
        .map(|o| o.iter().map(|x| x.name.clone()).collect())
        .unwrap_or_default();

    Some((chosen.name.clone(), options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(typename: &str, name: &str, options: &[&str]) -> FieldNode {
        FieldNode {
            typename: typename.to_owned(),
            name: name.to_owned(),
            options: if options.is_empty() {
                None
            } else {
                Some(
                    options
                        .iter()
                        .map(|n| OptionNode {
                            name: (*n).to_owned(),
                        })
                        .collect(),
                )
            },
        }
    }

    #[test]
    fn status_is_preferred_over_other_single_selects() {
        let nodes = vec![
            field("ProjectV2Field", "Title", &[]),
            field("ProjectV2SingleSelectField", "Priority", &["High"]),
            field(
                "ProjectV2SingleSelectField",
                "Status",
                &["Todo", "In Progress", "Done"],
            ),
        ];
        let (name, options) = pick_board_field(&nodes).unwrap();
        assert_eq!(name, "Status");
        assert_eq!(options, vec!["Todo", "In Progress", "Done"]);
    }

    #[test]
    fn falls_back_to_the_first_single_select_then_to_nothing() {
        let nodes = vec![field("ProjectV2SingleSelectField", "Sprint", &["1"])];
        assert_eq!(pick_board_field(&nodes).unwrap().0, "Sprint");

        let only_text = vec![field("ProjectV2Field", "Notes", &[])];
        assert!(pick_board_field(&only_text).is_none());
    }

    #[test]
    fn items_response_parses_drafts_and_redacted_items() {
        let json = r#"{"node":{"items":{"pageInfo":{"hasNextPage":true,"endCursor":"c1"},
            "nodes":[
              {"id":"i1","content":{"__typename":"Issue","number":12,"title":"Fix the flux capacitor"},
               "fieldValues":{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"In Progress","field":{"name":"Status"}}]}},
              {"id":"i2","content":{"__typename":"DraftIssue","title":"Write the changelog"},"fieldValues":{"nodes":[]}},
              {"id":"i3","content":{"__typename":"Redacted"},"fieldValues":null}
            ]}}}"#;
        let data: ItemsData = serde_json::from_str(json).unwrap();
        let items = data.node.unwrap().items;
        assert!(items.page_info.has_next_page);
        assert_eq!(items.page_info.end_cursor.as_deref(), Some("c1"));
        assert_eq!(items.nodes[0].content.as_ref().unwrap().number, Some(12));
        assert_eq!(items.nodes[1].content.as_ref().unwrap().number, None);
        assert!(items.nodes[2].content.as_ref().unwrap().title.is_none());
        assert!(items.nodes[2].field_values.is_none());
    }
}
