//! Virus scans of finished downloads.
//!
//! The scanner runs as a program with separate arguments, never through a
//! shell. A custom command line is split into arguments first and `{path}` is
//! replaced afterwards, inside each argument, so a file name can neither add
//! arguments nor run a command, whatever characters it holds.
//!
//! Exit codes: Defender's `MpCmdRun.exe` returns 0 for no threat and 2 for a
//! threat. A custom scanner follows the common convention (ClamAV's among
//! others): 0 clean, 1 threat found. Anything else is a failed scan.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::Scan;
use crate::settings::Scanner;

/// The placeholder replaced by the file to scan.
pub const PATH_TOKEN: &str = "{path}";
/// A scan that runs longer than this is stopped and counts as failed.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_millis(200);
/// Keep this much of the scanner's output as the result's detail.
const DETAIL_CHARS: usize = 300;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Split a command line into the program and its arguments. Whitespace
/// separates arguments; double quotes group, so `"C:\Program Files\x.exe"` is
/// one. Backslashes are literal, as in Windows paths. The command must name a
/// program and contain `{path}`.
pub fn parse_command(line: &str) -> Result<(String, Vec<String>), String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut in_token = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_token = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_token {
                    parts.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            c => {
                current.push(c);
                in_token = true;
            }
        }
    }
    if quoted {
        return Err("The scanner command has an unclosed quote.".to_string());
    }
    if in_token {
        parts.push(current);
    }
    let mut parts = parts.into_iter();
    let program = parts
        .next()
        .filter(|p| !p.is_empty())
        .ok_or("The scanner command is empty.")?;
    let args: Vec<String> = parts.collect();
    if !args.iter().any(|a| a.contains(PATH_TOKEN)) {
        return Err(format!(
            "The scanner command must contain {PATH_TOKEN} where the file goes."
        ));
    }
    Ok((program, args))
}

/// The program and arguments that scan `file`.
pub fn command_for(scanner: Scanner, custom: &str, file: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let file = file.display().to_string();
    match scanner {
        Scanner::Defender => {
            let base = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
            let exe = PathBuf::from(base).join("Windows Defender").join("MpCmdRun.exe");
            let args = ["-Scan", "-ScanType", "3", "-File", &file, "-DisableRemediation"];
            Ok((exe, args.iter().map(|a| a.to_string()).collect()))
        }
        Scanner::Custom => {
            let (program, args) = parse_command(custom)?;
            let args = args.iter().map(|a| a.replace(PATH_TOKEN, &file)).collect();
            Ok((PathBuf::from(program), args))
        }
    }
}

/// What an exit code means for `scanner`.
fn outcome(scanner: Scanner, code: Option<i32>, output: String) -> Scan {
    let threat_code = match scanner {
        Scanner::Defender => 2,
        Scanner::Custom => 1,
    };
    match code {
        Some(0) => Scan::Clean,
        Some(c) if c == threat_code => Scan::Threat { detail: output },
        Some(c) => Scan::Failed {
            detail: join_detail(&format!("the scanner exited with code {c}"), &output),
        },
        None => Scan::Failed {
            detail: "the scanner was stopped".to_string(),
        },
    }
}

fn join_detail(head: &str, output: &str) -> String {
    if output.is_empty() {
        head.to_string()
    } else {
        format!("{head}: {output}")
    }
}

/// Scan `file` and wait for the result.
pub fn scan(scanner: Scanner, custom: &str, file: &Path) -> Scan {
    let (program, args) = match command_for(scanner, custom, file) {
        Ok(c) => c,
        Err(detail) => return Scan::Failed { detail },
    };
    let mut child = match Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Scan::Failed {
                detail: format!("could not run {}: {e}", program.display()),
            }
        }
    };

    // Drain both pipes on their own threads so a chatty scanner never blocks.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

    let deadline = Instant::now() + SCAN_TIMEOUT;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Scan::Failed {
                    detail: format!("the scan took longer than {} minutes", SCAN_TIMEOUT.as_secs() / 60),
                };
            }
            Err(e) => {
                return Scan::Failed {
                    detail: format!("could not wait for the scanner: {e}"),
                }
            }
        }
    };
    let mut output = stdout.join().unwrap_or_default();
    output.extend(stderr.join().unwrap_or_default());
    outcome(scanner, code, tail(&String::from_utf8_lossy(&output)))
}

/// The last `DETAIL_CHARS` characters of the output, on one line.
fn tail(output: &str) -> String {
    let one_line = output.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = one_line.chars().count();
    one_line.chars().skip(count.saturating_sub(DETAIL_CHARS)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_programs_and_arguments() {
        let (prog, args) =
            parse_command(r#""C:\Program Files\AV\scan.exe"  /scan  "{path}" --log=C:\x"#).unwrap();
        assert_eq!(prog, r"C:\Program Files\AV\scan.exe");
        assert_eq!(args, ["/scan", "{path}", r"--log=C:\x"]);
    }

    #[test]
    fn rejects_bad_commands() {
        assert!(parse_command("").is_err());
        assert!(parse_command("   ").is_err());
        assert!(parse_command("scan.exe /all").is_err(), "no {{path}}");
        assert!(parse_command(r#""scan.exe {path}"#).is_err(), "unclosed quote");
    }

    #[test]
    fn a_file_name_stays_one_argument() {
        // Quotes, spaces and shell operators in the name never split it.
        let file = Path::new(r#"C:\dl\a" & calc.exe & ".pdf"#);
        let (_, args) = command_for(Scanner::Custom, "scan.exe --file={path} -q", file).unwrap();
        assert_eq!(args, [format!("--file={}", file.display()), "-q".to_string()]);

        let (exe, args) = command_for(Scanner::Defender, "", file).unwrap();
        assert!(exe.ends_with(r"Windows Defender\MpCmdRun.exe"));
        assert_eq!(args[..5], ["-Scan", "-ScanType", "3", "-File", &file.display().to_string()]);
    }

    #[test]
    fn exit_codes() {
        assert_eq!(outcome(Scanner::Defender, Some(0), String::new()), Scan::Clean);
        assert!(matches!(outcome(Scanner::Defender, Some(2), "x".into()), Scan::Threat { .. }));
        assert!(matches!(outcome(Scanner::Defender, Some(1), String::new()), Scan::Failed { .. }));
        assert!(matches!(outcome(Scanner::Custom, Some(1), String::new()), Scan::Threat { .. }));
        assert!(matches!(outcome(Scanner::Custom, Some(2), String::new()), Scan::Failed { .. }));
        assert!(matches!(outcome(Scanner::Custom, None, String::new()), Scan::Failed { .. }));
    }

    /// Runs real programs: `cmd.exe` stands in for a scanner, so the test can
    /// pick the exit code. (UIDM itself never runs a scanner through cmd.)
    #[test]
    fn runs_a_scanner_and_reads_its_exit_code() {
        let cmd = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        let file = std::env::temp_dir().join("uidm scan test.txt");
        std::fs::write(&file, b"x").unwrap();
        let run = |code: u8| scan(Scanner::Custom, &format!(r#""{cmd}" /c exit {code} {{path}}"#), &file);
        assert_eq!(run(0), Scan::Clean);
        assert!(matches!(run(1), Scan::Threat { .. }));
        assert!(matches!(run(7), Scan::Failed { .. }));
        let missing = scan(Scanner::Custom, r"C:\no\such\scanner.exe {path}", &file);
        assert!(matches!(missing, Scan::Failed { detail } if detail.contains("could not run")));
        std::fs::remove_file(&file).ok();
    }
}
