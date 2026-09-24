use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SETTINGS_SCHEMA: &str = "urn:ikenga:settings:v1";
pub const SETTINGS_VERSION: u32 = 1;
pub const RESOLVED_DEFAULT_FIELDS: &[&str] = &[
    "appearance.theme",
    "appearance.mode",
    "appearance.density",
    "appearance.tintStrength",
    "projects.extraRoots",
    "engines.defaultEngineId",
    "engines.defaultShellId",
    "engines.customShellProfiles",
    "engines.agentEnvironment",
    "engines.agentWslDistro",
    "engines.resumeTerminals",
    "workspace.userName",
    "workspace.claudeBrowserMode",
    "workspace.claudeWatchEnabled",
    "workspace.sidebarCollapsed",
    "workspace.artifact.defaultSink",
    "workspace.artifact.stackMode",
    "workspace.artifact.terminalHandoff",
    "workspace.artifact.folderOverrides",
    "workspace.artifact.artifactSinkOverrides",
    "workspace.artifact.showResolved",
    "storage.screenshotDirectory",
    "about.updates.autoCheck",
    "about.updates.autoInstallApp",
    "about.updates.autoInstallPkgs",
];

pub const SECTION_NAMES: [&str; 9] = [
    "appearance",
    "projects",
    "engines",
    "workspace",
    "secrets",
    "integrations",
    "people",
    "storage",
    "about",
];

pub const PERSONAL_ONLY_FIELDS: &[&str] = &[
    "workspace.userName",
    "workspace.claudeBrowserMode",
    "workspace.claudeWatchEnabled",
    "workspace.sidebarCollapsed",
    "workspace.explorerSections",
    "workspace.onboarding",
    "storage.screenshotDirectory",
    "about.updates",
    "about.updates.autoCheck",
    "about.updates.autoInstallApp",
    "about.updates.autoInstallPkgs",
];

pub const PROJECT_ONLY_FIELDS: &[&str] = &[
    "projects.extraRoots",
    "workspace.lastAgent",
    "workspace.lastAgent.kind",
    "workspace.lastAgent.customCommand",
];

pub fn is_project_only_field(path: &str) -> bool {
    PROJECT_ONLY_FIELDS.contains(&path)
}

pub fn is_personal_only_field(path: &str) -> bool {
    PERSONAL_ONLY_FIELDS.contains(&path)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingsDocument {
    #[serde(rename = "$schema", default = "default_schema")]
    pub schema: String,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub appearance: Map<String, Value>,
    #[serde(default)]
    pub projects: Map<String, Value>,
    #[serde(default)]
    pub engines: Map<String, Value>,
    #[serde(default)]
    pub workspace: Map<String, Value>,
    #[serde(default)]
    pub secrets: Map<String, Value>,
    #[serde(default)]
    pub integrations: Map<String, Value>,
    #[serde(default)]
    pub people: Map<String, Value>,
    #[serde(default)]
    pub storage: Map<String, Value>,
    #[serde(default)]
    pub about: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_schema() -> String {
    SETTINGS_SCHEMA.to_string()
}

fn default_version() -> u32 {
    SETTINGS_VERSION
}

impl Default for SettingsDocument {
    fn default() -> Self {
        Self {
            schema: default_schema(),
            version: SETTINGS_VERSION,
            appearance: Map::new(),
            projects: Map::new(),
            engines: Map::new(),
            workspace: Map::new(),
            secrets: Map::new(),
            integrations: Map::new(),
            people: Map::new(),
            storage: Map::new(),
            about: Map::new(),
            extra: Map::new(),
        }
    }
}

impl SettingsDocument {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut root: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("invalid settings JSON: {e}"))?;
        let object = root
            .as_object_mut()
            .ok_or_else(|| "settings root must be an object".to_string())?;
        let version = match object.get("version") {
            None => 0u64,
            Some(value) => value
                .as_u64()
                .ok_or_else(|| "settings version must be an integer".to_string())?,
        };
        if version > SETTINGS_VERSION as u64 {
            return Err(format!(
                "settings version {version} is newer than supported version {SETTINGS_VERSION}"
            ));
        }
        if let Some(schema) = object.get("$schema") {
            let schema = schema
                .as_str()
                .ok_or_else(|| "settings $schema must be a string".to_string())?;
            if schema != SETTINGS_SCHEMA {
                return Err(format!("unsupported settings schema: {schema}"));
            }
        }
        if !object.contains_key("$schema") {
            object.insert("$schema".to_string(), Value::String(default_schema()));
        }
        object.insert("version".to_string(), Value::from(SETTINGS_VERSION as u64));
        let mut document: Self =
            serde_json::from_value(root).map_err(|e| format!("settings shape is invalid: {e}"))?;
        document.schema = default_schema();
        document.version = SETTINGS_VERSION;
        document.validate()?;
        Ok(document)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut value =
            serde_json::to_value(self).map_err(|e| format!("serialize settings: {e}"))?;
        if let Some(object) = value.as_object_mut() {
            object.insert("$schema".to_string(), Value::String(self.schema.clone()));
            object.insert("version".to_string(), Value::from(self.version as u64));
        }
        let mut bytes =
            serde_json::to_vec_pretty(&value).map_err(|e| format!("serialize settings: {e}"))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn section(&self, name: &str) -> Option<&Map<String, Value>> {
        match name {
            "appearance" => Some(&self.appearance),
            "projects" => Some(&self.projects),
            "engines" => Some(&self.engines),
            "workspace" => Some(&self.workspace),
            "secrets" => Some(&self.secrets),
            "integrations" => Some(&self.integrations),
            "people" => Some(&self.people),
            "storage" => Some(&self.storage),
            "about" => Some(&self.about),
            _ => None,
        }
    }

    fn section_mut(&mut self, name: &str) -> Option<&mut Map<String, Value>> {
        match name {
            "appearance" => Some(&mut self.appearance),
            "projects" => Some(&mut self.projects),
            "engines" => Some(&mut self.engines),
            "workspace" => Some(&mut self.workspace),
            "secrets" => Some(&mut self.secrets),
            "integrations" => Some(&mut self.integrations),
            "people" => Some(&mut self.people),
            "storage" => Some(&mut self.storage),
            "about" => Some(&mut self.about),
            _ => None,
        }
    }

    pub fn get_field(&self, path: &str) -> Option<&Value> {
        let parts: Vec<&str> = path.split('.').collect();
        let section = *parts.first()?;
        let mut current = self.section(section)?;
        for (index, part) in parts[1..].iter().enumerate() {
            let value = current.get(*part)?;
            if index + 1 == parts.len() - 1 {
                return Some(value);
            }
            current = value.as_object()?;
        }
        None
    }

    pub fn set_field(&mut self, path: &str, value: Value) -> Result<(), String> {
        if !is_known_field(path) {
            return Err(format!("unknown settings field: {path}"));
        }
        validate_field(path, &value)?;
        let parts: Vec<&str> = path.split('.').collect();
        let section_name = parts[0];
        let section = self
            .section_mut(section_name)
            .ok_or_else(|| format!("unknown settings section: {section_name}"))?;
        set_map_path(section, &parts[1..], value)?;
        self.validate()
    }

    pub fn remove_field(&mut self, path: &str) -> Result<(), String> {
        if !is_known_field(path) {
            return Err(format!("unknown settings field: {path}"));
        }
        let parts: Vec<&str> = path.split('.').collect();
        let section_name = parts[0];
        let section = self
            .section_mut(section_name)
            .ok_or_else(|| format!("unknown settings section: {section_name}"))?;
        remove_map_path(section, &parts[1..]);
        Ok(())
    }

    pub fn merge(&self, overlay: &Self) -> Self {
        let mut result = self.clone();
        merge_map(&mut result.appearance, &overlay.appearance);
        merge_map(&mut result.projects, &overlay.projects);
        merge_map(&mut result.engines, &overlay.engines);
        merge_map(&mut result.workspace, &overlay.workspace);
        merge_map(&mut result.secrets, &overlay.secrets);
        merge_map(&mut result.integrations, &overlay.integrations);
        merge_map(&mut result.people, &overlay.people);
        merge_map(&mut result.storage, &overlay.storage);
        merge_map(&mut result.about, &overlay.about);
        for (key, value) in &overlay.extra {
            result.extra.insert(key.clone(), value.clone());
        }
        result.schema = SETTINGS_SCHEMA.to_string();
        result.version = SETTINGS_VERSION;
        result
    }

    pub fn resolved(&self) -> Self {
        let mut result = self.clone();
        for field in RESOLVED_DEFAULT_FIELDS {
            if result.get_field(field).is_none() {
                if let Some(value) = default_value(field) {
                    let _ = result.set_field(field, value);
                }
            }
        }
        if result.get_field("workspace.explorerSections").is_none() {
            let _ = result.set_field("workspace.explorerSections", default_explorer_sections());
        }
        if result.get_field("workspace.onboarding").is_none() {
            let _ = result.set_field("workspace.onboarding", default_onboarding());
        }
        if result.get_field("workspace.lastAgent").is_none() {
            let _ = result.set_field(
                "workspace.lastAgent",
                serde_json::json!({ "kind": null, "customCommand": null }),
            );
        }
        result
    }

    pub fn project_overlay(&self) -> Self {
        let mut overlay = self.clone();
        for field in PERSONAL_ONLY_FIELDS {
            let _ = overlay.remove_field(field);
        }
        overlay
    }

    pub fn has_content(&self) -> bool {
        !self.leaf_paths().is_empty() || !self.extra.is_empty()
    }

    pub fn leaf_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        for section in SECTION_NAMES {
            if let Some(map) = self.section(section) {
                collect_leaf_paths(section, map, &mut paths);
            }
        }
        paths.sort();
        paths
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.secrets.is_empty() {
            return Err("settings.secrets is reserved".to_string());
        }
        for section in SECTION_NAMES {
            let map = self
                .section(section)
                .ok_or_else(|| format!("missing settings section: {section}"))?;
            for (key, value) in map {
                validate_field(&format!("{section}.{key}"), value)?;
            }
        }
        if let Some(artifact) = self.workspace.get("artifact") {
            let map = artifact
                .as_object()
                .ok_or_else(|| "workspace.artifact must be an object".to_string())?;
            for (key, value) in map {
                validate_field(&format!("workspace.artifact.{key}"), value)?;
                if key == "folderOverrides" {
                    let folders = value.as_object().ok_or_else(|| {
                        "workspace.artifact.folderOverrides must be an object".to_string()
                    })?;
                    for (path, entry) in folders {
                        let entry = entry.as_object().ok_or_else(|| {
                            format!("workspace.artifact.folderOverrides.{path} must be an object")
                        })?;
                        for (field, field_value) in entry {
                            validate_field(
                                &format!("workspace.artifact.folderOverrides.{path}.{field}"),
                                field_value,
                            )?;
                        }
                    }
                } else if key == "artifactSinkOverrides" {
                    let sinks = value.as_object().ok_or_else(|| {
                        "workspace.artifact.artifactSinkOverrides must be an object".to_string()
                    })?;
                    for (path, sink) in sinks {
                        let sink = sink.as_str().ok_or_else(|| {
                            format!(
                                "workspace.artifact.artifactSinkOverrides.{path} must be a string"
                            )
                        })?;
                        if !matches!(sink, "inherit" | "auto" | "terminal" | "chi" | "clipboard")
                            && !sink.starts_with("terminal:")
                        {
                            return Err(format!(
                                "invalid workspace.artifact.artifactSinkOverrides.{path}"
                            ));
                        }
                    }
                } else if key == "showResolved" {
                    let values = value.as_object().ok_or_else(|| {
                        "workspace.artifact.showResolved must be an object".to_string()
                    })?;
                    for (path, resolved) in values {
                        if !resolved.is_boolean() {
                            return Err(format!(
                                "workspace.artifact.showResolved.{path} must be boolean"
                            ));
                        }
                    }
                }
            }
        }
        if let Some(updates) = self.about.get("updates") {
            let map = updates
                .as_object()
                .ok_or_else(|| "about.updates must be an object".to_string())?;
            for (key, value) in map {
                validate_field(&format!("about.updates.{key}"), value)?;
            }
        }
        if let Some(last_agent) = self.workspace.get("lastAgent") {
            let map = last_agent
                .as_object()
                .ok_or_else(|| "workspace.lastAgent must be an object".to_string())?;
            for (key, value) in map {
                validate_field(&format!("workspace.lastAgent.{key}"), value)?;
            }
        }
        Ok(())
    }
}

fn set_map_path(map: &mut Map<String, Value>, parts: &[&str], value: Value) -> Result<(), String> {
    if parts.is_empty() {
        return Err("empty settings field path".to_string());
    }
    if parts.len() == 1 {
        map.insert(parts[0].to_string(), value);
        return Ok(());
    }
    let entry = map
        .entry(parts[0].to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let child = entry
        .as_object_mut()
        .ok_or_else(|| format!("settings field {} must be an object", parts[0]))?;
    set_map_path(child, &parts[1..], value)
}

fn remove_map_path(map: &mut Map<String, Value>, parts: &[&str]) {
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        map.remove(parts[0]);
        return;
    }
    let remove_parent = match map.get_mut(parts[0]).and_then(Value::as_object_mut) {
        Some(child) => {
            remove_map_path(child, &parts[1..]);
            child.is_empty()
        }
        None => false,
    };
    if remove_parent {
        map.remove(parts[0]);
    }
}

fn merge_map(base: &mut Map<String, Value>, overlay: &Map<String, Value>) {
    for (key, value) in overlay {
        if let (Some(base_value), Some(overlay_value)) = (base.get_mut(key), value.as_object()) {
            if base_value.is_object() {
                if let Some(base_object) = base_value.as_object_mut() {
                    merge_map(base_object, overlay_value);
                    continue;
                }
            }
        }
        base.insert(key.clone(), value.clone());
    }
}

fn collect_leaf_paths(prefix: &str, map: &Map<String, Value>, out: &mut Vec<String>) {
    for (key, value) in map {
        let path = format!("{prefix}.{key}");
        if value.as_array().is_some_and(|items| items.is_empty()) {
            continue;
        }
        if let Some(child) = value.as_object() {
            if child.is_empty() {
                continue;
            }
            collect_leaf_paths(&path, child, out);
            continue;
        }
        out.push(path);
    }
}

pub fn is_known_field(path: &str) -> bool {
    matches!(
        path,
        "appearance.theme"
            | "appearance.mode"
            | "appearance.density"
            | "appearance.tintStrength"
            | "projects.extraRoots"
            | "engines.defaultEngineId"
            | "engines.defaultShellId"
            | "engines.customShellProfiles"
            | "engines.agentEnvironment"
            | "engines.agentWslDistro"
            | "engines.resumeTerminals"
            | "workspace.userName"
            | "workspace.claudeBrowserMode"
            | "workspace.claudeWatchEnabled"
            | "workspace.sidebarCollapsed"
            | "workspace.explorerSections"
            | "workspace.onboarding"
            | "workspace.artifact"
            | "workspace.artifact.defaultSink"
            | "workspace.artifact.stackMode"
            | "workspace.artifact.terminalHandoff"
            | "workspace.artifact.folderOverrides"
            | "workspace.artifact.artifactSinkOverrides"
            | "workspace.artifact.showResolved"
            | "workspace.lastAgent"
            | "workspace.lastAgent.kind"
            | "workspace.lastAgent.customCommand"
            | "storage.screenshotDirectory"
            | "about.updates"
            | "about.updates.autoCheck"
            | "about.updates.autoInstallApp"
            | "about.updates.autoInstallPkgs"
    )
}

fn validate_field(path: &str, value: &Value) -> Result<(), String> {
    if path.starts_with("secrets.")
        || path.starts_with("integrations.")
        || path.starts_with("people.")
    {
        return Ok(());
    }
    let invalid = format!("invalid settings value at {path}");
    if value.is_null() {
        return if matches!(
            path,
            "engines.defaultEngineId"
                | "engines.defaultShellId"
                | "engines.agentWslDistro"
                | "workspace.lastAgent.kind"
                | "workspace.lastAgent.customCommand"
                | "storage.screenshotDirectory"
        ) {
            Ok(())
        } else {
            Err(invalid.clone())
        };
    }
    match path {
        "appearance.theme" => ensure_enum(value, &["A", "B", "C"], &invalid),
        "appearance.mode" => ensure_enum(value, &["light", "dark", "system"], &invalid),
        "appearance.density" => {
            ensure_enum(value, &["compact", "comfortable", "spacious"], &invalid)
        }
        "appearance.tintStrength" => ensure_enum(value, &["off", "subtle", "strong"], &invalid),
        "projects.extraRoots" => ensure_string_array(value, &invalid),
        "engines.defaultEngineId" | "engines.defaultShellId" | "engines.agentWslDistro" => {
            ensure_string(value, &invalid)
        }
        "engines.customShellProfiles" => ensure_array(value, &invalid),
        "engines.agentEnvironment" => ensure_enum(value, &["native", "wsl"], &invalid),
        "engines.resumeTerminals" => ensure_bool(value, &invalid),
        "workspace.userName" | "storage.screenshotDirectory" => ensure_string(value, &invalid),
        "workspace.claudeBrowserMode" => ensure_enum(value, &["layered", "roots"], &invalid),
        "workspace.claudeWatchEnabled" | "workspace.sidebarCollapsed" => {
            ensure_bool(value, &invalid)
        }
        "workspace.explorerSections" => ensure_array(value, &invalid),
        "workspace.onboarding" | "workspace.artifact" | "workspace.lastAgent" => {
            ensure_object(value, &invalid)
        }
        "workspace.artifact.defaultSink" => {
            ensure_enum(value, &["auto", "terminal", "chi", "clipboard"], &invalid)
        }
        "workspace.artifact.stackMode" => ensure_enum(value, &["collapsed", "expanded"], &invalid),
        "workspace.artifact.terminalHandoff" => {
            ensure_enum(value, &["attach", "keep", "ask"], &invalid)
        }
        "workspace.artifact.folderOverrides"
        | "workspace.artifact.artifactSinkOverrides"
        | "workspace.artifact.showResolved" => ensure_object(value, &invalid),
        "workspace.lastAgent.kind" => {
            ensure_enum(value, &["claude", "codex", "gemini", "custom"], &invalid)
        }
        "workspace.lastAgent.customCommand" => ensure_string(value, &invalid),
        "about.updates" => ensure_object(value, &invalid),
        "about.updates.autoCheck"
        | "about.updates.autoInstallApp"
        | "about.updates.autoInstallPkgs" => ensure_bool(value, &invalid),
        path if path.starts_with("secrets.")
            || path.starts_with("integrations.")
            || path.starts_with("people.") =>
        {
            Ok(())
        }
        path if path.starts_with("workspace.artifact.folderOverrides.") => {
            let field = path.rsplit('.').next().unwrap_or_default();
            match field {
                "defaultSink" => {
                    ensure_enum(value, &["auto", "terminal", "chi", "clipboard"], &invalid)
                }
                "stackMode" => ensure_enum(value, &["collapsed", "expanded"], &invalid),
                _ => Err(invalid.clone()),
            }
        }
        _ => Ok(()),
    }
}

fn ensure_string(value: &Value, invalid: &str) -> Result<(), String> {
    if value.is_string() {
        Ok(())
    } else {
        Err(invalid.to_string())
    }
}

fn ensure_enum(value: &Value, values: &[&str], invalid: &str) -> Result<(), String> {
    let Some(value) = value.as_str() else {
        return Err(invalid.to_string());
    };
    if values.contains(&value) {
        Ok(())
    } else {
        Err(invalid.to_string())
    }
}

fn ensure_bool(value: &Value, invalid: &str) -> Result<(), String> {
    if value.is_boolean() {
        Ok(())
    } else {
        Err(invalid.to_string())
    }
}

fn ensure_array(value: &Value, invalid: &str) -> Result<(), String> {
    if value.is_array() {
        Ok(())
    } else {
        Err(invalid.to_string())
    }
}

fn ensure_object(value: &Value, invalid: &str) -> Result<(), String> {
    if value.is_object() {
        Ok(())
    } else {
        Err(invalid.to_string())
    }
}

fn ensure_string_array(value: &Value, invalid: &str) -> Result<(), String> {
    let Some(values) = value.as_array() else {
        return Err(invalid.to_string());
    };
    if values.iter().all(Value::is_string) {
        Ok(())
    } else {
        Err(invalid.to_string())
    }
}

pub fn default_value(path: &str) -> Option<Value> {
    match path {
        "appearance.theme" => Some(Value::String("A".to_string())),
        "appearance.mode" => Some(Value::String("dark".to_string())),
        "appearance.density" => Some(Value::String("comfortable".to_string())),
        "appearance.tintStrength" => Some(Value::String("subtle".to_string())),
        "projects.extraRoots" => Some(Value::Array(Vec::new())),
        "engines.defaultEngineId" | "engines.defaultShellId" | "engines.agentWslDistro" => {
            Some(Value::Null)
        }
        "engines.customShellProfiles" => Some(Value::Array(Vec::new())),
        "engines.agentEnvironment" => Some(Value::String("native".to_string())),
        "engines.resumeTerminals" => Some(Value::Bool(true)),
        "workspace.userName" => Some(Value::String(String::new())),
        "workspace.claudeBrowserMode" => Some(Value::String("layered".to_string())),
        "workspace.claudeWatchEnabled" => Some(Value::Bool(true)),
        "workspace.sidebarCollapsed" => Some(Value::Bool(false)),
        "workspace.artifact.defaultSink" => Some(Value::String("auto".to_string())),
        "workspace.artifact.stackMode" => Some(Value::String("collapsed".to_string())),
        "workspace.artifact.terminalHandoff" => Some(Value::String("ask".to_string())),
        "workspace.artifact.folderOverrides"
        | "workspace.artifact.artifactSinkOverrides"
        | "workspace.artifact.showResolved" => Some(Value::Object(Map::new())),
        "workspace.lastAgent.kind" | "workspace.lastAgent.customCommand" => Some(Value::Null),
        "storage.screenshotDirectory" => Some(Value::Null),
        "about.updates.autoCheck" => Some(Value::Bool(true)),
        "about.updates.autoInstallApp" => Some(Value::Bool(false)),
        "about.updates.autoInstallPkgs" => Some(Value::Bool(true)),
        _ => None,
    }
}

fn default_explorer_sections() -> Value {
    serde_json::json!([
        { "id": "files", "source": "shell", "order": 0, "collapsed": false },
        { "id": "artifacts", "source": "shell", "order": 1, "collapsed": false },
        { "id": "sessions", "source": "shell", "order": 2, "collapsed": false },
        { "id": "ngwa-project", "source": "shell", "order": 3, "collapsed": true },
        { "id": "automations", "source": "shell", "order": 4, "collapsed": false },
        { "id": "todos", "source": "shell", "order": 5, "collapsed": true },
        { "id": "scratchpads", "source": "shell", "order": 6, "collapsed": true },
        { "id": "views", "source": "shell", "order": 7, "collapsed": true }
    ])
}

fn default_onboarding() -> Value {
    serde_json::json!({
        "version": 2,
        "startedAt": null,
        "completedAt": null,
        "mode": "first_run",
        "activeIndex": 0,
        "steps": {
            "welcome": { "status": "pending" },
            "agent": { "status": "pending" },
            "roots": { "status": "pending" },
            "packages": { "status": "pending" },
            "connectors": { "status": "pending" },
            "scaffolding": { "status": "pending" },
            "appearance": { "status": "pending" },
            "summary": { "status": "pending" }
        },
        "selectedAgentId": null,
        "loreGlossSeen": []
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyEncoding {
    Json,
    Raw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyTarget {
    Personal,
    Project,
}

#[derive(Clone, Copy, Debug)]
pub struct LegacyBinding {
    pub field: &'static str,
    pub encoding: LegacyEncoding,
    pub target: LegacyTarget,
}

pub fn legacy_binding(key: &str) -> Option<LegacyBinding> {
    let (field, encoding) = match key {
        "agent.defaultEngineId" | "agent.chatAdapterId" => {
            ("engines.defaultEngineId", LegacyEncoding::Json)
        }
        "claude.watchEnabled" => ("workspace.claudeWatchEnabled", LegacyEncoding::Json),
        "workspace.claudeBrowserMode" => ("workspace.claudeBrowserMode", LegacyEncoding::Json),
        "workspace.sidebarCollapsed" => ("workspace.sidebarCollapsed", LegacyEncoding::Json),
        "workspace.explorerSections" => ("workspace.explorerSections", LegacyEncoding::Json),
        "projects.extraRoots" => ("projects.extraRoots", LegacyEncoding::Json),
        "onboarding.state" => ("workspace.onboarding", LegacyEncoding::Json),
        "user.name" => ("workspace.userName", LegacyEncoding::Json),
        "updates.autoCheck" => ("about.updates.autoCheck", LegacyEncoding::Json),
        "updates.autoInstallApp" => ("about.updates.autoInstallApp", LegacyEncoding::Json),
        "updates.autoInstallPkgs" => ("about.updates.autoInstallPkgs", LegacyEncoding::Json),
        "appearance.theme" => ("appearance.theme", LegacyEncoding::Json),
        "appearance.mode" => ("appearance.mode", LegacyEncoding::Json),
        "appearance.density" => ("appearance.density", LegacyEncoding::Json),
        "appearance.tintStrength" => ("appearance.tintStrength", LegacyEncoding::Json),
        "terminal.default_shell_id" => ("engines.defaultShellId", LegacyEncoding::Raw),
        "terminal.custom_shell_profiles" => ("engines.customShellProfiles", LegacyEncoding::Json),
        "terminal.agent_env_kind" => ("engines.agentEnvironment", LegacyEncoding::Raw),
        "terminal.agent_wsl_distro" => ("engines.agentWslDistro", LegacyEncoding::Raw),
        "terminal.resume_on_start" => ("engines.resumeTerminals", LegacyEncoding::Raw),
        "artifact-grid.default-sink" => ("workspace.artifact.defaultSink", LegacyEncoding::Raw),
        "artifact-grid.stack-mode" => ("workspace.artifact.stackMode", LegacyEncoding::Raw),
        "artifact-wizard.terminalHandoff" => {
            ("workspace.artifact.terminalHandoff", LegacyEncoding::Raw)
        }
        _ if key.starts_with("artifact-grid.folder.") => {
            ("workspace.artifact.folderOverrides", LegacyEncoding::Raw)
        }
        _ if key.starts_with("artifact-studio.sink.") => (
            "workspace.artifact.artifactSinkOverrides",
            LegacyEncoding::Raw,
        ),
        _ if key.starts_with("artifact-grid:show-resolved:") => {
            ("workspace.artifact.showResolved", LegacyEncoding::Raw)
        }
        _ if key.starts_with("artifact-wizard.lastAgent.") => {
            ("workspace.lastAgent.kind", LegacyEncoding::Raw)
        }
        _ if key.starts_with("artifact-wizard.lastAgentCustom.") => {
            ("workspace.lastAgent.customCommand", LegacyEncoding::Raw)
        }
        _ => return None,
    };
    let target = if key.starts_with("artifact-wizard.lastAgent")
        || key.starts_with("artifact-wizard.lastAgentCustom")
    {
        LegacyTarget::Project
    } else {
        LegacyTarget::Personal
    };
    Some(LegacyBinding {
        field,
        encoding,
        target,
    })
}

pub fn is_known_legacy_key(key: &str) -> bool {
    legacy_binding(key).is_some()
}

pub fn legacy_project_id(key: &str) -> Option<&str> {
    if let Some(id) = key.strip_prefix("artifact-wizard.lastAgentCustom.") {
        return (!id.is_empty()).then_some(id);
    }
    if let Some(id) = key.strip_prefix("artifact-wizard.lastAgent.") {
        return (!id.is_empty()).then_some(id);
    }
    None
}

fn dynamic_path(key: &str) -> Option<(&'static str, String)> {
    if let Some(rest) = key.strip_prefix("artifact-grid.folder.") {
        if let Some(value) = rest.strip_suffix(".default-sink") {
            return Some((
                "workspace.artifact.folderOverrides.defaultSink",
                value.to_string(),
            ));
        }
        if let Some(value) = rest.strip_suffix(".stack-mode") {
            return Some((
                "workspace.artifact.folderOverrides.stackMode",
                value.to_string(),
            ));
        }
    }
    if let Some(path) = key.strip_prefix("artifact-studio.sink.") {
        return Some(("workspace.artifact.artifactSinkOverrides", path.to_string()));
    }
    if let Some(path) = key.strip_prefix("artifact-grid:show-resolved:") {
        return Some(("workspace.artifact.showResolved", path.to_string()));
    }
    if let Some(id) = key.strip_prefix("artifact-wizard.lastAgentCustom.") {
        return Some(("workspace.lastAgent.customCommand", id.to_string()));
    }
    if let Some(id) = key.strip_prefix("artifact-wizard.lastAgent.") {
        return Some(("workspace.lastAgent.kind", id.to_string()));
    }
    None
}

fn normalize_legacy_raw(key: &str, raw: &str) -> Option<String> {
    if matches!(raw, "sidepane" | "both")
        && (key == "artifact-grid.default-sink"
            || key.starts_with("artifact-grid.folder.")
            || key.starts_with("artifact-studio.sink."))
    {
        return Some("clipboard".to_string());
    }
    if key == "terminal.agent_env_kind" && raw == "posix" {
        return Some("native".to_string());
    }
    None
}

fn normalize_legacy_value(key: &str, value: Value) -> Value {
    let Some(raw) = value.as_str() else {
        return value;
    };
    if let Some(normalized) = normalize_legacy_raw(key, raw) {
        return Value::String(normalized);
    }
    value
}

pub fn decode_legacy_value(binding: &LegacyBinding, key: &str, raw: &str) -> Result<Value, String> {
    let normalized = normalize_legacy_raw(key, raw);
    let raw = normalized.as_deref().unwrap_or(raw);
    if raw.is_empty() {
        return Ok(Value::Null);
    }
    if binding.encoding == LegacyEncoding::Json {
        return serde_json::from_str(raw)
            .map(|value| normalize_legacy_value(key, value))
            .map_err(|e| format!("invalid JSON value for {key}: {e}"));
    }
    if raw == "null"
        && matches!(
            binding.field,
            "engines.defaultShellId" | "engines.agentWslDistro" | "workspace.lastAgent.kind"
        )
    {
        return Ok(Value::Null);
    }
    match binding.field {
        "engines.resumeTerminals" => match raw {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("invalid boolean value for {key}")),
        },
        "workspace.artifact.showResolved" => match raw {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("invalid boolean value for {key}")),
        },
        "engines.customShellProfiles" | "workspace.onboarding" => {
            serde_json::from_str(raw).map_err(|e| format!("invalid JSON value for {key}: {e}"))
        }
        _ => Ok(Value::String(raw.to_string())),
    }
}

pub fn encode_legacy_value(binding: &LegacyBinding, value: &Value) -> Option<String> {
    if binding.encoding == LegacyEncoding::Json {
        return serde_json::to_string(value).ok();
    }
    encode_raw_value(binding.field, value)
}

fn encode_raw_value(field: &str, value: &Value) -> Option<String> {
    if field == "workspace.artifact.showResolved" {
        return match value {
            Value::Null => Some(String::new()),
            Value::Bool(true) => Some("1".to_string()),
            Value::Bool(false) => Some("0".to_string()),
            _ => None,
        };
    }
    match value {
        Value::Null => Some(String::new()),
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
        _ => None,
    }
}

fn map_field(document: &mut SettingsDocument, field: &str) -> Map<String, Value> {
    document
        .get_field(field)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub fn apply_legacy_value(
    document: &mut SettingsDocument,
    key: &str,
    raw: &str,
) -> Result<bool, String> {
    let Some(binding) = legacy_binding(key) else {
        return Ok(false);
    };
    if key == "agent.chatAdapterId" && document.get_field(binding.field).is_some() {
        return Ok(false);
    }
    let value = decode_legacy_value(&binding, key, raw)?;
    if key.starts_with("artifact-grid.folder.") {
        let rest = key
            .strip_prefix("artifact-grid.folder.")
            .ok_or_else(|| format!("invalid folder setting: {key}"))?;
        let (path, child) = if let Some(path) = rest.strip_suffix(".default-sink") {
            (path, "defaultSink")
        } else if let Some(path) = rest.strip_suffix(".stack-mode") {
            (path, "stackMode")
        } else {
            return Err(format!("invalid folder setting: {key}"));
        };
        let mut overrides = map_field(document, "workspace.artifact.folderOverrides");
        if raw.is_empty() {
            if let Some(entry) = overrides.get_mut(path).and_then(Value::as_object_mut) {
                entry.remove(child);
                if entry.is_empty() {
                    overrides.remove(path);
                }
            }
        } else {
            let entry = overrides
                .entry(path.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            let object = entry
                .as_object_mut()
                .ok_or_else(|| format!("folder override is not an object: {key}"))?;
            object.insert(child.to_string(), value);
        }
        document.set_field(
            "workspace.artifact.folderOverrides",
            Value::Object(overrides),
        )?;
        return Ok(true);
    }
    if key.starts_with("artifact-studio.sink.") || key.starts_with("artifact-grid:show-resolved:") {
        let (field, entry) =
            dynamic_path(key).ok_or_else(|| format!("invalid dynamic setting: {key}"))?;
        let map_field_name = field
            .rsplit_once('.')
            .map(|(base, _)| base)
            .unwrap_or(field);
        let mut map = map_field(document, map_field_name);
        if raw.is_empty() {
            map.remove(&entry);
        } else {
            map.insert(entry, value);
        }
        document.set_field(map_field_name, Value::Object(map))?;
        return Ok(true);
    }
    if binding.target == LegacyTarget::Project {
        let child = field_child(binding.field);
        let mut last_agent = map_field(document, "workspace.lastAgent");
        if raw.is_empty() {
            last_agent.remove(child);
        } else {
            last_agent.insert(child.to_string(), value);
        }
        document.set_field("workspace.lastAgent", Value::Object(last_agent))?;
        return Ok(true);
    }
    document.set_field(binding.field, value)?;
    Ok(true)
}

fn field_child(field: &str) -> &str {
    field.rsplit('.').next().unwrap_or(field)
}

pub fn legacy_value_present(document: &SettingsDocument, key: &str) -> bool {
    let Some(binding) = legacy_binding(key) else {
        return false;
    };
    if key.starts_with("artifact-grid.folder.") {
        let Some(rest) = key.strip_prefix("artifact-grid.folder.") else {
            return false;
        };
        let (path, child) = if let Some(path) = rest.strip_suffix(".default-sink") {
            (path, "defaultSink")
        } else if let Some(path) = rest.strip_suffix(".stack-mode") {
            (path, "stackMode")
        } else {
            return false;
        };
        return document
            .get_field("workspace.artifact.folderOverrides")
            .and_then(Value::as_object)
            .and_then(|map| map.get(path))
            .and_then(Value::as_object)
            .is_some_and(|entry| entry.contains_key(child));
    }
    if key.starts_with("artifact-studio.sink.") || key.starts_with("artifact-grid:show-resolved:") {
        let Some((field, entry)) = dynamic_path(key) else {
            return false;
        };
        let map_field_name = field
            .rsplit_once('.')
            .map(|(base, _)| base)
            .unwrap_or(field);
        return document
            .get_field(map_field_name)
            .and_then(Value::as_object)
            .is_some_and(|map| map.contains_key(&entry));
    }
    document.get_field(binding.field).is_some()
}

pub fn read_legacy_value(document: &SettingsDocument, key: &str) -> Option<String> {
    let binding = legacy_binding(key)?;
    if key.starts_with("artifact-grid.folder.") {
        let rest = key.strip_prefix("artifact-grid.folder.")?;
        let (path, child) = if let Some(path) = rest.strip_suffix(".default-sink") {
            (path, "defaultSink")
        } else if let Some(path) = rest.strip_suffix(".stack-mode") {
            (path, "stackMode")
        } else {
            return None;
        };
        let value = document
            .get_field("workspace.artifact.folderOverrides")?
            .as_object()?
            .get(path)?
            .as_object()?
            .get(child)?;
        return encode_raw_value(child, value);
    }
    if key.starts_with("artifact-studio.sink.") || key.starts_with("artifact-grid:show-resolved:") {
        let (field, entry) = dynamic_path(key)?;
        let map_field_name = field
            .rsplit_once('.')
            .map(|(base, _)| base)
            .unwrap_or(field);
        let value = document
            .get_field(map_field_name)?
            .as_object()?
            .get(&entry)?;
        return encode_raw_value(field, value);
    }
    let value = document.get_field(binding.field)?;
    if value.is_null() {
        return None;
    }
    encode_legacy_value(&binding, value)
}

pub fn legacy_values_for_document(
    document: &SettingsDocument,
    project_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut values = Vec::new();
    for key in [
        "agent.defaultEngineId",
        "agent.chatAdapterId",
        "claude.watchEnabled",
        "workspace.claudeBrowserMode",
        "workspace.sidebarCollapsed",
        "workspace.explorerSections",
        "projects.extraRoots",
        "onboarding.state",
        "user.name",
        "updates.autoCheck",
        "updates.autoInstallApp",
        "updates.autoInstallPkgs",
        "appearance.theme",
        "appearance.mode",
        "appearance.density",
        "appearance.tintStrength",
        "terminal.default_shell_id",
        "terminal.custom_shell_profiles",
        "terminal.agent_env_kind",
        "terminal.agent_wsl_distro",
        "terminal.resume_on_start",
        "artifact-grid.default-sink",
        "artifact-grid.stack-mode",
        "artifact-wizard.terminalHandoff",
    ] {
        if let Some(value) = read_legacy_value(document, key) {
            values.push((key.to_string(), value));
        } else if let Some(binding) = legacy_binding(key) {
            if legacy_value_present(document, key) {
                if let Some(value) = encode_legacy_value(&binding, &Value::Null) {
                    values.push((key.to_string(), value));
                }
            } else if let Some(default) = default_value(binding.field) {
                if let Some(value) = encode_legacy_value(&binding, &default) {
                    values.push((key.to_string(), value));
                }
            }
        }
    }
    if let Some(folder_overrides) = document
        .get_field("workspace.artifact.folderOverrides")
        .and_then(Value::as_object)
    {
        for (path, entry) in folder_overrides {
            if let Some(entry) = entry.as_object() {
                if let Some(value) = entry.get("defaultSink").and_then(Value::as_str) {
                    values.push((
                        format!("artifact-grid.folder.{path}.default-sink"),
                        value.to_string(),
                    ));
                }
                if let Some(value) = entry.get("stackMode").and_then(Value::as_str) {
                    values.push((
                        format!("artifact-grid.folder.{path}.stack-mode"),
                        value.to_string(),
                    ));
                }
            }
        }
    }
    if let Some(sinks) = document
        .get_field("workspace.artifact.artifactSinkOverrides")
        .and_then(Value::as_object)
    {
        for (path, value) in sinks {
            if let Some(value) = value.as_str() {
                values.push((format!("artifact-studio.sink.{path}"), value.to_string()));
            }
        }
    }
    if let Some(show_resolved) = document
        .get_field("workspace.artifact.showResolved")
        .and_then(Value::as_object)
    {
        for (path, value) in show_resolved {
            if let Some(value) = encode_raw_value("workspace.artifact.showResolved", value) {
                values.push((format!("artifact-grid:show-resolved:{path}"), value));
            }
        }
    }
    if let Some(project_id) = project_id {
        if let Some(kind) = document
            .get_field("workspace.lastAgent.kind")
            .and_then(Value::as_str)
        {
            values.push((
                format!("artifact-wizard.lastAgent.{project_id}"),
                kind.to_string(),
            ));
        }
        if let Some(command) = document
            .get_field("workspace.lastAgent.customCommand")
            .and_then(Value::as_str)
        {
            values.push((
                format!("artifact-wizard.lastAgentCustom.{project_id}"),
                command.to_string(),
            ));
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_values_override_only_present_personal_fields() {
        let personal = SettingsDocument::parse(
            br#"{"$schema":"urn:ikenga:settings:v1","version":1,"appearance":{"theme":"A","mode":"dark"}}"#,
        )
        .unwrap();
        let project = SettingsDocument::parse(
            br#"{"$schema":"urn:ikenga:settings:v1","version":1,"appearance":{"theme":"C"}}"#,
        )
        .unwrap();
        let effective = personal.merge(&project);
        assert_eq!(
            effective.appearance.get("theme"),
            Some(&Value::String("C".into()))
        );
        assert_eq!(
            effective.appearance.get("mode"),
            Some(&Value::String("dark".into()))
        );
        assert_eq!(project.leaf_paths(), vec!["appearance.theme"]);
    }

    #[test]
    fn dynamic_legacy_values_keep_nested_shape() {
        let mut document = SettingsDocument::default();
        apply_legacy_value(
            &mut document,
            "artifact-grid.folder./work/a.default-sink",
            "terminal",
        )
        .unwrap();
        apply_legacy_value(
            &mut document,
            "artifact-wizard.lastAgent.project-a",
            "codex",
        )
        .unwrap();
        assert_eq!(
            document
                .get_field("workspace.artifact.folderOverrides")
                .and_then(Value::as_object)
                .and_then(|value| value.get("/work/a"))
                .and_then(Value::as_object)
                .and_then(|value| value.get("defaultSink"))
                .and_then(Value::as_str),
            Some("terminal")
        );
        assert_eq!(
            document
                .get_field("workspace.lastAgent.kind")
                .and_then(Value::as_str),
            Some("codex")
        );
    }

    #[test]
    fn explicit_null_is_not_replaced_by_a_legacy_value_during_migration() {
        let mut personal = SettingsDocument::default();
        personal
            .set_field("engines.defaultEngineId", Value::Null)
            .unwrap();
        assert!(legacy_value_present(&personal, "agent.defaultEngineId"));
    }

    #[test]
    fn personal_only_fields_do_not_override_from_a_project_document() {
        let mut personal = SettingsDocument::default();
        personal
            .set_field("workspace.userName", Value::String("Ada".into()))
            .unwrap();
        let mut project = SettingsDocument::default();
        project
            .set_field("workspace.userName", Value::String("Project".into()))
            .unwrap();
        let effective = personal.merge(&project.project_overlay());
        assert_eq!(
            effective.get_field("workspace.userName").unwrap().as_str(),
            Some("Ada")
        );
    }

    #[test]
    fn unknown_section_fields_survive_a_document_round_trip() {
        let document = SettingsDocument::parse(
            br#"{"version":1,"appearance":{"futureFlag":true},"futureTop":{"x":1}}"#,
        )
        .unwrap();
        let bytes = document.to_bytes().unwrap();
        let round_trip = SettingsDocument::parse(&bytes).unwrap();
        assert_eq!(
            round_trip.appearance.get("futureFlag").unwrap(),
            &Value::Bool(true)
        );
        assert_eq!(
            round_trip.extra.get("futureTop").unwrap().get("x").unwrap(),
            &Value::from(1i64)
        );
    }

    #[test]
    fn retired_sink_values_normalize_before_validation() {
        let mut document = SettingsDocument::default();
        apply_legacy_value(&mut document, "artifact-grid.default-sink", r#""sidepane""#).unwrap();
        apply_legacy_value(&mut document, "artifact-studio.sink./work/a", r#""both""#).unwrap();
        apply_legacy_value(&mut document, "terminal.agent_env_kind", "posix").unwrap();
        assert_eq!(
            document
                .get_field("workspace.artifact.defaultSink")
                .and_then(Value::as_str),
            Some("clipboard")
        );
        assert_eq!(
            document
                .get_field("engines.agentEnvironment")
                .and_then(Value::as_str),
            Some("native")
        );
    }

    #[test]
    fn removing_nested_fields_prunes_empty_parents() {
        let mut document = SettingsDocument::default();
        document
            .set_field("workspace.lastAgent.kind", Value::String("codex".into()))
            .unwrap();
        document.remove_field("workspace.lastAgent.kind").unwrap();
        assert!(document.get_field("workspace.lastAgent").is_none());
        assert!(!document.has_content());
    }

    #[test]
    fn resolved_defaults_preserve_explicit_null() {
        let mut document = SettingsDocument::default();
        document
            .set_field("engines.defaultEngineId", Value::Null)
            .unwrap();
        let resolved = document.resolved();
        assert_eq!(
            resolved.get_field("engines.defaultEngineId"),
            Some(&Value::Null)
        );
        assert_eq!(
            resolved
                .get_field("appearance.theme")
                .and_then(Value::as_str),
            Some("A")
        );
    }

    #[test]
    fn nested_known_values_are_validated_before_persistence() {
        let mut document = SettingsDocument::default();
        assert!(document
            .set_field(
                "workspace.artifact",
                serde_json::json!({
                    "folderOverrides": {
                        "project": { "defaultSink": "sidepane" }
                    }
                }),
            )
            .is_err());
    }

    #[test]
    fn secret_values_are_not_accepted_in_the_settings_document() {
        let with_secret = br#"{"version":1,"secrets":{"token":"do-not-store"}}"#;
        assert!(SettingsDocument::parse(with_secret).is_err());
    }

    #[test]
    fn invalid_version_and_section_shape_are_rejected() {
        let newer = br#"{"version":2}"#;
        assert!(SettingsDocument::parse(newer).is_err());
        let malformed = br#"{"version":1,"appearance":[]}"#;
        assert!(SettingsDocument::parse(malformed).is_err());
    }

    #[test]
    fn legacy_values_round_trip_through_both_encodings() {
        let mut document = SettingsDocument::default();
        apply_legacy_value(&mut document, "user.name", r#""Ada""#).unwrap();
        apply_legacy_value(&mut document, "terminal.resume_on_start", "false").unwrap();
        apply_legacy_value(&mut document, "artifact-grid:show-resolved:/work/a", "1").unwrap();
        assert_eq!(
            read_legacy_value(&document, "user.name").as_deref(),
            Some(r#""Ada""#)
        );
        assert_eq!(
            read_legacy_value(&document, "terminal.resume_on_start").as_deref(),
            Some("false")
        );
        assert_eq!(
            read_legacy_value(&document, "artifact-grid:show-resolved:/work/a").as_deref(),
            Some("1")
        );
    }
}
