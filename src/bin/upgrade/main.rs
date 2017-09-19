//! `cargo upgrade`
#![warn(missing_docs, missing_debug_implementations, missing_copy_implementations, trivial_casts,
       trivial_numeric_casts, unsafe_code, unstable_features, unused_import_braces,
       unused_qualifications)]

extern crate cargo_metadata;
extern crate docopt;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate serde_derive;
extern crate toml;

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::process;

extern crate cargo_edit;
use cargo_edit::{get_latest_dependency, Dependency, Manifest};

mod errors {
    error_chain!{
        links {
            CargoEditLib(::cargo_edit::Error, ::cargo_edit::ErrorKind);
        }

        foreign_links {
            // cargo-metadata doesn't (yet) export `ErrorKind`)
            Metadata(::cargo_metadata::Error);
        }
    }
}
use errors::*;

static USAGE: &'static str = r"
Upgrade all dependencies in a manifest file to the latest version.

Usage:
    cargo upgrade [--all] [--dependency <dep>...] [--manifest-path <path>] [options]
    cargo upgrade (-h | --help)
    cargo upgrade (-V | --version)

Options:
    --all                       Upgrade all packages in the workspace.
    -d --dependency <dep>       Specific dependency to upgrade. If this option is used, only the
                                specified dependencies will be upgraded.
    --manifest-path <path>      Path to the manifest to upgrade.
    --allow-prerelease          Include prerelease versions when fetching from crates.io (e.g.
                                '0.6.0-alpha'). Defaults to false.
    -h --help                   Show this help page.
    -V --version                Show version.

Dev, build, and all target dependencies will also be upgraded. Only dependencies from crates.io are
supported. Git/path dependencies will be ignored.

All packages in the workspace will be upgraded if the `--all` flag is supplied. The `--all` flag may
be supplied in the presence of a virtual manifest.
";

/// Docopts input args.
#[derive(Debug, Deserialize)]
struct Args {
    /// `--dependency -d <dep>`
    flag_dependency: Vec<String>,
    /// `--manifest-path <path>`
    flag_manifest_path: Option<String>,
    /// `--version`
    flag_version: bool,
    /// `--all`
    flag_all: bool,
    /// `--allow-prerelease`
    flag_allow_prerelease: bool,
}

fn is_version_dependency(dep: &toml::Value) -> bool {
    if let Some(table) = dep.as_table() {
        !table.contains_key("git") && !table.contains_key("path")
    } else {
        true
    }
}

fn update_manifest(
    manifest_path: &Option<String>,
    only_update: &[String],
    allow_prerelease: bool,
) -> Result<()> {
    let manifest_path = manifest_path.as_ref().map(From::from);
    let mut manifest = Manifest::open(&manifest_path)?;

    for (table_path, table) in manifest.get_sections() {
        for (name, old_value) in &table {
            if (only_update.is_empty() || only_update.contains(name)) &&
                is_version_dependency(old_value)
            {
                let latest_version = get_latest_dependency(name, allow_prerelease)?;

                manifest.update_table_entry(&table_path, &latest_version)?;
            }
        }
    }

    let mut file = Manifest::find_file(&manifest_path)?;
    manifest.write_to_file(&mut file)?;

    Ok(())
}

fn update_manifest_from_cache(
    manifest_path: &Option<String>,
    only_update: &[String],
    new_deps: &HashMap<String, Dependency>,
) -> Result<()> {
    let manifest_path = manifest_path.as_ref().map(From::from);
    let mut manifest = Manifest::open(&manifest_path)?;

    for (table_path, table) in manifest.get_sections() {
        for (name, old_value) in &table {
            if (only_update.is_empty() || only_update.contains(name)) &&
                is_version_dependency(old_value)
            {
                let latest_version = &new_deps[name];

                manifest.update_table_entry(&table_path, latest_version)?;
            }
        }
    }

    let mut file = Manifest::find_file(&manifest_path)?;
    manifest
        .write_to_file(&mut file)
        .chain_err(|| "Failed to write new manifest contents")
}

/// Get a list of the paths of all the (non-virtual) manifests in the workspace.
fn get_workspace_manifests(manifest_path: &Option<String>) -> Result<Vec<String>> {
    Ok(
        cargo_metadata::metadata_deps(manifest_path.as_ref().map(|p| Path::new(p)), true)
            .chain_err(|| "Failed to get metadata")?
            .packages
            .iter()
            .map(|p| p.manifest_path.clone())
            .collect(),
    )
}

/// Look up all current direct crates.io dependencies in the workspace. Then get the latest version
/// for each.
fn get_all_new_deps(
    manifest_path: &Option<String>,
    allow_prerelease: bool,
) -> Result<HashMap<String, Dependency>> {
    let mut new_deps = HashMap::new();

    cargo_metadata::metadata_deps(manifest_path.as_ref().map(|p| Path::new(p)), true)
        .chain_err(|| "Failed to get metadata")?
        .packages
        .iter()
        .flat_map(|package| package.dependencies.to_owned())
        .map(|dependency| {
            if !new_deps.contains_key(&dependency.name) {
                new_deps.insert(
                    dependency.name.clone(),
                    get_latest_dependency(&dependency.name, allow_prerelease)?,
                );
            }
            Ok(())
        })
        .collect::<Result<Vec<()>>>()?;

    Ok(new_deps)
}

/// Get the latest versions of the specified crates.io dependencies.
fn get_specified_new_deps(
    depedencies: &[String],
    allow_prerelease: bool,
) -> Result<HashMap<String, Dependency>> {
    depedencies
        .into_iter()
        .map(|dep| {
            Ok((
                dep.to_owned(),
                get_latest_dependency(dep, allow_prerelease)?,
            ))
        })
        .collect()
}

fn update_workspace_manifests(
    manifest_path: &Option<String>,
    only_update: &[String],
    allow_prerelease: bool,
) -> Result<()> {
    let new_deps = if !only_update.is_empty() {
        get_specified_new_deps(only_update, allow_prerelease)?
    } else {
        get_all_new_deps(manifest_path, allow_prerelease)?
    };

    get_workspace_manifests(manifest_path).and_then(|manifests| {
        for manifest in manifests {
            update_manifest_from_cache(&Some(manifest), only_update, &new_deps)?
        }

        Ok(())
    })
}

fn main() {
    let args = docopt::Docopt::new(USAGE)
        .and_then(|d| d.deserialize::<Args>())
        .unwrap_or_else(|err| err.exit());

    if args.flag_version {
        println!("cargo-upgrade version {}", env!("CARGO_PKG_VERSION"));
        process::exit(0);
    }

    let output = if args.flag_all {
        update_workspace_manifests(
            &args.flag_manifest_path,
            &args.flag_dependency,
            args.flag_allow_prerelease,
        )
    } else {
        update_manifest(
            &args.flag_manifest_path,
            &args.flag_dependency,
            args.flag_allow_prerelease,
        )
    };

    if let Err(err) = output {
        let mut stderr = io::stderr();

        writeln!(stderr, "Command failed due to unhandled error: {}\n", err).unwrap();

        for e in err.iter().skip(1) {
            writeln!(stderr, "Caused by: {}", e).unwrap();
        }

        if let Some(backtrace) = err.backtrace() {
            writeln!(stderr, "Backtrace: {:?}", backtrace).unwrap();
        }

        process::exit(1);
    }
}
