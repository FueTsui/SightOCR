//! Files appended to the setup program.
//!
//! Layout: `[setup program][file data...][index JSON][footer]`. The footer
//! locates the index and carries the SHA-256 of everything after the program,
//! so a damaged download is detected before anything is installed. Each file
//! also carries its own size and SHA-256, checked as it is extracted.
//! The installed uninstaller uses the same layout with an index only.

use anyhow::{anyhow, bail, ensure, Context, Result};
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

const MAGIC: [u8; 16] = *b"SightOCR-Setup\0\x01";
const FOOTER_LEN: u64 = 16 + 8 * 3 + 32;
const MAX_INDEX_LEN: u64 = 4 * 1024 * 1024;
const PRODUCT: &str = "SightOCR";

/// Files every package must contain; `skills/**` and `docs/*.md` are added
/// from the package folder as found.
const REQUIRED: &[&str] = &[
    "SightOCR.exe",
    "sightocr-mcp.exe",
    "README.md",
    "LICENSE",
    "resources/oneocr/oneocr.dll",
    "resources/oneocr/onnxruntime.dll",
    "resources/oneocr/oneocr.onemodel",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub product: String,
    pub version: String,
    pub uninstaller: bool,
    pub files: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Relative path with `/` separators.
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// Offset from the start of the appended data.
    pub offset: u64,
    /// Bytes stored in the package (compressed size when `deflate`).
    pub stored: u64,
    pub deflate: bool,
}

pub struct Package {
    pub exe: PathBuf,
    /// Length of the setup program itself (where the appended data starts).
    pub stub_len: u64,
    index_end: u64,
    hash: [u8; 32],
    pub index: Index,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A package path as a safe relative path below the installation folder.
pub fn relative_path(path: &str) -> Result<PathBuf> {
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', ':', '\0'])
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && path.len() < 260,
        "安装包中的文件路径无效：{path}"
    );
    let relative = PathBuf::from(path.replace('/', "\\"));
    ensure!(
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "安装包中的文件路径无效：{path}"
    );
    Ok(relative)
}

struct Footer {
    stub_len: u64,
    index_offset: u64,
    index_len: u64,
    hash: [u8; 32],
}

fn read_footer(file: &mut File, len: u64) -> Result<Option<Footer>> {
    if len < FOOTER_LEN {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(len - FOOTER_LEN))?;
    let mut footer = [0u8; FOOTER_LEN as usize];
    file.read_exact(&mut footer)?;
    if footer[..16] != MAGIC {
        return Ok(None);
    }
    let number = |at: usize| u64::from_le_bytes(footer[at..at + 8].try_into().unwrap());
    let (stub_len, index_offset, index_len) = (number(16), number(24), number(32));
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&footer[40..72]);
    Ok(Some(Footer {
        stub_len,
        index_offset,
        index_len,
        hash,
    }))
}

impl Package {
    /// Read the appended index. `None` when nothing is appended.
    pub fn open(exe: &Path) -> Result<Option<Self>> {
        let mut file = File::open(exe).context("无法读取安装程序")?;
        let len = file.metadata()?.len();
        let Some(Footer {
            stub_len,
            index_offset,
            index_len,
            hash,
        }) = read_footer(&mut file, len)?
        else {
            return Ok(None);
        };
        let index_end = stub_len
            .checked_add(index_offset)
            .and_then(|start| start.checked_add(index_len))
            .ok_or_else(|| anyhow!("安装包结构无效"))?;
        ensure!(
            stub_len >= 64 && index_len <= MAX_INDEX_LEN && index_end == len - FOOTER_LEN,
            "安装包结构无效"
        );
        file.seek(SeekFrom::Start(stub_len + index_offset))?;
        let mut bytes = vec![0; index_len as usize];
        file.read_exact(&mut bytes)?;
        let index: Index = serde_json::from_slice(&bytes).context("安装包目录无效")?;
        ensure!(index.product == PRODUCT, "这不是 SightOCR 安装包");
        for entry in &index.files {
            relative_path(&entry.path)?;
            ensure!(
                index.uninstaller
                    || entry
                        .offset
                        .checked_add(entry.stored)
                        .is_some_and(|end| end <= index_offset),
                "安装包结构无效"
            );
        }
        Ok(Some(Self {
            exe: exe.to_owned(),
            stub_len,
            index_end,
            hash,
            index,
        }))
    }

    pub fn total_size(&self) -> u64 {
        self.index.files.iter().map(|entry| entry.size).sum()
    }

    /// Check the SHA-256 of all appended data.
    pub fn verify(&self, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
        let mut file = File::open(&self.exe).context("无法读取安装程序")?;
        file.seek(SeekFrom::Start(self.stub_len))?;
        let total = self.index_end - self.stub_len;
        let mut reader = file.take(total);
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 1 << 20];
        let mut done = 0;
        loop {
            let read = reader.read(&mut buffer).context("无法读取安装包")?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            done += read as u64;
            progress(done, total);
        }
        ensure!(
            done == total && digest.finalize().as_slice() == self.hash,
            "安装包已损坏（校验失败），请重新下载 SightOCR 安装包"
        );
        Ok(())
    }

    /// Write one file to `destination`, checking its size and SHA-256.
    pub fn extract(
        &self,
        entry: &Entry,
        destination: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        ensure!(!self.index.uninstaller, "卸载程序中不包含程序文件");
        let mut file = File::open(&self.exe).context("无法读取安装程序")?;
        file.seek(SeekFrom::Start(self.stub_len + entry.offset))?;
        let stored = file.take(entry.stored);
        let mut reader: Box<dyn Read> = if entry.deflate {
            Box::new(DeflateDecoder::new(stored))
        } else {
            Box::new(stored)
        };
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = BufWriter::new(
            File::create(destination)
                .with_context(|| format!("无法写入 {}", destination.display()))?,
        );
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 1 << 20];
        let mut written = 0u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .with_context(|| format!("安装包中的 {} 已损坏", entry.path))?;
            if read == 0 {
                break;
            }
            written += read as u64;
            ensure!(written <= entry.size, "安装包中的 {} 大小不符", entry.path);
            digest.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .with_context(|| format!("无法写入 {}，请检查磁盘空间", destination.display()))?;
            progress(read as u64);
        }
        output
            .flush()
            .with_context(|| format!("无法写入 {}，请检查磁盘空间", destination.display()))?;
        ensure!(
            written == entry.size && hex(&digest.finalize()) == entry.sha256,
            "安装包中的 {} 校验失败",
            entry.path
        );
        Ok(())
    }

    /// Write the uninstaller: this program followed by the file list only.
    pub fn write_uninstaller(&self, destination: &Path) -> Result<()> {
        let mut index = self.index.clone();
        index.uninstaller = true;
        for entry in &mut index.files {
            entry.offset = 0;
            entry.stored = 0;
            entry.deflate = false;
        }
        let mut output = BufWriter::new(File::create(destination)?);
        let mut source = File::open(&self.exe)?.take(self.stub_len);
        let copied = std::io::copy(&mut source, &mut output)?;
        ensure!(copied == self.stub_len, "无法复制卸载程序");
        let bytes = serde_json::to_vec(&index)?;
        output.write_all(&bytes)?;
        output.write_all(&footer(
            self.stub_len,
            0,
            bytes.len() as u64,
            &Sha256::digest(&bytes),
        ))?;
        output.flush()?;
        Ok(())
    }
}

fn footer(stub_len: u64, index_offset: u64, index_len: u64, hash: &[u8]) -> Vec<u8> {
    let mut footer = Vec::with_capacity(FOOTER_LEN as usize);
    footer.extend_from_slice(&MAGIC);
    footer.extend_from_slice(&stub_len.to_le_bytes());
    footer.extend_from_slice(&index_offset.to_le_bytes());
    footer.extend_from_slice(&index_len.to_le_bytes());
    footer.extend_from_slice(hash);
    footer
}

/// Files of a package folder, as package paths.
pub fn package_files(source: &Path) -> Result<Vec<String>> {
    let mut files: Vec<String> = REQUIRED.iter().map(|path| path.to_string()).collect();
    for path in &files {
        ensure!(
            source.join(relative_path(path)?).is_file(),
            "打包目录缺少 {path}"
        );
    }
    let docs = source.join("docs");
    if docs.is_dir() {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&docs)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_file() && name.to_ascii_lowercase().ends_with(".md") {
                names.push(format!("docs/{name}"));
            }
        }
        names.sort();
        files.extend(names);
    }
    fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(root, &path, files)?;
            } else {
                let relative = path
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(relative);
            }
        }
        Ok(())
    }
    if source.join("skills").is_dir() {
        walk(source, &source.join("skills"), &mut files)?;
    }
    for path in &files {
        relative_path(path)?;
    }
    Ok(files)
}

/// Build a setup package: `program` (its own appended data, if any, is
/// dropped) followed by the files of `source`.
pub fn pack(program: &Path, source: &Path, output: &Path, version: &str) -> Result<()> {
    let program_len = match Package::open(program)? {
        Some(package) => package.stub_len,
        None => std::fs::metadata(program)?.len(),
    };
    let files = package_files(source)?;
    let temporary = output.with_extension("partial");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = BufWriter::new(File::create(&temporary)?);
    std::io::copy(&mut File::open(program)?.take(program_len), &mut out)?;
    let mut digest = Sha256::new();
    let mut offset = 0u64;
    let mut entries = Vec::new();
    for path in files {
        let bytes = std::fs::read(source.join(relative_path(&path)?))
            .with_context(|| format!("无法读取 {path}"))?;
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&bytes)?;
        let compressed = encoder.finish()?;
        let deflate = (compressed.len() as u64) * 100 < (bytes.len() as u64) * 97;
        let stored: &[u8] = if deflate { &compressed } else { &bytes };
        out.write_all(stored)?;
        digest.update(stored);
        entries.push(Entry {
            sha256: hex(&Sha256::digest(&bytes)),
            path,
            size: bytes.len() as u64,
            offset,
            stored: stored.len() as u64,
            deflate,
        });
        offset += stored.len() as u64;
    }
    let index = Index {
        product: PRODUCT.into(),
        version: version.into(),
        uninstaller: false,
        files: entries,
    };
    let bytes = serde_json::to_vec(&index)?;
    out.write_all(&bytes)?;
    digest.update(&bytes);
    out.write_all(&footer(
        program_len,
        offset,
        bytes.len() as u64,
        &digest.finalize(),
    ))?;
    out.flush()?;
    drop(out);
    if output.exists() {
        std::fs::remove_file(output)?;
    }
    std::fs::rename(&temporary, output)?;
    let check = Package::open(output)?.ok_or_else(|| anyhow!("生成的安装包无法读取"))?;
    check.verify(&mut |_, _| {})?;
    if check.index.files.is_empty() {
        bail!("安装包中没有文件");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_package_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for path in REQUIRED {
            let target = dir.path().join(relative_path(path).unwrap());
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, path.repeat(500)).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/USAGE.md"), "usage").unwrap();
        std::fs::write(dir.path().join("docs/notes.txt"), "not packaged").unwrap();
        std::fs::create_dir_all(dir.path().join("skills/sightocr")).unwrap();
        std::fs::write(dir.path().join("skills/sightocr/SKILL.md"), "skill").unwrap();
        dir
    }

    #[test]
    fn pack_extract_and_uninstaller_round_trip() {
        let source = fake_package_dir();
        let work = tempfile::tempdir().unwrap();
        let program = work.path().join("stub.exe");
        let mut stub = b"MZ".to_vec();
        stub.resize(4096, 7);
        std::fs::write(&program, &stub).unwrap();
        let output = work.path().join("SightOCR-Setup-9.9.9.exe");
        pack(&program, source.path(), &output, "9.9.9").unwrap();

        let package = Package::open(&output).unwrap().unwrap();
        assert_eq!(package.stub_len, 4096);
        assert_eq!(package.index.version, "9.9.9");
        let paths: Vec<_> = package
            .index
            .files
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert!(paths.contains(&"docs/USAGE.md") && paths.contains(&"skills/sightocr/SKILL.md"));
        assert!(!paths.contains(&"docs/notes.txt"));
        package.verify(&mut |_, _| {}).unwrap();
        for entry in &package.index.files {
            let target = work
                .path()
                .join("out")
                .join(relative_path(&entry.path).unwrap());
            package.extract(entry, &target, &mut |_| {}).unwrap();
            assert_eq!(
                std::fs::read(&target).unwrap(),
                std::fs::read(source.path().join(relative_path(&entry.path).unwrap())).unwrap()
            );
        }

        let uninstaller = work.path().join("unins000.exe");
        package.write_uninstaller(&uninstaller).unwrap();
        let reopened = Package::open(&uninstaller).unwrap().unwrap();
        assert!(reopened.index.uninstaller);
        assert_eq!(reopened.index.files.len(), package.index.files.len());
        reopened.verify(&mut |_, _| {}).unwrap();
        assert_eq!(&std::fs::read(&uninstaller).unwrap()[..4096], &stub[..]);

        // Packing from a package keeps only its program part.
        let again = work.path().join("again.exe");
        pack(&output, source.path(), &again, "9.9.9").unwrap();
        assert_eq!(Package::open(&again).unwrap().unwrap().stub_len, 4096);
    }

    #[test]
    fn damaged_packages_are_rejected() {
        let source = fake_package_dir();
        let work = tempfile::tempdir().unwrap();
        let program = work.path().join("stub.exe");
        std::fs::write(&program, vec![b'M'; 2048]).unwrap();
        let output = work.path().join("setup.exe");
        pack(&program, source.path(), &output, "1.0.0").unwrap();
        let mut bytes = std::fs::read(&output).unwrap();
        bytes[2100] ^= 0xff;
        std::fs::write(&output, &bytes).unwrap();
        let package = Package::open(&output).unwrap().unwrap();
        assert!(package.verify(&mut |_, _| {}).is_err());
        assert!(Package::open(&program).unwrap().is_none());
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        for path in ["", "/x", "a/../b", "a\\b", "C:/x", "./a", "a//b"] {
            assert!(relative_path(path).is_err(), "{path}");
        }
        assert_eq!(relative_path("a/b.txt").unwrap(), PathBuf::from(r"a\b.txt"));
    }
}
