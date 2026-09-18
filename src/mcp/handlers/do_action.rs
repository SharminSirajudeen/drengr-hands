use super::*;

/// Poll until the requested condition holds or the timeout expires. Every branch
/// returns a description rather than an error: a wait that timed out still waited.
async fn do_wait(transport: &dyn DeviceTransport, args: &Value) -> String {
    let until = args.get("until").and_then(|u| u.as_str());
    let timeout_secs = args.get("timeout").and_then(|t| t.as_u64()).unwrap_or(5);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    match until {
        Some("stable") => {
            let mut last_frame: Vec<u8> = Vec::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if std::time::Instant::now() > deadline {
                    break;
                }
                let s = transport.screenshot().await.unwrap_or_default();
                if !last_frame.is_empty()
                    && crate::screen::optimize::frames_settled(&last_frame, &s)
                {
                    break;
                }
                last_frame = s;
            }
            "Waited for screen to stabilize".to_string()
        }
        Some(cond) if cond.starts_with("network:idle") => {
            let _ = transport.clear_http_logs().await;
            let mut total_calls = 0u32;
            let mut idle_polls = 0u32;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if std::time::Instant::now() > deadline {
                    break;
                }
                let calls = transport.capture_http_logs().await.unwrap_or_default();
                if calls.is_empty() {
                    idle_polls += 1;
                    if idle_polls >= 2 {
                        break; // 1s of network silence
                    }
                } else {
                    total_calls += calls.len() as u32;
                    idle_polls = 0;
                    let _ = transport.clear_http_logs().await;
                }
            }
            format!("Network idle ({} calls observed)", total_calls)
        }
        Some(cond) if cond.starts_with("element:") => {
            let target = &cond["element:".len()..];
            let target_lower = target.to_lowercase();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if std::time::Instant::now() > deadline {
                    break;
                }
                let elems = transport.ui_tree().await.unwrap_or_default();
                if elems.iter().any(|e| e.matches_text(&target_lower)) {
                    break;
                }
            }
            format!("Waited for element '{}'", target)
        }
        _ => {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            "Waited 1s".to_string()
        }
    }
}

/// Swipe toward one edge until the screen stops changing or the swipe budget is
/// spent, and report how many swipes it took. Byte-identical re-encoded PNGs are
/// the strictest possible test, which is why frames_settled exists: a blinking
/// cursor kept this loop swiping a list that had stopped moving.
async fn scroll_to_edge(
    transport: &dyn DeviceTransport,
    direction: &str,
    screen_width: u32,
    screen_height: u32,
) -> u32 {
    let mut last_frame: Vec<u8> = Vec::new();
    let mut swipe_count = 0u32;
    for _ in 0..crate::transport::MAX_SCROLL_SWIPES {
        let (from, to) = crate::transport::swipe_coords(direction, screen_width, screen_height);
        if transport
            .swipe(from, to, crate::transport::DEFAULT_SWIPE_DURATION_MS)
            .await
            .is_err()
        {
            break;
        }
        swipe_count += 1;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let s = transport.screenshot().await.unwrap_or_default();
        if !last_frame.is_empty() && crate::screen::optimize::frames_settled(&last_frame, &s) {
            break;
        }
        last_frame = s;
    }
    swipe_count
}

impl McpHandlers {
    /// Resolve element `n` to a tap point: this process's last look, or what a
    /// previous CLI process left on disk. Every action arm goes through here.
    /// The arms that called tap_coordinates directly resolved nothing from a
    /// one-shot CLI, so `tap --element N` worked while `long_press --element N`
    /// did not.
    async fn resolve_element(
        &self,
        transport: &dyn DeviceTransport,
        n: usize,
    ) -> Option<(i32, i32)> {
        let annotated_guard = self.last_annotated.lock().await;
        if let Some(a) = annotated_guard.as_ref() {
            return ScreenAnnotator::tap_coordinates(a, n);
        }
        drop(annotated_guard);
        ScreenAnnotator::observation_for_device(transport.id())?
            .elements
            .iter()
            .find(|e| e.number == n)
            .map(|e| (e.tap_x, e.tap_y))
    }

    /// The soft keyboard overlays the bottom of the screen, so a tap aimed under
    /// it lands on a key instead of the target. Dismiss first, then tap.
    async fn tap_clear_of_keyboard(
        &self,
        transport: &dyn DeviceTransport,
        x: i32,
        y: i32,
        screen_height: u32,
    ) -> Result<(), String> {
        if let Ok(true) = transport.is_keyboard_visible().await {
            let keyboard_top = (screen_height as f64 * 0.6) as i32;
            if y > keyboard_top {
                let _ = transport.dismiss_keyboard().await;
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
        transport
            .tap(x, y)
            .await
            .map_err(|e| format!("Tap failed: {}", e))
    }

    /// Tap one of four ways, in priority order: normalized coordinates, visible
    /// text, an element number, or an element number found by scrolling.
    async fn do_tap(
        &self,
        transport: &dyn DeviceTransport,
        args: &Value,
        element_num: Option<usize>,
        screen_width: u32,
        screen_height: u32,
    ) -> Result<String, String> {
        // Framework-blind coordinate tap (the north-star primitive): normalized
        // 0-1 → native tap space. The only way to act on apps with no
        // accessibility tree — Flutter, games, in-app web, canvas.
        if let (Some(nx), Some(ny)) = (
            args.get("x").and_then(|v| v.as_f64()),
            args.get("y").and_then(|v| v.as_f64()),
        ) {
            let (px, py) = norm_to_px(nx, ny, screen_width, screen_height);
            transport
                .tap(px, py)
                .await
                .map_err(|e| format!("Tap failed: {}", e))?;
            return Ok(format!(
                "Tapped ({:.3}, {:.3})",
                nx.clamp(0.0, 1.0),
                ny.clamp(0.0, 1.0)
            ));
        }

        let max_scroll = args
            .get("max_scroll")
            .and_then(|m| m.as_u64())
            .unwrap_or(12) as usize;

        if let Some(target) = args.get("element_text").and_then(|t| t.as_str()) {
            let found = self
                .scroll_to_find_element(
                    transport,
                    target,
                    max_scroll,
                    (screen_width, screen_height),
                )
                .await;
            let (x, y) = found.ok_or_else(|| format!(
                "Element '{}' not found after {} scroll attempts. Retry with a higher max_scroll (e.g. max_scroll=30), or scroll_to_top first if you may have scrolled past it.",
                target, max_scroll
            ))?;
            self.tap_clear_of_keyboard(transport, x, y, screen_height)
                .await?;
            return Ok(format!("Tapped '{}'", target));
        }

        let n = element_num.ok_or("tap requires 'element' or 'element_text' parameter")?;
        if let Some((x, y)) = self.resolve_element(transport, n).await {
            self.tap_clear_of_keyboard(transport, x, y, screen_height)
                .await?;
            return Ok(format!("Tapped #{}", n));
        }

        let scroll_to_find = args
            .get("scroll_to_find")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        if !scroll_to_find {
            return Err(format!(
                "Element #{} not found. Use scroll_to_find=true to search off-screen.",
                n
            ));
        }

        let label = {
            let annotated = self.last_annotated.lock().await;
            annotated.as_ref().and_then(|a| {
                a.elements
                    .iter()
                    .find(|e| e.number == n)
                    .map(|e| e.element.display_label().to_string())
            })
        }
        .ok_or_else(|| format!("Element #{} not found", n))?;

        let (x, y) = self
            .scroll_to_find_element(transport, &label, max_scroll, (screen_width, screen_height))
            .await
            .ok_or_else(|| format!("Element '{}' not found after scrolling", label))?;
        transport
            .tap(x, y)
            .await
            .map_err(|e| format!("Tap failed: {}", e))?;
        Ok(format!("Scrolled to and tapped '{}'", label))
    }

    /// Record this package as the session's app and open a session for it.
    async fn start_session_for(&self, transport: &dyn DeviceTransport, pkg: &str) {
        *self.app_package.lock().await = Some(pkg.to_string());
        let device_id = transport
            .device_info()
            .await
            .map(|d| d.id)
            .unwrap_or_else(|_| "unknown".to_string());
        self.start_session(pkg, &device_id).await;
    }

    /// Tap an element so the field under it takes focus before text is sent.
    /// A tap that resolves to nothing is not an error here: the caller may be
    /// typing into whatever already holds focus.
    async fn focus_element(&self, transport: &dyn DeviceTransport, element_num: Option<usize>) {
        let Some(n) = element_num else { return };
        if let Some((x, y)) = self.resolve_element(transport, n).await {
            let _ = transport.tap(x, y).await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }

    /// Handle drengr_do — execute action + return situation report.
    pub(super) async fn handle_do(&self, args: Value) -> ToolResult {
        let transport = match self.ensure_or_autoprovision(&args).await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(e),
        };

        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("Missing required parameter: action"),
        };

        let element_num = args
            .get("element")
            .and_then(|e| e.as_u64())
            .map(|n| n as usize);
        let text = args.get("text").and_then(|t| t.as_str());
        let direction = args.get("direction").and_then(|d| d.as_str());
        let package = args.get("package").and_then(|p| p.as_str());
        let format = args
            .get("format")
            .and_then(|f| f.as_str())
            .unwrap_or_else(|| super::default_format());

        if let Err(e) = super::check_format(format, super::DO_FORMATS) {
            return ToolResult::error(e);
        }

        // Hoist screen_size once (avoids redundant ADB call in swipe + post-action)
        let (screen_width, screen_height) = transport
            .screen_size()
            .await
            .unwrap_or(crate::transport::DEFAULT_SCREEN_SIZE);

        // The frame BEFORE markers were drawn on it. Hashing the annotated JPEG
        // and comparing it to a later raw PNG compared two encodings of two
        // different images, so it never matched and every swipe reported that the
        // screen had changed.
        let pre_frame: Option<Arc<AnnotatedScreen>> = if action == "swipe" {
            self.last_annotated.lock().await.clone()
        } else {
            None
        };

        let _ = transport.clear_http_logs().await;

        // Opens the window this step will record. Everything the sink receives
        // from here on belongs to this action, whichever source saw it.
        let step_started_ms = crate::session::now_ms();

        let action_desc = match action {
            "tap" => match self
                .do_tap(
                    transport.as_ref(),
                    &args,
                    element_num,
                    screen_width,
                    screen_height,
                )
                .await
            {
                Ok(desc) => desc,
                Err(e) => return ToolResult::error(e),
            },
            "type" => {
                let input_text = match text {
                    Some(t) => t,
                    None => return ToolResult::error("type requires 'text' parameter"),
                };
                self.focus_element(transport.as_ref(), element_num).await;
                if let Err(e) = transport.type_text(input_text).await {
                    return ToolResult::error(format!("Type failed: {}", e));
                }
                format!("Typed \"{}\"", input_text)
            }
            "swipe" => {
                if let (Some(nx), Some(ny), Some(nx2), Some(ny2)) = (
                    args.get("x").and_then(|v| v.as_f64()),
                    args.get("y").and_then(|v| v.as_f64()),
                    args.get("x2").and_then(|v| v.as_f64()),
                    args.get("y2").and_then(|v| v.as_f64()),
                ) {
                    let (fx, fy) = norm_to_px(nx, ny, screen_width, screen_height);
                    let (tx, ty) = norm_to_px(nx2, ny2, screen_width, screen_height);
                    let from = crate::screen::Point { x: fx, y: fy };
                    let to = crate::screen::Point { x: tx, y: ty };
                    if let Err(e) = transport
                        .swipe(from, to, crate::transport::DEFAULT_SWIPE_DURATION_MS)
                        .await
                    {
                        return ToolResult::error(format!("Swipe failed: {}", e));
                    }
                    format!("Swiped ({:.2},{:.2})→({:.2},{:.2})", nx, ny, nx2, ny2)
                } else {
                    let dir =
                        match direction {
                            Some(d) => d,
                            None => return ToolResult::error(
                                "swipe requires 'direction', or normalized x/y/x2/y2 coordinates",
                            ),
                        };
                    let (from, to) =
                        crate::transport::swipe_coords(dir, screen_width, screen_height);
                    if let Err(e) = transport
                        .swipe(from, to, crate::transport::DEFAULT_SWIPE_DURATION_MS)
                        .await
                    {
                        return ToolResult::error(format!("Swipe failed: {}", e));
                    }
                    format!("Swiped {}", dir)
                }
            }
            "long_press" => {
                // Hold time is meaningful to apps where press depth or duration
                // carries intent, so it is a parameter rather than a constant.
                let hold_ms = args
                    .get("duration_ms")
                    .and_then(|v| v.as_u64())
                    .map(|v| v.clamp(50, 10_000) as u32)
                    .unwrap_or(1000);
                if let (Some(nx), Some(ny)) = (
                    args.get("x").and_then(|v| v.as_f64()),
                    args.get("y").and_then(|v| v.as_f64()),
                ) {
                    let (px, py) = norm_to_px(nx, ny, screen_width, screen_height);
                    if let Err(e) = transport.long_press(px, py, hold_ms).await {
                        return ToolResult::error(format!("Long press failed: {}", e));
                    }
                    format!(
                        "Long pressed ({:.3}, {:.3}) for {}ms",
                        nx.clamp(0.0, 1.0),
                        ny.clamp(0.0, 1.0),
                        hold_ms
                    )
                } else {
                    let n = match element_num {
                        Some(n) => n,
                        None => {
                            return ToolResult::error(
                                "long_press requires 'element' or normalized x/y",
                            )
                        }
                    };
                    let coords = self.resolve_element(transport.as_ref(), n).await;
                    match coords {
                        Some((x, y)) => {
                            if let Err(e) = transport.long_press(x, y, hold_ms).await {
                                return ToolResult::error(format!("Long press failed: {}", e));
                            }
                            format!("Long pressed #{} for {}ms", n, hold_ms)
                        }
                        None => return ToolResult::error(format!("Element #{} not found", n)),
                    }
                }
            }
            "back" => {
                if let Err(e) = transport.press_key(crate::transport::keycode::BACK).await {
                    return ToolResult::error(format!("Back failed: {}", e));
                }
                "Pressed back".to_string()
            }
            "home" => {
                if let Err(e) = transport.press_key(crate::transport::keycode::HOME).await {
                    return ToolResult::error(format!("Home failed: {}", e));
                }
                "Pressed home".to_string()
            }
            "launch" => {
                let pkg = match package {
                    Some(p) => p,
                    None => return ToolResult::error("launch requires 'package' parameter"),
                };
                if let Err(e) = transport.launch_app(pkg).await {
                    return ToolResult::error(format!("Launch failed: {}", e));
                }
                self.start_session_for(transport.as_ref(), pkg).await;
                for _ in 0..6 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(activity) = transport.current_activity().await {
                        if activity.contains(pkg) {
                            break;
                        }
                    }
                }
                format!("Launched {}", pkg)
            }
            "key" => {
                let keycode_str = args.get("keycode").and_then(|k| k.as_str()).unwrap_or("");
                let keycode = match keycode_str.to_lowercase().as_str() {
                    "enter" | "return" => crate::transport::keycode::ENTER,
                    "delete" | "backspace" | "del" => crate::transport::keycode::DELETE,
                    "tab" => crate::transport::keycode::TAB,
                    "back" => crate::transport::keycode::BACK,
                    "home" => crate::transport::keycode::HOME,
                    "move_end" | "end" => crate::transport::keycode::MOVE_END,
                    other => other.parse::<i32>().unwrap_or(0),
                };
                if keycode == 0 {
                    return ToolResult::error(
                        "key requires 'keycode' parameter (named key or numeric code)",
                    );
                }
                if let Err(e) = transport.press_key(keycode).await {
                    return ToolResult::error(format!("Key failed: {}", e));
                }
                format!("Pressed key '{}'", keycode_str)
            }
            "start_recording" => match transport.start_recording().await {
                Ok(path) => format!("Recording started → {}", path),
                Err(e) => return ToolResult::error(format!("start_recording failed: {}", e)),
            },
            "stop_recording" => match transport.stop_recording().await {
                Ok(path) => format!("Recording stopped → {}", path),
                Err(e) => return ToolResult::error(format!("stop_recording failed: {}", e)),
            },
            "install" => {
                let apk_path = match args.get("apk").and_then(|a| a.as_str()) {
                    Some(p) => p,
                    None => return ToolResult::error("install requires 'apk' parameter"),
                };
                // Validate against the extension the path actually carries. Trying
                // .apk first and reporting its error meant a missing .app bundle was
                // reported as the wrong file extension, which sends the user after a
                // problem they do not have.
                let ext = [".apk", ".ipa", ".app"]
                    .into_iter()
                    .find(|e| apk_path.ends_with(e))
                    .unwrap_or(".apk");
                if let Err(e) = crate::validate::validate_file_path(apk_path, ext) {
                    return ToolResult::error(format!("Invalid app path: {}", e));
                }
                if let Err(e) = transport.install_app(apk_path).await {
                    return ToolResult::error(format!("Install failed: {}", e));
                }
                if let Some(pkg) = package {
                    if let Err(e) = transport.launch_app(pkg).await {
                        return ToolResult::error(format!("Installed but launch failed: {}", e));
                    }
                    self.start_session_for(transport.as_ref(), pkg).await;
                    format!("Installed and launched {}", pkg)
                } else {
                    format!("Installed {}", apk_path)
                }
            }
            "clear_and_type" => {
                let input_text = match text {
                    Some(t) => t,
                    None => return ToolResult::error("clear_and_type requires 'text' parameter"),
                };
                self.focus_element(transport.as_ref(), element_num).await;
                if let Err(e) = transport.clear_focused_field().await {
                    return ToolResult::error(format!("Clear failed: {}", e));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if let Err(e) = transport.type_text(input_text).await {
                    return ToolResult::error(format!("Type failed: {}", e));
                }
                format!("Cleared and typed \"{}\"", input_text)
            }
            "scroll_to_bottom" => format!(
                "Scrolled to bottom ({} swipes)",
                scroll_to_edge(transport.as_ref(), "up", screen_width, screen_height).await
            ),
            "scroll_to_top" => format!(
                "Scrolled to top ({} swipes)",
                scroll_to_edge(transport.as_ref(), "down", screen_width, screen_height).await
            ),
            "wait" => do_wait(transport.as_ref(), &args).await,
            "draw_path" => {
                let pts = match parse_points(args.get("points")) {
                    Some(p) if p.len() >= 2 => p,
                    _ => return ToolResult::error(
                        "draw_path requires 'points' as >=2 [x,y] pairs, e.g. points=[[100,400],[100,800]]",
                    ),
                };
                let duration_ms = args
                    .get("duration_ms")
                    .and_then(|d| d.as_u64())
                    .unwrap_or(800) as u32;
                let points: Vec<crate::screen::ui_element::Point> = pts
                    .iter()
                    .map(|(x, y)| crate::screen::ui_element::Point::new(*x, *y))
                    .collect();
                if let Err(e) = transport.draw_path(&points, duration_ms).await {
                    return ToolResult::error(format!("draw_path failed: {}", e));
                }
                format!("Drew path ({} points)", points.len())
            }
            "swipe_with_velocity" => {
                let dir = match direction {
                    Some(d) => d,
                    None => {
                        return ToolResult::error(
                            "swipe_with_velocity requires 'direction' parameter",
                        )
                    }
                };
                let velocity = args
                    .get("velocity")
                    .and_then(|v| {
                        v.as_f64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    })
                    .unwrap_or(2000.0) as f32;
                let (from, to) = crate::transport::swipe_coords(dir, screen_width, screen_height);
                if let Err(e) = transport.swipe_with_velocity(from, to, velocity).await {
                    return ToolResult::error(format!("swipe_with_velocity failed: {}", e));
                }
                format!("Swiped {} at {} pts/s", dir, velocity)
            }
            "go_home" => {
                if let Err(e) = transport.go_home().await {
                    return ToolResult::error(format!("go_home failed: {}", e));
                }
                "Went to home screen".to_string()
            }
            "launch_app" => {
                let pkg = match package {
                    Some(p) => p,
                    None => return ToolResult::error("launch_app requires 'package' parameter"),
                };
                if let Err(e) = transport.launch_app(pkg).await {
                    return ToolResult::error(format!("launch_app failed: {}", e));
                }
                *self.app_package.lock().await = Some(pkg.to_string());
                format!("Launched {}", pkg)
            }
            "terminate_app" => {
                let pkg = match package {
                    Some(p) => p,
                    None => return ToolResult::error("terminate_app requires 'package' parameter"),
                };
                if let Err(e) = transport.terminate_app(pkg).await {
                    return ToolResult::error(format!("terminate_app failed: {}", e));
                }
                format!("Terminated {}", pkg)
            }
            "open_url" | "deep_link" => {
                let url = match args.get("url").and_then(|u| u.as_str()) {
                    Some(u) => u,
                    None => return ToolResult::error(format!("{action} requires 'url' parameter")),
                };
                if let Err(e) = transport.open_url(url).await {
                    return ToolResult::error(format!("{action} failed: {}", e));
                }
                if action == "deep_link" {
                    format!("Opened deep link {}", url)
                } else {
                    format!("Opened {}", url)
                }
            }
            "spotlight_search" => {
                let query = match args.get("query").and_then(|q| q.as_str()).or(text) {
                    Some(q) => q,
                    None => {
                        return ToolResult::error(
                            "spotlight_search requires 'query' (or 'text') parameter",
                        )
                    }
                };
                if let Err(e) = transport.spotlight_search(query).await {
                    return ToolResult::error(format!("spotlight_search failed: {}", e));
                }
                format!("Spotlight searched '{}'", query)
            }
            "clear_app_data" => {
                let pkg = match package {
                    Some(p) => p,
                    None => {
                        return ToolResult::error("clear_app_data requires 'package' parameter")
                    }
                };
                if let Err(e) = transport.clear_app_data(pkg).await {
                    return ToolResult::error(format!("clear_app_data failed: {}", e));
                }
                format!("Cleared data for {}", pkg)
            }
            "reset_app" => {
                let pkg = match package {
                    Some(p) => p,
                    None => return ToolResult::error("reset_app requires 'package' parameter"),
                };
                if let Err(e) = transport.reset_app(pkg).await {
                    return ToolResult::error(format!("reset_app failed: {}", e));
                }
                format!("Reset {}", pkg)
            }
            "list_installed_apps" => match transport.list_installed_apps().await {
                Ok(apps) => format!("Installed apps ({}): {}", apps.len(), apps.join(", ")),
                Err(e) => return ToolResult::error(format!("list_installed_apps failed: {}", e)),
            },
            "uninstall" => {
                let pkg = match package {
                    Some(p) => p,
                    None => return ToolResult::error("uninstall requires 'package' parameter"),
                };
                if let Err(e) = transport.uninstall_app(pkg).await {
                    return ToolResult::error(format!("uninstall failed: {}", e));
                }
                format!("Uninstalled {}", pkg)
            }
            "set_location" => {
                let (lat, lng) = match (
                    args.get("lat").and_then(|v| v.as_f64()),
                    args.get("lng").and_then(|v| v.as_f64()),
                ) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return ToolResult::error("set_location requires numeric 'lat' and 'lng'"),
                };
                if let Err(e) = transport.set_location(lat, lng).await {
                    return ToolResult::error(format!("set_location failed: {}", e));
                }
                format!("Set location to {},{}", lat, lng)
            }
            "clear_location" => {
                if let Err(e) = transport.clear_location().await {
                    return ToolResult::error(format!("clear_location failed: {}", e));
                }
                "Cleared simulated location".to_string()
            }
            "set_appearance" => {
                let dark = args
                    .get("dark")
                    .and_then(|v| v.as_bool())
                    .unwrap_or_else(|| {
                        args.get("dark")
                            .and_then(|v| v.as_str())
                            .map(|s| {
                                s.eq_ignore_ascii_case("dark") || s.eq_ignore_ascii_case("true")
                            })
                            .unwrap_or(true)
                    });
                if let Err(e) = transport.set_appearance(dark).await {
                    return ToolResult::error(format!("set_appearance failed: {}", e));
                }
                format!("Set appearance to {}", if dark { "dark" } else { "light" })
            }
            "simulate_biometric" => {
                let matches = args
                    .get("matches")
                    .or_else(|| args.get("match"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if let Err(e) = transport.simulate_biometric(matches).await {
                    return ToolResult::error(format!("simulate_biometric failed: {}", e));
                }
                format!(
                    "Simulated biometric ({})",
                    if matches { "match" } else { "no match" }
                )
            }
            "pasteboard_set" => {
                let t = match text {
                    Some(t) => t,
                    None => return ToolResult::error("pasteboard_set requires 'text' parameter"),
                };
                if let Err(e) = transport.pasteboard_set(t).await {
                    return ToolResult::error(format!("pasteboard_set failed: {}", e));
                }
                format!("Set clipboard ({} chars)", t.len())
            }
            "pasteboard_get" => match transport.pasteboard_get().await {
                Ok(s) => format!("Clipboard: {}", s),
                Err(e) => return ToolResult::error(format!("pasteboard_get failed: {}", e)),
            },
            "grant_permission" => {
                let perm = match args.get("permission").and_then(|p| p.as_str()) {
                    Some(p) => p,
                    None => {
                        return ToolResult::error(
                            "grant_permission requires 'permission' (e.g. location, camera)",
                        )
                    }
                };
                let pkg = match package {
                    Some(p) => p,
                    None => return ToolResult::error("grant_permission requires 'package'"),
                };
                if let Err(e) = transport.grant_permission(perm, pkg).await {
                    return ToolResult::error(format!("grant_permission failed: {}", e));
                }
                format!("Granted '{}' to {}", perm, pkg)
            }
            "set_orientation" => {
                let rot: u8 = match args.get("orientation").and_then(|v| v.as_str()) {
                    Some("portrait") => 0,
                    Some("landscape") | Some("landscape_left") => 1,
                    Some("portrait_upside_down") => 2,
                    Some("landscape_right") => 3,
                    Some(other) => return ToolResult::error(format!(
                        "unknown orientation '{}' (portrait|landscape|landscape_left|landscape_right|portrait_upside_down)", other
                    )),
                    None => return ToolResult::error("set_orientation requires 'orientation'"),
                };
                if let Err(e) = transport.set_orientation(rot).await {
                    return ToolResult::error(format!("set_orientation failed: {}", e));
                }
                format!(
                    "Set orientation to {}",
                    args.get("orientation")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                )
            }
            "unlock" => {
                if let Err(e) = transport.unlock().await {
                    return ToolResult::error(format!("unlock failed: {}", e));
                }
                "Unlocked the device".to_string()
            }
            "alert_text" => match transport.alert_text().await {
                Ok(Some(t)) => format!("Alert: {}", t),
                Ok(None) => "No alert is showing".to_string(),
                Err(e) => return ToolResult::error(format!("alert_text failed: {}", e)),
            },
            "alert_accept" => {
                if let Err(e) = transport.alert_accept().await {
                    return ToolResult::error(format!("alert_accept failed: {}", e));
                }
                "Accepted the alert".to_string()
            }
            "alert_dismiss" => {
                if let Err(e) = transport.alert_dismiss().await {
                    return ToolResult::error(format!("alert_dismiss failed: {}", e));
                }
                "Dismissed the alert".to_string()
            }
            "app_state" => {
                let pkg = match package {
                    Some(p) => p,
                    None => return ToolResult::error("app_state requires 'package' parameter"),
                };
                match transport.app_state(pkg).await {
                    Ok(s) => format!("{} is {}", pkg, crate::transport::app_state_name(s)),
                    Err(e) => return ToolResult::error(format!("app_state failed: {}", e)),
                }
            }
            _ => return ToolResult::error(format!("Unknown action: {}", action)),
        };

        // Poll until the screen stops moving instead of a blind 500ms. The old
        // sleep let observe() shoot the screenshot and dump the tree seconds
        // apart mid-transition, so a launch returned a splash screenshot beside
        // an onboarding tree, and a navigation returned zero elements because
        // the next screen had not arrived yet. Settling first makes the two
        // agree, because a settled screen does not change between the calls.
        let (settle_min, settle_timeout) = if action == "launch_app" || action == "install" {
            (
                crate::transport::SETTLE_LAUNCH_MIN_MS,
                crate::transport::SETTLE_LAUNCH_TIMEOUT_MS,
            )
        } else {
            (
                crate::transport::SETTLE_ACTION_MIN_MS,
                crate::transport::SETTLE_ACTION_TIMEOUT_MS,
            )
        };
        crate::transport::wait_for_screen_stable(
            transport.as_ref(),
            std::time::Duration::from_millis(settle_min),
            std::time::Duration::from_millis(settle_timeout),
        )
        .await;

        // Logcat sees a strict subset — no request headers, no request body — so
        // these go into the shared sink tagged as such, beside whatever the SDK
        // pushed while the action ran.
        let logcat_calls = transport.capture_http_logs().await.unwrap_or_default();
        let logcat_count = logcat_calls.len();
        self.network_history
            .extend(NetworkSource::Logcat, logcat_calls.clone());

        let network_summary = if logcat_count == 0 {
            None
        } else {
            let recent = self.network_history.snapshot();
            let start = recent.len().saturating_sub(logcat_count);
            Some(crate::network::sink::summarize(&recent[start..]))
        };

        let (post_screenshot, elements, tree_error) = match transport.observe().await {
            Ok(o) => (o.frame, o.elements, o.tree_error),
            Err(e) => (Vec::new(), Vec::new(), Some(e.to_string())),
        };
        let activity = crate::transport::activity_or_unknown(transport.as_ref()).await;
        let screen_package = extract_package(&activity);

        let mut report = self.situation.lock().await.report_after_action(
            "default",
            action,       // canonical action name ("tap", "launch_app") — used by HintEngine
            &action_desc, // human-readable description ("Tapped #3 (Login)") — for display only
            crate::situation::ObservedScreen {
                activity: &activity,
                package: package.unwrap_or(""),
                elements: &elements,
                tree_available: tree_error.is_none(),
            },
        );

        // Swipe stuck false positive fix: use visual diff as fallback.
        // Flutter exposes all children in a11y tree regardless of scroll position,
        // so element hash doesn't change even though the screen visually scrolled.
        if action == "swipe" && report.stuck {
            if let Some(ref pre) = pre_frame {
                // frames_settled tolerates a blinking cursor, which exact bytes do
                // not: it is the comparator written for "is this the same screen".
                if !crate::screen::optimize::frames_settled(&pre.source_frame, &post_screenshot) {
                    report.stuck = false;
                    report.screen_changed = true;
                }
            }
        }

        let annotated: Option<Arc<AnnotatedScreen>> = {
            if post_screenshot.is_empty() || oversized(&post_screenshot).is_some() {
                // Same dimension cap look applies: this path decodes a full frame
                // too, and only look was guarded.
                None
            } else {
                self.annotate_stable(
                    &post_screenshot,
                    &elements,
                    (screen_width, screen_height),
                    None,
                    transport.id(),
                )
                .await
                .ok()
                .map(Arc::new)
            }
        };

        if let Some(ref a) = annotated {
            *self.last_annotated.lock().await = Some(a.clone());
            // `do` renumbers the screen it just navigated to, so the numbering it
            // RETURNS must be the numbering the next process resolves against.
            // Writing it only in `look` meant a second `do --element N` from a
            // shell resolved against the screen before this action.
            let ctx = self
                .observation_context(&*transport, &activity, screen_package)
                .await;
            ScreenAnnotator::persist_observation(&ctx, &a.elements);
        }

        let interactive = annotated
            .as_ref()
            .map(|a| annotated_elements_to_json(&a.elements))
            .unwrap_or_default();

        {
            let mut session_guard = self.session.lock().await;
            if let Some(ref mut session) = *session_guard {
                // Persist the clean post-action frame so the on-disk session is
                // a reproducible storyboard (what a dashboard renders), not just
                // captions + diffs. save_screenshot exists but was never called.
                let shot = if post_screenshot.is_empty() {
                    None
                } else {
                    crate::session::save_screenshot(
                        &session.id,
                        report.step as usize,
                        &post_screenshot,
                    )
                    .ok()
                };
                // The whole window, not one source. Passing only the logcat
                // calls here is why drengr_query(session) reported fewer errors
                // than network and analyze on any app running the SDK.
                session.record_step(
                    report.step as usize,
                    &action_desc,
                    &activity,
                    report.screen_changed,
                    self.network_history.since(step_started_ms),
                    shot,
                );
                let _ = session.save();
            }
        }

        let nav_ctx = self.nav_context_for(&activity).await;

        if format == "text" {
            let scrollable = elements.iter().any(|e| e.scrollable);
            // Same seam as handle_look: the scene must carry the annotator's ids,
            // not a fresh 1..N, or the numbers it prints resolve to nothing.
            let numbered: Vec<(usize, &crate::screen::ui_element::UiElement)> = annotated
                .as_ref()
                .map(|a| a.elements.iter().map(|e| (e.number, &e.element)).collect())
                .unwrap_or_default();
            let scene = TextSceneBuilder::new(screen_width, screen_height)
                .with_activity(&activity)
                .with_scrollable(scrollable)
                .build_with_ids(&numbered);

            let mut response = json!({
                "action": action_desc,
                "step": report.step,
                "situation": report.to_json(),
                "screen": {
                    "activity": activity,
                    "package": screen_package,
                    "width": screen_width,
                    "height": screen_height,
                },
                // No `elements` array beside the scene: same data twice made
                // the cheap format the expensive one. Same rule as drengr_look.
                "text_scene": scene.description,
                "element_count": annotated.as_ref().map(|a| a.elements.len()).unwrap_or(0),
            });
            if let Some(ref summary) = network_summary {
                response["network_calls"] = summary.clone();
            }
            if let Some(nav) = nav_ctx {
                response["navigation_context"] = nav;
            }
            if annotated.as_ref().is_none_or(|a| a.elements.is_empty()) {
                response["hint"] = json!(NO_TREE_HINT);
            }
            if let Some(ref e) = tree_error {
                response["tree_error"] = json!(e);
            }
            if post_screenshot.is_empty() {
                // The image path reported this and the text path did not, so a dead
                // device was indistinguishable from a screen with nothing on it.
                response["error"] = json!("Post-action screenshot failed");
            }

            ToolResult::text(serde_json::to_string_pretty(&response).unwrap_or_default())
        } else {
            match annotated {
                Some(a) => {
                    use crate::screen::optimize::downscale_or_original;
                    use base64::prelude::{Engine as _, BASE64_STANDARD};
                    let image_base64 = if format != "clean" {
                        BASE64_STANDARD.encode(downscale_or_original(&a.image_data))
                    } else {
                        BASE64_STANDARD.encode(downscale_or_original(&post_screenshot))
                    };
                    let mut text_info = json!({
                        "action": action_desc,
                        "step": report.step,
                        "situation": report.to_json(),
                        "screen": {
                            "activity": activity,
                            "package": screen_package,
                            "width": screen_width,
                            "height": screen_height,
                        },
                        "elements": interactive,
                        "element_count": a.elements.len(),
                    });
                    if a.elements.is_empty() {
                        text_info["hint"] = json!(NO_TREE_HINT);
                    }
                    if let Some(ref e) = tree_error {
                        text_info["tree_error"] = json!(e);
                    }
                    if let Some(ref summary) = network_summary {
                        text_info["network_calls"] = summary.clone();
                    }
                    if let Some(nav) = nav_ctx {
                        text_info["navigation_context"] = nav;
                    }

                    ToolResult::image_and_text(
                        image_base64,
                        serde_json::to_string_pretty(&text_info).unwrap_or_default(),
                    )
                }
                None => {
                    let response = json!({
                        "action": action_desc,
                        "step": report.step,
                        "situation": report.to_json(),
                        "error": "Post-action screenshot failed",
                    });
                    ToolResult::text(serde_json::to_string_pretty(&response).unwrap_or_default())
                }
            }
        }
    }
}
