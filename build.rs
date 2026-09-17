//! Build script: computes a stable SHA-256 over the embedded Swift runner
//! source tree (`drengr-runner/`) and exposes the first 12 hex chars as
//! `DRENGR_RUNNER_SHA` via `cargo:rustc-env`. This is one of the three
//! components of the build-cache key (see runbooks/v060-drengr-runner §8).

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    // Hash the SAME root that `include_dir!()` embeds in bootstrap.rs — single
    // source of truth. Editing project.yml or README.md changes embedded bytes
    // and must shift the cache key.
    let roots = [Path::new("drengr-runner").to_path_buf()];

    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        collect_files(root, &mut files);
    }
    files.sort();

    let mut hasher = Sha256::new();
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        // Hash the path so file renames/additions/removals shift the digest
        // even if the byte content is identical.
        hasher.update(file.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        match fs::read(file) {
            Ok(bytes) => hasher.update(&bytes),
            Err(e) => {
                // A transient read error here would silently desync the cache key
                // from reality — better to fail the build.
                panic!("build.rs: failed to read {}: {}", file.display(), e);
            }
        }
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", b);
    }
    let sha12: String = hex.chars().take(12).collect();
    println!("cargo:rustc-env=DRENGR_RUNNER_SHA={}", sha12);

    // Re-run if the roots themselves disappear/appear.
    for root in &roots {
        println!("cargo:rerun-if-changed={}", root.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // missing root — empty contribution
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => collect_files(&path, out),
            Ok(ft) if ft.is_file() => out.push(path),
            _ => {}
        }
    }
}
