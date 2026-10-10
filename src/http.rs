use std::sync::OnceLock;
use std::time::Duration;

/// A dead or blackholed network fails in the connect phase, so this is the
/// bound that decides whether startup stays usable. No call site set one, which
/// is why each burned its whole request bound before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// The bound the call sites had already converged on by majority.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The one HTTP client. Override per call with `RequestBuilder::timeout`.
///
/// `build` fails only if the TLS backend will not initialise, on which
/// `reqwest::Client::new()` panics identically and after which no HTTP call in
/// this process could succeed anyway.
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("drengr/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("TLS backend initialisation")
    })
}

/// The npm version newer than this binary, if there is one.
///
/// "Newer", not "different": while npm still serves an older build, a `!=`
/// check told every user of a newer one to `drengr update` back down to it.
pub async fn newer_npm_version() -> Result<Option<String>, String> {
    let resp = client()
        .get("https://registry.npmjs.org/drengr/latest")
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let latest = json
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or("registry returned no version")?;
    Ok(is_newer(latest, env!("CARGO_PKG_VERSION")).then(|| latest.to_string()))
}

/// Semver precedence: `major.minor.patch`, then a release outranks its own
/// pre-releases (`0.12.0-rc.1` < `0.12.0`). Build metadata is ignored. A version
/// that does not parse is never newer, so a malformed registry answer cannot
/// start a nag.
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => cmp_version(&l, &c).is_gt(),
        _ => false,
    }
}

type Version<'a> = ((u64, u64, u64), Option<&'a str>);

fn parse_version(v: &str) -> Option<Version<'_>> {
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (v, None),
    };
    let mut it = core.split('.').map(|n| n.parse().ok());
    let triple = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some((triple, pre))
}

fn cmp_version(a: &Version, b: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering::{Greater, Less};
    a.0.cmp(&b.0).then_with(|| match (a.1, b.1) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => Greater,
        (Some(_), None) => Less,
        (Some(x), Some(y)) => cmp_pre_release(x, y),
    })
}

/// Dot-separated identifiers left to right: numbers numerically and below words,
/// words as text, and a shorter list below a longer one it prefixes.
fn cmp_pre_release(x: &str, y: &str) -> std::cmp::Ordering {
    let (xs, ys): (Vec<&str>, Vec<&str>) = (x.split('.').collect(), y.split('.').collect());
    xs.iter()
        .zip(&ys)
        .map(|(a, b)| match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(m), Ok(n)) => m.cmp(&n),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            _ => a.cmp(b),
        })
        .find(|o| o.is_ne())
        .unwrap_or_else(|| xs.len().cmp(&ys.len()))
}

#[cfg(test)]
mod version_order {
    use super::is_newer;

    #[test]
    fn an_older_registry_version_is_not_an_update() {
        assert!(!is_newer("0.10.13", "0.11.0"));
    }

    #[test]
    fn a_newer_registry_version_is_an_update() {
        assert!(is_newer("0.11.1", "0.11.0"));
        assert!(is_newer("0.12.0", "0.11.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
    }

    #[test]
    fn same_or_unparseable_is_not_an_update() {
        assert!(!is_newer("0.11.0", "0.11.0"));
        assert!(!is_newer("garbage", "0.11.0"));
        assert!(!is_newer("0.12", "0.11.0"));
        assert!(!is_newer("0.12.0.1", "0.11.0"));
    }

    #[test]
    fn a_release_outranks_its_pre_releases() {
        assert!(is_newer("0.12.0", "0.12.0-rc.1"));
        assert!(!is_newer("0.12.0-rc.1", "0.12.0"));
        assert!(is_newer("0.12.0-rc.1", "0.11.0"));
    }

    #[test]
    fn pre_release_identifiers_follow_semver_order() {
        assert!(is_newer("0.12.0-rc.10", "0.12.0-rc.2"));
        assert!(is_newer("0.12.0-rc", "0.12.0-beta"));
        assert!(is_newer("0.12.0-rc.1", "0.12.0-rc"));
        assert!(is_newer("0.12.0-alpha", "0.12.0-1"));
        assert!(!is_newer("0.12.0+build.5", "0.12.0"));
    }
}

/// The guard.
///
/// `reqwest::Client::new()` has no timeout of any kind. When every call site
/// builds its own client, the bound becomes a per-site decision, and a site
/// that forgets is indistinguishable from one that chose. This test reads all
/// of `src/` as source text and fails on a client built outside the allowlist,
/// so the next such site cannot land quietly.
#[cfg(test)]
mod guard {
    use std::path::{Path, PathBuf};

    /// Files allowed to build their own client, each with the reason it is a
    /// deliberate profile rather than drift. Adding an entry is a reviewable act.
    const ALLOWED: &[(&str, &str)] = &[
        ("src/http.rs", "the shared client itself"),
        (
            "src/transport/appium/client.rs",
            "a third-party cloud hub, not our API: its Timeouts profile runs to minutes for session create, app install and screen recording",
        ),
        (
            "src/driver/client.rs",
            "a driver on loopback that must not reuse connections (pool_max_idle_per_host(0)); every call there already carries its own bound",
        ),
    ];

    const NEEDLES: [&str; 3] = ["Client::new(", "Client::builder(", "ClientBuilder::new("];

    /// A needle counts only where the preceding character is not an identifier
    /// one, which keeps `DriverClient::new(` and `LlmClient::new(` out while
    /// keeping `reqwest::Client::new(` in.
    fn calls_needle(code: &str, qualified_only: bool) -> bool {
        NEEDLES.iter().any(|n| {
            code.match_indices(n).any(|(i, _)| {
                let before = &code[..i];
                if qualified_only {
                    return before.ends_with("reqwest::");
                }
                !before
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
        })
    }

    /// `reqwest::Client::new()` always counts. A bare `Client::new()` counts
    /// only where the file imported reqwest's `Client`, so this crate's own
    /// unrelated `mcp::clients::Client` cannot trip the guard.
    fn builds_a_client(code: &str) -> bool {
        let imports_reqwest_client = code
            .lines()
            .any(|l| l.trim_start().starts_with("use reqwest::") && l.contains("Client"));
        calls_needle(code, !imports_reqwest_client)
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Every `.rs` under `src/`, as (path relative to the crate root, code with
    /// comments and string interiors blanked so a needle only ever matches code).
    fn sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut paths = Vec::new();
        rs_files(&root.join("src"), &mut paths);
        paths.sort();
        paths
            .iter()
            .filter_map(|p| {
                let rel = p
                    .strip_prefix(root)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/");
                let src = std::fs::read_to_string(p).ok()?;
                Some((rel, crate::source_guard::without_comments_or_strings(&src)))
            })
            .collect()
    }

    #[test]
    fn every_http_call_goes_through_the_shared_client() {
        let sources = sources();
        assert!(
            sources.len() >= 70,
            "the guard read {} files out of a crate that has far more; it is no longer reading \
             the tree it exists to police",
            sources.len()
        );

        let offenders: Vec<&str> = sources
            .iter()
            .filter(|(path, _)| !ALLOWED.iter().any(|(a, _)| a == path))
            .filter(|(_, code)| builds_a_client(code))
            .map(|(path, _)| path.as_str())
            .collect();
        assert!(
            offenders.is_empty(),
            "{:?} build their own reqwest client. Use crate::http::client(), which bounds both \
             connect and request; add an explicit .timeout(..) at the call site if a longer \
             bound is justified. The only files exempt are {:?}.",
            offenders,
            ALLOWED
        );
    }

    #[test]
    fn every_allowlisted_file_still_builds_a_client() {
        let sources = sources();
        let stale: Vec<&(&str, &str)> = ALLOWED
            .iter()
            .filter(|(path, _)| {
                !sources
                    .iter()
                    .any(|(p, code)| p == path && builds_a_client(code))
            })
            .collect();
        assert!(
            stale.is_empty(),
            "{:?} are exempt but no longer build a client — either the needles stopped matching \
             real code, or an allowlist nobody prunes is how the next exemption gets waved through",
            stale
        );
    }

    #[tokio::test]
    async fn the_shared_client_bounds_a_connect_that_never_answers() {
        // TEST-NET-1: routable by definition, answered by nothing, so the SYN is
        // dropped rather than refused. Without CONNECT_TIMEOUT this hangs.
        let start = std::time::Instant::now();
        let err = super::client()
            .get("http://192.0.2.1/never")
            .send()
            .await
            .expect_err("a blackholed address cannot answer");
        assert!(
            err.is_timeout() || err.is_connect(),
            "expected a timeout, got {err}"
        );
        // Under REQUEST_TIMEOUT, so dropping CONNECT_TIMEOUT fails this rather
        // than falling back on the total bound unnoticed.
        assert!(
            start.elapsed() < super::CONNECT_TIMEOUT + std::time::Duration::from_secs(1),
            "the shared client took {:?} to give up on a blackholed address",
            start.elapsed()
        );
    }
}
