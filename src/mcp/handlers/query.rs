use super::*;

/// How well `label` answers a search for `needle`, both already lowercased.
/// A word, not a score: we know the KIND of match exactly, and a 0.0-1.0 number
/// would be precision we do not have.
pub(super) fn query_match_kind(label: &str, needle: &str) -> &'static str {
    match MatchKind::of(label, needle) {
        MatchKind::Exact => "exact",
        MatchKind::Prefix => "prefix",
        MatchKind::Substring => "substring",
    }
}

/// Ordering lives in the enum, not in a second function that re-matches the
/// strings this one prints. Renaming a label there used to break ranking here
/// with no compiler error.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum MatchKind {
    Exact,
    Prefix,
    Substring,
}

impl MatchKind {
    fn of(label: &str, needle: &str) -> Self {
        if label == needle {
            Self::Exact
        } else if label.starts_with(needle) {
            Self::Prefix
        } else {
            Self::Substring
        }
    }
}

/// Best label match plus how many matched at all. One implementation so the
/// in-memory and on-disk find paths cannot rank differently, which is how the
/// element projections drifted apart before.
pub(super) fn best_label_match<T>(
    items: &[T],
    needle: &str,
    label_of: impl Fn(&T) -> String,
) -> (Option<usize>, usize) {
    let mut hits: Vec<(usize, String)> = items
        .iter()
        .enumerate()
        .map(|(i, item)| (i, label_of(item).to_lowercase()))
        .filter(|(_, label)| label.contains(needle))
        .collect();
    hits.sort_by_key(|(_, label)| (MatchKind::of(label, needle), label.len()));
    (hits.first().map(|(i, _)| *i), hits.len())
}

impl McpHandlers {
    /// `drengr_query(question="setup")` — one-shot device provisioning.
    ///
    /// Picks an existing booted device (Android via adb or iOS via simctl).
    /// If none is available and `headless=true`, boots one with the standard
    /// CI-friendly flag set. Connects the resulting transport, lists installed
    /// apps with display names, and returns the lot in a single response so an
    /// agent's first interaction with Drengr can be "set me up" rather than
    /// shelling to `emulator`/`simctl` recipes.
    ///
    /// Args (all optional):
    ///   - `platform`: `"android" | "ios" | "any"` (default `"any"`)
    ///   - `headless`: bool (default `false`) — auto-boot if no device is up
    ///   - `show_window`: bool (default `false`) — Android watch mode; opens
    ///     the emulator window instead of booting `-no-window`
    ///   - `app_kind`: `"user" | "system" | "all"` (default `"user"` — the
    ///     short list of user-installable apps, what an agent typically wants)
    pub(super) async fn handle_setup_query(&self, args: &Value) -> ToolResult {
        // Cap the apps list returned in-band. Real devices report 30-200+
        // apps; an unbounded list saturates an LLM's context every call.
        // 60 fits the screen of common launcher pages and is plenty for
        // fuzzy-match. Callers that need everything can request app_kind=all
        // and read `app_count_omitted` to know they should narrow.
        const APPS_MAX: usize = 60;

        let platform = args
            .get("platform")
            .and_then(|p| p.as_str())
            .unwrap_or("any");
        let headless = args
            .get("headless")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        // Android watch mode: open the emulator window so a human can see
        // gestures. Agents leave this false. iOS renders without a window
        // regardless — use `open -a Simulator` to watch it.
        let show_window = args
            .get("show_window")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        // Default to user apps — short list is what an agent actually wants
        // when picking what to open. System packages explode the list with
        // 100+ background services that aren't user-facing.
        let app_kind = args
            .get("app_kind")
            .and_then(|s| s.as_str())
            .unwrap_or("user");

        let want_android = matches!(platform, "android" | "any");
        let want_ios = matches!(platform, "ios" | "any");
        if !want_android && !want_ios {
            return ToolResult::error(format!(
                "Unknown platform '{}' — expected android, ios, or any",
                platform
            ));
        }

        // Step 1: prefer an already-booted device that matches the requested
        // platform. Filter by platform first, then classify, so two matching
        // devices are refused rather than resolved to whichever adb listed
        // first, and DRENGR_DEVICE is honoured on this path too.
        use crate::transport::detect::Selection;
        let matching: Vec<_> = crate::transport::detect::detect_devices()
            .await
            .into_iter()
            .filter(|d| match d.os {
                crate::transport::DeviceOs::Android => want_android,
                crate::transport::DeviceOs::Ios => want_ios,
            })
            .collect();
        let existing = match crate::transport::detect::classify(
            matching,
            std::env::var("DRENGR_DEVICE").ok(),
        ) {
            Selection::Resolved(d) => Some(d),
            Selection::None => None,
            Selection::PinnedButMissing { wanted, available } => {
                return ToolResult::error(format!(
                    "DRENGR_DEVICE is set to `{wanted}`, which is not connected. Attached: {}",
                    available.join(", ")
                ));
            }
            Selection::Ambiguous(devices) => {
                return ToolResult::error(format!(
                    "{} devices are connected and none is pinned: {}. Set DRENGR_DEVICE to one of                      these ids, or pass `device` explicitly.",
                    devices.len(),
                    devices
                        .iter()
                        .map(|d| format!("{} ({})", d.id, d.model))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        };

        let (device, started_by_us) = if let Some(d) = existing {
            (d, false)
        } else if !headless {
            return ToolResult::error(format!(
                "No {} device is booted. Either start one manually (Android: `emulator -avd <name>`, iOS: `xcrun simctl boot <udid>`) or call again with `headless=true` to auto-boot.",
                if platform == "any" { "compatible" } else { platform }
            ));
        } else {
            // Auto-boot. On `platform=any`, try Android first — emulator boot
            // is faster on a clean CI runner than waiting for an iOS sim
            // runtime download. The preference only matters in the auto-boot
            // branch; an already-booted iOS sim above takes priority.
            let mut errors = serde_json::Map::new();
            let mut booted: Option<crate::transport::DetectedDevice> = None;

            if want_android {
                match crate::transport::boot::boot_android(None, !show_window).await {
                    Ok(b) => {
                        booted = Some(crate::transport::DetectedDevice {
                            id: b.id,
                            os: crate::transport::DeviceOs::Android,
                            model: "Android Emulator".to_string(),
                            sdk_version: None,
                        });
                    }
                    Err(e) => {
                        errors.insert("android".into(), json!(e.to_string()));
                    }
                }
            }

            if booted.is_none() && want_ios {
                match crate::transport::boot::boot_ios_simulator(None).await {
                    Ok(b) => {
                        booted = Some(crate::transport::DetectedDevice {
                            id: b.id,
                            os: crate::transport::DeviceOs::Ios,
                            model: "iOS Simulator".to_string(),
                            sdk_version: None,
                        });
                    }
                    Err(e) => {
                        errors.insert("ios".into(), json!(e.to_string()));
                    }
                }
            }

            match booted {
                Some(d) => (d, true),
                None => {
                    return ToolResult::text(
                        serde_json::to_string_pretty(&json!({
                            "ok": false,
                            "reason_code": "auto_boot_failed",
                            "errors": serde_json::Value::Object(errors),
                            "hint": "Create an AVD (`avdmanager create avd`) for Android, or download a Simulator runtime in Xcode > Settings > Components for iOS."
                        }))
                        .unwrap_or_default(),
                    );
                }
            }
        };

        let device_id = device.id.clone();
        let device_os = device.os.to_string();
        let device_model = device.model.clone();

        // Step 2: connect transport. Returning variant gives us the Arc to
        // use immediately — avoids a re-lock + TOCTOU window where another
        // caller could swap the registered transport between insert and read.
        let transport = crate::transport::create_transport(&device);
        let arc_transport = match self
            .set_transport_with_id_returning(device_id.clone(), transport)
            .await
        {
            Some(t) => t,
            None => {
                return ToolResult::error(format!("Invalid device id rejected: {:?}", device_id))
            }
        };

        // Step 3: list apps with display names.
        let apps = match arc_transport.list_apps_with_names().await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("setup: list_apps_with_names failed: {}", e);
                Vec::new()
            }
        };
        let filtered: Vec<&crate::transport::AppInfo> = apps
            .iter()
            .filter(|a| match app_kind {
                "user" => matches!(a.kind, crate::transport::AppKind::User),
                "system" => matches!(a.kind, crate::transport::AppKind::System),
                _ => true,
            })
            .collect();
        let filtered_total = filtered.len();
        let returned: Vec<&crate::transport::AppInfo> =
            filtered.into_iter().take(APPS_MAX).collect();
        let omitted = filtered_total.saturating_sub(returned.len());

        // First-ever setup on this machine → offer a 30-second live demo.
        let first_session = !crate::paths::drengr_dir()
            .map(|d| d.join("activated").exists())
            .unwrap_or(false);
        if first_session {
            if let Ok(dir) = crate::paths::ensure_drengr_dir() {
                let _ = std::fs::write(dir.join("activated"), chrono::Utc::now().to_rfc3339());
            }
        }
        let suggested_task = if device_os == "ios" {
            json!({ "app": "com.apple.Preferences", "task": "Turn on Airplane Mode" })
        } else {
            json!({ "app": "com.android.settings", "task": "Open the Network & internet settings" })
        };

        ToolResult::text(
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "device_id": device_id,
                "platform": device_os,
                "model": device_model,
                "auto_boot_requested": headless,
                "started_by_us": started_by_us,
                "app_kind": app_kind,
                "apps": returned,
                "app_count_omitted": omitted,
                "first_session": first_session,
                "suggested_task": suggested_task,
            }))
            .unwrap_or_default(),
        )
    }

    /// Handle drengr_query — read-only questions.
    pub(super) async fn handle_query(&self, args: Value) -> ToolResult {
        let question = match args.get("question").and_then(|q| q.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("Missing required parameter: question"),
        };

        // All queries are free. The only Pro gate is cloud_devices (checked in connect handler).
        // Daily usage limit is enforced in dispatch(), not here.

        match question {
            "capabilities" => {
                // Return the structured action+query catalog. The LLM reads this once
                // per session to discover the full surface (or any time it's stuck).
                // active_platform=None for now — actions tag their own platform support
                // and the LLM filters by the active device. Future: detect from transport.
                let hints_enabled = crate::situation::hints::hints_enabled();
                let body = crate::mcp::capabilities::capabilities_response(None, hints_enabled);
                ToolResult::text(serde_json::to_string_pretty(&body).unwrap_or_default())
            }
            "devices" => {
                let devices = crate::transport::detect::detect_devices().await;
                let connected = self.transports.lock().await;
                let active = self.active_device.lock().await;
                let device_list: Vec<Value> = devices
                    .iter()
                    .map(|d| {
                        let is_connected = connected.contains_key(&d.id);
                        let is_active = active.as_deref() == Some(&d.id);
                        json!({
                            "id": d.id,
                            "os": d.os.to_string(),
                            "model": d.model,
                            "connected": is_connected,
                            "active": is_active,
                        })
                    })
                    .collect();
                if device_list.is_empty() {
                    return ToolResult::text(
                        "No devices found.\n\n\
                         To connect a device:\n\
                         • Android: connect via USB (enable USB debugging) or start an emulator\n\
                         • iOS: open Simulator from Xcode (macOS only)\n\n\
                         Then call drengr_query(question='devices') again."
                            .to_string(),
                    );
                }
                ToolResult::text(
                    serde_json::to_string_pretty(&json!({"devices": device_list}))
                        .unwrap_or_default(),
                )
            }
            "activity" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                let activity = crate::transport::activity_or_unknown(transport.as_ref()).await;
                let package = activity.split('/').next().unwrap_or("").to_string();
                ToolResult::text(
                    serde_json::to_string_pretty(&json!({
                        "activity": activity,
                        "package": package,
                    }))
                    .unwrap_or_default(),
                )
            }
            "connect" => {
                let cloud = args.get("cloud").and_then(|c| c.as_str());
                if let Some(cloud_provider) = cloud {
                    let device_name = args
                        .get("device")
                        .and_then(|d| d.as_str())
                        .unwrap_or("default");
                    let os_version = args
                        .get("os_version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("latest");
                    let app = args.get("app").and_then(|a| a.as_str());
                    match crate::transport::create_cloud_transport(
                        cloud_provider,
                        device_name,
                        os_version,
                        app,
                    )
                    .await
                    {
                        Ok(t) => {
                            self.set_transport(t).await;
                            ToolResult::text(
                                serde_json::to_string_pretty(&json!({
                                    "connected": true,
                                    "cloud": cloud_provider,
                                    "device": device_name,
                                    "os_version": os_version,
                                }))
                                .unwrap_or_default(),
                            )
                        }
                        Err(e) => ToolResult::error(format!("Cloud connect failed: {}", e)),
                    }
                } else {
                    let requested_id = args.get("device").and_then(|d| d.as_str());

                    // Check if the device is already connected — just switch to it
                    if let Some(id) = requested_id {
                        let transports = self.transports.lock().await;
                        // Exact match, then prefix match (same logic as resolve_transport)
                        let matched_id = if transports.contains_key(id) {
                            Some(id.to_string())
                        } else {
                            transports
                                .keys()
                                .find(|k| k.starts_with(id) || id.starts_with(k.as_str()))
                                .cloned()
                        };
                        drop(transports);

                        if let Some(mid) = matched_id {
                            // Clone transport out of map, then drop lock before async call
                            let transport = self.transports.lock().await.get(&mid).cloned();
                            if let Some(t) = transport {
                                // Transport confirmed — now safe to set active
                                *self.active_device.lock().await = Some(mid.clone());
                                let info = t.device_info().await.ok();
                                return ToolResult::text(
                                    serde_json::to_string_pretty(&json!({
                                        "connected": true,
                                        "device": mid,
                                        "os": info.as_ref().map(|i| i.os.as_str()).unwrap_or("unknown"),
                                        "model": info.as_ref().map(|i| i.model.as_str()).unwrap_or("unknown"),
                                    }))
                                    .unwrap_or_default(),
                                );
                            }
                            // Transport disappeared — fall through to re-detect
                        }
                    }

                    // Not already connected — detect and create new transport
                    let devices = crate::transport::detect::detect_devices().await;
                    if devices.is_empty() {
                        return ToolResult::error("No devices found.");
                    }
                    let device = if let Some(id) = requested_id {
                        match devices.iter().find(|d| d.id == id).or_else(|| {
                            devices
                                .iter()
                                .find(|d| d.id.starts_with(id) || id.starts_with(&d.id))
                        }) {
                            Some(d) => d,
                            None => {
                                let available: Vec<&str> =
                                    devices.iter().map(|d| d.id.as_str()).collect();
                                return ToolResult::error(format!(
                                    "Device '{}' not found. Available: {:?}",
                                    id, available
                                ));
                            }
                        }
                    } else {
                        &devices[0]
                    };
                    let device_id = device.id.clone();
                    let device_os = device.os.to_string();
                    let device_model = device.model.clone();
                    let transport = crate::transport::create_transport(device);
                    self.set_transport_with_id(device_id.clone(), transport)
                        .await;
                    ToolResult::text(
                        serde_json::to_string_pretty(&json!({
                            "connected": true,
                            "device": device_id,
                            "os": device_os,
                            "model": device_model,
                        }))
                        .unwrap_or_default(),
                    )
                }
            }
            "ui_dump" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                match transport.raw_ui_tree().await {
                    Ok(xml) => ToolResult::text(xml),
                    Err(e) => ToolResult::error(format!("UI dump failed: {}", e)),
                }
            }
            "setup" => self.handle_setup_query(&args).await,
            "screen_stream" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                match transport.screen_stream_url().await {
                    Ok(Some(url)) => ToolResult::text(
                        serde_json::to_string_pretty(&json!({
                            "stream_url": url,
                            "type": "mjpeg",
                            "usage": "Use as <img src> in a browser or dashboard for live device mirroring"
                        }))
                        .unwrap_or_default(),
                    ),
                    Ok(None) => ToolResult::text(json!({
                        "stream_url": null,
                        "message": "Live streaming not yet available for this device. Use drengr_look for screenshots."
                    }).to_string()),
                    Err(e) => ToolResult::error(format!("screen_stream failed: {}", e)),
                }
            }
            "crash" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                let package = args.get("package").and_then(|p| p.as_str()).unwrap_or("");
                if !package.is_empty() && !crate::validate::is_valid_package_name(package) {
                    return ToolResult::error("Invalid package name");
                }
                let (reason, detail) = transport.death_report(package).await;
                let in_foreground = transport
                    .is_app_in_foreground(package)
                    .await
                    .unwrap_or(false);
                ToolResult::text(
                    serde_json::to_string_pretty(&json!({
                        // back-compat boolean; `reason` is the precise diagnosis:
                        // running | crashed | anr | killed | clean_exit | device_lost
                        "crashed": matches!(reason.as_str(), "crashed" | "anr"),
                        "in_foreground": in_foreground,
                        "reason": reason,
                        "detail": detail,
                    }))
                    .unwrap_or_default(),
                )
            }
            "find" => {
                let target = args.get("target").and_then(|t| t.as_str()).unwrap_or("");
                if target.is_empty() {
                    return ToolResult::error("find requires 'target' parameter");
                }

                let annotated_guard = self.last_annotated.lock().await;
                match annotated_guard.as_ref() {
                    Some(annotated) => {
                        let target_lower = target.to_lowercase();
                        let (best, total_hits) = best_label_match(
                            &annotated.elements,
                            &target_lower,
                            // Rank on the label we actually REPORT. Ranking on
                            // display_label made find answer match "exact" against
                            // a class name the response then refuses to show, which
                            // is the fake precision confidence: 1.0 was removed for.
                            |e| e.element.label_or_empty().to_string(),
                        );
                        match best.map(|i| &annotated.elements[i]) {
                            Some(e) => {
                                // Reuse the shared projection rather than hand-
                                // rolling a second one: this copy silently kept
                                // the three-field shape (no bounds, no state)
                                // after the main path grew them.
                                let label_lower = e.element.label_or_empty().to_lowercase();
                                let mut found = json!({
                                    "found": true,
                                    "match": query_match_kind(&label_lower, &target_lower),
                                    // Ambiguity the old hardcoded confidence hid:
                                    // several labels can contain the same needle.
                                    "matches": total_hits,
                                });
                                if let Some(el) =
                                    super::annotated_elements_to_json(std::slice::from_ref(e))
                                        .into_iter()
                                        .next()
                                {
                                    found["element"] = el;
                                }
                                ToolResult::text(
                                    serde_json::to_string_pretty(&found).unwrap_or_default(),
                                )
                            }
                            None => ToolResult::text(
                                serde_json::to_string_pretty(&json!({"found": false}))
                                    .unwrap_or_default(),
                            ),
                        }
                    }
                    // A one-shot CLI has no in-memory screen, but the `look`
                    // that numbered these elements left them on disk.
                    None => {
                        // Only a record for THIS device can answer: the numbers
                        // are coordinates in that device's own logical space.
                        let observation = match self.resolve_transport(&args).await {
                            Some(t) => ScreenAnnotator::observation_for_device(t.id()),
                            None => None,
                        };
                        let persisted = observation
                            .as_ref()
                            .map(|o| o.elements.as_slice())
                            .unwrap_or_default();
                        if persisted.is_empty() {
                            return ToolResult::error(
                                "No screen observed yet. Call drengr_look first.",
                            );
                        }
                        let target_lower = target.to_lowercase();
                        let (best, total_hits) =
                            best_label_match(persisted, &target_lower, |e| e.label.clone());
                        match best.map(|i| &persisted[i]) {
                            Some(e) => {
                                let label_lower = e.label.to_lowercase();
                                // Only what was actually stored: the disk record has
                                // no bounds or class, and inventing them would be a
                                // richer answer than the evidence supports.
                                // From disk, so it describes whatever screen the
                                // last process saw, not necessarily this one. Say
                                // so rather than claiming a plain hit.
                                let meta = observation.as_ref();
                                ToolResult::text(
                                    serde_json::to_string_pretty(&json!({
                                        "found": true,
                                        "from_disk": true,
                                        "observed_at": meta.map(|o| o.written_at.as_str()).unwrap_or(""),
                                        "observed_activity": meta.map(|o| o.activity.as_str()).unwrap_or(""),
                                        "match": query_match_kind(&label_lower, &target_lower),
                                        "matches": total_hits,
                                        "element": {
                                            "n": e.number,
                                            "text": e.label,
                                            "tap": [e.tap_x, e.tap_y],
                                        },
                                    }))
                                    .unwrap_or_default(),
                                )
                            }
                            None => ToolResult::text(
                                serde_json::to_string_pretty(&json!({"found": false}))
                                    .unwrap_or_default(),
                            ),
                        }
                    }
                }
            }
            "explore" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                let package = match args.get("package").and_then(|p| p.as_str()) {
                    Some(p) => p.to_string(),
                    None => return ToolResult::error("explore requires 'package' parameter"),
                };
                if !crate::validate::is_valid_package_name(&package) {
                    return ToolResult::error("Invalid package name");
                }
                let config = explore::ExploreConfig {
                    app_package: package.clone(),
                    max_screens: 15,
                    ..Default::default()
                };
                match explore::explore_app(transport.as_ref(), &config).await {
                    Ok(map) => {
                        let screens = map.screens.len();
                        let edges = map.edges.len();
                        let path = explore::save_screen_map(&map)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| "unknown".to_string());
                        *self.app_package.lock().await = Some(package);
                        ToolResult::text(
                            serde_json::to_string_pretty(&json!({
                                "screens": screens,
                                "edges": edges,
                                "saved_to": path,
                            }))
                            .unwrap_or_default(),
                        )
                    }
                    Err(e) => ToolResult::error(format!("Exploration failed: {}", e)),
                }
            }
            "session" => {
                let session_guard = self.session.lock().await;
                match session_guard.as_ref() {
                    Some(session) => ToolResult::text(
                        serde_json::to_string_pretty(&json!({
                            "session_id": session.id,
                            "app_package": session.app_package,
                            "started_at": session.started_at,
                            "steps": session.steps.len(),
                            "total_network_calls": session.total_network_calls(),
                            "total_network_errors": session.total_network_errors(),
                        }))
                        .unwrap_or_default(),
                    ),
                    None => ToolResult::text(
                        serde_json::to_string_pretty(&json!({"active_session": false}))
                            .unwrap_or_default(),
                    ),
                }
            }
            "logcat" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                let package = args.get("package").and_then(|p| p.as_str());
                let pkg = if let Some(p) = package {
                    p.to_string()
                } else {
                    self.app_package.lock().await.clone().unwrap_or_default()
                };
                if pkg.is_empty() {
                    return ToolResult::error(
                        "logcat requires 'package' parameter (or launch an app first)",
                    );
                }
                let filter = match args.get("filter").and_then(|f| f.as_str()) {
                    Some(f) => match crate::validate::sanitize_logcat_filter(f) {
                        Ok(sanitized) => Some(sanitized),
                        Err(e) => {
                            return ToolResult::error(format!("Invalid logcat filter: {}", e))
                        }
                    },
                    None => None,
                };
                let lines =
                    (args.get("lines").and_then(|l| l.as_u64()).unwrap_or(50) as usize).min(500);

                match transport.read_logs(&pkg, filter.as_deref(), lines).await {
                    Ok(entries) => {
                        let count = entries.len();
                        ToolResult::text(
                            serde_json::to_string_pretty(&json!({
                                "package": pkg,
                                "entries": entries,
                                "count": count,
                            }))
                            .unwrap_or_default(),
                        )
                    }
                    Err(e) => ToolResult::error(format!("Failed to read logs: {}", e)),
                }
            }
            "keyboard" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                let visible = transport.is_keyboard_visible().await.unwrap_or(false);
                let (_, sh) = transport
                    .screen_size()
                    .await
                    .unwrap_or(crate::transport::DEFAULT_SCREEN_SIZE);
                let height_estimate = if visible { (sh as f64 * 0.4) as u32 } else { 0 };
                ToolResult::text(
                    serde_json::to_string_pretty(&json!({
                        "visible": visible,
                        "height_estimate": height_estimate,
                    }))
                    .unwrap_or_default(),
                )
            }
            "network" => {
                let last = args.get("last").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
                let url_filter = args.get("url_filter").and_then(|u| u.as_str());
                let status_filter = args
                    .get("status_filter")
                    .and_then(|s| s.as_u64())
                    .map(|s| s as u16);

                let history = self.network_history.snapshot();
                let mut events = history.clone();

                if let Some(url_pat) = url_filter {
                    let pat_lower = url_pat.to_lowercase();
                    events.retain(|e| e.event.url.to_lowercase().contains(&pat_lower));
                }
                if let Some(status) = status_filter {
                    // An event whose status was never captured does not match a
                    // status filter. It is excluded because it is unknown, not
                    // because it is known to differ.
                    events.retain(|e| e.event.status == Some(status));
                }
                if let Some(want) = args.get("source").and_then(|s| s.as_str()) {
                    events.retain(|e| e.source.as_str() == want);
                }

                let start = events.len().saturating_sub(last);
                let recent = &events[start..];

                // Sources see different things, so the answer says which saw
                // what. A merged list with no provenance would read as one
                // fidelity and quietly overstate the logcat and SDK entries.
                let (sources, fidelity) = crate::network::sink::provenance(recent);
                ToolResult::text(
                    serde_json::to_string_pretty(&json!({
                        "total_captured": history.len(),
                        "returned": recent.len(),
                        "sources": sources,
                        "fidelity": fidelity,
                        "calls": crate::network::sink::summarize(recent),
                    }))
                    .unwrap_or_default(),
                )
            }
            "app_state" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                let pkg_arg = args
                    .get("package")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string());
                let pkg = match pkg_arg {
                    Some(p) if !p.is_empty() => p,
                    _ => {
                        let tracked = self.app_package.lock().await.clone().unwrap_or_default();
                        if tracked.is_empty() {
                            return ToolResult::error("app_state requires 'package' parameter");
                        }
                        tracked
                    }
                };

                let crashed = transport.check_crash_logcat(&pkg).await;
                let lifecycle = transport.app_state(&pkg).await;
                let activity = if matches!(lifecycle, Ok(4)) {
                    crate::transport::activity_or_unknown(transport.as_ref()).await
                } else {
                    String::new()
                };

                let lifecycle_name = || match &lifecycle {
                    Ok(s) => Ok(crate::transport::app_state_name(*s).to_string()),
                    Err(e) => Err(format!("app_state failed: {}", e)),
                };
                // A crash check that could not run says so; it never lends its
                // silence to the lifecycle answer as though the app were fine.
                let (state, crash_check) = match &crashed {
                    Ok(true) => ("crashed".to_string(), "crashed".to_string()),
                    Ok(false) => match lifecycle_name() {
                        Ok(s) => (s, "clean".to_string()),
                        Err(e) => return ToolResult::error(e),
                    },
                    Err(e) => match lifecycle_name() {
                        Ok(s) => (s, format!("unavailable: {}", e)),
                        Err(e) => return ToolResult::error(e),
                    },
                };

                ToolResult::text(
                    serde_json::to_string_pretty(&json!({
                        "state": state,
                        "crash_check": crash_check,
                        "activity": activity,
                        "package": pkg,
                    }))
                    .unwrap_or_default(),
                )
            }
            "assert" => {
                let conditions_str = args
                    .get("conditions")
                    .and_then(|c| c.as_str())
                    .unwrap_or("[]");
                let conditions = match crate::validate::validate_assert_conditions(conditions_str) {
                    Ok(c) => c,
                    Err(e) => return ToolResult::error(format!("Invalid conditions: {}", e)),
                };

                let annotated_guard = self.last_annotated.lock().await;
                let annotated = match annotated_guard.as_ref() {
                    Some(a) => a,
                    None => {
                        return ToolResult::error("No screen observed yet. Call drengr_look first.")
                    }
                };

                let mut results = Vec::new();
                let mut all_pass = true;

                for cond in &conditions {
                    let target_text = cond.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    let expected_visible = cond
                        .get("visible")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let expected_type = cond.get("type").and_then(|t| t.as_str());

                    let target_lower = target_text.to_lowercase();
                    let found = annotated.elements.iter().find(|e| {
                        // Same rule as find: an assertion that passes against a
                        // class name is a false positive where they cost most.
                        e.element
                            .label_or_empty()
                            .to_lowercase()
                            .contains(&target_lower)
                    });

                    let (met, reason) = match (found, expected_visible) {
                        (Some(e), true) => {
                            if let Some(exp_type) = expected_type {
                                if e.element
                                    .short_class()
                                    .to_lowercase()
                                    .contains(&exp_type.to_lowercase())
                                {
                                    (
                                        true,
                                        format!(
                                            "Found '{}' as {}",
                                            target_text,
                                            e.element.short_class()
                                        ),
                                    )
                                } else {
                                    (
                                        false,
                                        format!(
                                            "Found '{}' but type is {} (expected {})",
                                            target_text,
                                            e.element.short_class(),
                                            exp_type
                                        ),
                                    )
                                }
                            } else {
                                (true, format!("Found '{}'", target_text))
                            }
                        }
                        (None, true) => (false, format!("'{}' not found on screen", target_text)),
                        (Some(_), false) => (
                            false,
                            format!("'{}' is visible (expected not visible)", target_text),
                        ),
                        (None, false) => (true, format!("'{}' correctly not visible", target_text)),
                    };

                    if !met {
                        all_pass = false;
                    }
                    results.push(json!({ "condition": cond, "met": met, "reason": reason }));
                }

                ToolResult::text(
                    serde_json::to_string_pretty(&json!({
                        "pass": all_pass,
                        "results": results,
                    }))
                    .unwrap_or_default(),
                )
            }
            "diff" => {
                let transport = match self.resolve_transport(&args).await {
                    Some(t) => t,
                    None => return ToolResult::error(NO_DEVICE_HINT),
                };
                let baseline_str = match args.get("baseline").and_then(|b| b.as_str()) {
                    Some(p) => p,
                    None => {
                        return ToolResult::error(
                            "diff requires 'baseline' parameter (path to PNG)",
                        )
                    }
                };
                let baseline_path = match crate::validate::validate_file_path(baseline_str, ".png")
                {
                    Ok(p) => p,
                    Err(e) => return ToolResult::error(format!("Invalid baseline path: {}", e)),
                };
                let baseline_data = match std::fs::read(&baseline_path) {
                    Ok(d) => d,
                    Err(e) => return ToolResult::error(format!("Failed to read baseline: {}", e)),
                };
                let current_data = match transport.screenshot().await {
                    Ok(d) => d,
                    Err(e) => return ToolResult::error(format!("Screenshot failed: {}", e)),
                };

                match crate::screen::optimize::pixel_diff_percentage(&baseline_data, &current_data)
                {
                    Ok((percentage, changed, total)) => ToolResult::text(
                        serde_json::to_string_pretty(&json!({
                            "diff_percentage": format!("{:.1}", percentage),
                            "changed_pixels": changed,
                            "total_pixels": total,
                            "baseline": baseline_path,
                        }))
                        .unwrap_or_default(),
                    ),
                    Err(e) => ToolResult::error(format!("Diff failed: {}", e)),
                }
            }
            "analyze" => self.handle_analyze().await,
            _ => ToolResult::error(format!("Unknown question: {}", question)),
        }
    }

    /// Handle analyze query — structured session analysis with tiered output.
    /// Free: step count + teaser. Pro: full analysis. Team: element-level audit.
    pub(super) async fn handle_analyze(&self) -> ToolResult {
        let session_guard = self.session.lock().await;
        let session = match session_guard.as_ref() {
            Some(s) => s,
            None => return ToolResult::error("No active session. Launch an app first with drengr_do(action='launch', package='...')"),
        };

        let steps = &session.steps;
        let network_events = self.network_history.snapshot();

        let mut stuck_points = Vec::new();
        let mut screen_changes = 0u32;
        for step in steps {
            if !step.screen_changed {
                stuck_points.push(json!({
                    "step": step.step,
                    "action": step.action,
                    "activity": step.activity,
                }));
            } else {
                screen_changes += 1;
            }
        }

        // A call whose status was never captured is not an error and is not a
        // success. Counting it either way would be a claim the capture cannot
        // support, so it is reported on its own line instead.
        let network_unverifiable = network_events
            .iter()
            .filter(|e| e.event.status.is_none())
            .count();
        let network_errors: Vec<_> = network_events
            .iter()
            .filter(|e| e.event.is_error() == Some(true))
            .map(|e| {
                json!({
                    "url": e.event.url,
                    "status": e.event.status,
                    "method": e.event.method,
                    "source": e.source.as_str(),
                })
            })
            .collect();

        let mut analysis = json!({
            "session": {
                "app": &session.app_package,
                "device": &session.device_id,
                "total_steps": steps.len(),
                "screen_changes": screen_changes,
            },
            "stuck_points": stuck_points,
            "network": {
                "total_calls": network_events.len(),
                "errors": network_errors,
                "unverifiable": network_unverifiable,
            },
            "efficiency": {
                "stuck_ratio": if steps.is_empty() { 0.0 } else {
                    stuck_points.len() as f64 / steps.len() as f64
                },
            },
        });

        {
            // Outside the analyze_full gate on purpose: a count is not a full audit,
            // and an app shipping untappable-by-name controls should hear about it.
            // Always emitted, so elements_checked: 0 says "nothing examined" rather
            // than the key vanishing and looking like a clean bill of health.
            let annotated_guard = self.last_annotated.lock().await;
            let (issues, checked) = match annotated_guard.as_ref() {
                Some(a) => (
                    super::clickable_without_label(&a.elements),
                    a.elements.len(),
                ),
                None => (0, 0),
            };
            analysis["accessibility"] = json!({
                "clickable_without_label": issues,
                "elements_checked": checked,
            });
        }

        {
            let annotated_guard = self.last_annotated.lock().await;
            if let Some(ref a) = *annotated_guard {
                let element_audit: Vec<_> = a
                    .elements
                    .iter()
                    .take(20)
                    .map(|e| {
                        // An audit that reports an unlabelled node as labelled
                        // "View" hides the exact defect an audit exists to find,
                        // because display_label() falls back to the class name.
                        let labelled = e.element.is_labelled();
                        json!({
                            "number": e.number,
                            "label": e.element.label_or_empty(),
                            // An audit wants the field present even when false: it is
                            // reporting ON these properties, not carrying them along.
                            "unlabelled": !labelled,
                            "disabled": !e.element.enabled,
                            "type": e.element.class,
                            "clickable": e.element.clickable,
                        })
                    })
                    .collect();
                analysis["element_audit"] = json!(element_audit);
            }
        }

        ToolResult::text(serde_json::to_string_pretty(&analysis).unwrap_or_default())
    }
}
