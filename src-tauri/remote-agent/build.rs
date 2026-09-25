//! Names the source the agent is built from: `MEWORK_AGENT_SOURCE_ID`, a digest
//! of everything that decides how an agent behaves — its manifest, this script,
//! every file under `src/`, and the shared Git crate its `git` helper runs. The
//! host links this crate as a library and so carries the same name; an agent
//! built from other source carries another, whatever version number either of
//! them claims (see `SOURCE_ID` in lib.rs).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let mut files = vec![PathBuf::from("Cargo.toml"), PathBuf::from("build.rs")];
    collect(&root, Path::new("src"), &mut files);
    // The Git helper (`mework-remote git`) is the shared crate's code: an agent
    // built from other Git source answers the same requests differently.
    files.push(PathBuf::from("../git-core/Cargo.toml"));
    collect(&root, Path::new("../git-core/src"), &mut files);
    files.sort();
    let mut hasher = Sha256::new();
    for relative in &files {
        let bytes = std::fs::read(root.join(relative)).expect("read the agent's source");
        // Line endings are how a checkout spells a file, not what it says: a
        // Windows clone that converted them builds the same agent.
        let text: Vec<u8> = bytes
            .iter()
            .enumerate()
            .filter(|(index, byte)| !(**byte == b'\r' && bytes.get(index + 1) == Some(&b'\n')))
            .map(|(_, byte)| *byte)
            .collect();
        let name = relative.to_string_lossy().replace('\\', "/");
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((text.len() as u64).to_le_bytes());
        hasher.update(&text);
    }
    println!("cargo:rustc-env=MEWORK_AGENT_SOURCE_ID={:x}", hasher.finalize());
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../git-core/Cargo.toml");
    println!("cargo:rerun-if-changed=../git-core/src");
}

/// The Rust files under `root/relative`, as paths relative to `root`. Dot files
/// are left out: they are an editor's or a file system's (macOS writes `._*`
/// beside every file it copies to a foreign volume), not the crate's.
fn collect(root: &Path, relative: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(root.join(relative)).expect("list the agent's source");
    for entry in entries {
        let entry = entry.expect("list the agent's source");
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = relative.join(&name);
        let kind = entry.file_type().expect("inspect the agent's source");
        if kind.is_dir() {
            collect(root, &path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}
