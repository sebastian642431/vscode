//! Installing extensions from `.vsix` packages, and Open VSX URLs.
//!
//! A `.vsix` is a ZIP whose extension files live under an `extension/`
//! directory alongside `extension.vsixmanifest`. [`install_vsix`] unzips it
//! (guarding against zip-slip path traversal) and returns the extension root
//! plus its parsed manifest, ready to hand to the [`host`](crate::host).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::manifest::ExtensionManifest;

/// An extension unpacked onto disk.
#[derive(Clone, Debug)]
pub struct InstalledExtension {
    /// The directory containing the extension's `package.json` (the VSIX
    /// `extension/` folder).
    pub root: PathBuf,
    pub manifest: ExtensionManifest,
}

/// Extract `vsix_path` into `dest_dir` and read its manifest.
pub fn install_vsix(vsix_path: &Path, dest_dir: &Path) -> io::Result<InstalledExtension> {
    let file = fs::File::open(vsix_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // `enclosed_name` rejects absolute paths and `..` traversal (zip-slip).
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest_dir.join(&rel);

        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)?;
        io::copy(&mut entry, &mut out)?;
    }

    let root = dest_dir.join("extension");
    let manifest_path = root.join("package.json");
    let manifest_json = fs::read_to_string(&manifest_path).map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("VSIX has no extension/package.json: {e}"),
        )
    })?;
    let manifest = ExtensionManifest::from_json(&manifest_json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(InstalledExtension { root, manifest })
}

/// Read an already-unpacked extension directory (containing `package.json`).
pub fn open_installed(root: &Path) -> io::Result<InstalledExtension> {
    let manifest_json = fs::read_to_string(root.join("package.json"))?;
    let manifest = ExtensionManifest::from_json(&manifest_json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(InstalledExtension {
        root: root.to_path_buf(),
        manifest,
    })
}

/// Open VSX (the open marketplace editor-rs targets) URL builders. The actual
/// download is left to the caller's HTTP client so this crate needs no network
/// stack; these produce the canonical registry URLs.
pub mod open_vsx {
    /// Metadata for the latest version: `GET` returns JSON with a
    /// `files.download` URL.
    pub fn latest_meta_url(namespace: &str, name: &str) -> String {
        format!("https://open-vsx.org/api/{namespace}/{name}/latest")
    }

    /// Metadata for a specific version.
    pub fn version_meta_url(namespace: &str, name: &str, version: &str) -> String {
        format!("https://open-vsx.org/api/{namespace}/{name}/{version}")
    }

    /// The direct `.vsix` download URL for a specific version.
    pub fn download_url(namespace: &str, name: &str, version: &str) -> String {
        format!(
            "https://open-vsx.org/api/{namespace}/{name}/{version}/file/{namespace}.{name}-{version}.vsix"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// Build a minimal valid VSIX in memory and write it to `path`.
    fn make_vsix(path: &Path, package_json: &str, main_js: &str) {
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip.start_file("extension/package.json", opts).unwrap();
        zip.write_all(package_json.as_bytes()).unwrap();
        zip.start_file("extension/out/extension.js", opts).unwrap();
        zip.write_all(main_js.as_bytes()).unwrap();
        zip.start_file("extension.vsixmanifest", opts).unwrap();
        zip.write_all(b"<xml/>").unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn installs_and_reads_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let vsix = dir.path().join("test.vsix");
        make_vsix(
            &vsix,
            r#"{"name":"demo","publisher":"acme","main":"./out/extension.js"}"#,
            "exports.activate = function(){};",
        );

        let dest = dir.path().join("unpacked");
        let installed = install_vsix(&vsix, &dest).unwrap();
        assert_eq!(installed.manifest.id(), "acme.demo");
        assert!(installed.root.join("package.json").exists());
        assert!(installed.root.join("out/extension.js").exists());
    }

    #[test]
    fn open_vsx_urls() {
        assert_eq!(
            open_vsx::download_url("rust-lang", "rust-analyzer", "0.3.0"),
            "https://open-vsx.org/api/rust-lang/rust-analyzer/0.3.0/file/rust-lang.rust-analyzer-0.3.0.vsix"
        );
        assert_eq!(
            open_vsx::latest_meta_url("ms-python", "python"),
            "https://open-vsx.org/api/ms-python/python/latest"
        );
    }
}
