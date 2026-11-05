//! The Mac's menus (kinds 24, 25 and 27; MacMenu.swift). JSON only, every field optional.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MacMenuItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// "✓", "–" and so on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<String>,
    /// The shortcut as the Mac draws it ("⇧⌘S"). Text only, never a key command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submenu: Option<bool>,
}

/// A top level (`menus`), or the answer to one fetch (`menu`, `items`) or one press (`pressed`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MacMenu {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(rename = "bundleID", default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menus: Option<Vec<MacMenuItem>>,
    /// The device's token this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answering: Option<i64>,
    /// The id of the menu `items` belong to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<MacMenuItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Kind 27. Without an `id` it is the subscription to the top level.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FetchMenu {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The title the device showed; the host acts only while the item there still has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<i64>,
}

/// Kind 25.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PressMenuItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_json, encode_json};

    #[test]
    fn a_top_level_decodes_and_unknown_keys_are_ignored() {
        let j = br#"{"version":3,"app":"Notes","bundleID":"com.apple.Notes","menus":[{"id":"1","title":"File","submenu":true},{"id":"2","title":"Edit","enabled":false}],"x":1}"#;
        let m: MacMenu = decode_json(j).unwrap();
        assert_eq!((m.version, m.app.as_deref(), m.bundle_id.as_deref()), (Some(3), Some("Notes"), Some("com.apple.Notes")));
        assert_eq!(m.menus.as_ref().unwrap().len(), 2);
        assert_eq!(m.menus.unwrap()[1].enabled, Some(false));
    }

    #[test]
    fn requests_omit_nils() {
        assert_eq!(encode_json(&FetchMenu::default()), b"{}");
        let f = FetchMenu { version: Some(2), id: Some("1.4".into()), title: Some("Open Recent".into()), token: Some(7) };
        assert_eq!(encode_json(&f), br#"{"version":2,"id":"1.4","title":"Open Recent","token":7}"#);
        let p = PressMenuItem { version: Some(2), id: Some("1.0".into()), title: Some("New".into()), token: Some(8) };
        assert_eq!(encode_json(&p), br#"{"version":2,"id":"1.0","title":"New","token":8}"#);
    }
}
