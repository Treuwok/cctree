use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use cctree::i18n::{Msg, detect_lang, msg};

const USAGE: &str = "\
Usage: cctree [OPTIONS]

Options:
      --version  Print version and exit
      --help     Print this help and exit
";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    /// No arguments: run the interactive view.
    Run,
    Version,
    Help,
    /// Hidden developer flag: write an anonymised snapshot to the given file.
    DumpSnapshot(PathBuf),
}

/// Parses the arguments after the program name. `--dump-snapshot` is hidden:
/// it is accepted here but deliberately not listed in [`USAGE`].
fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut args = args.into_iter();
    let cli = match args.next().as_deref() {
        None => Cli::Run,
        Some("--version") => Cli::Version,
        Some("--help") => Cli::Help,
        Some("--dump-snapshot") => match args.next() {
            Some(path) => Cli::DumpSnapshot(PathBuf::from(path)),
            None => return Err("--dump-snapshot needs a file path".to_string()),
        },
        Some(other) => return Err(format!("unknown argument: {other}")),
    };
    match args.next() {
        None => Ok(cli),
        Some(extra) => Err(format!("unexpected argument: {extra}")),
    }
}

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)) {
        Ok(Cli::Run) => {
            let lang = detect_lang(&|name| std::env::var(name).ok(), windows_langid());
            if !std::io::stdout().is_terminal() {
                eprintln!("cctree: {}", msg(lang, &Msg::NotATty));
                return ExitCode::from(1);
            }
            match cctree::ui::app::run(lang) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cctree: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Cli::Version) => {
            println!("cctree {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Cli::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Cli::DumpSnapshot(path)) => match cctree::dump::write_dump(&path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "cctree: could not write the snapshot to {} ({e}). No snapshot was saved. \
                     Check that the folder exists and that you can write to it.",
                    path.display()
                );
                ExitCode::FAILURE
            }
        },
        Err(msg) => {
            eprintln!("cctree: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The Windows user interface language, for [`detect_lang`].
#[cfg(windows)]
fn windows_langid() -> Option<u16> {
    // SAFETY: takes no arguments and only reads the user's setting.
    Some(unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() })
}

#[cfg(not(windows))]
fn windows_langid() -> Option<u16> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        parse_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn known_arguments() {
        assert_eq!(parse(&[]), Ok(Cli::Run));
        assert_eq!(parse(&["--version"]), Ok(Cli::Version));
        assert_eq!(parse(&["--help"]), Ok(Cli::Help));
        assert_eq!(
            parse(&["--dump-snapshot", "/tmp/x.json"]),
            Ok(Cli::DumpSnapshot(PathBuf::from("/tmp/x.json")))
        );
    }

    #[test]
    fn unknown_or_incomplete_arguments_are_errors() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["-h"]).is_err());
        assert!(parse(&["--dump-snapshot"]).is_err());
        assert!(parse(&["--version", "--help"]).is_err());
        assert!(parse(&["--dump-snapshot", "a", "b"]).is_err());
    }

    #[test]
    fn usage_does_not_advertise_the_hidden_flag() {
        assert!(!USAGE.contains("dump"));
    }
}
