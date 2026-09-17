use serde_json::{json, Value};

/// Sauce Labs regional endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SauceRegion {
    UsWest1,
    EuCentral1,
    UsEast4,
}

impl SauceRegion {
    pub fn from_env() -> Self {
        match std::env::var("SAUCE_REGION").unwrap_or_default().as_str() {
            "eu-central-1" | "eu" => Self::EuCentral1,
            "us-east-4" | "us-east" => Self::UsEast4,
            _ => Self::UsWest1,
        }
    }

    fn host(&self) -> &str {
        match self {
            Self::UsWest1 => "ondemand.us-west-1.saucelabs.com",
            Self::EuCentral1 => "ondemand.eu-central-1.saucelabs.com",
            Self::UsEast4 => "ondemand.us-east-4.saucelabs.com",
        }
    }
}

/// Cloud device provider for Appium.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudProvider {
    BrowserStack,
    SauceLabs {
        region: SauceRegion,
    },
    AwsDeviceFarm,
    LambdaTest,
    Perfecto {
        cloud_name: String,
    },
    Kobiton,
    /// Custom Appium server (local, self-hosted, or any compatible endpoint)
    Custom {
        hub_url: String,
    },
}

/// Provider-specific configuration derived from the enum variant.
pub struct ProviderConfig {
    pub hub_url: String,
    pub capabilities_key: String,
    pub needs_basic_auth: bool,
}

impl CloudProvider {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "browserstack" | "bs" => Self::BrowserStack,
            "saucelabs" | "sauce" => Self::SauceLabs {
                region: SauceRegion::from_env(),
            },
            "aws" | "device_farm" | "devicefarm" => Self::AwsDeviceFarm,
            "lambdatest" | "lt" => Self::LambdaTest,
            "perfecto" => Self::Perfecto {
                cloud_name: std::env::var("PERFECTO_CLOUD").unwrap_or_default(),
            },
            "kobiton" => Self::Kobiton,
            "custom" => Self::Custom {
                hub_url: std::env::var("APPIUM_HUB_URL")
                    .unwrap_or_else(|_| "http://localhost:4723".into()),
            },
            other if other.starts_with("http") => Self::Custom {
                hub_url: other.to_string(),
            },
            _ => Self::Custom {
                hub_url: std::env::var("APPIUM_HUB_URL")
                    .unwrap_or_else(|_| "http://localhost:4723".into()),
            },
        }
    }

    pub fn config(&self) -> ProviderConfig {
        match self {
            Self::BrowserStack => ProviderConfig {
                hub_url: "https://hub-cloud.browserstack.com/wd/hub".into(),
                capabilities_key: "bstack:options".into(),
                needs_basic_auth: true,
            },
            Self::SauceLabs { region } => ProviderConfig {
                hub_url: format!("https://{}/wd/hub", region.host()),
                capabilities_key: "sauce:options".into(),
                needs_basic_auth: true,
            },
            Self::AwsDeviceFarm => ProviderConfig {
                hub_url: std::env::var("AWS_DEVICE_FARM_URL")
                    .unwrap_or_else(|_| "https://devicefarm.us-west-2.amazonaws.com".into()),
                capabilities_key: "aws:options".into(),
                needs_basic_auth: true,
            },
            Self::LambdaTest => ProviderConfig {
                hub_url: "https://mobile-hub.lambdatest.com/wd/hub".into(),
                capabilities_key: "lt:options".into(),
                needs_basic_auth: true,
            },
            Self::Perfecto { cloud_name } => {
                // Sanitize cloud_name: must be alphanumeric/hyphens only (it's a subdomain).
                // Prevents path traversal or host injection via PERFECTO_CLOUD env var.
                let safe_name: String = cloud_name
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '-')
                    .collect();
                ProviderConfig {
                    hub_url: format!(
                        "https://{}.perfectomobile.com/nexperience/perfectomobile/wd/hub",
                        safe_name
                    ),
                    capabilities_key: "perfecto:options".into(),
                    needs_basic_auth: false, // Perfecto uses securityToken in capabilities
                }
            }
            Self::Kobiton => ProviderConfig {
                hub_url: "https://api.kobiton.com/wd/hub".into(),
                capabilities_key: "kobiton:options".into(),
                needs_basic_auth: true,
            },
            Self::Custom { hub_url } => ProviderConfig {
                hub_url: hub_url.clone(),
                capabilities_key: "appium:options".into(),
                // Custom hubs don't send basic auth by default — credentials are only
                // embedded in capabilities if the user explicitly provides them.
                // This prevents credential exfiltration if a malicious URL is injected.
                needs_basic_auth: false,
            },
        }
    }

    /// Human-readable name for logging.
    pub fn name(&self) -> &str {
        match self {
            Self::BrowserStack => "BrowserStack",
            Self::SauceLabs { .. } => "Sauce Labs",
            Self::AwsDeviceFarm => "AWS Device Farm",
            Self::LambdaTest => "LambdaTest",
            Self::Perfecto { .. } => "Perfecto",
            Self::Kobiton => "Kobiton",
            Self::Custom { .. } => "Custom",
        }
    }
}

/// Configuration for creating an Appium cloud session.
pub struct AppiumConfig {
    pub provider: CloudProvider,
    pub server_url: Option<String>,
    pub username: String,
    pub access_key: String,
    pub device_name: String,
    pub platform: String,
    pub os_version: String,
    pub app: Option<String>,
}

/// Session-shaping options resolved from the environment once, at connect.
/// Read here rather than at each use so a session cannot change behaviour
/// half-way through, and so tests can construct the options directly.
pub(super) struct SessionOptions {
    pub auto_accept_alerts: bool,
}

impl SessionOptions {
    pub fn from_env() -> Self {
        Self {
            auto_accept_alerts: env_flag("DRENGR_APPIUM_AUTO_ACCEPT_ALERTS"),
        }
    }
}

impl Default for SessionOptions {
    /// `autoAcceptAlerts` OFF. With it on, XCUITest dismisses every alert before
    /// `alert_text` can look, so `alert_text` reports "nothing showing" for a
    /// permission dialog that did appear and was answered without us. A
    /// capability that silently disables another capability has to be opt-in.
    fn default() -> Self {
        Self {
            auto_accept_alerts: false,
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Build WebDriver capabilities for the cloud provider.
pub(super) fn build_capabilities(config: &AppiumConfig, options: &SessionOptions) -> Value {
    let is_android = config.platform.to_lowercase().contains("android");
    let mut caps = json!({
        "platformName": config.platform,
        "appium:deviceName": config.device_name,
        "appium:platformVersion": config.os_version,
        "appium:automationName": if is_android { "UiAutomator2" } else { "XCUITest" },
        "appium:newCommandTimeout": 300,
    });

    if let Some(app) = &config.app {
        caps["appium:app"] = json!(app);
    }

    if is_android {
        // Lower screenshot quality for faster transfer.
        caps["appium:screenshotQuality"] = json!(1);
    } else if options.auto_accept_alerts {
        caps["appium:autoAcceptAlerts"] = json!(true);
    }

    let provider_config = config.provider.config();

    // Embed credentials in provider-specific capability block
    if !config.username.is_empty() {
        let creds = match &config.provider {
            CloudProvider::Perfecto { .. } => json!({ "securityToken": config.access_key }),
            _ => json!({
                "userName": config.username,
                "accessKey": config.access_key,
            }),
        };
        caps[&provider_config.capabilities_key] = creds;
    }

    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_provider_parsing() {
        assert_eq!(
            CloudProvider::parse("browserstack"),
            CloudProvider::BrowserStack
        );
        assert_eq!(CloudProvider::parse("bs"), CloudProvider::BrowserStack);
        assert!(matches!(
            CloudProvider::parse("saucelabs"),
            CloudProvider::SauceLabs { .. }
        ));
        assert!(matches!(
            CloudProvider::parse("sauce"),
            CloudProvider::SauceLabs { .. }
        ));
        assert!(matches!(
            CloudProvider::parse("lambdatest"),
            CloudProvider::LambdaTest
        ));
        assert!(matches!(
            CloudProvider::parse("lt"),
            CloudProvider::LambdaTest
        ));
        assert!(matches!(
            CloudProvider::parse("aws"),
            CloudProvider::AwsDeviceFarm
        ));
        assert!(matches!(
            CloudProvider::parse("kobiton"),
            CloudProvider::Kobiton
        ));
        assert!(matches!(
            CloudProvider::parse("custom"),
            CloudProvider::Custom { .. }
        ));
        assert!(matches!(
            CloudProvider::parse("unknown"),
            CloudProvider::Custom { .. }
        ));
    }

    #[test]
    fn test_provider_config_urls() {
        assert!(CloudProvider::BrowserStack
            .config()
            .hub_url
            .contains("browserstack"));
        assert!(CloudProvider::SauceLabs {
            region: SauceRegion::UsWest1
        }
        .config()
        .hub_url
        .contains("saucelabs"));
        assert!(CloudProvider::LambdaTest
            .config()
            .hub_url
            .contains("lambdatest"));
        assert!(CloudProvider::Kobiton.config().hub_url.contains("kobiton"));
        assert!(CloudProvider::AwsDeviceFarm
            .config()
            .hub_url
            .contains("devicefarm"));
    }

    fn config_for(platform: &str, provider: CloudProvider, app: Option<&str>) -> AppiumConfig {
        AppiumConfig {
            provider,
            server_url: None,
            username: "user".to_string(),
            access_key: "key".to_string(),
            device_name: if platform == "Android" {
                "Pixel 6".into()
            } else {
                "iPhone 14".into()
            },
            platform: platform.to_string(),
            os_version: "13.0".to_string(),
            app: app.map(str::to_string),
        }
    }

    #[test]
    fn test_build_capabilities_android() {
        let caps = build_capabilities(
            &config_for("Android", CloudProvider::BrowserStack, Some("bs://app-id")),
            &SessionOptions::default(),
        );
        assert_eq!(caps["platformName"], "Android");
        assert_eq!(caps["appium:deviceName"], "Pixel 6");
        assert_eq!(caps["appium:platformVersion"], "13.0");
        assert_eq!(caps["appium:automationName"], "UiAutomator2");
        assert_eq!(caps["appium:app"], "bs://app-id");
        assert_eq!(caps["bstack:options"]["userName"], "user");
        assert!(caps.get("appium:autoAcceptAlerts").is_none());
    }

    #[test]
    fn test_build_capabilities_ios() {
        let caps = build_capabilities(
            &config_for(
                "iOS",
                CloudProvider::SauceLabs {
                    region: SauceRegion::UsWest1,
                },
                None,
            ),
            &SessionOptions::default(),
        );
        assert_eq!(caps["appium:automationName"], "XCUITest");
        assert!(caps.get("appium:app").is_none());
        assert_eq!(caps["sauce:options"]["userName"], "user");
        assert!(caps.get("appium:screenshotQuality").is_none());
    }

    #[test]
    fn test_custom_provider_no_extras() {
        let mut config = config_for(
            "Android",
            CloudProvider::Custom {
                hub_url: "http://my-server:4723".into(),
            },
            None,
        );
        config.username = String::new();
        config.access_key = String::new();
        let caps = build_capabilities(&config, &SessionOptions::default());
        assert!(caps.get("bstack:options").is_none());
        assert!(caps.get("sauce:options").is_none());
    }

    #[test]
    fn ios_alerts_are_left_for_the_alert_methods_to_see() {
        // autoAcceptAlerts makes XCUITest answer every dialog before we look, so
        // alert_text reports "nothing showing" for an alert that did appear.
        let caps = build_capabilities(
            &config_for("iOS", CloudProvider::BrowserStack, None),
            &SessionOptions::default(),
        );
        assert!(
            caps.get("appium:autoAcceptAlerts").is_none(),
            "alert handling was taken away from alert_text/alert_accept/alert_dismiss by default"
        );
    }

    #[test]
    fn auto_accepting_alerts_is_available_but_has_to_be_asked_for() {
        let caps = build_capabilities(
            &config_for("iOS", CloudProvider::BrowserStack, None),
            &SessionOptions {
                auto_accept_alerts: true,
            },
        );
        assert_eq!(caps["appium:autoAcceptAlerts"], true);
    }

    #[test]
    fn android_never_carries_the_ios_alert_capability() {
        for opts in [
            SessionOptions::default(),
            SessionOptions {
                auto_accept_alerts: true,
            },
        ] {
            let caps = build_capabilities(
                &config_for("Android", CloudProvider::BrowserStack, None),
                &opts,
            );
            assert!(caps.get("appium:autoAcceptAlerts").is_none());
        }
    }

    #[test]
    fn only_an_affirmative_value_turns_the_flag_on() {
        for on in ["1", "true", "TRUE", "yes", " on "] {
            std::env::set_var("DRENGR_TEST_FLAG", on);
            assert!(env_flag("DRENGR_TEST_FLAG"), "{on:?} should read as on");
        }
        for off in ["0", "false", "no", "", "maybe"] {
            std::env::set_var("DRENGR_TEST_FLAG", off);
            assert!(!env_flag("DRENGR_TEST_FLAG"), "{off:?} should read as off");
        }
        std::env::remove_var("DRENGR_TEST_FLAG");
    }

    #[test]
    fn test_all_providers_have_names() {
        assert_eq!(CloudProvider::BrowserStack.name(), "BrowserStack");
        assert_eq!(
            CloudProvider::SauceLabs {
                region: SauceRegion::EuCentral1
            }
            .name(),
            "Sauce Labs"
        );
        assert_eq!(CloudProvider::AwsDeviceFarm.name(), "AWS Device Farm");
        assert_eq!(CloudProvider::LambdaTest.name(), "LambdaTest");
        assert_eq!(
            CloudProvider::Perfecto {
                cloud_name: "test".into()
            }
            .name(),
            "Perfecto"
        );
        assert_eq!(CloudProvider::Kobiton.name(), "Kobiton");
        assert_eq!(
            CloudProvider::Custom {
                hub_url: "http://x".into()
            }
            .name(),
            "Custom"
        );
    }
}
