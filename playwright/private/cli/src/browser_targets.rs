use std::path::PathBuf;

use serde::Serialize;

use crate::{
    browsers::{BrowserData, Browsers},
    download_paths::{DownloadPaths, Platform},
    platform_groups::PlatformGroup,
};

/// Returns the first chromium revision that ships as a Chrome for Testing (CfT)
/// build, or `None` if the browser never uses CfT URLs.
///
/// Playwright 1.57 switched the chromium family from the legacy
/// `builds/chromium/{revision}/...` archives to Chrome for Testing
/// `builds/cft/{browserVersion}/...` archives. The revisions below are the first
/// CfT revisions observed in `playwright-core`'s `browsers.json`
/// (chromium / chromium-headless-shell: 1194 in 1.56 -> 1200 in 1.57;
/// chromium-tip-of-tree(-headless-shell): 1371 in 1.56 -> 1380 in 1.57).
/// Keeping the threshold here means older `browsers.json` files (Playwright < 1.57)
/// continue to resolve to the legacy URLs.
/// See <https://github.com/microsoft/playwright/releases/tag/v1.57.0>
fn cft_min_revision(browser_name: &str) -> Option<u32> {
    match browser_name {
        "chromium" | "chromium-headless-shell" => Some(1200),
        "chromium-tip-of-tree" | "chromium-tip-of-tree-headless-shell" => Some(1380),
        _ => None,
    }
}

/// Returns the first revision whose ARM64 Linux build ships as Chrome for Testing,
/// or `None` if it never does. chromium switched in Playwright 1.63 (revision
/// 1243); chromium-tip-of-tree was dropped before publishing one.
fn cft_linux_arm64_min_revision(browser_name: &str) -> Option<u32> {
    match browser_name {
        "chromium" | "chromium-headless-shell" => Some(1243),
        _ => None,
    }
}

/// Builds the Chrome for Testing download path for the given chromium-family
/// browser and platform, or `None` if that platform keeps using the legacy
/// chromium archives (ARM64 Linux, see `cft_linux_arm64_min_revision`).
fn cft_download_path(
    browser_name: &str,
    platform: &Platform,
    browser_version: &str,
    revision: u32,
) -> Option<String> {
    let is_headless = browser_name.ends_with("-headless-shell");
    let platform_group: PlatformGroup = platform.clone().into();
    let suffix = match platform_group {
        PlatformGroup::LinuxX86_64 if is_headless => "linux64/chrome-headless-shell-linux64.zip",
        PlatformGroup::LinuxX86_64 => "linux64/chrome-linux64.zip",
        PlatformGroup::MacosX86_64 if is_headless => "mac-x64/chrome-headless-shell-mac-x64.zip",
        PlatformGroup::MacosX86_64 => "mac-x64/chrome-mac-x64.zip",
        PlatformGroup::MacosArm64 if is_headless => "mac-arm64/chrome-headless-shell-mac-arm64.zip",
        PlatformGroup::MacosArm64 => "mac-arm64/chrome-mac-arm64.zip",
        PlatformGroup::LinuxArm64
            if !cft_linux_arm64_min_revision(browser_name).is_some_and(|min| revision >= min) =>
        {
            return None
        }
        PlatformGroup::LinuxArm64 if is_headless => "linux-arm64/chrome-headless-shell-linux-arm64.zip",
        PlatformGroup::LinuxArm64 => "linux-arm64/chrome-linux-arm64.zip",
    };
    Some(format!("builds/cft/{browser_version}/{suffix}"))
}

#[derive(Debug, Serialize, Clone)]
pub struct BrowserTarget {
    pub http_file_workspace_name: String,
    pub http_file_path: String,
    pub label: String,
    pub output_dir: String,
    pub platform: Platform,
    pub browser: String,
    pub browser_name: String,
}

#[derive(Debug, Serialize)]
pub struct HttpFile {
    pub name: String,
    pub path: String,
}

impl From<BrowserTarget> for HttpFile {
    fn from(value: BrowserTarget) -> Self {
        HttpFile {
            name: value.http_file_workspace_name,
            path: value.http_file_path,
        }
    }
}

pub fn get_browser_rules(
    browsers_workspace_name_prefix: &str,
    browser_json_path: &PathBuf,
) -> std::io::Result<Vec<BrowserTarget>> {
    let browsers_json = std::fs::read_to_string(browser_json_path)?;
    let browsers: Browsers = serde_json::from_str(&browsers_json)?;

    let download_paths_json = include_str!("download_paths.json");
    let download_paths: DownloadPaths = serde_json::from_str(download_paths_json)?;

    let has_headless = browsers
        .browsers
        .iter()
        .any(|b| b.name.ends_with("-headless-shell"));

    let mut browser_rules: Vec<BrowserTarget> = browsers
        .browsers
        .into_iter()
        .flat_map(|browser| {
            if has_headless {
                return vec![browser];
            }
            // Handle headless browser variants
            match browser.name.as_str() {
                "chromium" | "chromium-tip-of-tree" => vec![
                    browser.clone(),
                    BrowserData {
                        name: format!("{}-headless-shell", browser.name),
                        ..browser
                    },
                ],
                _ => vec![browser],
            }
        })
        .flat_map(|browser| {
            let paths = download_paths.paths.get(&browser.name);
            if paths.is_none() {
                return vec![];
            }
            let browser_rules: Vec<BrowserTarget> = paths
                .unwrap()
                .paths
                .iter()
                .filter_map(|(platform, template)| {
                    if *platform == Platform::Unknown {
                        return None;
                    }
                    match (
                        template,
                        serde_json::to_string(platform)
                            .map(|name| name.trim_matches('"').to_string()),
                        serde_json::to_string(&browser.name)
                            .map(|name| name.trim_matches('"').to_string()),
                    ) {
                        (Some(template), Ok(platform_str), Ok(browser_name)) => {
                            let has_revision_override = browser
                                .revision_overrides
                                .as_ref()
                                .and_then(|overrides| overrides.get(platform))
                                .is_some();

                            let revision = browser
                                .revision_overrides
                                .as_ref()
                                .and_then(|overrides| overrides.get(platform))
                                .unwrap_or(&browser.revision);

                            let snake_case_browser_name = browser_name.replace("-", "_");
                            let browser_directory_prefix = if has_revision_override {
                                format!(
                                    "{}_{}_{}",
                                    snake_case_browser_name, platform_str, "special"
                                )
                            } else {
                                snake_case_browser_name
                            };

                            // Chrome for Testing (Playwright >= 1.57) replaces the
                            // legacy revision-based chromium URL with a browserVersion
                            // based one for most platforms. The threshold is keyed off
                            // the base revision so older browsers.json files keep using
                            // the legacy URLs.
                            let base_revision = browser.revision.parse::<u32>().ok();
                            let uses_cft = cft_min_revision(&browser.name)
                                .zip(base_revision)
                                .map(|(min_revision, base_revision)| base_revision >= min_revision)
                                .unwrap_or(false);
                            let http_file_path = if uses_cft {
                                browser
                                    .browser_version
                                    .as_deref()
                                    .zip(base_revision)
                                    .and_then(|(version, base_revision)| {
                                        cft_download_path(
                                            &browser.name,
                                            platform,
                                            version,
                                            base_revision,
                                        )
                                    })
                                    .unwrap_or_else(|| template.replace("%s", revision))
                            } else {
                                template.replace("%s", revision)
                            };

                            Some(BrowserTarget {
                                http_file_workspace_name: format!(
                                    "{browsers_workspace_name_prefix}-{browser_name}-{platform_str}"
                                ),
                                http_file_path,
                                label: format!("{browser_name}-{platform_str}"),
                                output_dir: format!(
                                    "{platform_str}/{}-{}",
                                    browser_directory_prefix, browser.revision
                                ),
                                browser_name,
                                platform: platform.clone(),
                                browser: browser.name.clone(),
                            })
                        }
                        _ => None,
                    }
                })
                .collect();

            browser_rules
        })
        .collect();

    browser_rules.sort_by(|a, b| a.label.cmp(&b.label));

    Ok(browser_rules)
}
