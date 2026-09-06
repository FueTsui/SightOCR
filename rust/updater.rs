//! Verified updates from the project's public GitHub release assets.
//!
//! Network work runs on the caller's background thread. No response body, URL,
//! proxy credentials, or raw network error is exposed in an error message.

use crate::config::ProxyConfig;
use anyhow::{anyhow, ensure, Result};
use reqwest::blocking::{Client, Response};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[cfg(test)]
mod tests;

const RELEASE_URL: &str = "https://api.github.com/repos/FueTsui/SightOCR/releases/latest";
const MAX_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 256 * 1024;
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024 * 1024;
const ATTEMPTS: usize = 3;
const HELPER_NAME: &str = "SightOCR-Updater.exe";
const MANIFEST_NAME: &str = "update.json";
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub enum UpdateProgress {
    Checking,
    Downloading {
        version: String,
        downloaded: u64,
        total: u64,
    },
    Verifying {
        version: String,
    },
}

#[derive(Debug)]
pub struct PreparedUpdate {
    // Drop the read lock before TempDir attempts to remove the staged installer.
    // On Windows this handle denies writes/deletes from verification to launch.
    _verified_file: File,
    directory: TempDir,
    version: String,
    expected_size: u64,
    expected_hash: [u8; 32],
}

impl PreparedUpdate {
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Start a temporary copy of this executable as an update supervisor, then
    /// allow the caller to exit normally. It reports installation failures even
    /// after the original UI has exited, and restores the previous app on error.
    pub fn launch(self, install_dir: &Path) -> Result<()> {
        ensure!(
            install_dir.is_absolute() && install_dir.is_dir(),
            "无法确定 SightOCR 的安装目录"
        );
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use std::process::{Command, Stdio};

            let helper = self.directory.path().join(HELPER_NAME);
            let current = std::env::current_exe().map_err(|_| anyhow!("无法找到更新辅助程序"))?;
            std::fs::copy(current, &helper)
                .map_err(|_| anyhow!("无法准备更新辅助程序，请检查临时目录和磁盘空间"))?;
            let manifest = UpdateManifest {
                version: self.version.clone(),
                install_dir: install_dir.to_owned(),
                size: self.expected_size,
                sha256: self
                    .expected_hash
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
                parent_pid: std::process::id(),
            };
            let manifest_path = self.directory.path().join(MANIFEST_NAME);
            let bytes =
                serde_json::to_vec(&manifest).map_err(|_| anyhow!("无法生成更新交接信息"))?;
            ensure!(bytes.len() as u64 <= MAX_MANIFEST_BYTES, "更新交接信息过长");
            std::fs::write(&manifest_path, bytes).map_err(|_| anyhow!("无法保存更新交接信息"))?;
            Command::new(helper)
                .arg("--apply-update")
                .arg(manifest_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; no shell is involved.
                .spawn()
                .map_err(|_| anyhow!("无法启动更新辅助程序，请重试或手动下载安装包"))?;
            // The helper reopens and verifies the package before using it. Keep
            // the files in the user's temp directory for the helper; failed
            // launches and other early returns remove the complete staging tree.
            drop(self._verified_file);
            let _ = self.directory.keep();
            Ok(())
        }
        #[cfg(not(windows))]
        anyhow::bail!("自动安装更新仅支持 Windows")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateManifest {
    version: String,
    install_dir: PathBuf,
    size: u64,
    sha256: String,
    parent_pid: u32,
}

fn read_manifest(path: &Path, helper: &Path) -> Result<(UpdateManifest, PathBuf, [u8; 32])> {
    ensure!(
        path.file_name().is_some_and(|name| name == MANIFEST_NAME)
            && helper.file_name().is_some_and(|name| name == HELPER_NAME),
        "更新交接程序或文件名无效"
    );
    let directory = helper
        .parent()
        .ok_or_else(|| anyhow!("更新辅助程序目录无效"))?
        .canonicalize()
        .map_err(|_| anyhow!("无法访问更新辅助程序目录"))?;
    let manifest_path = path
        .canonicalize()
        .map_err(|_| anyhow!("无法读取更新交接文件"))?;
    ensure!(
        manifest_path.parent() == Some(directory.as_path()),
        "更新交接文件不在辅助程序目录内"
    );
    let file = File::open(&manifest_path).map_err(|_| anyhow!("无法读取更新交接文件"))?;
    ensure!(
        file.metadata()?.len() <= MAX_MANIFEST_BYTES,
        "更新交接信息超过允许的大小"
    );
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("无法读取更新交接文件"))?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "更新交接信息超过允许的大小"
    );
    let manifest: UpdateManifest =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("更新交接信息无效"))?;
    numeric_version(&manifest.version)?;
    ensure!(
        !manifest.version.starts_with('v')
            && manifest.install_dir.is_absolute()
            && manifest.install_dir.is_dir()
            && manifest.parent_pid > 0
            && (68..=MAX_INSTALLER_BYTES).contains(&manifest.size),
        "更新交接信息中的版本、目录或文件大小无效"
    );
    let installer = directory.join(format!("SightOCR-Setup-{}.exe", manifest.version));
    let hash = parse_hash(&manifest.sha256)?;
    Ok((manifest, installer, hash))
}

fn apply_manifest(
    manifest_path: &Path,
    helper: &Path,
    wait_for_parent: impl FnOnce(&UpdateManifest) -> Result<()>,
    install: impl FnOnce(&Path, &Path) -> Result<i32>,
    restore: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let (manifest, installer, hash) = read_manifest(manifest_path, helper)?;
    // Wait in the helper rather than racing the installer's own shutdown timer.
    // The GUI's worker can need its entire 90-second HTTP timeout to unwind.
    wait_for_parent(&manifest)?;
    let result = (|| {
        let _verified = open_verified(&installer, manifest.size, &hash)?;
        let code = install(&installer, &manifest.install_dir)?;
        ensure!(code == 0, "安装程序未能完成更新（退出码 {code}）");
        Ok(())
    })();
    match result {
        Ok(()) => Ok(()), // The successful installer owns the single restart.
        Err(error) => match restore(&manifest.install_dir) {
            Ok(()) => Err(anyhow!("{error}。已重新启动 SightOCR，请稍后重试更新")),
            Err(_) => Err(anyhow!(
                "{error}。无法重新启动 SightOCR，请手动启动或重新下载安装包"
            )),
        },
    }
}

/// Internal entry point for the temporary update supervisor. Call before normal
/// argument parsing/single-instance acquisition, and display any returned error.
/// This never downloads anything: it revalidates the staged package, waits for
/// the original process, supervises Setup, and restores SightOCR if Setup fails.
pub fn apply_update(manifest_path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        let helper = std::env::current_exe().map_err(|_| anyhow!("无法确定更新辅助程序位置"))?;
        apply_manifest(
            manifest_path,
            &helper,
            wait_for_original_process,
            |installer, directory| {
                let mut directory_arg = OsString::from("/DIR=");
                directory_arg.push(directory);
                let mut log_arg = OsString::from("/LOG=");
                let log_path = installer
                    .parent()
                    .ok_or_else(|| anyhow!("更新安装包路径无效"))?
                    .join("installer.log");
                log_arg.push(&log_path);
                let status = Command::new(installer)
                    .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/UPDATE"])
                    .arg(directory_arg)
                    .arg(log_arg)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000)
                    .status()
                    .map_err(|_| anyhow!("无法运行更新安装程序"))?;
                let code = status.code().unwrap_or(-1);
                ensure!(
                    code == 0,
                    "安装程序未能完成更新（退出码 {code}）。日志：{}",
                    log_path.display()
                );
                Ok(code)
            },
            |directory| {
                Command::new(directory.join("SightOCR.exe"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000)
                    .spawn()
                    .map_err(|_| anyhow!("无法重新启动 SightOCR"))?;
                Ok(())
            },
        )
    }
    #[cfg(not(windows))]
    anyhow::bail!("自动安装更新仅支持 Windows")
}

#[cfg(windows)]
fn wait_for_original_process(manifest: &UpdateManifest) -> Result<()> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    ensure!(
        manifest.parent_pid != std::process::id(),
        "更新交接进程信息无效"
    );
    // SAFETY: Query/synchronize access only; the numeric PID is never terminated.
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            manifest.parent_pid,
        )
    };
    if process.is_null() {
        // SAFETY: Read immediately after the failed OpenProcess call.
        if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
            return Ok(()); // The original process exited before the helper started.
        }
        anyhow::bail!("无法确认 SightOCR 已退出，更新未开始，请稍后重试");
    }
    let result = (|| {
        // SAFETY: The process handle remains open through this query and wait.
        if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
            return Ok(());
        }
        let mut name = vec![0u16; 32768];
        let mut length = name.len() as u32;
        // SAFETY: name has writable storage for the supplied UTF-16 length.
        let queried =
            unsafe { QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut length) };
        if queried == 0 {
            ensure!(
                // SAFETY: The process handle is still owned and open.
                unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0,
                "无法确认 SightOCR 进程路径，更新未开始"
            );
            return Ok(());
        }
        let original = PathBuf::from(std::ffi::OsString::from_wide(&name[..length as usize]));
        let target = manifest.install_dir.join("SightOCR.exe");
        // A reused PID belonging to another executable must never delay an update.
        let original = original
            .canonicalize()
            .map_err(|_| anyhow!("无法确认原 SightOCR 进程路径，更新未开始"))?;
        let target = target
            .canonicalize()
            .map_err(|_| anyhow!("无法确认原 SightOCR 安装路径，更新未开始"))?;
        if original != target {
            return Ok(());
        }
        // Allow the longest HTTP request plus cleanup time.
        ensure!(
            // SAFETY: Wait only on the owned, verified original process handle.
            unsafe { WaitForSingleObject(process, 180_000) } == WAIT_OBJECT_0,
            "SightOCR 尚未完全退出，更新未开始；请退出后重新检查更新"
        );
        Ok(())
    })();
    // SAFETY: This is the single close of the owned OpenProcess handle.
    unsafe { CloseHandle(process) };
    result
}

#[derive(Debug)]
enum NetworkFailure {
    Connection,
    Status(u16),
}

impl fmt::Display for NetworkFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection => f.write_str("更新连接失败或下载中断，请检查网络和代理后重试"),
            Self::Status(403 | 429) => f.write_str("GitHub 暂时限制了更新请求，请稍后重试"),
            Self::Status(404) => f.write_str("尚未找到已发布的更新，请稍后重试"),
            Self::Status(status) => write!(f, "更新请求失败（HTTP {status}），请稍后重试"),
        }
    }
}

impl std::error::Error for NetworkFailure {}

fn retryable(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<NetworkFailure>(),
        Some(NetworkFailure::Connection | NetworkFailure::Status(408 | 500..=599))
    )
}

fn retry<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    for attempt in 0..ATTEMPTS {
        match operation() {
            Err(error) if attempt + 1 < ATTEMPTS && retryable(&error) => {
                #[cfg(not(test))]
                std::thread::sleep(Duration::from_millis(500 * (attempt as u64 + 1)));
            }
            result => return result,
        }
    }
    unreachable!("the final attempt always returns")
}

fn allowed_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
        && match url.host_str() {
            Some("api.github.com") => url.path() == "/repos/FueTsui/SightOCR/releases/latest",
            Some("github.com") => url
                .path()
                .starts_with("/FueTsui/SightOCR/releases/download/"),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => true,
            _ => false,
        }
}

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() < 5 && allowed_redirect(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("update redirect rejected")
        }
    })
}

fn build_client(proxy: &ProxyConfig) -> Result<Client> {
    crate::services::apply_proxy(
        Client::builder()
            .https_only(true)
            .redirect(redirect_policy())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .user_agent(concat!("SightOCR-Updater/", env!("CARGO_PKG_VERSION"))),
        proxy,
    )?
    .build()
    .map_err(|_| anyhow!("无法初始化更新网络连接或代理"))
}

fn request(client: &Client, url: &str, metadata: bool) -> Result<Response> {
    let mut request = client.get(url).header("Accept-Encoding", "identity");
    if metadata {
        request = request
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .timeout(Duration::from_secs(30));
    }
    let response = request.send().map_err(|_| NetworkFailure::Connection)?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        NetworkFailure::Status(response.status().as_u16())
    );
    Ok(response)
}

fn fetch_bounded(client: &Client, url: &str, metadata: bool, limit: u64) -> Result<Vec<u8>> {
    retry(|| {
        let response = request(client, url, metadata)?;
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= limit),
            "更新信息超过允许的大小"
        );
        let mut bytes = Vec::new();
        response
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| NetworkFailure::Connection)?;
        ensure!(bytes.len() as u64 <= limit, "更新信息超过允许的大小");
        Ok(bytes)
    })
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    state: String,
    size: u64,
    digest: Option<String>,
}

struct AvailableUpdate {
    version: String,
    installer: Asset,
    checksums: Option<Asset>,
    digest: Option<[u8; 32]>,
}

fn numeric_version(version: &str) -> Result<[u64; 3]> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let mut parts = version.split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        let part = parts.next().ok_or_else(|| anyhow!("更新版本号格式无效"))?;
        ensure!(
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0')),
            "更新版本号格式无效"
        );
        *number = part.parse().map_err(|_| anyhow!("更新版本号格式无效"))?;
    }
    ensure!(parts.next().is_none(), "更新版本号格式无效");
    Ok(numbers)
}

fn parse_hash(hash: &str) -> Result<[u8; 32]> {
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "更新包的 SHA256 校验信息无效"
    );
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hash[index * 2..index * 2 + 2], 16)
            .map_err(|_| anyhow!("更新包的 SHA256 校验信息无效"))?;
    }
    Ok(bytes)
}

fn asset_digest(asset: &Asset) -> Result<Option<[u8; 32]>> {
    match asset.digest.as_deref() {
        None | Some("") => Ok(None),
        Some(value) => parse_hash(
            value
                .strip_prefix("sha256:")
                .ok_or_else(|| anyhow!("更新包缺少受支持的 SHA256 校验信息"))?,
        )
        .map(Some),
    }
}

fn validate_asset(asset: &Asset, tag: &str, limit: u64) -> Result<()> {
    ensure!(
        asset.state == "uploaded",
        "更新文件尚未上传完成，请稍后重试"
    );
    ensure!(asset.size > 0 && asset.size <= limit, "更新文件大小无效");
    // Compare the entire canonical URL so a release cannot substitute another
    // repository, tag, filename, encoded path, query string, or HTTP mirror.
    let expected = format!(
        "https://github.com/FueTsui/SightOCR/releases/download/{tag}/{}",
        asset.name
    );
    ensure!(
        asset.browser_download_url == expected,
        "更新文件地址不是官方发布地址"
    );
    Ok(())
}

fn parse_release(bytes: &[u8], current_version: &str) -> Result<Option<AvailableUpdate>> {
    ensure!(
        bytes.len() as u64 <= MAX_METADATA_BYTES,
        "更新信息超过允许的大小"
    );
    let release: Release =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("无法解析 GitHub 更新信息"))?;
    ensure!(!release.draft && !release.prerelease, "该更新尚未正式发布");
    if numeric_version(&release.tag_name)? <= numeric_version(current_version)? {
        return Ok(None);
    }
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name)
        .to_owned();
    let filename = format!("SightOCR-Setup-{version}.exe");
    ensure!(
        release
            .assets
            .iter()
            .filter(|asset| asset.name == filename)
            .count()
            == 1,
        "该版本缺少匹配的 SightOCR 安装包"
    );
    ensure!(
        release
            .assets
            .iter()
            .filter(|asset| asset.name == "SHA256SUMS.txt")
            .count()
            <= 1,
        "更新校验文件不唯一"
    );
    let mut installer = None;
    let mut checksums = None;
    for asset in release.assets {
        if asset.name == filename {
            validate_asset(&asset, &release.tag_name, MAX_INSTALLER_BYTES)?;
            installer = Some(asset);
        } else if asset.name == "SHA256SUMS.txt" {
            checksums = Some(asset);
        }
    }
    let installer = installer.ok_or_else(|| anyhow!("该版本缺少匹配的 SightOCR 安装包"))?;
    let digest = asset_digest(&installer)?;
    if digest.is_none() {
        let checksums = checksums
            .as_ref()
            .ok_or_else(|| anyhow!("该版本没有可信的 SHA256 校验信息，无法自动安装"))?;
        validate_asset(checksums, &release.tag_name, MAX_CHECKSUM_BYTES)?;
    }
    Ok(Some(AvailableUpdate {
        version,
        installer,
        checksums,
        digest,
    }))
}

fn checksum_for(bytes: &[u8], filename: &str) -> Result<[u8; 32]> {
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("更新校验文件编码无效"))?;
    let mut found = None;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let mut columns = line.split_whitespace();
        let Some(hash) = columns.next() else { continue };
        let Some(name) = columns.next() else { continue };
        if name.strip_prefix('*').unwrap_or(name) != filename {
            continue;
        }
        ensure!(
            found.is_none() && columns.next().is_none(),
            "更新校验文件包含重复或无效记录"
        );
        found = Some(parse_hash(hash)?);
    }
    found.ok_or_else(|| anyhow!("校验文件中缺少当前更新安装包的 SHA256"))
}

fn stream_download(
    mut reader: impl Read,
    mut writer: impl Write,
    expected_size: u64,
    version: &str,
    notify: &impl Fn(UpdateProgress),
) -> Result<()> {
    let mut downloaded = 0;
    let mut last_notification = Instant::now();
    let mut buffer = [0; 64 * 1024];
    notify(UpdateProgress::Downloading {
        version: version.into(),
        downloaded,
        total: expected_size,
    });
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| NetworkFailure::Connection)?;
        if count == 0 {
            break;
        }
        downloaded += count as u64;
        ensure!(downloaded <= expected_size, "下载的更新包超过发布大小");
        writer
            .write_all(&buffer[..count])
            .map_err(|_| anyhow!("无法保存更新包，请检查临时目录和磁盘空间"))?;
        if downloaded == expected_size || last_notification.elapsed() >= Duration::from_millis(200)
        {
            notify(UpdateProgress::Downloading {
                version: version.into(),
                downloaded,
                total: expected_size,
            });
            last_notification = Instant::now();
        }
    }
    ensure!(downloaded == expected_size, NetworkFailure::Connection);
    writer
        .flush()
        .map_err(|_| anyhow!("无法保存完整的更新安装包"))?;
    Ok(())
}

fn open_verified(path: &Path, expected_size: u64, expected_hash: &[u8; 32]) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: keep the verified bytes immutable until launch.
    }
    let mut file = options
        .open(path)
        .map_err(|_| anyhow!("无法读取已下载的更新包"))?;
    ensure!(
        file.metadata()?.len() == expected_size,
        "更新安装包大小校验失败"
    );
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| anyhow!("无法校验更新安装包"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    ensure!(
        digest.finalize().as_slice() == expected_hash,
        "更新安装包 SHA256 校验失败，请重试"
    );
    // Refuse HTML/error pages and renamed archives even if a bad release happens
    // to provide their checksums. An installer must be a Windows PE executable.
    ensure!(expected_size >= 68, "更新安装包不是有效的 Windows 程序");
    file.rewind()?;
    let mut header = [0; 64];
    file.read_exact(&mut header)?;
    ensure!(&header[..2] == b"MZ", "更新安装包不是有效的 Windows 程序");
    let pe_offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as u64;
    ensure!(
        pe_offset >= 64 && pe_offset <= expected_size - 4,
        "更新安装包 PE 头无效"
    );
    file.seek(SeekFrom::Start(pe_offset))?;
    let mut signature = [0; 4];
    file.read_exact(&mut signature)?;
    ensure!(&signature == b"PE\0\0", "更新安装包 PE 头无效");
    Ok(file)
}

fn stage_download(
    update: &AvailableUpdate,
    expected_hash: &[u8; 32],
    notify: &impl Fn(UpdateProgress),
    download: impl FnOnce(&Path) -> Result<()>,
) -> Result<PreparedUpdate> {
    let directory = tempfile::Builder::new()
        .prefix("SightOCR-update-")
        .tempdir()
        .map_err(|_| anyhow!("无法创建更新临时目录"))?;
    let installer = directory.path().join(&update.installer.name);
    download(&installer)?;
    notify(UpdateProgress::Verifying {
        version: update.version.clone(),
    });
    let verified_file = open_verified(&installer, update.installer.size, expected_hash)?;
    Ok(PreparedUpdate {
        _verified_file: verified_file,
        directory,
        version: update.version.clone(),
        expected_size: update.installer.size,
        expected_hash: *expected_hash,
    })
}

/// Check stable versions numerically, download only the matching official setup,
/// and return an installer only after size, SHA256, and PE verification succeeds.
/// A missing checksum is a hard error; release notes are never an authority.
pub fn check_and_download(
    proxy: &ProxyConfig,
    notify: impl Fn(UpdateProgress),
) -> Result<Option<PreparedUpdate>> {
    notify(UpdateProgress::Checking);
    let client = build_client(proxy)?;
    let metadata = fetch_bounded(&client, RELEASE_URL, true, MAX_METADATA_BYTES)?;
    let Some(update) = parse_release(&metadata, env!("CARGO_PKG_VERSION"))? else {
        return Ok(None);
    };
    let expected_hash = match update.digest {
        Some(digest) => digest,
        None => {
            let asset = update
                .checksums
                .as_ref()
                .ok_or_else(|| anyhow!("缺少更新校验文件"))?;
            let bytes = fetch_bounded(
                &client,
                &asset.browser_download_url,
                false,
                MAX_CHECKSUM_BYTES,
            )?;
            ensure!(bytes.len() as u64 == asset.size, "更新校验文件大小不完整");
            if let Some(digest) = asset_digest(asset)? {
                ensure!(
                    Sha256::digest(&bytes).as_slice() == digest,
                    "更新校验文件 SHA256 不匹配"
                );
            }
            checksum_for(&bytes, &update.installer.name)?
        }
    };
    stage_download(&update, &expected_hash, &notify, |installer| {
        retry(|| {
            let response = request(&client, &update.installer.browser_download_url, false)?;
            ensure!(
                response
                    .content_length()
                    .is_none_or(|length| length == update.installer.size),
                "服务器返回的更新安装包大小与发布信息不一致"
            );
            let file = File::create(installer).map_err(|_| anyhow!("无法创建更新安装包"))?;
            stream_download(
                response,
                file,
                update.installer.size,
                &update.version,
                &notify,
            )
        })
    })
    .map(Some)
}
