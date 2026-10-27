//! Command-line switches. Public switches use Inno Setup's spelling and are
//! case-insensitive, because SightOCR 2.0.x updaters and existing scripts call
//! the setup program with them.

use anyhow::{bail, Result};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quiet {
    /// Full window: options, progress and result.
    Interactive,
    /// Progress window only (`/SILENT`).
    Silent,
    /// No window (`/VERYSILENT`).
    VerySilent,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub quiet: Quiet,
    pub suppress_msgboxes: bool,
    /// In-app update: keep the installed choices and restart SightOCR.
    pub update: bool,
    pub no_launch: bool,
    pub dir: Option<PathBuf>,
    /// `Some(None)` for `/LOG` without a file name.
    pub log: Option<Option<PathBuf>>,
    /// `/TASKS=` selection; listed tasks are selected, the rest are not.
    pub tasks: Option<Vec<String>>,
    pub elevated_child: bool,
    pub result_file: Option<PathBuf>,
    /// Public switches, passed on unchanged when elevating.
    pub forward: Vec<OsString>,
}

#[derive(Debug)]
pub enum Command {
    Pack { source: PathBuf, output: PathBuf },
    Cleanup { dir: PathBuf },
    Run(Options),
}

/// Value of `/NAME=value` when `argument` is that switch (case-insensitive).
fn switch_value(argument: &str, name: &str) -> Option<String> {
    let prefix_len = name.len() + 1;
    if argument.len() >= prefix_len
        && argument.is_char_boundary(prefix_len)
        && argument[..name.len()].eq_ignore_ascii_case(name)
        && argument.as_bytes()[name.len()] == b'='
    {
        Some(argument[prefix_len..].trim().trim_matches('"').to_owned())
    } else {
        None
    }
}

fn directory(value: &str, what: &str) -> Result<PathBuf> {
    let trimmed = value.trim_end_matches(['\\', '/']);
    let path = PathBuf::from(if trimmed.ends_with(':') {
        value
    } else {
        trimmed
    });
    if value.is_empty() || !path.is_absolute() {
        bail!("{what}必须是完整的绝对路径：{value}");
    }
    Ok(path)
}

pub fn parse(raw: &[OsString]) -> Result<Command> {
    let text: Vec<String> = raw
        .iter()
        .map(|argument| {
            argument
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("命令行参数包含无效字符"))
        })
        .collect::<Result<_>>()?;
    match text.first().map(String::as_str) {
        Some("--pack") if text.len() == 3 => {
            return Ok(Command::Pack {
                source: PathBuf::from(&text[1]),
                output: PathBuf::from(&text[2]),
            })
        }
        Some("--cleanup") if text.len() == 2 => {
            return Ok(Command::Cleanup {
                dir: directory(&text[1], "卸载目录")?,
            })
        }
        _ => {}
    }
    let mut options = Options {
        quiet: Quiet::Interactive,
        suppress_msgboxes: false,
        update: false,
        no_launch: false,
        dir: None,
        log: None,
        tasks: None,
        elevated_child: false,
        result_file: None,
        forward: Vec::new(),
    };
    for (argument, original) in text.iter().zip(raw) {
        let upper = argument.to_ascii_uppercase();
        if argument == "--elevated-child" {
            options.elevated_child = true;
            continue;
        }
        if let Some(value) = argument.strip_prefix("--result-file=") {
            options.result_file = Some(PathBuf::from(value));
            continue;
        }
        options.forward.push(original.clone());
        match upper.as_str() {
            "/SILENT" => {
                if options.quiet == Quiet::Interactive {
                    options.quiet = Quiet::Silent;
                }
            }
            "/VERYSILENT" => options.quiet = Quiet::VerySilent,
            "/SUPPRESSMSGBOXES" => options.suppress_msgboxes = true,
            "/UPDATE" => options.update = true,
            "/NOLAUNCH" => options.no_launch = true,
            "/LOG" => options.log = Some(None),
            // Accepted for compatibility; SightOCR never needs a restart.
            "/NORESTART" | "/SP-" | "/NOCANCEL" | "/NOICONS" => {}
            _ => {
                if let Some(value) = switch_value(argument, "/DIR") {
                    options.dir = Some(directory(&value, "安装目录")?);
                } else if let Some(value) = switch_value(argument, "/LOG") {
                    options.log = Some((!value.is_empty()).then(|| PathBuf::from(value)));
                } else if let Some(value) = switch_value(argument, "/TASKS") {
                    options.tasks = Some(
                        value
                            .split(',')
                            .map(|task| task.trim().to_ascii_lowercase())
                            .filter(|task| !task.is_empty())
                            .collect(),
                    );
                }
                // Other switches (for example /LANG=) are ignored, as before.
            }
        }
    }
    Ok(Command::Run(options))
}

/// Whether the raw command line asked for no message boxes; used only when
/// the command line itself could not be parsed.
pub fn quiet_requested(raw: &[OsString]) -> bool {
    raw.iter().any(|argument| {
        argument.to_str().is_some_and(|text| {
            text.eq_ignore_ascii_case("/VERYSILENT")
                || text.eq_ignore_ascii_case("/SUPPRESSMSGBOXES")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(arguments: &[&str]) -> Options {
        let raw: Vec<OsString> = arguments.iter().map(OsString::from).collect();
        match parse(&raw).unwrap() {
            Command::Run(options) => options,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn updater_command_line_from_2_0_3_is_understood() {
        let options = run(&[
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/UPDATE",
            r"/DIR=C:\Program Files\SightOCR",
            r"/LOG=C:\Users\a b\AppData\Local\Temp\SightOCR-update-x\installer.log",
        ]);
        assert_eq!(options.quiet, Quiet::VerySilent);
        assert!(options.suppress_msgboxes && options.update && !options.no_launch);
        assert_eq!(
            options.dir.unwrap(),
            PathBuf::from(r"C:\Program Files\SightOCR")
        );
        assert_eq!(
            options.log.unwrap().unwrap(),
            PathBuf::from(r"C:\Users\a b\AppData\Local\Temp\SightOCR-update-x\installer.log")
        );
        assert_eq!(options.forward.len(), 6);
    }

    #[test]
    fn switches_are_case_insensitive_and_internal_flags_are_not_forwarded() {
        let options = run(&[
            "/silent",
            "/update",
            "/nolaunch",
            r#"/dir="D:\Apps\SightOCR\""#,
            "/Tasks=desktopicon, AutoStart",
            "--elevated-child",
            r"--result-file=C:\Temp\SightOCR-Setup-1.result",
        ]);
        assert_eq!(options.quiet, Quiet::Silent);
        assert!(options.update && options.no_launch && options.elevated_child);
        assert_eq!(options.dir.unwrap(), PathBuf::from(r"D:\Apps\SightOCR"));
        assert_eq!(options.tasks.unwrap(), vec!["desktopicon", "autostart"]);
        assert_eq!(options.forward.len(), 5);
        assert!(options.result_file.is_some());
    }

    #[test]
    fn relative_or_empty_directories_are_rejected() {
        for argument in ["/DIR=SightOCR", "/DIR=", r"/DIR=..\x"] {
            assert!(parse(&[OsString::from(argument)]).is_err(), "{argument}");
        }
        assert_eq!(
            run(&[r"/DIR=D:\"]).dir.unwrap(),
            PathBuf::from(r"D:\"),
            "a drive root stays a root (it is rejected later as a destination)"
        );
    }
}
