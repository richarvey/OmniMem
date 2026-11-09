//! Running a CLI command at a terminal, as a person would (#37): under
//! `script`, which gives it a pseudo-terminal, answering the prompt once it
//! appears. The prompt is waited for rather than the answer piped in up
//! front, since BSD `script` can deliver end-of-file before the program
//! reads.

#![allow(dead_code)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a terminal run printed (stdout and stderr together, as a terminal
/// shows them) and whether it succeeded.
pub struct TerminalRun {
    pub success: bool,
    pub output: String,
}

fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Run `bin args` at a pseudo-terminal with `envs` set and `remove` unset,
/// and type `answer` when the program asks.
pub fn in_terminal(
    bin: &Path,
    args: &[OsString],
    envs: &[(&str, OsString)],
    remove: &[&str],
    answer: &str,
) -> TerminalRun {
    let mut command = Command::new("script");
    if cfg!(target_os = "macos") {
        command.arg("-q").arg("/dev/null").arg(bin).args(args);
    } else {
        let line = std::iter::once(bin.as_os_str())
            .chain(args.iter().map(OsString::as_os_str))
            .map(|a| shell_quote(&a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        command.args(["-qefc", &line, "/dev/null"]);
    }
    for (k, v) in envs {
        command.env(k, v);
    }
    for k in remove {
        command.env_remove(k);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("script runs");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut stdout = child.stdout.take().unwrap();
    let reader = {
        let seen = seen.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        })
    };
    let mut stdin = child.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut answered = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if !answered && String::from_utf8_lossy(&seen.lock().unwrap()).contains("characters of") {
            stdin.write_all(format!("{answer}\n").as_bytes()).unwrap();
            stdin.flush().unwrap();
            answered = true;
        }
        assert!(Instant::now() < deadline, "the command never finished");
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    reader.join().unwrap();
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).ok();
    TerminalRun {
        success: status.success(),
        output: format!("{}{err}", String::from_utf8_lossy(&seen.lock().unwrap())),
    }
}

/// The last six characters of a key: what the prompt asks for.
pub fn tail(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    chars[chars.len().saturating_sub(6)..].iter().collect()
}

/// The key a person-only command asks about, if `args` is one: the third
/// argument of `trust confirm|reject|restore|confirm-source` and of
/// `contradictions resolve|accept`.
pub fn person_only_key<'a>(args: &[&'a str]) -> Option<&'a str> {
    match args {
        [
            "trust",
            "confirm" | "reject" | "restore" | "confirm-source",
            key,
            ..,
        ]
        | ["contradictions", "resolve" | "accept", key, ..] => Some(key),
        _ => None,
    }
}

/// A terminal run as an `Output`, with what it printed as both stdout and
/// stderr (a terminal doesn't keep them apart), and the prompt and the
/// typed answer taken off the front.
pub fn as_output(run: TerminalRun) -> std::process::Output {
    use std::os::unix::process::ExitStatusExt;
    let text = run.output.replace('\r', "");
    let text = match text.find("characters of") {
        Some(at) => text[at..]
            .split_once('\n')
            .map_or("", |(_, rest)| rest)
            .to_owned(),
        None => text,
    };
    // A `script` that echoes the typed answer leaves it on its own line.
    let text = text.trim_start_matches('\n').to_owned();
    std::process::Output {
        status: std::process::ExitStatus::from_raw(if run.success { 0 } else { 1 << 8 }),
        stdout: text.clone().into_bytes(),
        stderr: text.into_bytes(),
    }
}
