use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::screen::ui_element::UiElement;
use crate::transport::DeviceTransport;

/// A map of all discovered screens in an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenMap {
    pub app: String,
    pub explored_at: String,
    pub screens: Vec<ScreenNode>,
    pub edges: Vec<NavigationEdge>,
}

/// A discovered screen with its elements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenNode {
    pub id: String,
    pub activity: String,
    pub elements: Vec<ScreenElement>,
}

/// An element on a discovered screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenElement {
    pub text: String,
    #[serde(rename = "type")]
    pub element_type: String,
    pub navigates_to: Option<String>,
}

/// A navigation edge between two screens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigationEdge {
    pub from: String,
    pub action: String,
    pub to: String,
}

/// Configuration for the exploration run.
pub struct ExploreConfig {
    pub app_package: String,
    pub max_screens: usize,
    pub max_actions_per_screen: usize,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        Self {
            app_package: String::new(),
            max_screens: 20,
            max_actions_per_screen: 10,
        }
    }
}

/// Run BFS exploration of an app to build a screen map.
pub async fn explore_app(
    transport: &dyn DeviceTransport,
    config: &ExploreConfig,
) -> Result<ScreenMap> {
    let mut screens: Vec<ScreenNode> = Vec::new();
    let mut edges: Vec<NavigationEdge> = Vec::new();
    let mut visited_activities: HashSet<String> = HashSet::new();
    let mut explore_queue: VecDeque<String> = VecDeque::new();

    // Launch app
    transport
        .launch_app(&config.app_package)
        .await
        .context("Failed to launch app")?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Capture initial screen
    let initial_activity = crate::transport::activity_or_unknown(transport).await;
    explore_queue.push_back(initial_activity.clone());

    println!("Exploring {}...", config.app_package);

    while let Some(current_activity) = explore_queue.pop_front() {
        if visited_activities.contains(&current_activity) {
            continue;
        }
        if screens.len() >= config.max_screens {
            println!("Reached max screens ({}), stopping.", config.max_screens);
            break;
        }

        visited_activities.insert(current_activity.clone());

        // Capture screen state
        let elements = transport.ui_tree().await.unwrap_or_default();
        let interactive: Vec<&UiElement> = elements
            .iter()
            .filter(|e| e.is_interactive())
            .take(config.max_actions_per_screen)
            .collect();

        let screen_id = activity_to_id(&current_activity);

        println!(
            "Screen {}: {} — {} elements",
            screens.len() + 1,
            screen_id,
            interactive.len()
        );

        let mut screen_elements: Vec<ScreenElement> = Vec::new();

        // Try tapping each interactive element to see if it navigates
        for elem in &interactive {
            let label = elem.display_label().to_string();
            let cx = elem.bounds.center_x();
            let cy = elem.bounds.center_y();

            // Skip text fields — typing doesn't navigate
            if elem.editable {
                screen_elements.push(ScreenElement {
                    text: label,
                    element_type: elem.short_class().to_string(),
                    navigates_to: None,
                });
                continue;
            }

            // Tap the element
            if transport.tap(cx, cy).await.is_err() {
                continue;
            }
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;

            // Check if we navigated
            let post_activity = crate::transport::activity_or_unknown(transport).await;

            if post_activity != current_activity {
                // Skip screens outside the target app (e.g. launcher, system settings)
                if !post_activity.contains(&config.app_package) {
                    transport
                        .press_key(crate::transport::keycode::BACK)
                        .await
                        .ok();
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    screen_elements.push(ScreenElement {
                        text: label,
                        element_type: elem.short_class().to_string(),
                        navigates_to: None,
                    });
                    continue;
                }

                // Navigation happened within the app
                let target_id = activity_to_id(&post_activity);

                screen_elements.push(ScreenElement {
                    text: label.clone(),
                    element_type: elem.short_class().to_string(),
                    navigates_to: Some(target_id.clone()),
                });

                edges.push(NavigationEdge {
                    from: screen_id.clone(),
                    action: format!("tap {}", label),
                    to: target_id.clone(),
                });

                println!("  {} → {} (via {})", screen_id, target_id, label);

                // Queue the new screen for exploration
                if !visited_activities.contains(&post_activity) {
                    explore_queue.push_back(post_activity.clone());
                }

                // Press back to return
                transport
                    .press_key(crate::transport::keycode::BACK)
                    .await
                    .ok();
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                // Verify we're back
                let back_activity = crate::transport::activity_or_unknown(transport).await;
                if back_activity != current_activity {
                    // Couldn't go back — relaunch and navigate
                    transport.launch_app(&config.app_package).await.ok();
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    break;
                }
            } else {
                // No navigation — record element without target
                screen_elements.push(ScreenElement {
                    text: label,
                    element_type: elem.short_class().to_string(),
                    navigates_to: None,
                });

                // Press back in case a dialog/popup appeared
                transport
                    .press_key(crate::transport::keycode::BACK)
                    .await
                    .ok();
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }

        // Add "back" edge if applicable
        if !screens.is_empty() {
            // Record that pressing back goes to the previous screen
            let back_activity = {
                transport
                    .press_key(crate::transport::keycode::BACK)
                    .await
                    .ok();
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                crate::transport::activity_or_unknown(transport).await
            };

            if back_activity != current_activity {
                let back_id = activity_to_id(&back_activity);
                edges.push(NavigationEdge {
                    from: screen_id.clone(),
                    action: "back".to_string(),
                    to: back_id,
                });

                // Return to current screen (navigate back forward)
                transport.launch_app(&config.app_package).await.ok();
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        screens.push(ScreenNode {
            id: screen_id,
            activity: current_activity,
            elements: screen_elements,
        });
    }

    let explored_at = chrono::Utc::now().to_rfc3339();

    println!(
        "\nFound {} screens, {} elements, {} navigation edges",
        screens.len(),
        screens.iter().map(|s| s.elements.len()).sum::<usize>(),
        edges.len()
    );

    Ok(ScreenMap {
        app: config.app_package.clone(),
        explored_at,
        screens,
        edges,
    })
}

fn maps_dir() -> PathBuf {
    crate::paths::drengr_dir_or(".").join("maps")
}

/// Save screen map to .drengr/maps/<package>.json
pub fn save_screen_map(map: &ScreenMap) -> Result<PathBuf> {
    if !crate::validate::is_valid_package_name(&map.app) {
        anyhow::bail!("Invalid package name: {}", map.app);
    }
    let maps_dir = maps_dir();
    std::fs::create_dir_all(&maps_dir)?;

    let path = maps_dir.join(format!("{}.json", map.app));
    let json = serde_json::to_string_pretty(map)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Load a previously saved screen map.
pub fn load_screen_map(app_package: &str) -> Result<Option<ScreenMap>> {
    if !crate::validate::is_valid_package_name(app_package) {
        anyhow::bail!("Invalid package name: {}", app_package);
    }
    let path = maps_dir().join(format!("{}.json", app_package));

    if !path.exists() {
        return Ok(None);
    }

    let json = std::fs::read_to_string(&path)?;
    let map: ScreenMap = serde_json::from_str(&json)?;
    Ok(Some(map))
}

/// Build navigation context for LLM from screen map + current activity.
pub fn navigation_context(map: &ScreenMap, current_activity: &str) -> serde_json::Value {
    let current_id = activity_to_id(current_activity);

    let reachable: Vec<serde_json::Value> = map
        .edges
        .iter()
        .filter(|e| e.from == current_id)
        .map(|e| {
            serde_json::json!({
                "screen": e.to,
                "via": e.action,
            })
        })
        .collect();

    // BFS shortest paths from current screen to all others
    let paths = bfs_shortest_paths(map, &current_id);
    let path_to_target = if paths.is_empty() {
        current_id.clone()
    } else {
        // Show the longest reachable path as a representative
        paths
            .values()
            .max_by_key(|p| p.len())
            .map(|p| p.join(" → "))
            .unwrap_or_else(|| current_id.clone())
    };

    serde_json::json!({
        "current_screen": current_id,
        "reachable_screens": reachable,
        "path_to_target": path_to_target,
        "total_app_screens": map.screens.len(),
        "explored_screens": map.screens.len(),
    })
}

/// BFS shortest paths from `start` to all reachable screens.
/// Returns a map of screen_id → path (vec of screen IDs from start to target).
pub(crate) fn bfs_shortest_paths(
    map: &ScreenMap,
    start: &str,
) -> std::collections::HashMap<String, Vec<String>> {
    use std::collections::HashMap;

    // Build adjacency list from edges
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &map.edges {
        adj.entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }

    let mut paths: HashMap<String, Vec<String>> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();

    visited.insert(start.to_string());
    queue.push_back((start.to_string(), vec![start.to_string()]));

    while let Some((current, path)) = queue.pop_front() {
        if let Some(neighbors) = adj.get(current.as_str()) {
            for &neighbor in neighbors {
                if !visited.contains(neighbor) {
                    visited.insert(neighbor.to_string());
                    let mut new_path = path.clone();
                    new_path.push(neighbor.to_string());
                    paths.insert(neighbor.to_string(), new_path.clone());
                    queue.push_back((neighbor.to_string(), new_path));
                }
            }
        }
    }

    paths
}

/// Convert activity name to a short ID.
pub(crate) fn activity_to_id(activity: &str) -> String {
    // "com.app/.ui.LoginActivity" → "login"
    let name = activity
        .rsplit('.')
        .next()
        .unwrap_or(activity)
        .trim_end_matches("Activity")
        .trim_end_matches("Fragment");

    name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activity_to_id() {
        assert_eq!(activity_to_id("com.app/.ui.LoginActivity"), "login");
        assert_eq!(activity_to_id("com.app/.DashboardActivity"), "dashboard");
        assert_eq!(activity_to_id("com.app/.SettingsFragment"), "settings");
        assert_eq!(activity_to_id("MainActivity"), "main");
    }

    #[test]
    fn test_screen_map_serialize() {
        let map = ScreenMap {
            app: "com.test".to_string(),
            explored_at: "2026-03-04T10:00:00Z".to_string(),
            screens: vec![ScreenNode {
                id: "login".to_string(),
                activity: "com.app/.LoginActivity".to_string(),
                elements: vec![
                    ScreenElement {
                        text: "Email".to_string(),
                        element_type: "EditText".to_string(),
                        navigates_to: None,
                    },
                    ScreenElement {
                        text: "Login".to_string(),
                        element_type: "Button".to_string(),
                        navigates_to: Some("dashboard".to_string()),
                    },
                ],
            }],
            edges: vec![NavigationEdge {
                from: "login".to_string(),
                action: "tap Login".to_string(),
                to: "dashboard".to_string(),
            }],
        };

        let json = serde_json::to_string_pretty(&map).unwrap();
        assert!(json.contains("\"login\""));
        assert!(json.contains("\"dashboard\""));
        assert!(json.contains("tap Login"));
    }

    #[test]
    fn test_screen_map_deserialize() {
        let json = r#"{
            "app": "com.test",
            "explored_at": "2026-03-04T10:00:00Z",
            "screens": [
                {
                    "id": "login",
                    "activity": "LoginActivity",
                    "elements": [
                        {"text": "Login", "type": "Button", "navigates_to": "dashboard"}
                    ]
                }
            ],
            "edges": [
                {"from": "login", "action": "tap Login", "to": "dashboard"}
            ]
        }"#;

        let map: ScreenMap = serde_json::from_str(json).unwrap();
        assert_eq!(map.screens.len(), 1);
        assert_eq!(map.edges.len(), 1);
        assert_eq!(
            map.screens[0].elements[0].navigates_to,
            Some("dashboard".to_string())
        );
    }

    #[test]
    fn test_navigation_context() {
        let map = ScreenMap {
            app: "com.app".to_string(),
            explored_at: String::new(),
            screens: vec![
                ScreenNode {
                    id: "login".to_string(),
                    activity: "LoginActivity".to_string(),
                    elements: vec![],
                },
                ScreenNode {
                    id: "dashboard".to_string(),
                    activity: "DashboardActivity".to_string(),
                    elements: vec![],
                },
            ],
            edges: vec![
                NavigationEdge {
                    from: "login".to_string(),
                    action: "tap Login".to_string(),
                    to: "dashboard".to_string(),
                },
                NavigationEdge {
                    from: "dashboard".to_string(),
                    action: "tap Profile".to_string(),
                    to: "profile".to_string(),
                },
            ],
        };

        let ctx = navigation_context(&map, "com.app/.LoginActivity");
        assert_eq!(ctx["current_screen"], "login");
        assert_eq!(ctx["total_app_screens"], 2);

        let reachable = ctx["reachable_screens"].as_array().unwrap();
        assert_eq!(reachable.len(), 1);
        assert_eq!(reachable[0]["screen"], "dashboard");
        assert_eq!(reachable[0]["via"], "tap Login");

        // path_to_target should include BFS path
        let path = ctx["path_to_target"].as_str().unwrap();
        assert!(path.contains("login"));
        assert!(path.contains("dashboard"));
    }

    #[test]
    fn test_navigation_context_no_edges() {
        let map = ScreenMap {
            app: "com.app".to_string(),
            explored_at: String::new(),
            screens: vec![],
            edges: vec![],
        };

        let ctx = navigation_context(&map, "UnknownActivity");
        let reachable = ctx["reachable_screens"].as_array().unwrap();
        assert!(reachable.is_empty());
    }

    #[test]
    fn test_explore_config_default() {
        let config = ExploreConfig::default();
        assert_eq!(config.max_screens, 20);
        assert_eq!(config.max_actions_per_screen, 10);
    }
}
