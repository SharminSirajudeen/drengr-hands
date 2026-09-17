//! The one login-shell probe.
//!
//! MCP clients launch tool servers with a minimal environment that never sourced
//! the user's profile, so a tool path exported in `.zshrc` is invisible to us. A
//! login shell is the only way to recover it, and that is the whole reason this
//! exists.
//!
//! It is also the most dangerous call in the transport layer, so it has exactly
//! one implementation. A login shell runs the user's profile: anything in there
//! that blocks (a prompt, a version manager reaching the network, a mounted
//! filesystem that is not answering) blocks us for as long as it likes. The
//! previous version called `.output()`, which waits for the child's pipes to
//! reach EOF with no deadline at all, from inside async functions, on every
//! single adb invocation.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A user's shell profile is theirs, not ours. This is long enough for a slow
/// one and short enough that a wedged one is a delay rather than a hang.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often to check whether the probe finished. Small enough to stay
/// responsive, large enough not to spin a core.
const POLL: Duration = Duration::from_millis(25);

/// Run `sh -lc <cmd>` and return its trimmed stdout, or `None`.
///
/// `None` means the probe did not answer: it failed to spawn, exited non-zero,
/// printed nothing, or ran past the deadline and was killed. It never means
/// "the tool is not installed", because this probe cannot tell those apart and
/// must not pretend otherwise.
pub fn login_shell(cmd: &str) -> Option<String> {
    let mut child = Command::new("sh")
        .args(["-lc", cmd])
        // stdin from /dev/null: a profile that prompts must hit EOF and give up
        // rather than wait on a terminal that is not there.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    // The shell outlived its welcome. Kill it and reap it, so a
                    // wedged profile cannot leave a zombie behind us.
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(
                        "login-shell probe exceeded {}s and was killed; \
                         set DRENGR_ADB_PATH or DRENGR_XCRUN_PATH to skip it",
                        PROBE_TIMEOUT.as_secs()
                    );
                    return None;
                }
                std::thread::sleep(POLL);
            }
            Err(_) => return None,
        }
    }

    // Read after exit. The payload is one path, far below the pipe buffer, so
    // the child cannot have blocked writing it.
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The single-probe guard.
///
/// There were two copies of this probe, one in `adb.rs` and one in `simctl.rs`,
/// and both were unbounded `.output()` calls made from inside async functions on
/// every resolve. A second copy is how a bound gets added to one and not the
/// other, which is the recurring defect in this codebase: a fix landing in one
/// path and not its twin.
///
/// The property: `sh -lc` is spawned in exactly one place, and that place bounds
/// it.
#[cfg(test)]
mod single_probe_guard {
    use crate::source_guard::{line_of, rust_files_under, without_comments};
    /// Files allowed to spawn a login shell, and why.
    const ALLOWED: &[(&str, &str)] = &[(
        "transport/probe.rs",
        "the one implementation: bounded by a deadline, killed and reaped on timeout, stdin closed",
    )];

    #[test]
    fn only_one_place_may_spawn_a_login_shell() {
        let sources = rust_files_under(&crate::source_guard::src_root());

        // Self-check: the guard is worthless if it stopped seeing the probe it
        // polices, so prove it still finds this file's own spawn.
        let mine = sources
            .iter()
            .find(|(n, _)| n == "transport/probe.rs")
            .map(|(_, s)| without_comments(s))
            .expect("guard must still read transport/probe.rs");
        assert!(
            mine.contains("\"-lc\""),
            "guard no longer sees the login-shell spawn it exists to police"
        );
        // And that the one allowed copy is actually bounded, not merely alone.
        for needle in ["PROBE_TIMEOUT", "try_wait", "kill()", "Stdio::null()"] {
            assert!(
                mine.contains(needle),
                "the one allowed probe lost `{needle}`: being the only copy is not the same as being bounded"
            );
        }

        let offenders: Vec<String> = sources
            .iter()
            .filter(|(name, _)| !ALLOWED.iter().any(|(a, _)| a == name))
            .flat_map(|(name, src)| {
                let code = without_comments(src);
                code.match_indices("\"-lc\"")
                    .map(|(at, _)| {
                        let line = line_of(&code, at);
                        format!("{name}:{line} spawns its own login shell")
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        assert!(
            offenders.is_empty(),
            "a second login-shell probe exists.\n{}\n\
             Call transport::probe::login_shell instead. A login shell runs the user's \
             profile, which can block forever, so it gets one bounded implementation.",
            offenders.join("\n")
        );
    }
}

/// The inherited-stdin guard.
///
/// Our stdin is the MCP transport. A device command that inherits it steals the
/// client's JSON-RPC, and `adb shell` does worse than hold it: it FORWARDS stdin
/// to the device, so the handshake got typed at the phone. With a device
/// attached at startup, `resolve_identity` ran `adb shell getprop ro.serialno`
/// before the read loop opened, the handshake was consumed, and the server read
/// EOF and answered nothing at all. It presented as a hang and it was theft.
///
/// The property: every process drengr spawns in the transport layer says what
/// its stdin is. Inheriting the MCP transport is never the right default.
#[cfg(test)]
mod inherited_stdin_guard {
    use crate::source_guard::{line_of, without_comments};

    /// Spawn sites allowed to leave stdin alone, with the reason.
    const ALLOWED: &[(&str, &str)] = &[(
        "probe.rs",
        "this file's own spawn sets Stdio::null explicitly and is covered by its own tests",
    )];

    /// The body of the function containing `at`, so a `let mut cmd = ...;` on one
    /// line and `cmd.stdin(..)` on the next both count.
    fn enclosing_fn(code: &str, at: usize) -> &str {
        let start = code[..at].rfind("fn ").unwrap_or(0);
        let open = match code[start..].find('{') {
            Some(o) => start + o,
            None => return &code[start..],
        };
        let mut depth = 0usize;
        for (k, c) in code[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &code[start..open + k];
                    }
                }
                _ => {}
            }
        }
        &code[start..]
    }

    #[test]
    fn every_transport_spawn_declares_its_stdin() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/transport");
        let mut files = Vec::new();
        fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(d) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        walk(&dir, &mut files);

        let mut sites = 0usize;
        let mut offenders = Vec::new();
        for f in &files {
            let name = f
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if name.ends_with("tests.rs") || ALLOWED.iter().any(|(a, _)| *a == name) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(f) else {
                continue;
            };
            let code = without_comments(&src);
            for (at, _) in code.match_indices("process::Command::new(") {
                sites += 1;
                if !enclosing_fn(&code, at).contains(".stdin(") {
                    let line = line_of(&code, at);
                    offenders.push(format!("{name}:{line} spawns without declaring stdin"));
                }
            }
        }

        // Self-check: a guard that found nothing to police is not passing, it is
        // broken.
        assert!(
            sites >= 5,
            "guard found only {sites} spawn sites under src/transport; it is no longer reading the tree it polices"
        );

        assert!(
            offenders.is_empty(),
            "a transport process inherits our stdin, which is the MCP transport.\n{}\n\
             Set stdin explicitly. `adb shell` forwards stdin to the device, so an \
             inherited one is handed to the phone.",
            offenders.join("\n")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_trimmed_stdout_of_a_successful_probe() {
        assert_eq!(
            login_shell("echo /usr/bin/adb").as_deref(),
            Some("/usr/bin/adb")
        );
    }

    #[test]
    fn a_probe_that_prints_nothing_is_none_not_empty_string() {
        assert_eq!(login_shell("true"), None);
    }

    #[test]
    fn a_failing_probe_is_none() {
        assert_eq!(login_shell("exit 1"), None);
    }

    /// The defect this module exists for: a profile that never returns must cost
    /// us the timeout, not the process.
    ///
    /// Run on its own thread with a bounded receive, because the failure mode
    /// under test is a hang. Calling `login_shell` directly here would make a
    /// broken deadline hang the suite instead of failing it, and a check that
    /// hangs on regression is not a check. The source guard above cannot cover
    /// this: it can see that the deadline code is present, not that it works.
    #[test]
    fn a_hanging_profile_is_killed_at_the_deadline() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let start = Instant::now();
            let got = login_shell("sleep 120");
            let _ = tx.send((got, start.elapsed()));
        });

        let (got, waited) = rx
            .recv_timeout(PROBE_TIMEOUT + Duration::from_secs(10))
            .expect(
            "login_shell did not return: the deadline is gone and a wedged profile now hangs us",
        );

        assert_eq!(got, None, "a killed probe answers None, never a value");
        assert!(
            waited >= PROBE_TIMEOUT,
            "returned in {waited:?}, before the {PROBE_TIMEOUT:?} deadline could have fired"
        );
    }

    /// stdin must be closed, or a profile that reads it waits on a terminal that
    /// is not there and we pay the full timeout for every discovery.
    #[test]
    fn stdin_is_closed_so_a_reading_profile_does_not_block() {
        let start = Instant::now();
        assert_eq!(login_shell("cat; echo done").as_deref(), Some("done"));
        assert!(
            start.elapsed() < PROBE_TIMEOUT,
            "reading stdin should hit EOF immediately, took {:?}",
            start.elapsed()
        );
    }
}
