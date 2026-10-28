// Persistent user places, independent of recent-location history. Missing paths
// remain in the book until the user relinks or removes them.
use crate::errors;
use crate::paths;
use std::collections::HashSet;
use std::fs;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Place {
    pub id: String,
    pub location: String,
    pub label: String,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlaceBook {
    pub version: i64,
    pub next: i64,
    pub entries: Vec<Place>,
}
#[cfg(target_os = "windows")]
fn identity(location: &str) -> String {
    fs::canonicalize(location)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| paths::normalize(location))
        .to_lowercase()
}
#[cfg(not(target_os = "windows"))]
fn identity(location: &str) -> String {
    fs::canonicalize(location)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| paths::normalize(location))
}
impl Default for PlaceBook {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaceBook {
    pub fn new() -> PlaceBook {
        PlaceBook {
            version: 1,
            next: 1,
            entries: vec![],
        }
    }
    pub fn decode(text: &str) -> Result<PlaceBook, errors::Error> {
        let book = serde_json::from_str::<PlaceBook>(text)?;
        if book.version != 1 || book.entries.len() > 128 {
            return Err(errors::new("Unsupported or oversized favorite locations"));
        };
        let mut ids = HashSet::new();
        for entry in &book.entries {
            if entry.id.is_empty() || ids.contains(&entry.id) {
                return Err(errors::new("Duplicate favorite identity"));
            };
            ids.insert(entry.id.clone());
        }
        Ok(book)
    }
    pub fn encode(&self) -> String {
        crate::services::to_json(&self)
    }
    pub fn add(&mut self, location: &str, label: &str) -> Result<String, errors::Error> {
        if location.is_empty() {
            return Err(errors::new("Enter a folder location"));
        };
        let wanted = identity(location);
        for entry in &self.entries {
            if identity(&entry.location) == wanted {
                return Ok(entry.id.clone());
            }
        }
        if self.entries.len() >= 128 {
            return Err(errors::new("The favorite locations limit is 128"));
        };
        let mut id = format!("place-{}", self.next);
        while self.entries.iter().any(|e| e.id == id) {
            self.next += 1;
            id = format!("place-{}", self.next)
        }
        self.next += 1;
        self.entries.push(Place {
            id: id.clone(),
            location: location.to_string(),
            label: if label.trim().is_empty() {
                paths::file_name(location).unwrap_or(location.to_string())
            } else {
                label.trim().to_string()
            },
        });
        Ok(id)
    }
    pub fn rename(&mut self, id: &str, label: &str) -> bool {
        if label.trim().is_empty() {
            return false;
        };
        for i in 0..self.entries.len() {
            if self.entries[i].id == id {
                self.entries[i].label = label.trim().to_string();
                return true;
            }
        }
        false
    }
    pub fn relink(&mut self, id: &str, location: &str) -> bool {
        if location.is_empty() {
            return false;
        };
        let wanted = identity(location);
        for entry in &self.entries {
            if entry.id != id && identity(&entry.location) == wanted {
                return false;
            }
        }
        for i in 0..self.entries.len() {
            if self.entries[i].id == id {
                self.entries[i].location = location.to_string();
                return true;
            }
        }
        false
    }
    pub fn remove(&mut self, id: &str) {
        self.entries = self
            .entries
            .iter()
            .filter(|e| e.id != id)
            .cloned()
            .collect::<Vec<_>>()
    }
    pub fn move_to(&mut self, id: &str, index: i64) -> bool {
        let Some(from) = self.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        let item = self.entries.remove(from);
        self.entries
            .insert((index.max(0).min(self.entries.len() as i64)) as usize, item);
        true
    }
}
#[cfg(test)]
mod tests {
    use super::PlaceBook;
    #[test]
    fn places_persist_identity_order_and_missing_locations() {
        let mut book = PlaceBook::new();
        let a = book.add("/missing/a", "A").unwrap();
        let b = book.add("/missing/b", "B").unwrap();
        assert_eq!(book.add("/missing/./a", "Alias").unwrap(), a);
        assert!(book.rename(&a, "Renamed"));
        assert!(book.move_to(&b, 0));
        assert!(book.relink(&a, "/missing/new"));
        let mut restored = PlaceBook::decode(&book.encode()).unwrap();
        assert_eq!(restored.entries[0].id, b);
        assert_eq!(restored.entries[1].label, "Renamed");
        restored.remove(&b);
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].location, "/missing/new")
    }
}
