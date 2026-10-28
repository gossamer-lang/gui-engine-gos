use crate::errors;
use crate::paths;
use crate::style::color::Color;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Swatch {
    pub id: String,
    pub name: String,
    pub css: String,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorPalette {
    pub version: i64,
    pub next: i64,
    pub favorites: Vec<Swatch>,
    pub recent: Vec<String>,
}
impl Default for ColorPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorPalette {
    pub fn new() -> ColorPalette {
        ColorPalette {
            version: 1,
            next: 1,
            favorites: vec![],
            recent: vec![],
        }
    }
    pub fn decode(text: &str) -> Result<ColorPalette, errors::Error> {
        let palette = serde_json::from_str::<ColorPalette>(text)?;
        if palette.version != 1 || palette.favorites.len() > 128 || palette.recent.len() > 16 {
            return Err(errors::new("Unsupported or oversized color palette"));
        };
        let mut ids = HashSet::new();
        for s in &palette.favorites {
            if s.id.is_empty() || ids.contains(&s.id) || Color::parse_css(&s.css).is_none() {
                return Err(errors::new("Invalid named swatch"));
            };
            ids.insert(s.id.clone());
        }
        for css in &palette.recent {
            if Color::parse_css(css).is_none() {
                return Err(errors::new("Invalid recent swatch"));
            }
        }
        Ok(palette)
    }
    pub fn load(file: &str) -> Result<ColorPalette, errors::Error> {
        if !paths::exists(file) {
            return Ok(ColorPalette::new());
        };
        ColorPalette::decode(&super::bounded_io::read_text(file, 1024 * 1024).map_err(errors::new)?)
    }
    pub fn save(&self, file: &str) -> Result<(), errors::Error> {
        crate::services::write_atomic(file, &crate::services::bounded_json(self, 1024 * 1024)?)
    }
    pub fn remember(&mut self, color: Color) {
        let css = color.css();
        let mut next = vec![css.clone()];
        for item in &self.recent {
            if *item != css && next.len() < 16 {
                next.push(item.to_string())
            }
        }
        self.recent = next
    }
    /// Saves `color` as a favorite named `name` and answers its id; a color
    /// already saved keeps its place and takes the new name.
    pub fn add(&mut self, name: &str, color: Color) -> Result<String, errors::Error> {
        if name.trim().is_empty() {
            return Err(errors::new("Name the favorite color"));
        };
        let css = color.css();
        for i in 0..self.favorites.len() {
            if self.favorites[i].css == css {
                self.favorites[i].name = name.trim().to_string();
                return Ok(self.favorites[i].id.clone());
            }
        }
        if self.favorites.len() >= 128 {
            return Err(errors::new("The favorite colors limit is 128"));
        };
        let mut id = format!("color-{}", self.next);
        while self.favorites.iter().any(|s| s.id == id) {
            self.next += 1;
            id = format!("color-{}", self.next)
        }
        self.next += 1;
        self.favorites.push(Swatch {
            id: id.clone(),
            name: name.trim().to_string(),
            css,
        });
        Ok(id)
    }
    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        if name.trim().is_empty() {
            return false;
        };
        for i in 0..self.favorites.len() {
            if self.favorites[i].id == id {
                self.favorites[i].name = name.trim().to_string();
                return true;
            }
        }
        false
    }
    pub fn remove(&mut self, id: &str) {
        self.favorites = self
            .favorites
            .iter()
            .filter(|s| s.id != id)
            .cloned()
            .collect::<Vec<_>>()
    }
}
#[cfg(test)]
mod tests {
    use super::ColorPalette;
    use crate::style::color::Color;

    #[test]
    fn swatches_round_trip_alpha_and_bound_recents_independently() {
        let mut p = ColorPalette::new();
        let id = p.add("Translucent", Color::rgba(1, 2, 3, 4)).unwrap();
        assert!(p.rename(&id, "Renamed"));
        for i in 0..40 {
            p.remember(Color::rgb(i, 0, 0))
        }
        p.remember(Color::red());
        p.remember(Color::red());
        assert_eq!(p.recent.len(), 16);
        assert_eq!(p.recent[0], Color::red().css());
        let text = crate::services::to_json(&p);
        let mut restored = ColorPalette::decode(&text).unwrap();
        assert_eq!(restored.favorites[0].css, "#01020304");
        restored.remove(&id);
        assert!(restored.favorites.is_empty());
        assert_eq!(restored.recent.len(), 16)
    }
    #[test]
    fn adding_a_saved_color_again_renames_it_instead_of_duplicating_it() {
        let mut p = ColorPalette::new();
        let sky = p.add("Sky", Color::rgb(30, 120, 200)).unwrap();
        p.add("Amber", Color::rgb(224, 160, 48)).unwrap();
        assert_eq!(p.add("Ocean", Color::rgb(30, 120, 200)).unwrap(), sky);
        assert_eq!(p.favorites.len(), 2);
        assert_eq!(p.favorites[0].name, "Ocean")
    }
}
