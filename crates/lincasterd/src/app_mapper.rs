use std::cmp::Reverse;

use lincaster_proto::{AppRuleConfig, Config};
use regex::Regex;
use tracing::{debug, info, trace, warn};

use crate::pipewire_registry::PwNode;

/// Compiled version of an app-matching rule.
struct CompiledRule {
    target_bus_id: String,
    priority: i32,
    process_name_re: Option<Regex>,
    app_name_re: Option<Regex>,
    client_name_re: Option<Regex>,
    flatpak_app_id: Option<String>,
}

/// Per-application stream router. Matches new PipeWire streams against
/// configured rules and returns the target bus ID for routing.
#[allow(dead_code)]
pub struct AppMapper {
    rules: Vec<CompiledRule>,
    default_bus: String,
}

#[allow(dead_code)]
impl AppMapper {
    pub fn new(config: &Config) -> Self {
        let mut rules: Vec<CompiledRule> = Vec::new();

        for rule in &config.app_rules {
            if !rule.enabled {
                continue;
            }
            match compile_rule(rule) {
                Ok(compiled) => rules.push(compiled),
                Err(e) => warn!("Skipping invalid app rule: {}", e),
            }
        }

        // Sort by priority descending (highest priority first)
        rules.sort_by_key(|r| Reverse(r.priority));

        info!("AppMapper initialized with {} active rules", rules.len());

        // Default bus for unmatched streams
        let default_bus = config
            .busses
            .first()
            .map(|b| b.bus_id.clone())
            .unwrap_or_else(|| "system".to_string());

        Self { rules, default_bus }
    }

    /// Match a PipeWire stream node against rules and return the target bus ID.
    /// Returns `Some(bus_id)` if a rule matches, `None` if no rule matches
    /// (stream should go to default routing).
    pub fn match_stream(&self, node: &PwNode) -> Option<String> {
        let process_name = node
            .props
            .get("application.process.binary")
            .map(|s| s.as_str())
            .or_else(|| node.props.get("application.name").map(|s| s.as_str()))
            .unwrap_or("");
        let app_name = node
            .props
            .get("application.name")
            .map(|s| s.as_str())
            .unwrap_or("");
        let client_name = node
            .props
            .get("client.name")
            .map(|s| s.as_str())
            .unwrap_or("");
        let flatpak_id = node
            .props
            .get("application.id")
            .map(|s| s.as_str())
            .unwrap_or("");

        trace!(
            "Matching stream: process='{}' app='{}' client='{}' flatpak='{}'",
            process_name,
            app_name,
            client_name,
            flatpak_id
        );

        for rule in &self.rules {
            if rule_matches(rule, process_name, app_name, client_name, flatpak_id) {
                debug!(
                    "Stream matched rule -> bus '{}' (priority {})",
                    rule.target_bus_id, rule.priority
                );
                return Some(rule.target_bus_id.clone());
            }
        }

        None
    }

    /// Get the default bus ID for streams that don't match any rule.
    pub fn default_bus(&self) -> &str {
        &self.default_bus
    }

    /// Reload rules from a new config. Called when config is updated at runtime.
    pub fn reload(&mut self, config: &Config) {
        let mut rules = Vec::new();
        for rule in &config.app_rules {
            if !rule.enabled {
                continue;
            }
            match compile_rule(rule) {
                Ok(compiled) => rules.push(compiled),
                Err(e) => warn!("Skipping invalid app rule on reload: {}", e),
            }
        }
        rules.sort_by_key(|r| Reverse(r.priority));
        info!("AppMapper reloaded with {} active rules", rules.len());
        self.rules = rules;
    }
}

fn compile_rule(rule: &AppRuleConfig) -> Result<CompiledRule, String> {
    let process_name_re = rule
        .match_criteria
        .process_name_regex
        .as_ref()
        .map(|s| Regex::new(s))
        .transpose()
        .map_err(|e| format!("Invalid process_name_regex: {}", e))?;

    let app_name_re = rule
        .match_criteria
        .app_name_regex
        .as_ref()
        .map(|s| Regex::new(s))
        .transpose()
        .map_err(|e| format!("Invalid app_name_regex: {}", e))?;

    let client_name_re = rule
        .match_criteria
        .client_name_regex
        .as_ref()
        .map(|s| Regex::new(s))
        .transpose()
        .map_err(|e| format!("Invalid client_name_regex: {}", e))?;

    Ok(CompiledRule {
        target_bus_id: rule.target_bus_id.clone(),
        priority: rule.priority,
        process_name_re,
        app_name_re,
        client_name_re,
        flatpak_app_id: rule.match_criteria.flatpak_app_id.clone(),
    })
}

fn rule_matches(
    rule: &CompiledRule,
    process_name: &str,
    app_name: &str,
    client_name: &str,
    flatpak_id: &str,
) -> bool {
    // All specified criteria must match (AND logic).
    // If a criterion is None, it's not checked (wildcard).
    if let Some(ref re) = rule.process_name_re {
        if !re.is_match(process_name) {
            return false;
        }
    }
    if let Some(ref re) = rule.app_name_re {
        if !re.is_match(app_name) {
            return false;
        }
    }
    if let Some(ref re) = rule.client_name_re {
        if !re.is_match(client_name) {
            return false;
        }
    }
    if let Some(ref expected_id) = rule.flatpak_app_id {
        if flatpak_id != expected_id {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_node(process_binary: &str, app_name: &str) -> PwNode {
        let mut props = HashMap::new();
        props.insert(
            "application.process.binary".to_string(),
            process_binary.to_string(),
        );
        props.insert("application.name".to_string(), app_name.to_string());
        PwNode {
            id: 1,
            name: "test-stream".to_string(),
            media_class: "Stream/Output/Audio".to_string(),
            description: "Test".to_string(),
            nick: "".to_string(),
            props,
        }
    }

    fn make_config_with_rules(rules: Vec<AppRuleConfig>) -> Config {
        Config {
            version: 1,
            device: lincaster_proto::DeviceConfig {
                usb_vendor_id: lincaster_proto::RODE_VENDOR_ID,
                usb_product_ids: vec![],
                alsa_card_id_hint: "test".to_string(),
                require_multitrack: false,
            },
            busses: lincaster_proto::config::default_busses(),
            routes: vec![],
            app_rules: rules,
            latency_mode: Default::default(),
        }
    }

    #[test]
    fn test_match_by_process_name() {
        let config = make_config_with_rules(vec![AppRuleConfig {
            match_criteria: lincaster_proto::MatchConfig {
                process_name_regex: Some("^discord$".to_string()),
                ..Default::default()
            },
            target_bus_id: "chat".to_string(),
            priority: 100,
            enabled: true,
        }]);

        let mapper = AppMapper::new(&config);
        let node = make_node("discord", "Discord");
        assert_eq!(mapper.match_stream(&node), Some("chat".to_string()));

        let node2 = make_node("firefox", "Firefox");
        assert_eq!(mapper.match_stream(&node2), None);
    }

    #[test]
    fn test_priority_ordering() {
        let config = make_config_with_rules(vec![
            AppRuleConfig {
                match_criteria: lincaster_proto::MatchConfig {
                    process_name_regex: Some("^discord$".to_string()),
                    ..Default::default()
                },
                target_bus_id: "game".to_string(),
                priority: 50,
                enabled: true,
            },
            AppRuleConfig {
                match_criteria: lincaster_proto::MatchConfig {
                    process_name_regex: Some("^discord$".to_string()),
                    ..Default::default()
                },
                target_bus_id: "chat".to_string(),
                priority: 100,
                enabled: true,
            },
        ]);

        let mapper = AppMapper::new(&config);
        let node = make_node("discord", "Discord");
        // Higher priority rule (chat, 100) should win
        assert_eq!(mapper.match_stream(&node), Some("chat".to_string()));
    }

    #[test]
    fn test_disabled_rules_skipped() {
        let config = make_config_with_rules(vec![AppRuleConfig {
            match_criteria: lincaster_proto::MatchConfig {
                process_name_regex: Some("^discord$".to_string()),
                ..Default::default()
            },
            target_bus_id: "chat".to_string(),
            priority: 100,
            enabled: false,
        }]);

        let mapper = AppMapper::new(&config);
        let node = make_node("discord", "Discord");
        assert_eq!(mapper.match_stream(&node), None);
    }

    #[test]
    fn test_regex_pattern_match() {
        let config = make_config_with_rules(vec![AppRuleConfig {
            match_criteria: lincaster_proto::MatchConfig {
                process_name_regex: Some("^(discord|zoom|teams)$".to_string()),
                ..Default::default()
            },
            target_bus_id: "chat".to_string(),
            priority: 100,
            enabled: true,
        }]);

        let mapper = AppMapper::new(&config);
        assert_eq!(
            mapper.match_stream(&make_node("discord", "")),
            Some("chat".to_string())
        );
        assert_eq!(
            mapper.match_stream(&make_node("zoom", "")),
            Some("chat".to_string())
        );
        assert_eq!(
            mapper.match_stream(&make_node("teams", "")),
            Some("chat".to_string())
        );
        assert_eq!(mapper.match_stream(&make_node("firefox", "")), None);
    }
}
