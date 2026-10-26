//! The TSX mini apps, served as MCP resources.
//!
//! One resource per `*.tsx` in `crates/bir-shell/mini-apps/`, embedded at
//! build time (`build.rs`). `mini-apps/apps.json` gives each one its title
//! and description. `BIR_MINI_APPS_DIR` reads both from that directory on
//! disk instead, so the apps can be edited without a rebuild.

use std::path::PathBuf;

use serde_json::{Value, json};

/// `nsh://bir/<id>`.
pub(crate) const URI_PREFIX: &str = "nsh://bir/";
pub(crate) const MIME_TYPE: &str = "text/x-nsh-tsx";
pub(crate) const DIR_ENV: &str = "BIR_MINI_APPS_DIR";

/// The apps `bir_app_open` may name.
pub(crate) const OPENABLE: [&str; 4] = ["forms", "tax-profile", "register", "calendar"];

static EMBEDDED: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/mini_apps.rs"));
static EMBEDDED_MANIFEST: &str = include_str!("../../mini-apps/apps.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MiniApp {
    pub id: String,
    pub title: String,
    pub description: String,
    pub source: String,
}

impl MiniApp {
    pub(crate) fn uri(&self) -> String {
        format!("{URI_PREFIX}{}", self.id)
    }

    fn descriptor(&self) -> Value {
        json!({
            "uri": self.uri(),
            "name": self.id,
            "title": self.title,
            "description": self.description,
            "mimeType": MIME_TYPE,
        })
    }
}

/// Where the apps come from: the binary, or a directory on disk.
#[derive(Debug, Clone)]
pub(crate) enum Catalog {
    Embedded,
    Dir(PathBuf),
}

impl Catalog {
    pub(crate) fn from_env() -> Self {
        match std::env::var_os(DIR_ENV).filter(|dir| !dir.is_empty()) {
            Some(dir) => Self::Dir(PathBuf::from(dir)),
            None => Self::Embedded,
        }
    }

    pub(crate) fn apps(&self) -> Result<Vec<MiniApp>, String> {
        let (manifest, sources) = match self {
            Self::Embedded => (
                EMBEDDED_MANIFEST.to_string(),
                EMBEDDED
                    .iter()
                    .map(|(id, source)| (id.to_string(), source.to_string()))
                    .collect(),
            ),
            Self::Dir(dir) => read_dir(dir)?,
        };
        let manifest: Value = serde_json::from_str(&manifest)
            .map_err(|error| format!("mini-apps/apps.json: {error}"))?;
        let entry = |id: &str| {
            manifest
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry["id"] == id))
                .cloned()
                .unwrap_or(Value::Null)
        };
        Ok(sources
            .into_iter()
            .map(|(id, source)| {
                let meta = entry(&id);
                MiniApp {
                    title: meta["title"].as_str().unwrap_or(&id).to_string(),
                    description: meta["description"].as_str().unwrap_or("").to_string(),
                    id,
                    source,
                }
            })
            .collect())
    }

    pub(crate) fn find(&self, id: &str) -> Result<Option<MiniApp>, String> {
        Ok(self.apps()?.into_iter().find(|app| app.id == id))
    }

    /// `resources/list`.
    pub(crate) fn list(&self) -> Result<Value, String> {
        let apps = self.apps()?;
        Ok(json!({ "resources": apps.iter().map(MiniApp::descriptor).collect::<Vec<_>>() }))
    }

    /// `resources/read`.
    pub(crate) fn read(&self, uri: &str) -> Result<Value, String> {
        let id = uri
            .strip_prefix(URI_PREFIX)
            .ok_or_else(|| format!("unknown resource `{uri}`: mini apps are {URI_PREFIX}<id>"))?;
        let app = self
            .find(id)?
            .ok_or_else(|| format!("unknown resource `{uri}`"))?;
        Ok(json!({
            "contents": [{ "uri": app.uri(), "mimeType": MIME_TYPE, "text": app.source }],
        }))
    }
}

/// `apps.json` (may be missing: titles fall back to the id) and every `*.tsx`.
fn read_dir(dir: &std::path::Path) -> Result<(String, Vec<(String, String)>), String> {
    let manifest = std::fs::read_to_string(dir.join("apps.json")).unwrap_or_else(|_| "[]".into());
    let entries =
        std::fs::read_dir(dir).map_err(|error| format!("{DIR_ENV} {}: {error}", dir.display()))?;
    let mut sources = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().is_none_or(|ext| ext != "tsx") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        sources.push((id.to_string(), source));
    }
    sources.sort();
    Ok((manifest, sources))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_forms_app_lists_and_reads() {
        let catalog = Catalog::Embedded;
        let listed = catalog.list().unwrap();
        let forms = listed["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|resource| resource["uri"] == "nsh://bir/forms")
            .expect("forms is shipped")
            .clone();
        assert_eq!(forms["mimeType"], MIME_TYPE);
        assert_eq!(forms["name"], "forms");
        assert_eq!(forms["title"], "BIR Forms");
        assert!(
            forms["description"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
        let read = catalog.read("nsh://bir/forms").unwrap();
        let content = &read["contents"][0];
        assert_eq!(content["uri"], "nsh://bir/forms");
        assert_eq!(content["mimeType"], MIME_TYPE);
        assert!(content["text"].as_str().unwrap().contains("from \"nsh\""));
        assert!(catalog.read("nsh://bir/nope").is_err());
        assert!(catalog.read("file:///etc/passwd").is_err());
    }

    #[test]
    fn a_dev_dir_overrides_the_embedded_apps() {
        let dir = std::env::temp_dir().join(format!("bir-mini-apps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("register.tsx"), "export default 1;").unwrap();
        std::fs::write(dir.join("notes.txt"), "not an app").unwrap();
        std::fs::write(
            dir.join("apps.json"),
            r#"[{"id":"register","title":"Sign in","description":"d"}]"#,
        )
        .unwrap();
        let catalog = Catalog::Dir(dir.clone());
        let listed = catalog.list().unwrap();
        let resources = listed["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "nsh://bir/register");
        assert_eq!(resources[0]["title"], "Sign in");
        assert_eq!(
            catalog.read("nsh://bir/register").unwrap()["contents"][0]["text"],
            "export default 1;"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
