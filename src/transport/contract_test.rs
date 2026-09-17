//! The contract guard.
//!
//! `DeviceTransport` is the one seam every device capability passes through, and
//! a default body that hands back a plausible value instead of an error is
//! invisible at the call site: `Ok(vec![])` reads as "the device has none of
//! those" and `Ok(false)` reads as "looked, it is not there". Both are the
//! opposite of the truth when the transport could not act at all.
//!
//! This test reads the trait as source, extracts every default body, and fails
//! on one that is neither an explicit error nor on the allowlist below. Adding
//! an entry to the allowlist is therefore a deliberate, reviewable act.

/// Defaults that are allowed not to error, each with the reason it is honest.
/// Two shapes qualify: a default that composes other trait methods (the work
/// happens in those, which error for themselves), and a default whose value is
/// itself the truthful answer rather than a stand-in for one.
const ALLOWED: &[(&str, &str)] = &[
    ("observe", "composition: screenshot + ui_tree, each of which errors for itself"),
    ("swipe_with_velocity", "composition: derives a duration and calls swipe"),
    ("draw_path", "composition: chains swipe per segment"),
    ("list_apps_with_names", "composition: list_installed_apps, which now errors"),
    ("go_home", "composition: press_key(HOME)"),
    ("reset_app", "composition: terminate + clear + launch, and launch must succeed"),
    ("death_report", "composition: is_connected + is_app_in_foreground; its fallback string is literally \"unknown\""),
    ("platform_kind", "no action taken: \"unknown\" is the honest tag, not a claim about the device"),
    ("id", "no action taken: \"\" is documented as cannot-verify and callers must not read it as a match"),
    ("resolve_identity", "nothing to resolve: a transport that already knows its id has completed this call"),
    ("cleanup_runtime", "nothing to tear down: a transport that installs no runtime has completed teardown"),
    ("screen_stream_url", "None is the answer, not a stand-in: a transport with no stream has no URL, and there is no looked-vs-could-not-look split to hide"),
];

/// One `fn` declared inside the trait.
struct TraitFn {
    name: String,
    /// `None` for a required method (signature ends in `;`).
    body: Option<String>,
}

/// Every `fn` declared directly in `pub trait DeviceTransport`, in source order.
fn trait_fns(src: &str) -> Vec<TraitFn> {
    let code = crate::source_guard::without_comments_or_strings(src);
    let decl = code
        .find("pub trait DeviceTransport")
        .expect("the trait is declared in this file");
    let open = decl + code[decl..].find('{').expect("the trait has a body");
    let chars: Vec<char> = code.chars().collect();
    let mut found = Vec::new();
    let (mut i, mut depth) = (open + 1, 1usize);
    while i < chars.len() && depth > 0 {
        match chars[i] {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        // Only signatures at the trait's own level, never bodies of the defaults.
        if depth == 1
            && chars[i..].starts_with(&['f', 'n', ' '])
            && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'))
        {
            let name: String = chars[i + 3..]
                .iter()
                .take_while(|c| c.is_alphanumeric() || **c == '_')
                .collect();
            let mut j = i + 3 + name.chars().count();
            let mut parens = 0usize;
            while j < chars.len() {
                match chars[j] {
                    '(' => parens += 1,
                    ')' => parens -= 1,
                    ';' if parens == 0 => break,
                    '{' if parens == 0 => break,
                    _ => {}
                }
                j += 1;
            }
            if chars.get(j) == Some(&'{') {
                let start = j + 1;
                let (mut k, mut d) = (start, 1usize);
                while k < chars.len() && d > 0 {
                    match chars[k] {
                        '{' => d += 1,
                        '}' => d -= 1,
                        _ => {}
                    }
                    k += 1;
                }
                found.push(TraitFn {
                    name,
                    body: Some(chars[start..k - 1].iter().collect()),
                });
                i = k;
                continue;
            }
            found.push(TraitFn { name, body: None });
            i = j;
            continue;
        }
        i += 1;
    }
    found
}

/// A body that admits it cannot act. A bare `Err(..)` match arm inside a larger
/// body does not qualify: only a body that errors is honest.
fn is_explicit_error(body: &str) -> bool {
    body.contains("bail!(") || body.contains("return Err(") || body.trim_start().starts_with("Err(")
}

fn trait_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/transport/mod.rs");
    std::fs::read_to_string(&path).expect("src/transport/mod.rs is readable")
}

#[test]
fn no_default_may_silently_claim_success() {
    let fns = trait_fns(&trait_source());
    let defaults: Vec<&TraitFn> = fns.iter().filter(|f| f.body.is_some()).collect();
    let required: Vec<&TraitFn> = fns.iter().filter(|f| f.body.is_none()).collect();

    // A parser that finds nothing passes everything. These floors, and the two
    // named signatures, fail the guard before it can be vacuously green.
    assert!(
        defaults.len() >= 35 && required.len() >= 12,
        "the guard parsed {} defaults and {} required methods out of a trait that has far more; \
         it is no longer reading the file it exists to police",
        defaults.len(),
        required.len()
    );
    for name in ["screenshot", "tap", "type_text"] {
        assert!(
            required.iter().any(|f| f.name == name),
            "{name} has no default body, so the guard must classify it as required"
        );
    }

    let offenders: Vec<String> = defaults
        .iter()
        .filter(|f| !ALLOWED.iter().any(|(n, _)| *n == f.name))
        .filter(|f| !is_explicit_error(f.body.as_deref().unwrap_or("")))
        .map(|f| f.name.clone())
        .collect();
    assert!(
        offenders.is_empty(),
        "{:?} have a default that returns success without acting. Either implement it per \
         transport or make the default an explicit error naming the method.",
        offenders
    );
}

#[test]
fn every_allowlisted_default_still_exists() {
    let fns = trait_fns(&trait_source());
    let stale: Vec<&str> = ALLOWED
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !fns.iter().any(|f| f.name == *n && f.body.is_some()))
        .collect();
    assert!(
        stale.is_empty(),
        "{:?} are allowlisted but are no longer trait defaults; an allowlist nobody prunes is \
         how the next exemption gets waved through",
        stale
    );
}
