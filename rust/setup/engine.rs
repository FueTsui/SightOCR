//! Installation, upgrade and removal.
//!
//! Upgrades replace files through a transaction: new files are extracted and
//! verified next to the installation first, then swapped in by renames. If
//! anything fails, every file is put back, so the previous SightOCR still
//! starts (an in-app updater relaunches it after a failed update).

use crate::env::{Env, UninstallKey};
use crate::payload::{relative_path, Package};
use crate::win::{self, same_path, Process};
use crate::{exit, logln, VERSION};
use anyhow::{anyhow, bail, ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const APP_EXE: &str = "SightOCR.exe";
const MCP_EXE: &str = "sightocr-mcp.exe";
const CLI_EXE: &str = "sightocr-cli.exe";
pub const UNINSTALLER: &str = "unins000.exe";
const ADMIN_MARKER: &str = "run-as-admin";
const SHORTCUT: &str = "SightOCR.lnk";
const URL: &str = "https://github.com/FueTsui/SightOCR";
/// Registration key names. The first is the spelling the earlier Inno Setup
/// packages actually wrote (`{{GUID}}` in the script); setup keeps using it
/// so upgrades replace that entry instead of adding a second one.
const KEY_NAMES: [&str; 2] = [
    "{B5261760-0D41-4798-9917-D5AA8C2510C8}}_is1",
    "{B5261760-0D41-4798-9917-D5AA8C2510C8}_is1",
];
/// Files earlier versions installed that are no longer shipped.
const OBSOLETE: &[&str] = &[
    "sightocr-cli.exe",
    "docs/CLI_MCP.md",
    "docs/RUST_REFACTOR.md",
    "docs/VALIDATION.md",
    "docs/UI_DESIGN.md",
];

// ------------------------------------------------------------------ errors

#[derive(Debug)]
pub struct Failure {
    pub code: i32,
    pub message: String,
}

impl Failure {
    fn new(code: i32, error: anyhow::Error) -> Self {
        let message = format!("{error:#}");
        logln!("FAILED ({code}): {message}");
        Self { code, message }
    }
}

// ------------------------------------------------------------------ survey

/// One registration of SightOCR in Windows' installed-apps list.
#[derive(Clone, Debug)]
pub struct Registration {
    pub key: UninstallKey,
    pub name: &'static str,
    pub dir: PathBuf,
    pub version: Option<String>,
    pub command: String,
}

#[derive(Clone, Debug)]
pub struct OldInstall {
    pub dir: PathBuf,
    pub uninstaller: Option<PathBuf>,
}

/// Every registered SightOCR installation, grouped by folder.
#[derive(Clone, Debug, Default)]
pub struct Survey {
    pub registrations: Vec<Registration>,
    pub installs: Vec<OldInstall>,
}

fn has_program(dir: &Path) -> bool {
    [APP_EXE, MCP_EXE, CLI_EXE]
        .iter()
        .any(|name| dir.join(name).is_file())
}

fn unquote(command: &str) -> PathBuf {
    PathBuf::from(command.trim().trim_matches('"'))
}

pub fn survey(env: &Env) -> Result<Survey> {
    let mut survey = Survey::default();
    for key in env.uninstall_keys() {
        for name in KEY_NAMES {
            let path = format!(r"{}\{name}", key.base);
            let Ok(Some(registry)) = win::open_key(key.hive, &path, key.view, false) else {
                continue;
            };
            let display = registry.get_string("DisplayName").unwrap_or_default();
            if display != "SightOCR" && !display.starts_with("SightOCR ") {
                continue;
            }
            let location = registry.get_string("InstallLocation").unwrap_or_default();
            ensure!(
                !location.trim().is_empty(),
                "旧版 SightOCR 安装登记不完整，请先从 Windows 设置卸载旧版。"
            );
            let dir = PathBuf::from(location.trim().trim_end_matches('\\'));
            ensure!(
                dir.is_absolute() && win::normalize(&dir).len() > 3,
                "旧版 SightOCR 安装目录无效：{}",
                dir.display()
            );
            let command = registry.get_string("UninstallString").unwrap_or_default();
            let uninstaller = unquote(&command);
            let valid = uninstaller
                .parent()
                .is_some_and(|parent| same_path(parent, &dir))
                && uninstaller
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| {
                        let lower = n.to_ascii_lowercase();
                        lower.starts_with("unins") && lower.ends_with(".exe")
                    })
                && uninstaller.is_file();
            logln!("Found registration {path} -> {}", dir.display());
            match survey
                .installs
                .iter_mut()
                .find(|install| same_path(&install.dir, &dir))
            {
                Some(install) => {
                    if install.uninstaller.is_none() && valid {
                        install.uninstaller = Some(uninstaller);
                    }
                }
                None => survey.installs.push(OldInstall {
                    dir: dir.clone(),
                    uninstaller: valid.then_some(uninstaller),
                }),
            }
            survey.registrations.push(Registration {
                key: key.clone(),
                name,
                dir,
                version: registry.get_string("DisplayVersion"),
                command,
            });
        }
    }
    Ok(survey)
}

/// Compare dotted numeric versions; missing parts count as 0.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |text: &str| -> Vec<u64> {
        text.trim_start_matches('v')
            .split('.')
            .map(|part| part.trim().parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parse(a), parse(b));
    for index in 0..a.len().max(b.len()) {
        let order = a.get(index).unwrap_or(&0).cmp(b.get(index).unwrap_or(&0));
        if order.is_ne() {
            return order;
        }
    }
    std::cmp::Ordering::Equal
}

fn highest<'a>(versions: impl Iterator<Item = &'a String>) -> Option<String> {
    versions.max_by(|a, b| compare_versions(a, b)).cloned()
}

impl Survey {
    pub fn highest_version(&self) -> Option<String> {
        highest(self.registrations.iter().filter_map(|r| r.version.as_ref()))
    }

    pub fn version_in(&self, dir: &Path) -> Option<String> {
        highest(
            self.registrations
                .iter()
                .filter(|r| same_path(&r.dir, dir))
                .filter_map(|r| r.version.as_ref()),
        )
    }

    /// The folder of the current installation, preferring the main registration.
    pub fn previous_dir(&self) -> Option<PathBuf> {
        self.registrations.first().map(|r| r.dir.clone())
    }

    /// Installations in other folders must be removable before setup continues.
    pub fn check_removable(&self, target: &Path) -> Result<()> {
        for install in &self.installs {
            if install.uninstaller.is_none()
                && !same_path(&install.dir, target)
                && has_program(&install.dir)
            {
                bail!(
                    "旧版 SightOCR 程序仍在，但没有可用的卸载程序：{}",
                    install.dir.display()
                );
            }
        }
        Ok(())
    }

    fn other_installs<'a>(&'a self, target: &'a Path) -> impl Iterator<Item = &'a OldInstall> + 'a {
        self.installs.iter().filter(move |install| {
            install.uninstaller.is_some() && !same_path(&install.dir, target)
        })
    }
}

/// Where to install: the requested folder, else the current installation,
/// else Program Files. Early releases' default folders move to Program Files:
/// always for in-app updates, and for normal installs when SightOCR is
/// registered there.
pub fn resolve_target(
    env: &Env,
    survey: &Survey,
    requested: Option<&Path>,
    update: bool,
) -> PathBuf {
    let dir = requested
        .map(Path::to_path_buf)
        .or_else(|| survey.previous_dir())
        .unwrap_or_else(|| env.default_dir());
    let legacy = env
        .legacy_dirs()
        .iter()
        .any(|legacy| same_path(legacy, &dir));
    let registered = survey
        .installs
        .iter()
        .any(|install| same_path(&install.dir, &dir));
    if legacy && (update || registered) {
        let moved = env.default_dir();
        logln!(
            "Moving legacy folder {} -> {}",
            dir.display(),
            moved.display()
        );
        return moved;
    }
    dir
}

/// Check a folder chosen in the window.
pub fn validate_target(dir: &Path) -> Result<PathBuf> {
    let text = dir.to_string_lossy();
    let trimmed = text.trim().trim_matches('"').trim_end_matches(['\\', '/']);
    let dir = PathBuf::from(trimmed);
    ensure!(
        dir.is_absolute()
            && win::normalize(&dir).len() > 3
            && !trimmed.contains(['*', '?', '<', '>', '|']),
        "请选择有效的安装位置（完整路径，且不能是磁盘根目录）"
    );
    Ok(dir)
}

// ------------------------------------------------------------------- tasks

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tasks {
    pub start_menu: bool,
    pub desktop: bool,
    pub autostart: bool,
    pub run_as_admin: bool,
}

impl Tasks {
    pub const DEFAULT: Tasks = Tasks {
        start_menu: true,
        desktop: false,
        autostart: false,
        run_as_admin: false,
    };

    /// `/TASKS=` names, as in the earlier Inno Setup package.
    pub fn from_names(names: &[String]) -> Self {
        let has = |name: &str| names.iter().any(|task| task == name);
        Tasks {
            start_menu: has("startmenu"),
            desktop: has("desktopicon"),
            autostart: has("autostart"),
            run_as_admin: has("runasadmin"),
        }
    }
}

fn autostart_command(dir: &Path) -> String {
    format!("\"{}\" --silent", dir.join(APP_EXE).display())
}

fn shortcut_points_to(lnk: &Path, dir: &Path) -> bool {
    win::shortcut_target(lnk).is_some_and(|target| same_path(&target, &dir.join(APP_EXE)))
}

/// The choices currently in effect for an installation in `dir`.
pub fn current_tasks(env: &Env, dir: &Path) -> Tasks {
    let any_shortcut = |dirs: Vec<PathBuf>| {
        dirs.iter()
            .any(|folder| shortcut_points_to(&folder.join(SHORTCUT), dir))
    };
    let autostart = win::open_key(win::Hive::CurrentUser, env.run_key(), 0, false)
        .ok()
        .flatten()
        .and_then(|key| key.get_string("SightOCR"))
        .is_some();
    Tasks {
        start_menu: any_shortcut(env.start_menu_dirs()),
        desktop: any_shortcut(env.desktop_dirs()),
        autostart,
        run_as_admin: dir.join(ADMIN_MARKER).is_file(),
    }
}

fn set_shortcut(folder: Option<&PathBuf>, dir: &Path, wanted: bool) -> Result<()> {
    let Some(folder) = folder else {
        return Ok(());
    };
    let lnk = folder.join(SHORTCUT);
    if wanted {
        win::create_shortcut(&lnk, &dir.join(APP_EXE), "截图识别与翻译")
            .with_context(|| format!("无法创建快捷方式 {}", lnk.display()))?;
    } else if shortcut_points_to(&lnk, dir) {
        std::fs::remove_file(&lnk)
            .with_context(|| format!("无法删除快捷方式 {}", lnk.display()))?;
    }
    Ok(())
}

fn set_autostart(env: &Env, dir: &Path, wanted: bool) -> Result<()> {
    let key = win::create_key(win::Hive::CurrentUser, env.run_key(), 0)
        .context("无法打开开机启动设置")?;
    if wanted {
        key.set_string("SightOCR", &autostart_command(dir))
    } else {
        key.delete_value("SightOCR")
    }
    .context("无法保存开机启动设置")
}

fn set_admin_marker(dir: &Path, wanted: bool) -> Result<()> {
    let marker = dir.join(ADMIN_MARKER);
    if wanted {
        std::fs::write(&marker, "1").context("无法保存管理员运行设置")
    } else if marker.exists() {
        std::fs::remove_file(&marker).context("无法清除管理员运行设置")
    } else {
        Ok(())
    }
}

fn apply_tasks(env: &Env, dir: &Path, tasks: Tasks) -> Vec<String> {
    let results = [
        set_admin_marker(dir, tasks.run_as_admin),
        set_autostart(env, dir, tasks.autostart),
        set_shortcut(env.start_menu_dirs().first(), dir, tasks.start_menu),
        set_shortcut(env.desktop_dirs().first(), dir, tasks.desktop),
    ];
    results
        .into_iter()
        .filter_map(|result| result.err())
        .map(|error| {
            logln!("Warning: {error:#}");
            format!("{error:#}")
        })
        .collect()
}

// --------------------------------------------------------------- processes

fn program_paths(dir: &Path, gui: bool) -> Vec<PathBuf> {
    let names: &[&str] = if gui {
        &[APP_EXE, MCP_EXE, CLI_EXE]
    } else {
        &[MCP_EXE, CLI_EXE]
    };
    names.iter().map(|name| dir.join(name)).collect()
}

fn targets(dirs: &[PathBuf], gui: bool) -> Vec<PathBuf> {
    dirs.iter()
        .flat_map(|dir| program_paths(dir, gui))
        .collect()
}

/// SightOCR programs from these folders that are running now.
pub fn running(dirs: &[PathBuf]) -> Result<Vec<Process>> {
    let wanted = targets(dirs, true);
    Process::find(|path| wanted.iter().any(|target| same_path(target, path))).map_err(Into::into)
}

/// Close running SightOCR programs in `dirs`. In-app updates refuse to stop a
/// running MCP server (its client would lose it silently) and wait up to two
/// minutes for SightOCR to finish; normal installs, already confirmed by the
/// user, wait five seconds and then end the program.
fn close_programs(dirs: &[PathBuf], update: bool, approved: bool) -> Result<()> {
    let processes = running(dirs)?;
    if processes.is_empty() {
        return Ok(());
    }
    let consoles = targets(dirs, false);
    if update
        && processes
            .iter()
            .any(|process| consoles.iter().any(|path| same_path(path, &process.path)))
    {
        bail!("此安装目录的 SightOCR MCP 正在运行。请在终端或 MCP 客户端停止服务后重试更新，尚未替换任何文件。");
    }
    ensure!(
        update || approved,
        "SightOCR 正在运行。请通过托盘菜单退出 SightOCR 后重试，尚未替换任何文件。"
    );
    for process in &processes {
        logln!(
            "Asking {} ({}) to exit",
            process.path.display(),
            process.pid
        );
        process.request_exit();
    }
    let deadline = std::time::Instant::now()
        + if update {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(5)
        };
    for process in &processes {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        process.wait(left.as_millis() as u32);
    }
    if !update {
        for process in processes.iter().filter(|process| !process.exited()) {
            logln!(
                "Ending {} ({}) after confirmation",
                process.path.display(),
                process.pid
            );
            process.terminate();
        }
        for process in &processes {
            process.wait(10_000);
        }
    }
    ensure!(
        processes.iter().all(Process::exited),
        "SightOCR 未能在规定时间内退出，尚未替换任何文件。请通过托盘菜单退出 SightOCR 后重试。"
    );
    Ok(())
}

// ----------------------------------------------------------- old versions

#[derive(Default)]
struct Carried {
    run_as_admin: bool,
    autostart: bool,
    start_menu: bool,
    desktop: bool,
}

/// Remove registered installations in other folders (moving to the new
/// folder), after backing up configuration kept in the old folder.
fn remove_other_installs(
    env: &Env,
    survey: &Survey,
    target: &Path,
    update: bool,
) -> Result<Carried> {
    let mut carried = Carried::default();
    let others: Vec<&OldInstall> = survey.other_installs(target).collect();
    if others.is_empty() {
        return Ok(carried);
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let run_value = win::open_key(win::Hive::CurrentUser, env.run_key(), 0, false)
        .ok()
        .flatten()
        .and_then(|key| key.get_string("SightOCR"));
    for (number, install) in others.iter().enumerate() {
        let dir = &install.dir;
        if update {
            carried.run_as_admin |= dir.join(ADMIN_MARKER).is_file();
            carried.autostart |= run_value
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&autostart_command(dir)));
            carried.start_menu |= env
                .start_menu_dirs()
                .iter()
                .any(|folder| shortcut_points_to(&folder.join(SHORTCUT), dir));
            carried.desktop |= env
                .desktop_dirs()
                .iter()
                .any(|folder| shortcut_points_to(&folder.join(SHORTCUT), dir));
        }
        let backup = env.backup_root().join(format!("{stamp}-{number}"));
        for name in ["config.json", "paths.json"] {
            let source = dir.join(name);
            if source.is_file() {
                std::fs::create_dir_all(&backup)
                    .and_then(|()| std::fs::copy(&source, backup.join(name)))
                    .map_err(|_| anyhow!("无法备份旧版配置，尚未开始卸载。"))?;
                logln!("Backed up {} to {}", source.display(), backup.display());
            }
        }
    }
    for install in &others {
        let uninstaller = install.uninstaller.as_ref().expect("filtered above");
        logln!("Uninstalling SightOCR in {}", install.dir.display());
        let status = std::process::Command::new(uninstaller)
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
            .status()
            .map_err(|_| anyhow!("无法启动旧版卸载程序，安装已停止。"))?;
        let code = status.code().unwrap_or(-1);
        ensure!(code == 0, "旧版卸载未成功（退出码 {code}），安装已停止。");
        ensure!(
            !has_program(&install.dir),
            "旧版程序文件仍然存在，安装已停止，请先完成旧版卸载。"
        );
    }
    // Clear entries the old uninstallers left behind, but only when they are
    // unchanged and their program is really gone.
    for registration in survey
        .registrations
        .iter()
        .filter(|registration| !same_path(&registration.dir, target))
    {
        let parent = &registration.key;
        let path = format!(r"{}\{}", parent.base, registration.name);
        let Ok(Some(key)) = win::open_key(parent.hive, &path, parent.view, false) else {
            continue;
        };
        let location = key.get_string("InstallLocation").unwrap_or_default();
        let command = key.get_string("UninstallString").unwrap_or_default();
        drop(key);
        ensure!(
            same_path(&PathBuf::from(location.trim()), &registration.dir)
                && command == registration.command
                && !has_program(&registration.dir),
            "旧版安装登记或程序文件在卸载期间发生变化，已停止清理。"
        );
        delete_registration(parent, registration.name)
            .map_err(|_| anyhow!("无法清除已卸载旧版的安装登记：{path}"))?;
    }
    Ok(carried)
}

fn delete_registration(parent: &UninstallKey, name: &str) -> Result<()> {
    if let Some(key) = win::open_key(parent.hive, &parent.base, parent.view, true)? {
        key.delete_tree(name)?;
    }
    Ok(())
}

// ------------------------------------------------------------- transaction

/// Files swapped into the installation folder, with what they replaced.
struct Transaction {
    staging: PathBuf,
    /// (installed path, backup of the previous file, new file placed)
    steps: Vec<(PathBuf, Option<PathBuf>, bool)>,
}

impl Transaction {
    fn new(staging: PathBuf) -> Self {
        Self {
            staging,
            steps: Vec::new(),
        }
    }

    fn backup_path(&self, relative: &Path) -> PathBuf {
        self.staging.join("old").join(relative)
    }

    fn set_aside(&self, path: &Path, relative: &Path) -> Result<Option<PathBuf>> {
        if !path.exists() {
            return Ok(None);
        }
        let backup = self.backup_path(relative);
        std::fs::create_dir_all(backup.parent().unwrap())?;
        std::fs::rename(path, &backup)
            .with_context(|| format!("无法替换 {}（文件可能正在使用）", path.display()))?;
        Ok(Some(backup))
    }

    /// Put `new` at `dir/relative`, keeping the previous file for rollback.
    fn replace(&mut self, dir: &Path, relative: &Path, new: &Path) -> Result<()> {
        let path = dir.join(relative);
        let backup = self.set_aside(&path, relative)?;
        self.steps.push((path.clone(), backup, false));
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::rename(new, &path).with_context(|| format!("无法写入 {}", path.display()))?;
        self.steps.last_mut().unwrap().2 = true;
        Ok(())
    }

    fn remove(&mut self, dir: &Path, relative: &Path) -> Result<()> {
        let path = dir.join(relative);
        if let Some(backup) = self.set_aside(&path, relative)? {
            self.steps.push((path, Some(backup), false));
        }
        Ok(())
    }

    fn rollback(&mut self) {
        logln!("Rolling back {} file changes", self.steps.len());
        for (path, backup, placed) in self.steps.drain(..).rev() {
            if placed {
                let _ = std::fs::remove_file(&path);
            }
            if let Some(backup) = backup {
                if let Err(error) = std::fs::rename(&backup, &path) {
                    logln!("Could not restore {}: {error}", path.display());
                }
            }
        }
    }
}

fn remove_tree_quietly(path: &Path) {
    if std::fs::remove_dir_all(path).is_err() && path.exists() {
        // An old program file may still be mapped by a process in another
        // session; remove what is left when Windows restarts.
        logln!("Leaving {} for removal at restart", path.display());
        if let Ok(entries) = walk_files(path) {
            for file in entries {
                win::delete_on_reboot(&file);
            }
        }
    }
}

fn walk_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            files.extend(walk_files(&entry.path())?);
        }
        files.push(entry.path());
    }
    Ok(files)
}

/// Files recorded by a previous uninstaller written by this setup.
fn files_recorded_by(uninstaller: &Path) -> Vec<String> {
    match Package::open(uninstaller) {
        Ok(Some(previous)) if previous.index.uninstaller => previous
            .index
            .files
            .into_iter()
            .map(|entry| entry.path)
            .collect(),
        _ => Vec::new(),
    }
}

/// Uninstall files left by the former Inno Setup package (`unins001.exe`, `.dat`, `.msg`).
fn inno_uninstall_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.len() == 12
                && lower.starts_with("unins")
                && lower[5..8].bytes().all(|byte| byte.is_ascii_digit())
                && [".exe", ".dat", ".msg"].contains(&&lower[8..])
                && lower != UNINSTALLER
        })
        .collect()
}

// ----------------------------------------------------------------- install

pub struct InstallPlan {
    pub dir: PathBuf,
    /// `None` keeps the installed choices (in-app update).
    pub tasks: Option<Tasks>,
    pub update: bool,
    /// The user agreed to close a running SightOCR.
    pub approved: bool,
}

pub struct Installed {
    pub dir: PathBuf,
    pub previous: Option<String>,
    pub warnings: Vec<String>,
}

pub type Progress<'a> = &'a (dyn Fn(f32, &str) + Sync);

pub fn install(
    env: &Env,
    package: &Package,
    plan: &InstallPlan,
    progress: Progress,
) -> Result<Installed, Failure> {
    let dir = &plan.dir;
    logln!("Installing SightOCR {VERSION} to {}", dir.display());
    progress(0.0, "正在校验安装包");
    package
        .verify(&mut |done, total| {
            progress(0.08 * done as f32 / total.max(1) as f32, "正在校验安装包")
        })
        .map_err(|error| Failure::new(exit::INIT_FAILED, error))?;
    ensure_index_version(package).map_err(|error| Failure::new(exit::INIT_FAILED, error))?;

    progress(0.08, "正在检查已安装的版本");
    let survey = survey(env).map_err(|error| Failure::new(exit::PREPARE_FAILED, error))?;
    survey
        .check_removable(dir)
        .map_err(|error| Failure::new(exit::PREPARE_FAILED, error))?;
    let previous = survey.version_in(dir);
    let mut dirs = vec![dir.clone()];
    dirs.extend(
        survey
            .other_installs(dir)
            .map(|install| install.dir.clone()),
    );
    progress(0.09, "正在关闭 SightOCR");
    close_programs(&dirs, plan.update, plan.approved)
        .map_err(|error| Failure::new(exit::PREPARE_FAILED, error))?;
    let carried = remove_other_installs(env, &survey, dir, plan.update)
        .map_err(|error| Failure::new(exit::PREPARE_FAILED, error))?;

    let fail = |error: anyhow::Error| Failure::new(exit::FATAL, error);
    std::fs::create_dir_all(dir)
        .with_context(|| format!("无法创建安装目录 {}", dir.display()))
        .map_err(fail)?;
    clear_stale_staging(dir);
    let staging = dir.join(format!(".setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(staging.join("new"))
        .with_context(|| format!("无法写入安装目录 {}", dir.display()))
        .map_err(fail)?;
    let extracted = (|| -> Result<()> {
        // 1. Extract and verify everything before touching installed files.
        let total = package.total_size().max(1) as f32;
        let done = std::cell::Cell::new(0u64);
        for entry in &package.index.files {
            let relative = relative_path(&entry.path)?;
            let label = format!("正在解压 {}", entry.path);
            package.extract(entry, &staging.join("new").join(&relative), &mut |bytes| {
                done.set(done.get() + bytes);
                progress(0.12 + 0.76 * done.get() as f32 / total, &label);
            })?;
        }
        package.write_uninstaller(&staging.join("new").join(UNINSTALLER))
    })();
    if let Err(error) = extracted {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(fail(error));
    }

    // 2. Swap files in, then register; undo every file change on failure.
    progress(0.9, "正在替换程序文件");
    let mut transaction = Transaction::new(staging.clone());
    let committed = (|| -> Result<()> {
        let mut shipped: Vec<String> = package
            .index
            .files
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        shipped.push(UNINSTALLER.into());
        for path in &shipped {
            let relative = relative_path(path)?;
            transaction.replace(dir, &relative, &staging.join("new").join(&relative))?;
        }
        let mut obsolete: Vec<String> = OBSOLETE.iter().map(|path| path.to_string()).collect();
        obsolete.extend(files_recorded_by(
            &transaction.backup_path(Path::new(UNINSTALLER)),
        ));
        obsolete.extend(inno_uninstall_files(dir));
        obsolete.retain(|path| !shipped.iter().any(|kept| kept.eq_ignore_ascii_case(path)));
        for path in obsolete {
            transaction.remove(dir, &relative_path(&path)?)?;
        }
        progress(0.96, "正在登记安装信息");
        register(env, dir, package)?;
        Ok(())
    })();
    if let Err(error) = committed {
        transaction.rollback();
        remove_tree_quietly(&staging);
        let mut failure = fail(error);
        failure.message.push_str("。已恢复原有文件。");
        return Err(failure);
    }
    remove_tree_quietly(&staging);
    remove_empty_dirs(dir, &["docs", "skills", r"resources\oneocr", "resources"]);
    let mut warnings = remove_duplicate_registrations(env, dir);

    // 3. Shortcuts and startup choices never undo a completed installation.
    progress(0.98, "正在应用安装选项");
    match plan.tasks {
        Some(tasks) => warnings.extend(apply_tasks(env, dir, tasks)),
        None => {
            if carried.run_as_admin {
                warnings.extend(set_admin_marker(dir, true).err().map(|e| format!("{e:#}")));
            }
            if carried.autostart {
                warnings.extend(
                    set_autostart(env, dir, true)
                        .err()
                        .map(|e| format!("{e:#}")),
                );
            }
            if carried.start_menu {
                warnings.extend(
                    set_shortcut(env.start_menu_dirs().first(), dir, true)
                        .err()
                        .map(|e| format!("{e:#}")),
                );
            }
            if carried.desktop {
                warnings.extend(
                    set_shortcut(env.desktop_dirs().first(), dir, true)
                        .err()
                        .map(|e| format!("{e:#}")),
                );
            }
        }
    }
    progress(1.0, "安装完成");
    logln!("Installed SightOCR {VERSION} in {}", dir.display());
    Ok(Installed {
        dir: dir.clone(),
        previous,
        warnings,
    })
}

fn ensure_index_version(package: &Package) -> Result<()> {
    ensure!(
        !package.index.uninstaller && package.index.version == VERSION,
        "安装包版本信息不一致，请重新下载 SightOCR 安装包"
    );
    Ok(())
}

fn clear_stale_staging(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".setup-") && entry.path().is_dir() {
                remove_tree_quietly(&entry.path());
            }
        }
    }
}

fn remove_empty_dirs(dir: &Path, relatives: &[&str]) {
    for relative in relatives {
        let path = dir.join(relative);
        if std::fs::read_dir(&path).is_ok_and(|mut entries| entries.next().is_none()) {
            let _ = std::fs::remove_dir(&path);
        }
    }
}

fn register(env: &Env, dir: &Path, package: &Package) -> Result<()> {
    let parent = env
        .uninstall_keys()
        .into_iter()
        .next()
        .expect("at least one key");
    let path = format!(r"{}\{}", parent.base, KEY_NAMES[0]);
    let key = win::create_key(parent.hive, &path, parent.view).context("无法写入安装登记")?;
    // Values written by the former Inno Setup package no longer apply.
    for name in key.value_names() {
        if name.starts_with("Inno Setup") {
            key.delete_value(&name)?;
        }
    }
    let uninstaller = dir.join(UNINSTALLER);
    let mut parts = VERSION
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let size_kb = (package.total_size() / 1024) as u32;
    let strings = [
        ("DisplayName", "SightOCR".to_owned()),
        ("DisplayVersion", VERSION.to_owned()),
        ("Publisher", "FueTsui".to_owned()),
        ("DisplayIcon", dir.join(APP_EXE).display().to_string()),
        ("InstallLocation", format!("{}\\", dir.display())),
        ("UninstallString", format!("\"{}\"", uninstaller.display())),
        (
            "QuietUninstallString",
            format!(
                "\"{}\" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART",
                uninstaller.display()
            ),
        ),
        ("URLInfoAbout", URL.to_owned()),
        ("HelpLink", URL.to_owned()),
        ("URLUpdateInfo", format!("{URL}/releases")),
        (
            "InstallDate",
            chrono::Local::now().format("%Y%m%d").to_string(),
        ),
    ];
    for (name, value) in strings {
        key.set_string(name, &value)?;
    }
    for (name, value) in [
        ("MajorVersion", parts.next().unwrap_or(0)),
        ("MinorVersion", parts.next().unwrap_or(0)),
        ("EstimatedSize", size_kb),
        ("NoModify", 1),
        ("NoRepair", 1),
    ] {
        key.set_dword(name, value)?;
    }
    logln!("Registered {path}");
    Ok(())
}

/// Remove other registrations of the same folder (another spelling or registry view).
fn remove_duplicate_registrations(env: &Env, dir: &Path) -> Vec<String> {
    let main = env
        .uninstall_keys()
        .into_iter()
        .next()
        .expect("at least one key");
    let mut warnings = Vec::new();
    for parent in env.uninstall_keys() {
        for name in KEY_NAMES {
            let is_main =
                parent.hive == main.hive && parent.view == main.view && name == KEY_NAMES[0];
            if is_main {
                continue;
            }
            let path = format!(r"{}\{name}", parent.base);
            let Ok(Some(key)) = win::open_key(parent.hive, &path, parent.view, false) else {
                continue;
            };
            let location = key.get_string("InstallLocation").unwrap_or_default();
            drop(key);
            if same_path(&PathBuf::from(location.trim()), dir) {
                if let Err(error) = delete_registration(&parent, name) {
                    warnings.push(format!("无法清理重复的安装登记 {path}：{error:#}"));
                }
            }
        }
    }
    warnings
}

/// Start SightOCR from `dir` (no window flags: it is a GUI program).
pub fn launch_app(dir: &Path) {
    let exe = dir.join(APP_EXE);
    logln!("Starting {}", exe.display());
    if let Err(error) = std::process::Command::new(&exe).current_dir(dir).spawn() {
        logln!("Could not start SightOCR: {error}");
    }
}

// --------------------------------------------------------------- uninstall

pub struct UninstallPlan {
    pub dir: PathBuf,
    pub remove_settings: bool,
    pub approved: bool,
}

pub fn uninstall(
    env: &Env,
    package: &Package,
    plan: &UninstallPlan,
    progress: Progress,
) -> Result<Vec<String>, Failure> {
    let dir = &plan.dir;
    logln!("Uninstalling SightOCR from {}", dir.display());
    progress(0.05, "正在关闭 SightOCR");
    close_programs(std::slice::from_ref(dir), false, plan.approved)
        .map_err(|error| Failure::new(exit::PREPARE_FAILED, error))?;

    clear_stale_staging(dir);
    let mut warnings = Vec::new();
    let mut files: Vec<String> = package
        .index
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    files.extend(OBSOLETE.iter().map(|path| path.to_string()));
    files.push(ADMIN_MARKER.into());
    let count = files.len().max(1) as f32;
    for (number, path) in files.iter().enumerate() {
        progress(
            0.1 + 0.7 * number as f32 / count,
            &format!("正在删除 {path}"),
        );
        let Ok(relative) = relative_path(path) else {
            continue;
        };
        let file = dir.join(relative);
        if file.is_file() && std::fs::remove_file(&file).is_err() {
            logln!("Removing {} at restart", file.display());
            win::delete_on_reboot(&file);
            warnings.push(format!("{} 正在使用，将在重启后删除", file.display()));
        }
    }
    let mut parents: Vec<PathBuf> = files
        .iter()
        .filter_map(|path| relative_path(path).ok())
        .flat_map(|relative| {
            relative
                .ancestors()
                .skip(1)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        })
        .filter(|relative| !relative.as_os_str().is_empty())
        .collect();
    parents.sort_by_key(|relative| std::cmp::Reverse(relative.components().count()));
    parents.dedup();
    for relative in parents {
        let _ = std::fs::remove_dir(dir.join(relative));
    }

    progress(0.85, "正在删除快捷方式和登记信息");
    for folder in env.start_menu_dirs().into_iter().chain(env.desktop_dirs()) {
        let lnk = folder.join(SHORTCUT);
        if shortcut_points_to(&lnk, dir) && std::fs::remove_file(&lnk).is_err() {
            warnings.push(format!("无法删除快捷方式 {}", lnk.display()));
        }
    }
    if let Ok(Some(key)) = win::open_key(win::Hive::CurrentUser, env.run_key(), 0, true) {
        if key
            .get_string("SightOCR")
            .is_some_and(|value| value.eq_ignore_ascii_case(&autostart_command(dir)))
        {
            let _ = key.delete_value("SightOCR");
        }
    }
    for parent in env.uninstall_keys() {
        for name in KEY_NAMES {
            let path = format!(r"{}\{name}", parent.base);
            let Ok(Some(key)) = win::open_key(parent.hive, &path, parent.view, false) else {
                continue;
            };
            let location = key.get_string("InstallLocation").unwrap_or_default();
            drop(key);
            if same_path(&PathBuf::from(location.trim()), dir) {
                if let Err(error) = delete_registration(&parent, name) {
                    warnings.push(format!("无法删除安装登记 {path}：{error:#}"));
                }
            }
        }
    }
    if plan.remove_settings {
        progress(0.92, "正在删除设置");
        for folder in env.user_data_dirs() {
            if folder.exists() {
                logln!("Removing settings {}", folder.display());
                if let Err(error) = std::fs::remove_dir_all(&folder) {
                    warnings.push(format!("无法删除 {}：{error}", folder.display()));
                }
            }
        }
    }
    schedule_self_removal(dir);
    progress(1.0, "卸载完成");
    logln!("Uninstalled SightOCR from {}", dir.display());
    Ok(warnings)
}

/// The uninstaller cannot delete itself while running: a copy in the temp
/// folder waits for it to exit, then removes it and the empty folder.
fn schedule_self_removal(dir: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let helper =
        std::env::temp_dir().join(format!("SightOCR-Uninstall-{}.exe", std::process::id()));
    let spawned = std::fs::copy(&exe, &helper).and_then(|_| {
        use std::os::windows::process::CommandExt;
        std::process::Command::new(&helper)
            .arg("--cleanup")
            .arg(dir)
            .creation_flags(0x0800_0000 | 0x0000_0008) // CREATE_NO_WINDOW | DETACHED_PROCESS
            .spawn()
    });
    if let Err(error) = spawned {
        logln!("Could not start the cleanup helper: {error}");
        win::delete_on_reboot(&dir.join(UNINSTALLER));
        win::delete_on_reboot(dir);
    }
}

/// Runs from the temp copy started by [`schedule_self_removal`].
pub fn cleanup_after_uninstall(dir: &Path) {
    let uninstaller = dir.join(UNINSTALLER);
    for _ in 0..240 {
        if !uninstaller.exists() || std::fs::remove_file(&uninstaller).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if uninstaller.exists() {
        win::delete_on_reboot(&uninstaller);
    }
    if std::fs::remove_dir(dir).is_err()
        && dir.exists()
        && std::fs::read_dir(dir).is_ok_and(|mut e| e.next().is_none())
    {
        win::delete_on_reboot(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        win::delete_on_reboot(&exe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("2.0.3", "2.1.0"), Ordering::Less);
        assert_eq!(compare_versions("2.10.0", "2.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("v2.1", "2.1.0"), Ordering::Equal);
    }

    #[test]
    fn inno_leftovers_are_recognised() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "unins000.exe",
            "unins001.exe",
            "unins001.dat",
            "unins001.msg",
            "uninstall.txt",
            "SightOCR.exe",
        ] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let mut found = inno_uninstall_files(dir.path());
        found.sort();
        assert_eq!(found, ["unins001.dat", "unins001.exe", "unins001.msg"]);
    }

    #[test]
    fn transaction_rolls_back_every_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("a.txt"), "old a").unwrap();
        std::fs::write(root.join("docs/old.md"), "old doc").unwrap();
        let staging = root.join(".setup-test");
        std::fs::create_dir_all(staging.join("new/docs")).unwrap();
        std::fs::write(staging.join("new/a.txt"), "new a").unwrap();
        std::fs::write(staging.join("new/docs/b.md"), "new b").unwrap();
        let mut transaction = Transaction::new(staging.clone());
        transaction
            .replace(root, Path::new("a.txt"), &staging.join("new/a.txt"))
            .unwrap();
        transaction
            .replace(
                root,
                Path::new(r"docs\b.md"),
                &staging.join("new/docs/b.md"),
            )
            .unwrap();
        transaction.remove(root, Path::new(r"docs\old.md")).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "new a"
        );
        assert!(!root.join("docs/old.md").exists());
        transaction.rollback();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "old a"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("docs/old.md")).unwrap(),
            "old doc"
        );
        assert!(!root.join("docs/b.md").exists());
    }

    #[test]
    fn task_names_match_the_former_package() {
        let tasks = Tasks::from_names(&["desktopicon".into(), "autostart".into()]);
        assert_eq!(
            tasks,
            Tasks {
                start_menu: false,
                desktop: true,
                autostart: true,
                run_as_admin: false
            }
        );
    }
}
