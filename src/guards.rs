//! Structural guards. Each of these forks was resolved once; these tests make
//! the fork impossible to reintroduce rather than merely discouraged.
//!
//! Every needle is assembled at runtime, so this file never matches itself.

#[cfg(test)]
mod tests {

    use std::path::{Path, PathBuf};

    fn src_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Every `src/**/*.rs` line matching `is_violation`, as `src/<rel>:<line>`.
    /// `exempt` holds the paths allowed to own the pattern.
    fn offenders(exempt: &[&str], is_violation: &dyn Fn(&str) -> bool) -> Vec<String> {
        let root = src_root();
        let mut stack = vec![root.clone()];
        let mut hits = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src/ is readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&root)
                    .expect("under src/")
                    .to_string_lossy()
                    .replace('\\', "/");
                if exempt.contains(&rel.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("rust file is utf-8");
                for (i, line) in text.lines().enumerate() {
                    if is_violation(line) {
                        hits.push(format!("src/{}:{}", rel, i + 1));
                    }
                }
            }
        }
        hits.sort();
        hits
    }

    fn env_read() -> String {
        format!("env::{}(", "var")
    }

    /// `DRENGR_HOME` is honoured only if every path runs through `paths`. A
    /// hand-built home join silently reads and writes a different tree: it made
    /// `uninstall` erase one directory while the license cache survived in
    /// another, and made tests scribble on the developer's real storage.
    #[test]
    fn no_file_builds_the_drengr_directory_by_hand() {
        let dir = format!(".{}", "drengr");
        let needles = [format!("join(\"{}", dir), format!("join(s!(\"{}", dir)];
        let hits = offenders(&["paths.rs"], &|line| {
            needles.iter().any(|n| line.contains(n.as_str()))
        });
        assert!(
            hits.is_empty(),
            "these sites bypass paths::drengr_dir() and so ignore DRENGR_HOME: {:?}",
            hits
        );
    }

    /// A second reader that consults only `ANDROID_HOME` disagrees with
    /// `android_sdk::sdk_root` on any machine exporting only `ANDROID_SDK_ROOT`,
    /// which is what Google's own CI images set.
    #[test]
    fn only_one_module_reads_the_android_sdk_root() {
        let read = env_read();
        let vars = [
            format!("ANDROID_{}", "HOME"),
            format!("ANDROID_{}", "SDK_ROOT"),
        ];
        let hits = offenders(&["transport/android_sdk.rs"], &|line| {
            line.contains(read.as_str()) && vars.iter().any(|v| line.contains(v.as_str()))
        });
        assert!(
            hits.is_empty(),
            "these sites re-read the SDK root instead of calling android_sdk::sdk_root(): {:?}",
            hits
        );
    }

    /// One resolver decides the endpoint. A second call site that reads a
    /// provider default directly would silently ignore DRENGR_BASE_URL.
    #[test]
    fn only_the_llm_client_resolves_its_endpoint() {
        let read = env_read();
        let var = format!("DRENGR_BASE{}", "_URL");
        let hits = offenders(&["ooda/llm/mod.rs"], &|line| {
            line.contains(read.as_str()) && line.contains(var.as_str())
        });
        assert!(
            hits.is_empty(),
            "these sites re-derive the endpoint instead of calling LlmClient::base_url(): {:?}",
            hits
        );
    }

    /// Nothing in the open tree may reach for a module that stayed closed, nor
    /// carry a commercial surface. Needles are assembled so this file never
    /// matches itself.
    #[test]
    fn no_public_file_reaches_for_a_closed_module() {
        let closed = [
            "license",
            "quota",
            "telemetry",
            "rate_limit",
            "fingerprint",
            "supabase",
            "cli_login",
        ];
        let mut needles: Vec<String> = closed.iter().map(|m| format!("crate::{}::", m)).collect();
        needles.push(format!("{}{}", "obf", "str"));
        needles.push(format!("{}{}", "gateway.", "drengr.dev"));
        needles.push(format!("{}{}", "drengr.dev/", "signup"));
        needles.push(format!("{} {}", "Upgrade to", "Pro"));

        for needle in &needles {
            let hits = offenders(&[], &|line: &str| line.contains(needle.as_str()));
            assert!(
                hits.is_empty(),
                "closed surface {:?} reappeared at {:?}",
                needle,
                hits
            );
        }
    }

    /// A rejected action must be recorded, not just logged. Without the history
    /// push the next prompt is byte-identical, so the model re-proposes the same
    /// invalid action every step until the budget is gone. Verified on a device:
    /// the diagnostic bundle went from 1 action to 5 once this landed.
    #[test]
    fn a_rejected_action_is_recorded_in_history() {
        let src = std::fs::read_to_string(src_root().join("ooda/mod.rs")).expect("ooda/mod.rs");
        let arm = src
            .split("Action rejected at step")
            .nth(1)
            .expect("the rejection arm still logs a warning");
        let arm: String = arm.chars().take(2000).collect();
        let needle = format!("history{}push", ".");
        assert!(
            arm.contains(needle.as_str()),
            "the rejection arm logs but does not record the step, so the next prompt cannot differ:\n{arm}"
        );
    }

    /// A transport that cannot perform an action must say so. `press_key` on iOS
    /// used to log a warning and return Ok, so the layer above reported
    /// "Pressed key 'enter'" while nothing happened — an agent then has to
    /// explain a screen that never changed. Found by driving the CLI by hand.
    #[test]
    fn an_unsupported_key_fails_instead_of_reporting_success() {
        let src =
            std::fs::read_to_string(src_root().join("transport/simctl.rs")).expect("simctl.rs");
        let body = src
            .split(&format!("async fn {}", "press_key"))
            .nth(1)
            .expect("press_key exists");
        let body: String = body.chars().take(2500).collect();
        let fallback = body
            .rsplit("other =>")
            .next()
            .expect("press_key has a catch-all arm")
            .to_string();
        let fallback: String = fallback.chars().take(300).collect();
        assert!(
            fallback.contains("Err("),
            "the catch-all arm of press_key returns success for a key it cannot send:\n{fallback}"
        );
    }

    /// The text scene must print the ids the annotator assigned. Numbering it
    /// 1..N instead printed numbers `drengr do --element n` could never resolve:
    /// the registry hands out stable ids seeded from the previous process, and
    /// those are what get persisted as tap targets. Found by driving the CLI.
    #[test]
    fn the_text_scene_prints_the_ids_that_can_be_tapped() {
        let positional = format!("build{}(&elements", "_capped");
        let plain = format!("{}(&elements)", ".build");
        for f in ["mcp/handlers/look.rs", "mcp/handlers/do_action.rs"] {
            let src = std::fs::read_to_string(src_root().join(f)).expect(f);
            assert!(
                !src.contains(positional.as_str()) && !src.contains(plain.as_str()),
                "{f} numbers its text scene positionally; it must render the annotator's ids"
            );
        }
    }

    /// Every parameter the drengr_do schema documents must be reachable from the
    /// CLI. Twelve were not: wait, install, open_url, deep_link, set_location,
    /// set_appearance, grant_permission and set_orientation were undrivable from
    /// a shell while `drengr --help` promised exactly that. Found by hand.
    #[test]
    fn every_documented_do_parameter_is_reachable_from_the_cli() {
        let tools = std::fs::read_to_string(src_root().join("mcp/tools.rs")).expect("tools.rs");
        let start = tools
            .find(&format!("\"name\": \"drengr{}\"", "_do"))
            .expect("drengr_do");
        let schema: String = tools[start..].chars().take(9000).collect();
        let mut documented: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        // Property entries sit at one fixed indent inside "properties"; matching
        // any `"x": {` also swept up inputSchema, properties and annotations.
        for line in schema.lines() {
            let indent = line.len() - line.trim_start().len();
            if indent != 16 {
                continue;
            }
            let l = line.trim();
            if l.ends_with("\": {") && l.starts_with('"') {
                if let Some(n) = l.trim_start_matches('"').split('"').next() {
                    documented.insert(n.to_string());
                }
            }
        }
        assert!(
            documented.len() > 15,
            "the schema scrape found only {} properties; the shape changed and this \
             guard is no longer reading it",
            documented.len()
        );
        documented.remove("action");
        // query-only properties share the schema block's tail.
        documented.remove("question");
        documented.remove("headless");

        let main = std::fs::read_to_string(src_root().join("main.rs")).expect("main.rs");
        let do_arm = main
            .split("Commands::Do {")
            .nth(1)
            .expect("the Do arm exists");
        let do_arm: String = do_arm.chars().take(6000).collect();

        let missing: Vec<&String> = documented
            .iter()
            .filter(|p| !do_arm.contains(&format!("\"{}\".into()", p)))
            .collect();
        assert!(
            missing.is_empty(),
            "documented drengr_do parameters the CLI cannot send: {missing:?}"
        );
    }

    /// A model that re-proposes an action the executor just refused will do it
    /// again, and again, until the budget is gone. Feeding the rejection back is
    /// necessary but not sufficient, so the loop ends the run on the second
    /// identical rejection with that reason instead of "exceeded max steps".
    #[test]
    fn a_twice_rejected_action_ends_the_run() {
        let src = std::fs::read_to_string(src_root().join("ooda/mod.rs")).expect("ooda/mod.rs");
        let arm = src
            .split("Action rejected at step")
            .nth(1)
            .expect("the rejection arm exists");
        let arm: String = arm.chars().take(1400).collect();
        assert!(
            arm.contains("last_rejection"),
            "the rejection arm does not compare against the previous rejection:\n{arm}"
        );
        assert!(
            arm.contains("return Ok("),
            "a repeated rejection does not end the run, so it burns the step budget:\n{arm}"
        );
    }

    /// A transport method that cannot do its job must say so. Three of them
    /// returned Ok while doing nothing — iOS keyboard dismissal, the iOS
    /// pasteboard write, and the Android recording pull — so callers proceeded
    /// as though a keyboard were gone, a clipboard were set, or a file existed.
    #[test]
    fn no_transport_method_reports_success_for_work_it_did_not_do() {
        let cases = [
            ("transport/simctl.rs", "dismiss_keyboard"),
            ("transport/simctl.rs", "pasteboard_set"),
            ("transport/adb.rs", "stop_recording"),
        ];
        for (file, method) in cases {
            let src = std::fs::read_to_string(src_root().join(file)).expect(file);
            let body = src
                .split(&format!("fn {method}"))
                .nth(1)
                .unwrap_or_else(|| panic!("{file} still defines {method}"));
            let body: String = body.chars().take(1200).collect();
            assert!(
                body.contains("bail!") || body.contains("?;"),
                "{file}::{method} has no failure path; it can only report success:\n{body}"
            );
        }
    }

    /// Every command this binary names in its own output must exist. Three
    /// separate strings survived the extraction pointing at commands that had
    /// been deleted — including the welcome line every MCP user sees once, which
    /// told them to run `drengr login`.
    #[test]
    fn no_string_names_a_subcommand_that_does_not_exist() {
        let main = std::fs::read_to_string(src_root().join("main.rs")).expect("main.rs");
        // The clap enum is the list of real commands; derive it rather than
        // restate it, or this guard goes stale the way the strings did.
        let commands: std::collections::BTreeSet<String> = main
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                l.strip_suffix(" {")
                    .or_else(|| l.strip_suffix(","))
                    .filter(|n| {
                        !n.is_empty()
                            && n.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                            && n.chars().all(|c| c.is_ascii_alphanumeric())
                    })
                    .map(|n| {
                        let mut out = String::new();
                        for (i, c) in n.chars().enumerate() {
                            if c.is_ascii_uppercase() && i > 0 {
                                out.push('-');
                            }
                            out.push(c.to_ascii_lowercase());
                        }
                        out
                    })
            })
            .collect();
        assert!(
            commands.contains("look") && commands.contains("doctor"),
            "could not read the command list out of main.rs; this guard is blind"
        );

        let needle = format!("{}drengr ", "`");
        let mut stale = Vec::new();
        // This file quotes the very strings it hunts for, in the comment above.
        for path in offenders(&["guards.rs"], &|line: &str| line.contains(needle.as_str())) {
            let (rel, _) = path.split_once(':').unwrap_or((path.as_str(), ""));
            let rel = rel.trim_start_matches("src/");
            let src = std::fs::read_to_string(src_root().join(rel)).expect(rel);
            for l in src.lines() {
                for seg in l.split(needle.as_str()).skip(1) {
                    // A flag is not a subcommand.
                    if seg.starts_with('-') {
                        continue;
                    }
                    let word: String = seg
                        .chars()
                        .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                        .collect();
                    if word.len() > 2 && !commands.contains(&word) {
                        stale.push(format!("{rel}: `drengr {word}`"));
                    }
                }
            }
        }
        stale.sort();
        stale.dedup();
        assert!(
            stale.is_empty(),
            "strings naming commands that do not exist: {stale:#?}"
        );
    }

    /// An action that claims a platform must have an implementation there. The
    /// trait's defaults bail, so a claim with no override is a promise the agent
    /// will spend a step discovering is false. `unlock` claimed iOS and always
    /// errored; ten trait docs described a WebDriverAgent this repo does not use.
    #[test]
    fn every_action_platform_claim_has_an_implementation() {
        let actions = std::fs::read_to_string(src_root().join("mcp/actions.rs")).expect("actions");
        let adb = std::fs::read_to_string(src_root().join("transport/adb.rs")).expect("adb");
        let sim = std::fs::read_to_string(src_root().join("transport/simctl.rs")).expect("simctl");
        let tr = std::fs::read_to_string(src_root().join("transport/mod.rs")).expect("mod");

        // Trait methods whose default refuses: these need a real override.
        let bails: std::collections::BTreeSet<&str> = tr
            .split("async fn ")
            .filter(|seg| {
                let head: String = seg.chars().take(400).collect();
                head.contains("anyhow::bail!")
            })
            .filter_map(|seg| seg.split('(').next())
            .collect();

        let mut broken = Vec::new();
        for block in actions.split("ActionDef {").skip(1) {
            let block: String = block.chars().take(500).collect();
            let Some(name) = block
                .split("name: \"")
                .nth(1)
                .and_then(|s| s.split('"').next())
            else {
                continue;
            };
            if !bails.contains(name) {
                continue;
            }
            let claims_android =
                block.contains("platforms: BOTH") || block.contains("platforms: ANDROID");
            let claims_ios = block.contains("platforms: BOTH") || block.contains("platforms: IOS");
            let sig = format!("async fn {name}(");
            if claims_android && !adb.contains(sig.as_str()) {
                broken.push(format!("{name}: claims android, adb.rs has no override"));
            }
            if claims_ios && !sim.contains(sig.as_str()) {
                broken.push(format!("{name}: claims ios, simctl.rs has no override"));
            }
        }
        assert!(
            broken.is_empty(),
            "actions promising a platform they cannot serve: {broken:#?}"
        );
    }

    /// A second list that must be kept in step with a first one drifts. These are
    /// the places it already had: `find_mcp_configs` knew four of eight hosts, so
    /// uninstall left Drengr wired into the other four; the CLI's hand-written
    /// action list reached 30 of 40 in a day. Both now derive from their source.
    #[test]
    fn no_surface_keeps_its_own_copy_of_a_canonical_list() {
        let main = std::fs::read_to_string(src_root().join("main.rs")).expect("main.rs");

        // The client catalog is the only place a host's config path may be written.
        let path_literal = format!("{}{}", "claude_desktop_", "config.json");
        assert!(
            !main.contains(path_literal.as_str()),
            "main.rs writes a client config path that mcp::clients::all already owns"
        );

        // Action names belong to the catalog; the CLI points at it instead. Count
        // only the doc block attached to `action`, not every other flag's help.
        let doc_block: String = main
            .split("action: String")
            .next()
            .unwrap_or("")
            .lines()
            .rev()
            .take_while(|l| l.trim().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");
        let listed = doc_block.matches(" | ").count();
        assert!(
            listed < 15,
            "the CLI is hand-listing actions again ({listed} in its own doc block); \
             point at `drengr query capabilities` instead"
        );
    }
}
