pub mod fast_path;
pub mod llm;
pub mod nav_path;
pub mod progress;
pub mod prompt;

pub use llm::{LlmClient, LlmProvider};
pub use prompt::{OodaAction, OodaDecision, OodaStepSummary};

use anyhow::{Context, Result};

use crate::run_outcome::{ProgressTrigger, RunOutcomeBuilder, RunOutcomeKind};
use crate::screen::annotate::ScreenAnnotator;
use crate::screen::optimize::ImageOptimizer;
use crate::screen::registry::ElementRegistry;
use crate::screen::text_scene::TextSceneBuilder;
use crate::situation::SituationEngine;
use crate::transport::DeviceTransport;

/// Pre-allocate at most this many slots for the per-step history buffer.
/// Bound prevents over-allocation when callers pass very large `max_steps`.
const HISTORY_PREALLOC_CAP: usize = 50;

/// Bail out of the loop after this many consecutive duplicate screens —
/// the agent has demonstrably stopped making progress.
const STUCK_DUP_THRESHOLD: u32 = 5;

/// Retire the nav planner after this many failed hops (label missing or
/// screen unchanged) — the map is stale, let the LLM drive.
const NAV_MAX_STRIKES: u32 = 2;

// Screen-settle polling (replaces blind sleeps): wait `min`, then screenshot
// every `POLL` until two consecutive frames match, capped at `timeout`.
const SETTLE_FOCUS_TIMEOUT_MS: u64 = 1_000;

/// Configuration for the OODA agent.
pub struct OodaConfig {
    pub task: String,
    pub app_package: String,
    pub max_steps: usize,
    pub device_id: String,
    /// Send a screenshot every step instead of only when the text scene is underlabeled.
    pub force_vision: bool,
    /// Run a post-action binary goal-completion query ("LLM as judge").
    pub verify_completion: bool,
    /// After the run finishes (success or failure), uninstall the WDA runner
    /// so the next run starts from a clean slate. iOS-only; no-op on Android.
    pub cleanup_wda: bool,
    /// When `Some`, `open_app` may only target packages in this list;
    /// anything else is refused. `None` is unrestricted (CLI default).
    pub allowed_apps: Option<Vec<String>>,
}

/// Result of an OODA run.
#[derive(Debug)]
pub struct OodaResult {
    pub task: String,
    pub success: bool,
    pub steps: usize,
    pub final_reasoning: String,
    pub history: Vec<OodaStepSummary>,
}

/// Outcomes that warrant a redacted on-disk diagnostic bundle (tier 2).
/// Anything else is a clean exit and the user shouldn't see noise.
fn outcome_warrants_bundle(outcome: RunOutcomeKind) -> bool {
    matches!(
        outcome,
        RunOutcomeKind::ProgressStuck
            | RunOutcomeKind::Crash
            | RunOutcomeKind::StepCap
            | RunOutcomeKind::DuplicateScreen
            | RunOutcomeKind::Timeout
    )
}

/// Emit tier-1 telemetry and (on failure outcomes) write a redacted tier-2
/// bundle. Called exactly once per run from each exit point in `run_ooda`.
fn finalize_run(
    builder: RunOutcomeBuilder,
    detector: &progress::ProgressDetector,
    history: &[OodaStepSummary],
    app_package: &str,
    final_activity: &str,
    outcome: RunOutcomeKind,
    step_count: usize,
) {
    if outcome_warrants_bundle(outcome) {
        let task_kind = crate::run_outcome::TaskKind::from_histogram(&builder.action_histogram);
        let final_activity_kind =
            crate::run_outcome::FinalActivityKind::from_activity(final_activity, app_package);
        let recent: Vec<&str> = detector.recent_scenes();
        let duration_ms = builder
            .started_at
            .elapsed()
            .as_millis()
            .min(u32::MAX as u128) as u32;
        let bundle = crate::diag::build_from_history(
            crate::diag::RunMeta {
                run_id: &builder.run_id,
                platform: &builder.platform,
                outcome: outcome.as_str(),
                model: &builder.model,
                provider: &builder.provider,
                version: env!("CARGO_PKG_VERSION"),
                task_kind: task_kind.as_str(),
                final_activity_kind: final_activity_kind.as_str(),
                step_count: step_count.min(u32::MAX as usize) as u32,
                duration_ms,
            },
            history,
            &recent,
        );
        match crate::diag::write_bundle(&bundle) {
            Ok(path) => {
                eprintln!("\n[diag] bundle saved: {}", path.display());
                eprintln!(
                    "[diag] redacted on disk — read it with `drengr diag show {}`\n",
                    builder.run_id
                );
            }
            Err(e) => tracing::warn!("failed to write diag bundle: {}", e),
        }
    }
}

/// Run the OODA agent loop.
///
/// This is the core of `drengr run`. It:
/// 1. Launches the app
/// 2. Observes the screen (screenshot + UI tree)
/// 3. Orients (builds text scene + situation report)
/// 4. Decides (sends prompt to LLM)
/// 5. Acts (executes the decision)
/// 6. Repeats until done or max_steps
pub async fn run_ooda(
    transport: &dyn DeviceTransport,
    llm: &LlmClient,
    config: &OodaConfig,
) -> Result<OodaResult> {
    let annotator = ScreenAnnotator::new();
    let mut optimizer = ImageOptimizer::new();
    let mut situation = SituationEngine::new();
    let mut registry = ElementRegistry::new();
    let mut history: Vec<OodaStepSummary> =
        Vec::with_capacity(config.max_steps.min(HISTORY_PREALLOC_CAP));
    let device_id = &config.device_id;
    let mut detector = progress::ProgressDetector::new(&config.task);
    let mut judge_ever_fired = false;
    let mut next_step_progress_hint: Option<String> = None;
    let mut outcome_builder = RunOutcomeBuilder::new(
        transport.platform_kind(),
        llm.model(),
        llm.provider().telemetry_tag(),
    );
    let mut last_activity = String::from("unknown");
    let progress_check_disabled = std::env::var("DRENGR_DISABLE_PROGRESS_CHECK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // Nav planner: a fresh explored ScreenMap + a pure-nav task lets DECIDE
    // run as deterministic BFS hops instead of LLM calls, until arrival.
    let screen_map = crate::explore::load_screen_map(&config.app_package)
        .ok()
        .flatten()
        .filter(nav_path::is_fresh);
    let mut nav_goal: Option<String> = screen_map
        .as_ref()
        .and_then(|m| nav_path::goal_screen_for_task(m, &config.task));
    let mut nav_strikes: u32 = 0;
    if let Some(goal) = &nav_goal {
        println!("[nav] screen map found — routing to '{}' without LLM", goal);
    }

    // Launch the app, then poll until the screen settles instead of a blind 2s.
    transport
        .launch_app(&config.app_package)
        .await
        .context("Failed to launch app")?;
    crate::transport::wait_for_screen_stable(
        transport,
        std::time::Duration::from_millis(crate::transport::SETTLE_LAUNCH_MIN_MS),
        std::time::Duration::from_millis(crate::transport::SETTLE_LAUNCH_TIMEOUT_MS),
    )
    .await;

    // Get screen dimensions
    let (screen_w, screen_h) = transport
        .screen_size()
        .await
        .unwrap_or(crate::transport::DEFAULT_SCREEN_SIZE);

    tracing::info!(
        "OODA starting: task=\"{}\" app={} max_steps={}",
        config.task,
        config.app_package,
        config.max_steps
    );

    // Carry the post-action observation (UI tree + activity + settled screenshot)
    // into the next step's OBSERVE — no action happens between them, so re-dumping
    // the UI tree (the Android bottleneck, ~0.4-1.2s), re-reading the activity, or
    // re-shooting the frame the settle poll just captured is pure waste. None on the
    // first step and after a rejected action (which skips the post-action observe).
    let mut carried: Option<(
        Vec<crate::screen::ui_element::UiElement>,
        String,
        Option<Vec<u8>>,
    )> = None;

    for step in 1..=config.max_steps {
        let step_start = std::time::Instant::now();

        // ── OBSERVE ── Reuse the previous step's post-action UI tree + activity +
        // settled screenshot if carried (the screen hasn't changed since); a fresh
        // screenshot is only fetched when the settle poll couldn't provide one.
        let observe_start = std::time::Instant::now();
        let (screenshot, elements, activity) = if let Some((els, act, shot)) = carried.take() {
            let shot = match shot {
                Some(s) => s,
                None => transport.screenshot().await.context("Screenshot failed")?,
            };
            (shot, els, act)
        } else {
            // Through the contract, not around it. Capturing in parallel here
            // returned a frame older than the tree beside it, which is the defect
            // observe() was made serial to remove: step 1 follows launch_app, so
            // this was the splash-frame case on the unattended path.
            let obs = transport.observe().await.context("Observe failed")?;
            let act = crate::transport::activity_or_unknown(transport).await;
            (obs.frame, obs.elements, act)
        };
        let observe_ms = observe_start.elapsed().as_millis();

        // Check for duplicate screen (stuck detection)
        let (_is_dup, dup_count) = optimizer.check_duplicate(&screenshot);
        if dup_count >= STUCK_DUP_THRESHOLD {
            tracing::warn!(
                "Screen unchanged for {} consecutive actions — agent is stuck",
                STUCK_DUP_THRESHOLD
            );
            finalize_run(
                outcome_builder,
                &detector,
                &history,
                &config.app_package,
                &last_activity,
                RunOutcomeKind::DuplicateScreen,
                step,
            );
            return Ok(OodaResult {
                task: config.task.clone(),
                success: false,
                steps: step,
                final_reasoning: format!(
                    "Stuck: screen unchanged for {} consecutive actions",
                    STUCK_DUP_THRESHOLD
                ),
                history,
            });
        }

        last_activity = activity.clone();

        // Per-step baseline so report_after_action diffs against the screen we just acted on.
        situation.observe(device_id, &activity, &elements);

        // ── ORIENT ──
        let orient_start = std::time::Instant::now();
        let scrollable = elements.iter().any(|e| e.scrollable);
        // Number the addressable set, not the raw tree. Assigning over everything
        // burned ids on elements the annotator then filtered out, which is the
        // same unfiltered-numbering defect that made MCP numbers arrive as 9, 11,
        // 12 with no 10.
        let numbered = registry.assign(crate::screen::ui_element::addressable(
            &elements,
            crate::screen::ui_element::max_addressable(),
        ));
        let scene = TextSceneBuilder::new(screen_w, screen_h)
            .with_activity(&activity)
            .with_scrollable(scrollable)
            .build_with_ids(&numbered);
        let orient_ms = orient_start.elapsed().as_millis();

        // Vision escalation depends only on the text scene, not the image — decide it
        // BEFORE annotating. On text-only steps (the common case) skip the whole image
        // pipeline (PNG decode + overlay draw + JPEG encode) and compute just the tap
        // map; the drawn overlay is only ever uploaded to a vision model.
        let use_vision = config.force_vision || scene.should_escalate_to_vision();

        let annotate_start = std::time::Instant::now();
        let annotated = if use_vision {
            {
                // Same dimension cap the MCP paths apply: this decodes a full
                // frame too, and was the one decoder left unguarded.
                if let Ok(reader) =
                    image::ImageReader::new(std::io::Cursor::new(&screenshot)).with_guessed_format()
                {
                    if let Ok((w, h)) = reader.into_dimensions() {
                        if w > 4096 || h > 4096 {
                            anyhow::bail!("Screenshot too large ({w}x{h}). Max 4096x4096.");
                        }
                    }
                }
                annotator.annotate_with_ids(&screenshot, &numbered, (screen_w, screen_h))?
            }
        } else {
            crate::screen::annotate::AnnotatedScreen {
                image_data: Vec::new(),
                elements: annotator.elements_for_ids(&numbered),
                // Text-only step: nothing was drawn, but the frame is still what
                // this observation saw.
                source_frame: screenshot.clone(),
            }
        };
        let annotate_ms = annotate_start.elapsed().as_millis();

        // ── DECIDE ──
        let mut stuck_hint = prompt::build_stuck_hint(&history);
        if let Some(extra) = next_step_progress_hint.take() {
            stuck_hint = Some(match stuck_hint {
                Some(s) => format!("{}\n{}", s, extra),
                None => extra,
            });
        }
        let allow_wait = stuck_hint.is_none();

        let prompt = prompt::generate_ooda_prompt(
            &config.task,
            step,
            config.max_steps,
            &history,
            &scene.description,
            scene.max_id,
            stuck_hint.as_deref(),
            allow_wait,
        );

        tracing::info!(
            "OODA step {} scene: activity=\"{}\" elements={} labeled_ratio={} vision_ready={} stuck_hint={} allow_wait={}",
            step,
            activity,
            scene.element_count,
            scene
                .labeled_ratio
                .map_or_else(|| "n/a".to_string(), |r| format!("{:.2}", r)),
            !scene.should_escalate_to_vision(),
            stuck_hint.is_some(),
            allow_wait,
        );
        tracing::debug!("OODA step {} full prompt:\n{}", step, prompt);

        // Nav planner: while a nav goal is active, DECIDE is a BFS hop over the
        // screen map executed as a label tap / back press — no LLM. Stale hops
        // (label gone) strike the planner; enough strikes retire it for the run.
        let mut nav_decision: Option<OodaDecision> = None;
        if let (Some(map), Some(goal)) = (screen_map.as_ref(), nav_goal.clone()) {
            match nav_path::next_hop(map, &activity, &goal) {
                nav_path::NavStatus::Arrived => {
                    tracing::info!(
                        target: "ooda_nav_path",
                        goal = %goal,
                        "arrived at goal screen — LLM confirms from here"
                    );
                    nav_goal = None;
                }
                nav_path::NavStatus::Hop(hop) => {
                    nav_decision = nav_path::hop_to_decision(&hop, &annotated.elements, &goal);
                    if nav_decision.is_none() {
                        nav_strikes += 1;
                        if nav_strikes >= NAV_MAX_STRIKES {
                            tracing::info!(
                                target: "ooda_nav_path",
                                "map stale for live screen — retiring planner for this run"
                            );
                            nav_goal = None;
                        }
                    }
                }
                nav_path::NavStatus::NoPath => {}
            }
        }

        // Fast-path: deterministic noun→element matcher + decision cache + nav
        // planner. All layers fall through to the LLM on miss/ambiguity.
        let fp_key = fast_path::cache_key(&config.task, &scene.description);
        let llm_start = std::time::Instant::now();
        let mut llm_ms = 0u128;
        let mut via_nav = false;
        let decision = if let Some(d) =
            fast_path::try_deterministic_match(&config.task, &annotated.elements)
        {
            d
        } else if let Some(d) = fast_path::try_cache_lookup(fp_key, &annotated.elements) {
            d
        } else if let Some(d) = nav_decision.take() {
            via_nav = true;
            d
        } else {
            let response = if use_vision {
                // Downscale before vision upload — most models accept up to ~1024px,
                // and a 1080p screenshot is ~2 MB JPEG. Downscaling to ~768px on
                // the long edge cuts upload bandwidth ~3-4× without quality loss
                // for UI inspection. Fails gracefully back to original on error.
                let upload_bytes =
                    crate::screen::optimize::downscale_for_vision(&annotated.image_data)
                        .unwrap_or_else(|_| annotated.image_data.clone());
                use base64::prelude::{Engine as _, BASE64_STANDARD};
                let image_base64 = BASE64_STANDARD.encode(&upload_bytes);
                llm.complete_with_image(&prompt, &image_base64, scene.max_id, allow_wait)
                    .await
            } else {
                llm.complete(&prompt, scene.max_id, allow_wait).await
            }
            .context("LLM call failed")?;
            llm_ms = llm_start.elapsed().as_millis();
            let d =
                prompt::parse_ooda_decision(&response).context("Failed to parse LLM decision")?;
            // Cache the LLM decision for future replays of the same (task, scene).
            fast_path::cache_insert(fp_key, &d, &annotated.elements);
            d
        };

        tracing::info!(
            "OODA step {}: {:?} — {}",
            step,
            decision.action,
            decision.reasoning
        );

        // ── Check if done ──
        // The model claiming completion is a claim, not a result. The judge was only
        // ever consulted on the post-action path, so this branch returned success on
        // the model's own word and recorded it as JudgePass, which made an unverified
        // pass indistinguishable from a verified one in the telemetry.
        if decision.done || decision.action == OodaAction::Done {
            let verdict = if config.verify_completion {
                llm.check_goal_complete(&config.task, &decision.reasoning, &scene.description)
                    .await
            } else {
                llm::JudgeVerdict::Unavailable("completion verification disabled".to_string())
            };
            // Only a judge that actually ANSWERED may refuse. An unreachable judge
            // means we do not know, and refusing on it would burn the whole step
            // budget against a device that already finished.
            if let llm::JudgeVerdict::NotComplete(ref why) = verdict {
                let retry = format!(
                    "COMPLETION CHECK: you reported done, but the screen does not show it ({}). Finish the task, then confirm the resulting state.",
                    why
                );
                next_step_progress_hint = Some(match next_step_progress_hint.take() {
                    Some(existing) => format!("{}\n{}", existing, retry),
                    None => retry,
                });
                tracing::info!(
                    "OODA step {} completion claim rejected by judge: {}",
                    step,
                    why
                );
                continue;
            }
            let kind = if verdict.answered() {
                outcome_builder.record_judge(true);
                RunOutcomeKind::JudgePass
            } else {
                tracing::info!(
                    "OODA step {} completion accepted unverified: {}",
                    step,
                    verdict.reason()
                );
                RunOutcomeKind::SelfReported
            };
            println!(
                "[done] Task complete (step {}): {}",
                step, decision.reasoning
            );
            finalize_run(
                outcome_builder,
                &detector,
                &history,
                &config.app_package,
                &last_activity,
                kind,
                step,
            );
            return Ok(OodaResult {
                task: config.task.clone(),
                success: true,
                steps: step,
                final_reasoning: decision.reasoning,
                history,
            });
        }

        // ── ACT ── (rejection skips the post-action observe/judge/history; counter still advances)
        let act_start = std::time::Instant::now();
        let action_desc = match execute_action(
            transport,
            &decision,
            &annotated,
            screen_w,
            screen_h,
            config.allowed_apps.as_deref(),
        )
        .await
        {
            Ok(desc) => {
                outcome_builder.record_action(decision.action.canonical_name());
                desc
            }
            Err(e) => {
                // The rejection has to reach the next prompt. Dropped, the prompt
                // is byte-identical and the model re-proposes the same invalid
                // action every step until the budget is gone.
                tracing::warn!("Action rejected at step {}: {}", step, e);
                history.push(OodaStepSummary {
                    step,
                    action: format!("{} (rejected)", decision.action.canonical_name()),
                    outcome: format!("Not executed: {e}. Choose a different action or supply the missing parameter."),
                    screen_changed: false,
                });
                continue;
            }
        };
        let act_ms = act_start.elapsed().as_millis();

        // Wait for the screen to settle before observing. The frame it returns is
        // deliberately discarded: it predates the tree dump below, and pairing
        // those two is the skew this loop just stopped doing.
        let _ = crate::transport::wait_for_screen_stable(
            transport,
            std::time::Duration::from_millis(crate::transport::SETTLE_ACTION_MIN_MS),
            std::time::Duration::from_millis(crate::transport::SETTLE_ACTION_TIMEOUT_MS),
        )
        .await;

        tracing::info!(
            "OODA step {} timings: total={}ms observe={}ms orient={}ms annotate={}ms llm={}ms({}) act={}ms",
            step,
            step_start.elapsed().as_millis(),
            observe_ms,
            orient_ms,
            annotate_ms,
            llm_ms,
            if use_vision { "vision" } else { "text" },
            act_ms,
        );

        // One observation, so the frame carried into the next step is not older
        // than the tree carried with it.
        let (post_obs, post_activity) = tokio::join!(
            transport.observe(),
            crate::transport::activity_or_unknown(transport),
        );
        let (post_frame, post_elements, post_tree_error) = match post_obs {
            Ok(o) => (Some(o.frame), o.elements, o.tree_error),
            Err(e) => (None, Vec::new(), Some(e.to_string())),
        };

        let report = situation.report_after_action(
            device_id,
            decision.action.canonical_name(),
            // canonical name for HintEngine
            &action_desc,
            crate::situation::ObservedScreen {
                activity: // human-readable for display
            &post_activity,
                package: &config.app_package,
                elements: &post_elements,
                tree_available: post_tree_error.is_none(),
            },
        );

        // A nav hop that changed nothing means the edge is stale — strike.
        if via_nav {
            if report.screen_changed || report.activity_changed {
                nav_strikes = 0;
            } else {
                nav_strikes += 1;
                if nav_strikes >= NAV_MAX_STRIKES {
                    tracing::info!(
                        target: "ooda_nav_path",
                        "nav hops not changing screen — retiring planner for this run"
                    );
                    nav_goal = None;
                }
            }
        }

        // Print progress
        let status = if report.crash {
            "[crash]"
        } else if report.stuck {
            "[stuck]"
        } else if report.activity_changed {
            "[nav]"
        } else if report.screen_changed {
            "[changed]"
        } else {
            "[same]"
        };
        println!(
            "  Step {}: {} {} [{}]",
            step, action_desc, status, post_activity
        );

        // Bail on crash
        if report.crash {
            finalize_run(
                outcome_builder,
                &detector,
                &history,
                &config.app_package,
                &post_activity,
                RunOutcomeKind::Crash,
                step,
            );
            return Ok(OodaResult {
                task: config.task.clone(),
                success: false,
                steps: step,
                final_reasoning: "App crashed".to_string(),
                history,
            });
        }
        last_activity = post_activity.clone();

        // Post-action judge: binary goal-check, only run on state change.
        // Reuse the post-action elements we already captured — re-fetching can
        // fail on live-updating screens (uiautomator needs UI idle).
        let mut latest_post_scene_desc: Option<String> = None;
        if config.verify_completion && (report.screen_changed || report.activity_changed) {
            let judge_start = std::time::Instant::now();
            let post_scrollable = post_elements.iter().any(|e| e.scrollable);
            let post_numbered = registry.assign(&post_elements);
            let post_scene = TextSceneBuilder::new(screen_w, screen_h)
                .with_activity(&post_activity)
                .with_scrollable(post_scrollable)
                .build_with_ids(&post_numbered);
            latest_post_scene_desc = Some(post_scene.description.clone());

            let post_verdict = llm
                .check_goal_complete(&config.task, &action_desc, &post_scene.description)
                .await;
            let judge_done = matches!(post_verdict, llm::JudgeVerdict::Complete);
            let judge_reason = post_verdict.reason().to_string();
            if post_verdict.answered() {
                judge_ever_fired = true;
                outcome_builder.record_judge(judge_done);
                tracing::info!(
                    "OODA step {} judge: done={} ({}ms) — {}",
                    step,
                    judge_done,
                    judge_start.elapsed().as_millis(),
                    judge_reason,
                );
            }

            if judge_done {
                println!(
                    "[done] Task complete (step {}, judge): {}",
                    step, judge_reason
                );
                finalize_run(
                    outcome_builder,
                    &detector,
                    &history,
                    &config.app_package,
                    &post_activity,
                    RunOutcomeKind::JudgePass,
                    step,
                );
                return Ok(OodaResult {
                    task: config.task.clone(),
                    success: true,
                    steps: step,
                    final_reasoning: judge_reason,
                    history,
                });
            }
        }

        // Update progress detector with the post-action scene + action desc.
        let scene_for_detector =
            latest_post_scene_desc.unwrap_or_else(|| scene.description.clone());
        detector.observe(&scene_for_detector, &action_desc);

        let det_hint = detector.deterministic_hint();
        let should_escalate = detector.should_escalate_to_meta_llm(step, judge_ever_fired);
        tracing::debug!(
            "OODA step {} progress: scroll_counts={:?} keyword_visible={} det_hint={} should_escalate={}",
            step,
            detector.scroll_counts(),
            detector.keyword_visible(),
            det_hint.is_some(),
            should_escalate,
        );

        if let Some(hint) = det_hint {
            tracing::info!("ProgressDetector hint: {}", hint);
            outcome_builder.record_progress(ProgressTrigger::ScrollSaturation);
            next_step_progress_hint = Some(hint);
        }

        if progress_check_disabled {
            tracing::debug!(
                "OODA step {} check_progress skipped (DRENGR_DISABLE_PROGRESS_CHECK set)",
                step
            );
        } else if should_escalate {
            let scenes = detector.recent_scenes();
            let (verdict, reason) = llm.check_progress(&config.task, &scenes).await;
            tracing::info!(
                "OODA step {} check_progress fired: verdict={:?} reason={}",
                step,
                verdict,
                reason
            );
            match verdict {
                // Never terminates the run. This verdict fires only when the agent
                // is already stalling, and its own prompt defines goal_visible as
                // the target being "ready to be acted on", which check_goal_complete
                // explicitly calls NOT completion. Ending here turned a stall into a
                // green test. It is a hint for the next step instead.
                llm::ProgressVerdict::GoalVisible => {
                    let visible_msg = format!(
                        "PROGRESS CHECK: the target is on screen ({}). Act on it now, then confirm the resulting state.",
                        reason
                    );
                    next_step_progress_hint = Some(match next_step_progress_hint.take() {
                        Some(existing) => format!("{}\n{}", existing, visible_msg),
                        None => visible_msg,
                    });
                }
                llm::ProgressVerdict::Stuck => {
                    outcome_builder.record_progress(ProgressTrigger::MetaLlmStuck);
                    let stuck_msg =
                        "PROGRESS CHECK: agent appears stuck — try a completely different approach.";
                    next_step_progress_hint = Some(match next_step_progress_hint.take() {
                        Some(existing) => format!("{}\n{}", existing, stuck_msg),
                        None => stuck_msg.to_string(),
                    });
                }
                llm::ProgressVerdict::MakingProgress => {}
            }
        }

        // Record history
        let outcome = if report.activity_changed {
            format!("Navigated to {}", post_activity)
        } else if report.screen_changed {
            let new = report.new_elements.join(", ");
            if new.is_empty() {
                "Screen changed".to_string()
            } else {
                format!("New: {}", new)
            }
        } else {
            "No change".to_string()
        };

        history.push(OodaStepSummary {
            step,
            action: action_desc,
            outcome,
            screen_changed: report.screen_changed,
        });

        // Carry this settled post-action observation into the next step's OBSERVE so
        // it doesn't re-dump the UI tree (or re-shoot the settled frame) for a screen
        // we just observed.
        // Carry the frame from the same observation as the tree. settled_shot was
        // captured before the dump ran, so carrying it paired a frame with a tree
        // taken a full dump later: the same skew, one step downstream.
        carried = Some((post_elements, post_activity, post_frame));
    }

    // Exceeded max steps. If the progress detector fired during the run, the
    // user almost certainly wants to know it was a stuck loop, not a clean
    // step-cap exit — so the outcome label leans on the richer signal.
    let outcome = if outcome_builder.progress_detector_fired {
        RunOutcomeKind::ProgressStuck
    } else {
        RunOutcomeKind::StepCap
    };
    finalize_run(
        outcome_builder,
        &detector,
        &history,
        &config.app_package,
        &last_activity,
        outcome,
        config.max_steps,
    );
    Ok(OodaResult {
        task: config.task.clone(),
        success: false,
        steps: config.max_steps,
        final_reasoning: format!("Exceeded max steps ({})", config.max_steps),
        history,
    })
}

/// Execute an OODA action on the device.
async fn execute_action(
    transport: &dyn DeviceTransport,
    decision: &OodaDecision,
    annotated: &crate::screen::annotate::AnnotatedScreen,
    screen_w: u32,
    screen_h: u32,
    allowed_apps: Option<&[String]>,
) -> Result<String> {
    match decision.action {
        OodaAction::Tap => {
            let n = decision
                .element
                .ok_or_else(|| anyhow::anyhow!("Tap requires element number"))?;
            let (x, y) = ScreenAnnotator::tap_coordinates(annotated, n)
                .ok_or_else(|| anyhow::anyhow!("Element #{} not found", n))?;
            transport.tap(x, y).await?;

            let label = annotated
                .elements
                .iter()
                .find(|e| e.number == n)
                .map(|e| e.element.display_label().to_string())
                .unwrap_or_default();
            Ok(format!("Tapped #{} ({})", n, label))
        }
        OodaAction::Type => {
            let text = decision
                .text
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Type requires text"))?;

            // Tap element to focus first, then poll until the keyboard/focus
            // animation settles instead of a blind 300ms.
            if let Some(n) = decision.element {
                if let Some((x, y)) = ScreenAnnotator::tap_coordinates(annotated, n) {
                    transport.tap(x, y).await?;
                    crate::transport::wait_for_screen_stable(
                        transport,
                        std::time::Duration::from_millis(crate::transport::SETTLE_ACTION_MIN_MS),
                        std::time::Duration::from_millis(SETTLE_FOCUS_TIMEOUT_MS),
                    )
                    .await;
                }
            }

            transport.type_text(text).await?;
            Ok(format!("Typed \"{}\"", text))
        }
        OodaAction::Scroll => {
            // Direction is required — a silent default to "down" previously caused
            // the agent to blindly scroll vertically when horizontal tabs needed swiping.
            let dir = decision.direction.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Scroll requires direction (up/down/left/right) — pick one explicitly"
                )
            })?;
            let (from, to) = crate::transport::swipe_coords(dir, screen_w, screen_h);
            // iOS UIScrollView momentum requires <250ms swipe duration.
            transport.swipe(from, to, 120).await?;
            Ok(format!("Scrolled {}", dir))
        }
        OodaAction::LongPress => {
            let n = decision
                .element
                .ok_or_else(|| anyhow::anyhow!("LongPress requires element number"))?;
            let (x, y) = ScreenAnnotator::tap_coordinates(annotated, n)
                .ok_or_else(|| anyhow::anyhow!("Element #{} not found", n))?;
            transport.long_press(x, y, 1000).await?;
            Ok(format!("Long pressed #{}", n))
        }
        OodaAction::PressBack => {
            transport.press_key(crate::transport::keycode::BACK).await?;
            Ok("Pressed back".to_string())
        }
        OodaAction::Wait => {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            Ok("Waited 1s".to_string())
        }
        OodaAction::Done => {
            // Shouldn't reach here — handled before execute
            Ok("Done".to_string())
        }
        OodaAction::OpenApp => {
            let name = decision
                .name
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("open_app requires name"))?;
            let installed = transport.list_installed_apps().await?;
            let matched = crate::ooda::prompt::match_app(&installed, name)
                .ok_or_else(|| anyhow::anyhow!("No installed app matches \"{}\"", name))?;
            // Defence-in-depth — transports also validate, but catch a malformed `pm list` line here.
            if !crate::validate::is_valid_package_name(&matched) {
                anyhow::bail!(
                    "OpenApp: matched package name \"{}\" failed validation",
                    matched
                );
            }
            // Allow-list gate — refuses launch even if the LLM was tricked.
            if let Some(allowed) = allowed_apps {
                if !allowed.iter().any(|p| p == &matched) {
                    tracing::warn!(
                        "OpenApp BLOCKED by allow-list: requested=\"{}\" matched=\"{}\" allowed={:?}",
                        name, matched, allowed,
                    );
                    anyhow::bail!(
                        "OpenApp: \"{}\" not in session allow-list — refusing to launch",
                        matched
                    );
                }
            }
            tracing::info!("OpenApp: resolved \"{}\" -> {}", name, matched);
            transport.launch_app(&matched).await?;
            Ok(format!("Opened {}", matched))
        }
        OodaAction::DrawPath => {
            let pts = decision
                .points
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("draw_path requires points"))?;
            if pts.len() < 2 {
                anyhow::bail!("draw_path requires at least 2 points");
            }
            let duration_ms = decision.duration_ms.unwrap_or(800);
            let points: Vec<crate::screen::ui_element::Point> = pts
                .iter()
                .map(|[x, y]| crate::screen::ui_element::Point::new(*x, *y))
                .collect();
            transport.draw_path(&points, duration_ms).await?;
            Ok(format!("Drew path ({} segments)", points.len() - 1))
        }
    }
}

#[cfg(test)]
mod observe_contract {
    /// The serial-capture fix lived in observe() while this loop hand-rolled its
    /// own parallel capture, so the unattended path kept the defect the MCP path
    /// had lost. Four reviewers found it independently.
    #[test]
    fn the_loop_observes_through_the_transport_contract() {
        // Assembled at runtime: a literal here would match itself via include_str!.
        let needle = format!("join!({}.screenshot()", "transport");
        let src = include_str!("mod.rs");
        assert!(
            !src.contains(&needle),
            "OODA must call transport.observe(), not capture in parallel itself"
        );
    }
}

#[cfg(test)]
mod tests {

    // A test suite's whole value is that PASSED means the task happened. Two paths
    // once returned success without the judge: the model's own `done`, and the
    // stall detector's goal_visible (which its own prompt defines as the target
    // being "ready to be acted on", not done). Both recorded JudgePass, so the
    // telemetry could not tell a verified pass from a self-report.
    fn ooda_source() -> String {
        std::fs::read_to_string("src/ooda/mod.rs").expect("read own source")
    }

    #[test]
    fn the_stall_verdict_cannot_end_a_run() {
        let src = ooda_source();
        let arm_start = src
            .find("ProgressVerdict::GoalVisible =>")
            .expect("arm exists");
        let arm = &src[arm_start..arm_start + 900];
        let next_arm = arm.find("ProgressVerdict::Stuck").unwrap_or(arm.len());
        let body = &arm[..next_arm];
        assert!(
            !body.contains("success: true"),
            "goal_visible ends the run as a pass again; it fires while the agent is stalling"
        );
        assert!(
            !body.contains("return Ok("),
            "goal_visible returns from the loop again instead of hinting the next step"
        );
    }

    #[test]
    fn a_completion_claim_is_checked_before_it_counts() {
        let src = ooda_source();
        let branch = src
            .find("if decision.done || decision.action == OodaAction::Done")
            .expect("done branch exists");
        let body = &src[branch
            ..src[branch..]
                .find("// \u{2500}\u{2500} ACT \u{2500}\u{2500}")
                .map_or(src.len(), |e| branch + e)];
        assert!(
            body.contains("check_goal_complete"),
            "the model's own `done` is accepted without asking the judge"
        );
        assert!(
            body.contains("SelfReported"),
            "an unverified pass is recorded as JudgePass, which fabricates a verified number"
        );
        // A judge that could not be reached is not a judge saying no. Refusing on
        // Unavailable makes one provider hiccup refuse every completion and burn
        // the whole step budget against a device that already finished.
        assert!(
            body.contains("JudgeVerdict::NotComplete"),
            "the refusal must fire only on an ANSWERED negative verdict"
        );
        assert!(
            !body.contains("!verified") && !body.contains("judge_available"),
            "the boolean judge contract is back, which cannot distinguish 'no' from 'could not ask'"
        );
    }

    #[test]
    fn a_self_report_is_not_a_judge_pass() {
        use crate::run_outcome::RunOutcomeKind;
        assert_ne!(
            RunOutcomeKind::SelfReported.as_str(),
            RunOutcomeKind::JudgePass.as_str(),
            "the two must stay distinguishable in telemetry"
        );
    }

    use super::*;

    #[test]
    fn test_ooda_config() {
        let config = OodaConfig {
            task: "login".to_string(),
            app_package: "com.app".to_string(),
            max_steps: 30,
            device_id: "emulator-5554".to_string(),
            force_vision: false,
            verify_completion: true,
            cleanup_wda: false,
            allowed_apps: None,
        };
        assert_eq!(config.max_steps, 30);
        assert!(config.verify_completion);
    }

    #[test]
    fn cleanup_wda_field_defaults_false() {
        // The `drengr run` CLI default is opt-in; the runner.rs path sets it
        // to false explicitly. Codify both here so a future regression trips
        // a unit test, not a surprise reinstall loop.
        let config = OodaConfig {
            task: "x".into(),
            app_package: "com.app".into(),
            max_steps: 1,
            device_id: "d".into(),
            force_vision: false,
            verify_completion: false,
            cleanup_wda: false,
            allowed_apps: None,
        };
        assert!(!config.cleanup_wda);
    }

    #[tokio::test]
    async fn cleanup_runtime_default_is_ok() {
        use crate::network::events::NetworkEvent;
        use crate::screen::ui_element::{DeviceInfo, Point, UiElement};
        use crate::transport::DeviceTransport;

        struct NoopTransport;

        #[async_trait::async_trait]
        impl DeviceTransport for NoopTransport {
            async fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
                Ok(vec![])
            }
            async fn ui_tree(&self) -> anyhow::Result<Vec<UiElement>> {
                Ok(vec![])
            }
            async fn tap(&self, _x: i32, _y: i32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn long_press(&self, _x: i32, _y: i32, _d: u32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn swipe(&self, _f: Point, _t: Point, _d: u32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn type_text(&self, _t: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn press_key(&self, _k: i32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn launch_app(&self, _p: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn screen_size(&self) -> anyhow::Result<(u32, u32)> {
                Ok((100, 100))
            }
            async fn is_connected(&self) -> bool {
                true
            }
            async fn is_app_in_foreground(&self, _p: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn clear_focused_field(&self) -> anyhow::Result<()> {
                Ok(())
            }
            async fn device_info(&self) -> anyhow::Result<DeviceInfo> {
                Ok(DeviceInfo {
                    id: "x".into(),
                    os: "test".into(),
                    model: "test".into(),
                    sdk_version: None,
                })
            }
            async fn current_activity(&self) -> anyhow::Result<String> {
                Ok("act".into())
            }
            async fn capture_http_logs(&self) -> anyhow::Result<Vec<NetworkEvent>> {
                Ok(vec![])
            }
        }

        let t = NoopTransport;
        assert!(t.cleanup_runtime().await.is_ok());
    }

    // ─── OpenApp allow-list scope-lock ────────────────────────────────────

    #[tokio::test]
    async fn open_app_blocked_when_not_in_allow_list() {
        use crate::network::events::NetworkEvent;
        use crate::screen::ui_element::{DeviceInfo, Point, UiElement};
        use crate::transport::DeviceTransport;

        struct InstalledTransport(Vec<String>);

        #[async_trait::async_trait]
        impl DeviceTransport for InstalledTransport {
            async fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
                Ok(vec![])
            }
            async fn ui_tree(&self) -> anyhow::Result<Vec<UiElement>> {
                Ok(vec![])
            }
            async fn tap(&self, _x: i32, _y: i32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn long_press(&self, _x: i32, _y: i32, _d: u32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn swipe(&self, _f: Point, _t: Point, _d: u32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn type_text(&self, _t: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn press_key(&self, _k: i32) -> anyhow::Result<()> {
                Ok(())
            }
            async fn launch_app(&self, _p: &str) -> anyhow::Result<()> {
                Ok(())
            }
            async fn screen_size(&self) -> anyhow::Result<(u32, u32)> {
                Ok((100, 100))
            }
            async fn is_connected(&self) -> bool {
                true
            }
            async fn is_app_in_foreground(&self, _p: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn clear_focused_field(&self) -> anyhow::Result<()> {
                Ok(())
            }
            async fn list_installed_apps(&self) -> anyhow::Result<Vec<String>> {
                Ok(self.0.clone())
            }
            async fn device_info(&self) -> anyhow::Result<DeviceInfo> {
                Ok(DeviceInfo {
                    id: "x".into(),
                    os: "test".into(),
                    model: "test".into(),
                    sdk_version: None,
                })
            }
            async fn current_activity(&self) -> anyhow::Result<String> {
                Ok("act".into())
            }
            async fn capture_http_logs(&self) -> anyhow::Result<Vec<NetworkEvent>> {
                Ok(vec![])
            }
        }

        let transport =
            InstalledTransport(vec!["com.example.app".into(), "com.victim.bank".into()]);
        let annotated = crate::screen::annotate::AnnotatedScreen {
            image_data: vec![],
            elements: vec![],
            source_frame: Vec::new(),
        };
        let decision = OodaDecision {
            action: OodaAction::OpenApp,
            element: None,
            text: None,
            direction: None,
            name: Some("bank".into()), // would resolve to com.victim.bank
            points: None,
            duration_ms: None,
            reasoning: "open the bank".into(),
            done: false,
        };

        // Allow-list ONLY contains the legitimate app — bank must be refused.
        let allowed = vec!["com.example.app".to_string()];
        let res = execute_action(&transport, &decision, &annotated, 100, 100, Some(&allowed)).await;
        assert!(res.is_err(), "expected allow-list to block cross-app open");
        let err = format!("{:?}", res.unwrap_err());
        assert!(
            err.contains("allow-list"),
            "error should mention allow-list, got: {}",
            err
        );

        // None means unrestricted — the same call should succeed.
        let res = execute_action(&transport, &decision, &annotated, 100, 100, None).await;
        assert!(
            res.is_ok(),
            "None allow-list should permit any installed app"
        );
    }

    #[test]
    fn test_ooda_result_success() {
        let result = OodaResult {
            task: "login".to_string(),
            success: true,
            steps: 5,
            final_reasoning: "Dashboard visible".to_string(),
            history: vec![],
        };
        assert!(result.success);
        assert_eq!(result.steps, 5);
    }
}
