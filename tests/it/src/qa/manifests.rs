//! Keep every crate on the fork's single release-please-managed version.
//!
//! The fork releases all crates in lockstep: members inherit `[workspace.package] version`, and
//! dependencies on workspace crates are path-only, so a release bumps one line. Upstream releases
//! version each crate separately, so its manifests are normalized with `--fix` before merging.
use std::{collections::BTreeSet, fs, path::Path};

use gix_error::{OptionExt, Result, ResultExt, bail, message};
use toml_edit::{DocumentMut, InlineTable, Item, TableLike, value};

use super::ops::{git, output};

const RELEASE_MANIFEST: &str = ".release-please-manifest.json";

pub fn execute(root: &Path, args: &[String]) -> Result<()> {
    let (fix, version) = match args {
        [] => (false, None),
        [flag] if flag == "--fix" => (true, None),
        [flag, option, version] if flag == "--fix" && option == "--version" => (true, Some(version.as_str())),
        _ => bail!(message!("Usage: manifests [--fix [--version VERSION]], got {args:?}")),
    };
    let listing = output(git(root)?.args(["ls-files", "-z", "--", "Cargo.toml", "*/Cargo.toml"]))?;
    let mut manifests = Vec::new();
    for name in listing.split(|byte| *byte == 0).filter(|name| !name.is_empty()) {
        let name = std::str::from_utf8(name).or_raise(|| message("Manifest path is not UTF-8"))?;
        let text = fs::read_to_string(root.join(name)).or_raise(|| message!("Could not read {name}"))?;
        let document = text
            .parse::<DocumentMut>()
            .or_raise(|| message!("Could not parse {name}"))?;
        manifests.push((name.to_owned(), text, document));
    }
    let members = members(&manifests);
    let mut stale = Vec::new();
    for (name, text, mut document) in manifests {
        let kind = if name == "Cargo.toml" {
            Manifest::Root(version)
        } else if has_own_workspace(&document) {
            Manifest::Standalone
        } else {
            Manifest::Member
        };
        normalize(&mut document, &members, kind).or_raise(|| message!("Could not normalize {name}"))?;
        if matches!(kind, Manifest::Root(_)) {
            check_release_version(root, &document)?;
        }
        let normalized = document.to_string();
        if normalized != text {
            stale.push((name, normalized));
        }
    }
    if fix {
        // Write only after every manifest normalized, so a failure leaves the checkout untouched.
        for (name, normalized) in &stale {
            fs::write(root.join(name), normalized).or_raise(|| message!("Could not write {name}"))?;
            println!("normalized {name}");
        }
    } else if !stale.is_empty() {
        let stale: Vec<_> = stale.into_iter().map(|(name, _)| name).collect();
        bail!(message!(
            "These manifests don't use the fork's lockstep version; run `qa exec manifests --fix`:\n{}",
            stale.join("\n")
        ));
    }
    Ok(())
}

/// Workspace members are the tracked manifests without a `[workspace]` of their own, plus the root.
fn members(manifests: &[(String, String, DocumentMut)]) -> BTreeSet<String> {
    manifests
        .iter()
        .filter(|(name, _, document)| name == "Cargo.toml" || !has_own_workspace(document))
        .filter_map(|(_, _, document)| document.get("package")?.get("name")?.as_str().map(str::to_owned))
        .collect()
}

fn has_own_workspace(document: &DocumentMut) -> bool {
    document.contains_key("workspace")
}

#[derive(Clone, Copy)]
pub(super) enum Manifest<'a> {
    /// The workspace root, optionally setting the workspace version.
    Root(Option<&'a str>),
    /// A crate that inherits the workspace version.
    Member,
    /// A crate with its own `[workspace]`, such as a fuzz target, which keeps its own version.
    Standalone,
}

/// Rewrite one manifest in place.
pub(super) fn normalize(document: &mut DocumentMut, members: &BTreeSet<String>, kind: Manifest<'_>) -> Result<()> {
    if let Manifest::Root(version) = kind {
        let workspace = document
            .get_mut("workspace")
            .and_then(Item::as_table_like_mut)
            .ok_or_raise(|| message("The root manifest has no [workspace]"))?;
        let package = workspace
            .entry("package")
            .or_insert(toml_edit::table())
            .as_table_like_mut()
            .ok_or_raise(|| message("workspace.package must be a table"))?;
        match version {
            Some(version) => {
                package.insert("version", value(version));
            }
            None if package.contains_key("version") => {}
            None => bail!(message(
                "The workspace has no version yet; pass --fix --version VERSION"
            )),
        }
        if package.get("publish").and_then(Item::as_bool) != Some(false) {
            package.insert("publish", value(false));
        }
        if let Some(dependencies) = workspace.get_mut("dependencies").and_then(Item::as_table_like_mut) {
            strip_versions(dependencies, members);
        }
    }
    if !matches!(kind, Manifest::Standalone)
        && let Some(package) = document.get_mut("package").and_then(Item::as_table_like_mut)
    {
        for key in ["version", "publish"] {
            if !is_inherited(package.get(key)) {
                let mut inherited = InlineTable::new();
                inherited.insert("workspace", true.into());
                inherited.set_dotted(true);
                package.insert(key, value(inherited));
            }
        }
    }
    for (key, item) in document.iter_mut() {
        match key.get() {
            "dependencies" | "dev-dependencies" | "build-dependencies" => {
                if let Some(dependencies) = item.as_table_like_mut() {
                    strip_versions(dependencies, members);
                }
            }
            "target" => {
                let Some(targets) = item.as_table_like_mut() else {
                    continue;
                };
                for (_, target) in targets.iter_mut() {
                    let Some(target) = target.as_table_like_mut() else {
                        continue;
                    };
                    for (kind, dependencies) in target.iter_mut() {
                        if matches!(kind.get(), "dependencies" | "dev-dependencies" | "build-dependencies")
                            && let Some(dependencies) = dependencies.as_table_like_mut()
                        {
                            strip_versions(dependencies, members);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_inherited(item: Option<&Item>) -> bool {
    item.and_then(|item| item.get("workspace")).and_then(Item::as_bool) == Some(true)
}

/// Make dependencies on workspace crates path-only: their versions are the fork's, never upstream's.
fn strip_versions(dependencies: &mut dyn TableLike, members: &BTreeSet<String>) {
    for (name, dependency) in dependencies.iter_mut() {
        let Some(dependency) = dependency.as_table_like_mut() else {
            continue;
        };
        let package = dependency.get("package").and_then(Item::as_str).unwrap_or(name.get());
        if members.contains(package) && dependency.contains_key("path") {
            dependency.remove("version");
        }
    }
}

/// release-please bumps the workspace version and its own manifest together; they must agree.
fn check_release_version(root: &Path, document: &DocumentMut) -> Result<()> {
    let Ok(text) = fs::read_to_string(root.join(RELEASE_MANIFEST)) else {
        return Ok(());
    };
    let released: serde_json::Value =
        serde_json::from_str(&text).or_raise(|| message!("Could not parse {RELEASE_MANIFEST}"))?;
    let released = released["."]
        .as_str()
        .ok_or_raise(|| message!("{RELEASE_MANIFEST} has no version for the root package"))?;
    let workspace = document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(Item::as_str);
    if workspace != Some(released) {
        bail!(message!(
            "The workspace version {workspace:?} differs from {released} in {RELEASE_MANIFEST}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(text: &str, kind: Manifest<'_>) -> String {
        let members = ["gix-hash", "gix-object", "gix"].map(str::to_owned).into();
        let mut document = text.parse::<DocumentMut>().expect("test manifests parse");
        normalize(&mut document, &members, kind).expect("test manifests normalize");
        document.to_string()
    }

    #[test]
    fn members_inherit_the_version_and_depend_on_members_by_path_only() {
        let upstream = r#"[package]
name = "gix-object"
version = "0.66.1"
edition = "2024"

[dependencies]
gix-hash = { version = "^0.28.1", path = "../gix-hash" }
renamed = { package = "gix", version = "0.90", path = "../gix", optional = true }
registry-only = { package = "gix-hash", version = "0.28" }
serde = { version = "1", features = ["derive"] }

[dev-dependencies.gix-hash]
version = "^0.28.1"
path = "../gix-hash"

[target.'cfg(unix)'.build-dependencies]
gix-hash = { version = "0.28", path = "../gix-hash" }
"#;
        let fork = normalized(upstream, Manifest::Member);
        assert_eq!(
            fork,
            r#"[package]
name = "gix-object"
version.workspace = true
edition = "2024"
publish.workspace = true

[dependencies]
gix-hash = { path = "../gix-hash" }
renamed = { package = "gix", path = "../gix", optional = true }
registry-only = { package = "gix-hash", version = "0.28" }
serde = { version = "1", features = ["derive"] }

[dev-dependencies.gix-hash]
path = "../gix-hash"

[target.'cfg(unix)'.build-dependencies]
gix-hash = { path = "../gix-hash" }
"#,
            "member versions come from the workspace and member dependencies carry no version requirement"
        );
        assert_eq!(
            normalized(&fork, Manifest::Member),
            fork,
            "normalizing twice changes nothing"
        );
    }

    #[test]
    fn the_root_owns_the_version_and_standalone_crates_keep_theirs() {
        let root = "[workspace]\nmembers = [\"gix\"]\n\n[package]\nname = \"gitoxide\"\nversion = \"0.61.0\"\n";
        let fork = normalized(root, Manifest::Root(Some("0.0.0")));
        assert_eq!(
            fork,
            "[workspace]\nmembers = [\"gix\"]\n\n[workspace.package]\nversion = \"0.0.0\"\npublish = false\n\n[package]\nname = \"gitoxide\"\nversion.workspace = true\npublish.workspace = true\n",
            "the root declares the lockstep version and keeps every crate unpublishable"
        );
        assert_eq!(
            normalized(&fork, Manifest::Root(None)),
            fork,
            "an existing workspace version is kept"
        );

        let fuzz = "[package]\nname = \"gix-hash-fuzz\"\nversion = \"0.0.0\"\n\n[dependencies.gix-hash]\nversion = \"0.28\"\npath = \"..\"\n\n[workspace]\nmembers = [\".\"]\n";
        assert_eq!(
            normalized(fuzz, Manifest::Standalone),
            "[package]\nname = \"gix-hash-fuzz\"\nversion = \"0.0.0\"\n\n[dependencies.gix-hash]\npath = \"..\"\n\n[workspace]\nmembers = [\".\"]\n",
            "standalone crates keep their own version but depend on members by path only"
        );
    }

    #[test]
    fn a_new_workspace_needs_an_explicit_version() {
        let mut document = "[workspace]\nmembers = []\n"
            .parse::<DocumentMut>()
            .expect("valid TOML");
        let error =
            normalize(&mut document, &BTreeSet::new(), Manifest::Root(None)).expect_err("no version to inherit");
        assert!(
            error.to_string().contains("--version"),
            "the error says how to supply the version: {error}"
        );
    }
}
