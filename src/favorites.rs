//! Accounts you star as favorites, kept locally in ~/.config/kvidre/favorites.json.
//! Starring is free; only reading their posts is a paid call.

use crate::auth::config_dir;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Favorite {
    /// Searched by ID, so a renamed account stays a favorite.
    pub id: String,
    /// Last known handle, for display.
    pub username: String,
}

fn path() -> Result<PathBuf> {
    Ok(config_dir()?.join("favorites.json"))
}

/// The saved favorites, or none if nothing has been starred yet.
pub fn load() -> Result<Vec<Favorite>> {
    let path = path()?;
    match fs::read_to_string(&path) {
        Ok(raw) => {
            serde_json::from_str(&raw).with_context(|| format!("reading {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(favorites: &[Favorite]) -> Result<()> {
    let path = path()?;
    fs::write(&path, serde_json::to_string_pretty(favorites)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// Adds the account, or removes it if it's already a favorite. Returns true if added.
pub fn toggle(favorites: &mut Vec<Favorite>, id: &str, username: &str) -> bool {
    if let Some(i) = favorites.iter().position(|f| f.id == id) {
        favorites.remove(i);
        false
    } else {
        favorites.push(Favorite {
            id: id.into(),
            username: username.into(),
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_adds_then_removes() {
        let mut favorites = Vec::new();
        assert!(toggle(&mut favorites, "20", "yunta"));
        assert!(toggle(&mut favorites, "30", "quiet"));
        assert_eq!(favorites.len(), 2);
        // Matched by ID, even if the handle changed.
        assert!(!toggle(&mut favorites, "20", "renamed"));
        assert_eq!(favorites[0].username, "quiet");
    }
}
