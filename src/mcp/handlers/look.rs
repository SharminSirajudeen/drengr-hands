use super::*;

impl McpHandlers {
    /// Handle drengr_look — observe the current screen.
    pub(super) async fn handle_look(&self, args: Value) -> ToolResult {
        let transport = match self.ensure_or_autoprovision(&args).await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(e),
        };

        let format = args
            .get("format")
            .and_then(|f| f.as_str())
            .unwrap_or_else(|| super::default_format());

        // 'grid' is the treeless fallback, so never wait on the tree it falls
        // back FROM — that dump is exactly what hangs (~10s, returns nothing).
        if format == "grid" {
            return self.look_grid(transport.as_ref()).await;
        }

        // Capture screenshot + UI tree together. On iOS this is one runner
        // round-trip (it returns both); elsewhere it's the default two calls.
        let (screenshot, elements, tree_error) = match transport.observe().await {
            Ok(o) if !o.frame.is_empty() => (o.frame, o.elements, o.tree_error),
            Ok(_) => return ToolResult::error("Screenshot failed: empty response from device"),
            Err(e) => return ToolResult::error(format!("Observe failed: {}", e)),
        };

        if let Some(err) = oversized(&screenshot) {
            return ToolResult::error(err);
        }

        let activity = crate::transport::activity_or_unknown(transport.as_ref()).await;
        let package = extract_package(&activity);

        self.situation
            .lock()
            .await
            .observe("default", &activity, &elements);

        let (screen_width, screen_height) = transport
            .screen_size()
            .await
            .unwrap_or(crate::transport::DEFAULT_SCREEN_SIZE);

        // Always annotate so drengr_do(tap) can resolve element numbers
        let annotated = match self
            .annotate_stable(
                &screenshot,
                &elements,
                (screen_width, screen_height),
                // Advertised in the tool schema and previously read by nothing.
                args.get("max_elements")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
                transport.id(),
            )
            .await
        {
            Ok(a) => Arc::new(a),
            Err(e) => return ToolResult::error(format!("Annotation failed: {}", e)),
        };
        *self.last_annotated.lock().await = Some(annotated.clone());
        // Also to disk: `drengr do` is a SEPARATE process and cannot see the
        // line above, so without this its --element lookup always misses.
        let ctx = self
            .observation_context(&*transport, &activity, package)
            .await;
        ScreenAnnotator::persist_observation(&ctx, &annotated.elements);

        let interactive = annotated_elements_to_json(&annotated.elements);
        let scrollable = elements.iter().any(|e| e.scrollable);

        let nav_ctx = self.nav_context_for(&activity).await;

        if format == "text" {
            // Render the ids the annotator actually assigned. Numbering the
            // scene 1..N here instead printed numbers that `drengr do
            // --element n` could never resolve: the registry hands out stable
            // ids seeded from the previous process, so they start wherever that
            // one left off, and those are the ids persisted as tap targets.
            let numbered: Vec<(usize, &crate::screen::ui_element::UiElement)> = annotated
                .elements
                .iter()
                .map(|e| (e.number, &e.element))
                .collect();
            let scene = TextSceneBuilder::new(screen_width, screen_height)
                .with_activity(&activity)
                .with_scrollable(scrollable)
                .build_with_ids(&numbered);

            // No `elements` array here on purpose. It carried the same data as
            // text_scene, so 'text' cost more than 'image' rather than less.
            let mut response = json!({
                "screen": {
                    "activity": activity,
                    "package": package,
                    "width": screen_width,
                    "height": screen_height,
                },
                "text_scene": scene.description,
                "scrollable": scrollable,
                "element_count": annotated.elements.len(),
            });
            if let Some(nav) = nav_ctx {
                response["navigation_context"] = nav;
            }
            if annotated.elements.is_empty() {
                response["hint"] = json!(NO_TREE_HINT);
            }
            if let Some(ref e) = tree_error {
                response["tree_error"] = json!(e);
            }

            ToolResult::text(serde_json::to_string_pretty(&response).unwrap_or_default())
        } else {
            use crate::screen::optimize::downscale_or_original;
            use base64::prelude::{Engine as _, BASE64_STANDARD};
            // 'clean' returns the frame unmarked: markers sit on top of the
            // text they label, which makes typography and layout unjudgeable.
            // Annotation still ran, so element numbers and tap targets stand.
            let clean = format == "clean";
            let image_base64 = if !clean {
                BASE64_STANDARD.encode(downscale_or_original(&annotated.image_data))
            } else {
                BASE64_STANDARD.encode(downscale_or_original(&screenshot))
            };
            let mut text_info = json!({
                "screen": {
                    "activity": activity,
                    "package": package,
                    "width": screen_width,
                    "height": screen_height,
                },
                "elements": interactive,
                "scrollable": scrollable,
                "element_count": annotated.elements.len(),
            });
            if let Some(nav) = nav_ctx {
                text_info["navigation_context"] = nav;
            }
            if annotated.elements.is_empty() {
                text_info["hint"] = json!(NO_TREE_HINT);
            }
            if let Some(ref e) = tree_error {
                text_info["tree_error"] = json!(e);
            }

            ToolResult::image_and_text(
                image_base64,
                serde_json::to_string_pretty(&text_info).unwrap_or_default(),
            )
        }
    }

    /// Screenshot + coordinate grid, no UI tree. Deliberately skips annotate,
    /// tap-target persistence and the situation engine: this is an aiming peek,
    /// not a state observation, and must not clobber either.
    async fn look_grid(&self, transport: &dyn DeviceTransport) -> ToolResult {
        use crate::screen::optimize::downscale_or_original;
        use base64::prelude::{Engine as _, BASE64_STANDARD};

        let screenshot = match transport.screenshot().await {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => return ToolResult::error("Screenshot failed: empty response from device"),
            Err(e) => return ToolResult::error(format!("Screenshot failed: {}", e)),
        };
        if let Some(err) = oversized(&screenshot) {
            return ToolResult::error(err);
        }

        let activity = crate::transport::activity_or_unknown(transport).await;
        let (screen_width, screen_height) = transport
            .screen_size()
            .await
            .unwrap_or(crate::transport::DEFAULT_SCREEN_SIZE);
        let grid = crate::screen::annotate::grid_overlay(&screenshot)
            .unwrap_or_else(|_| screenshot.clone());

        let info = json!({
            "screen": {
                "activity": activity,
                "package": extract_package(&activity),
                "width": screen_width,
                "height": screen_height,
            },
            "ui_tree": "not fetched (grid mode)",
            "hint": "Grid lines are 0–100% of width/height. Read the target's position, then drengr_do(action='tap', x=<0-1>, y=<0-1>).",
        });

        ToolResult::image_and_text(
            BASE64_STANDARD.encode(downscale_or_original(&grid)),
            serde_json::to_string_pretty(&info).unwrap_or_default(),
        )
    }
}
