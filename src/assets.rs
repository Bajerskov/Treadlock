//! Asset library: what the game needs, where it lives, and what happens when it
//! is missing.
//!
//! Every asset is declared here whether or not the file exists yet. A missing
//! one is not an error: the subsystem that wanted it falls back to something
//! generated at runtime, and `report` prints the shortfall. That keeps the game
//! runnable while the art and audio are still being made, and turns "what do we
//! still need?" into a command rather than a memory exercise.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Sound,
    Music,
    Model,
    /// A flat image standing in for distant scenery.
    Plate,
}

impl Kind {
    fn parse(word: &str) -> Option<Kind> {
        match word {
            "sound" => Some(Kind::Sound),
            "music" => Some(Kind::Music),
            "model" => Some(Kind::Model),
            "plate" => Some(Kind::Plate),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Sound => "sound",
            Kind::Music => "music",
            Kind::Model => "model",
            Kind::Plate => "plate",
        }
    }
}

pub struct Entry {
    pub kind: Kind,
    pub path: PathBuf,
    pub present: bool,
    /// What the game does when the file is absent. Written down so the manifest
    /// doubles as a brief for whoever is making the asset.
    pub fallback: String,
}

pub struct Library {
    root: PathBuf,
    entries: HashMap<String, Entry>,
    order: Vec<String>,
}

/// The manifest that ships with the game. Declaring it in code rather than only
/// on disk means a missing or damaged manifest file cannot silently shrink the
/// asset list, and the defaults are always there to compare against.
const DEFAULT_MANIFEST: &str = include_str!("../assets/manifest.txt");

impl Library {
    pub fn load(root: impl AsRef<Path>) -> Library {
        let root = root.as_ref().to_path_buf();
        // Prefer a manifest on disk so assets can be added without rebuilding,
        // and fall back to the built-in copy.
        let text = std::fs::read_to_string(root.join("manifest.txt"))
            .unwrap_or_else(|_| DEFAULT_MANIFEST.to_string());

        let mut entries = HashMap::new();
        let mut order = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            // kind  id  relative/path  fallback description
            //
            // Columns are padded for readability, so runs of spaces have to
            // collapse. `splitn` on whitespace does not do that - it hands back
            // an empty field per extra space - so the three fixed fields are
            // taken with `split_whitespace` and the description is whatever is
            // left of the line after them.
            let mut parts = line.split_whitespace();
            let (Some(kind), Some(id), Some(path)) = (parts.next(), parts.next(), parts.next())
            else {
                eprintln!("assets: line {} is malformed, skipping", number + 1);
                continue;
            };
            let Some(kind) = Kind::parse(kind) else {
                eprintln!("assets: line {} has unknown kind {kind:?}", number + 1);
                continue;
            };
            let fallback = parts.collect::<Vec<&str>>().join(" ");
            let path = root.join(path);
            let present = path.is_file();

            order.push(id.to_string());
            entries.insert(id.to_string(), Entry { kind, path, present, fallback });
        }
        Library { root, entries, order }
    }

    // The lookup the tests check the manifest with, and the accessor a caller
    // needs to read an entry's fallback text.
    #[allow(dead_code)]
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// Path to an asset that is actually on disk. `None` means the caller should
    /// use its fallback.
    pub fn path(&self, id: &str) -> Option<&Path> {
        self.entries
            .get(id)
            .filter(|e| e.present)
            .map(|e| e.path.as_path())
    }

    pub fn missing(&self) -> Vec<&str> {
        self.order
            .iter()
            .filter(|id| self.entries[*id].present == false)
            .map(|id| id.as_str())
            .collect()
    }

    /// Print the state of the library. This is the "what still needs making"
    /// list, grouped so it can be handed to whoever is making it.
    pub fn report(&self, verbose: bool) {
        let total = self.order.len();
        let present = self.order.iter().filter(|id| self.entries[*id].present).count();
        println!(
            "assets: {present}/{total} present in {}",
            self.root.display()
        );
        if present == total {
            return;
        }

        for kind in [Kind::Sound, Kind::Music, Kind::Model, Kind::Plate] {
            let missing: Vec<&String> = self
                .order
                .iter()
                .filter(|id| {
                    let e = &self.entries[*id];
                    e.kind == kind && !e.present
                })
                .collect();
            if missing.is_empty() {
                continue;
            }
            println!("  {} missing ({}):", kind.label(), missing.len());
            for id in missing {
                let entry = &self.entries[id];
                if verbose {
                    println!(
                        "    {id:<18} {}\n      -> {}",
                        entry.path.display(),
                        if entry.fallback.is_empty() {
                            "no fallback; this one is simply absent"
                        } else {
                            &entry.fallback
                        }
                    );
                } else {
                    println!("    {id}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in manifest has to parse, and has to declare the assets the
    /// code actually asks for by name. A typo here is otherwise invisible until
    /// something silently falls back forever.
    #[test]
    fn built_in_manifest_declares_everything_the_game_asks_for() {
        let library = Library::load("assets");
        for id in [
            "engine_idle",
            "engine_drive",
            "tyre_screech",
            "whoosh_by",
            "boost_pickup",
            "boost_loop",
            "impact",
            "wind_ambient",
            "music_race",
            "sky_gradient",
            "backdrop_far",
            "prop_pylon",
        ] {
            assert!(
                library.get(id).is_some(),
                "manifest does not declare {id:?}, which the game requests by name"
            );
        }
    }

    /// Every declared asset needs a fallback described, or a missing file turns
    /// into a mystery rather than a known gap.
    #[test]
    fn every_entry_documents_its_fallback() {
        let library = Library::load("assets");
        assert!(!library.order.is_empty(), "manifest parsed as empty");
        for id in &library.order {
            let entry = &library.entries[id];
            assert!(
                !entry.fallback.is_empty(),
                "{id} has no fallback described in the manifest"
            );
        }
    }
}
