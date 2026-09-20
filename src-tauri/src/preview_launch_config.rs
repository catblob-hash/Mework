//! `.mework/launch.json`, read and validated.
//!
//! A project describes its dev servers in this file, and everything the preview
//! feature later does — which process to spawn, on which port, which URL the
//! pane opens — is decided here. Nothing in this module spawns, listens, or
//! keeps state: it reads one file and returns a value, so those decisions stay
//! testable without a port or a child process.
//!
//! Two properties of the format are load-bearing and easy to lose.
//!
//! A localhost `url` must be a bare origin. The file is project data, not user
//! intent, so a repository that ships `"url": "http://localhost:9090/admin/wipe"`
//! must not be able to make the preview pane fetch that endpoint the moment the
//! project is opened. Path, query, and fragment are therefore rejected for
//! loopback hosts, and only the canonical loopback spellings are accepted so the
//! rule cannot be sidestepped with `127.0.0.2`, a trailing dot, or an
//! IPv4-mapped IPv6 address.
//!
//! An entry this build cannot run is *dropped*, while an entry it recognises but
//! whose fields are wrong is *malformed*. The distinction is what the model gets
//! told: a dropped VS Code `node-terminal` entry means "this format is not
//! supported", a malformed entry means "your file has a typo", and the two need
//! different advice.
//!
//! The strings the model sees are copied from the Claude Code desktop app and
//! pinned by tests rather than reworded here. The one substitution is the config
//! path: `.mework/launch.json`, because the source's `.claude/launch.json`
//! belongs to a product that may be installed alongside this one.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use jsonc_parser::{
    ast::{Object, ObjectPropName, Value as JsonValue},
    common::Ranged,
    ParseOptions,
};
use regex::Regex;
use url::Url;

/// The launch.json skeleton quoted back to the model whenever the file is
/// missing or unusable.
pub const LAUNCH_JSON_FORMAT: &str = "{\n  \"version\": \"0.0.1\",\n  \"configurations\": [\n    {\n      \"name\": \"<unique-name>\",\n      \"runtimeExecutable\": \"<command>\",\n      \"runtimeArgs\": [\"<args>\"],\n      \"port\": <port>\n    }\n  ]\n}";

/// The prose that always follows [`LAUNCH_JSON_FORMAT`].
pub const LAUNCH_JSON_FORMAT_NOTES: &str = "Set \"runtimeExecutable\" to the command (e.g. \"npm\"), \"runtimeArgs\" to the arguments (e.g. [\"run\", \"dev\"]), and \"port\" to the server port. An optional \"url\" (http/https) opens the preview there instead of http://localhost:<port>. A localhost \"url\" must be just the server's origin \u{2014} no path or query, matching the entry's port \u{2014} for example \"https://localhost:8443\" or \"http://app.localhost:3000\"; to show a specific page, navigate after the preview opens. Non-localhost URLs may carry paths and are subject to the user's permission and the organization's browsing policy. A configuration with \"url\" and no command attaches to an already-running server. Only include servers you actually need to preview.";

/// Port used when an entry has a command but names no port anywhere.
const DEFAULT_PORT: u32 = 3000;

/// Longest text any model-facing message quotes back from the file.
const SANITIZE_LIMIT: usize = 120;

/// One runnable (or attachable) entry from `configurations`.
///
/// `command` is `None` only for the attach form, where `url` points at a server
/// somebody else already started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub port: u32,
    pub env: BTreeMap<String, String>,
    /// `None` means the file said nothing; the caller decides the default.
    pub auto_port: Option<bool>,
    pub url: Option<String>,
}

/// The whole file, once every entry has been parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchConfigFile {
    pub servers: Vec<ServerConfig>,
    pub auto_verify: bool,
}

/// An entry whose shape this build supports but whose fields are wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MalformedEntry {
    pub name: String,
    pub reason: String,
}

/// Outcome of looking for a usable configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchDiscovery {
    NotFound,
    Unreadable {
        code: String,
        message: String,
    },
    ParseError {
        detail: String,
    },
    NoValidConfigs {
        configuration_count: usize,
        malformed: Vec<MalformedEntry>,
    },
    NameNotFound {
        available: Vec<String>,
    },
    Ok {
        config: LaunchConfigFile,
        malformed: Vec<MalformedEntry>,
    },
}

impl LaunchDiscovery {
    pub fn config(&self) -> Option<&LaunchConfigFile> {
        match self {
            Self::Ok { config, .. } => Some(config),
            _ => None,
        }
    }

    pub fn malformed(&self) -> &[MalformedEntry] {
        match self {
            Self::NoValidConfigs { malformed, .. } | Self::Ok { malformed, .. } => malformed,
            _ => &[],
        }
    }

    /// Every malformed entry name in file order, duplicates kept.
    pub fn malformed_names(&self) -> Vec<String> {
        self.malformed()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Malformed reasons keyed by name. Two entries sharing a name collapse into
    /// one that keeps the first position and the last reason, because the source
    /// accumulates these in a plain object.
    pub fn malformed_reasons(&self) -> Vec<(String, String)> {
        let mut reasons: Vec<(String, String)> = Vec::new();
        for entry in self.malformed() {
            match reasons.iter_mut().find(|(name, _)| *name == entry.name) {
                Some((_, reason)) => reason.clone_from(&entry.reason),
                None => reasons.push((entry.name.clone(), entry.reason.clone())),
            }
        }
        reasons
    }
}

/// Why a `preview_start` request was refused before anything was spawned.
// The shared prefix is the wire vocabulary these variants serialize to, not
// accidental repetition; see `as_str`.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchDenyReason {
    LaunchConfigMissing,
    LaunchConfigUnreadable,
    LaunchConfigInvalid,
    LaunchConfigNameNotFound,
}

impl LaunchDenyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LaunchConfigMissing => "launch_config_missing",
            Self::LaunchConfigUnreadable => "launch_config_unreadable",
            Self::LaunchConfigInvalid => "launch_config_invalid",
            Self::LaunchConfigNameNotFound => "launch_config_name_not_found",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchDenial {
    pub reason: LaunchDenyReason,
    pub message: String,
}

/// Reads and parses `<working_directory>/.mework/launch.json`.
pub struct LaunchConfigDiscovery {
    working_directory: PathBuf,
}

impl LaunchConfigDiscovery {
    pub fn new(working_directory: impl Into<PathBuf>) -> Self {
        Self {
            working_directory: working_directory.into(),
        }
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub fn launch_json_path(&self) -> PathBuf {
        launch_json_path(&self.working_directory)
    }

    /// Reads the file and resolves `name` against it. `None` keeps every usable
    /// entry.
    pub fn discover(&self, name: Option<&str>) -> LaunchDiscovery {
        match read_launch_json(&self.launch_json_path()) {
            ReadOutcome::Missing => LaunchDiscovery::NotFound,
            ReadOutcome::Unreadable { code, message } => {
                LaunchDiscovery::Unreadable { code, message }
            }
            ReadOutcome::Read(source) => self.parse_source(&source, name),
        }
    }

    /// The pure core of [`Self::discover`].
    pub fn parse_source(&self, source: &str, name: Option<&str>) -> LaunchDiscovery {
        let parsed = match jsonc_parser::parse_to_ast(
            source,
            &Default::default(),
            &launch_json_parse_options(),
        ) {
            Ok(result) => result.value,
            Err(error) => {
                return LaunchDiscovery::ParseError {
                    detail: format!("{} at offset {}", error.kind(), error.range().start),
                }
            }
        };
        let Some(JsonValue::Object(root)) = parsed else {
            return LaunchDiscovery::ParseError {
                detail: "file is not a JSON object".to_owned(),
            };
        };

        // `version` is in every file the app writes but is never validated or
        // acted on; reading it here keeps that deliberate rather than forgotten.
        let _version = root.get("version");

        let configurations = match root.get("configurations").map(|entry| &entry.value) {
            None => Vec::new(),
            Some(JsonValue::Array(array)) => array.elements.iter().collect::<Vec<_>>(),
            Some(_) => {
                return LaunchDiscovery::ParseError {
                    detail: "\"configurations\" must be an array".to_owned(),
                }
            }
        };
        let configuration_count = configurations.len();

        let mut servers = Vec::new();
        let mut malformed = Vec::new();
        for element in configurations {
            let JsonValue::Object(entry) = element else {
                continue;
            };
            match self.parse_configuration(entry) {
                Ok(Some(server)) => servers.push(server),
                Ok(None) => {}
                Err(reason) => malformed.push(MalformedEntry {
                    name: match entry.get("name").map(|property| &property.value) {
                        Some(JsonValue::StringLit(text)) => text.value.to_string(),
                        _ => "(unnamed entry)".to_owned(),
                    },
                    reason,
                }),
            }
        }

        if servers.is_empty() {
            return LaunchDiscovery::NoValidConfigs {
                configuration_count,
                malformed,
            };
        }

        // Only a literal `false` turns verification off.
        let auto_verify = !matches!(
            root.get("autoVerify").map(|entry| &entry.value),
            Some(JsonValue::BooleanLit(literal)) if !literal.value
        );
        let Some(name) = name else {
            return LaunchDiscovery::Ok {
                config: LaunchConfigFile {
                    servers,
                    auto_verify,
                },
                malformed,
            };
        };
        let wanted = name.to_lowercase();
        match servers
            .iter()
            .find(|server| server.name.to_lowercase() == wanted)
        {
            Some(server) => LaunchDiscovery::Ok {
                config: LaunchConfigFile {
                    servers: vec![server.clone()],
                    auto_verify,
                },
                malformed,
            },
            None => LaunchDiscovery::NameNotFound {
                available: servers.into_iter().map(|server| server.name).collect(),
            },
        }
    }

    pub fn resolve_variables(&self, value: &str) -> String {
        substitute_variables(value, &self.working_directory)
    }

    /// `Ok(None)` is an entry this build cannot run; `Err` is one it recognises
    /// but whose fields are wrong.
    fn parse_configuration(&self, entry: &Object) -> Result<Option<ServerConfig>, String> {
        let runtime_executable = string_field(entry, "runtimeExecutable")?;
        let runtime_args = string_array_field(entry, "runtimeArgs")?;
        let program = string_field(entry, "program")?;
        let args = string_array_field(entry, "args")?;
        let command = build_command(
            runtime_executable.as_deref(),
            &runtime_args,
            program.as_deref(),
            &args,
        );
        if command.is_none()
            && (entry.get("type").is_some()
                || entry.get("url").is_none()
                || entry.get("command").is_some())
        {
            return Ok(None);
        }

        let url = match entry.get("url") {
            None => None,
            Some(property) => Some(url_field_value(&property.value)?),
        };
        let command = command.map(|(program, arguments)| {
            (
                self.resolve_variables(&program),
                arguments
                    .iter()
                    .map(|argument| self.resolve_variables(argument))
                    .collect::<Vec<_>>(),
            )
        });

        let cwd = match string_field(entry, "cwd")?.filter(|value| !value.is_empty()) {
            None => self.working_directory.clone(),
            Some(raw) => {
                let resolved = self.resolve_variables(&raw);
                // Node's `path.isAbsolute` accepts a rooted path with no drive
                // letter on Windows, which is exactly what `has_root` reports.
                if Path::new(&resolved).has_root() {
                    PathBuf::from(resolved)
                } else {
                    self.working_directory.join(resolved)
                }
            }
        };

        let env = env_field(entry)?;
        let explicit_port = port_field(entry)?.filter(|port| *port != 0);
        let url_port = url
            .as_ref()
            .filter(|url| is_localhost_host(url.host_str().unwrap_or_default()))
            .map(url_port_or_default);
        if let (Some(url_port), Some(explicit_port)) = (url_port, explicit_port) {
            if url_port != explicit_port {
                return Err(format!(
                    "\"url\" is {} but this entry's server runs on port {explicit_port} \u{2014} a localhost \"url\" must point at the entry's own server",
                    json_quote(url.as_ref().map(Url::as_str).unwrap_or_default())
                ));
            }
        }
        let port = explicit_port
            .or_else(|| url_port.filter(|port| *port != 0))
            .or_else(|| {
                extract_port_from_command(
                    &env,
                    program.as_deref(),
                    runtime_executable.as_deref(),
                    &runtime_args,
                    &args,
                )
                .filter(|port| *port != 0)
            })
            .unwrap_or(if command.is_some() { DEFAULT_PORT } else { 0 });

        let name = string_field(entry, "name")?
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| match &command {
                Some((_, arguments)) => derive_name_from_args(arguments),
                None => url.as_ref().map(url_host).unwrap_or_default(),
            });
        let auto_port = bool_field(entry, "autoPort")?;
        let (command, args) = match command {
            Some((program, arguments)) => (Some(program), arguments),
            None => (None, Vec::new()),
        };

        Ok(Some(ServerConfig {
            name,
            command,
            args,
            cwd,
            port,
            env,
            auto_port,
            url: url.map(|url| url.as_str().to_owned()),
        }))
    }
}

/// Rewrites `autoVerify` in place, leaving every comment and every other byte of
/// the file exactly where it was.
///
/// The source edits the text with `jsonc-parser`'s `modify` + `applyEdits`, which
/// this crate has no equivalent of, so the property's own byte range is spliced
/// instead. `None` when the text is not a JSON object: there is nothing to edit,
/// and a preference toggle must never be the thing that replaces a file somebody
/// is halfway through writing.
pub fn set_auto_verify_source(source: &str, enabled: bool) -> Option<String> {
    let parsed =
        jsonc_parser::parse_to_ast(source, &Default::default(), &launch_json_parse_options())
            .ok()?;
    let JsonValue::Object(root) = parsed.value? else {
        return None;
    };
    let literal = if enabled { "true" } else { "false" };
    let mut updated = source.to_owned();
    match root
        .properties
        .iter()
        .find(|property| property_name(&property.name) == "autoVerify")
    {
        Some(property) => {
            let range = property.value.range();
            updated.replace_range(range.start..range.end, literal);
        }
        // Written first rather than last so the insertion never has to guess where
        // a trailing comment ends.
        None => {
            let insertion = if root.properties.is_empty() {
                format!("\n  \"autoVerify\": {literal}\n")
            } else {
                format!("\n  \"autoVerify\": {literal},")
            };
            updated.insert_str(root.range.start + 1, &insertion);
        }
    }
    Some(updated)
}

/// `runtimeExecutable` wins; `program` alone runs under `node`. An empty string
/// counts as absent, matching the source's truthiness checks.
fn build_command(
    runtime_executable: Option<&str>,
    runtime_args: &[String],
    program: Option<&str>,
    args: &[String],
) -> Option<(String, Vec<String>)> {
    let runtime_executable = runtime_executable.filter(|value| !value.is_empty());
    let program = program.filter(|value| !value.is_empty());
    if let Some(runtime_executable) = runtime_executable {
        let mut combined = runtime_args.to_vec();
        if let Some(program) = program {
            combined.push(program.to_owned());
        }
        combined.extend_from_slice(args);
        return Some((runtime_executable.to_owned(), combined));
    }
    program.map(|program| {
        let mut combined = vec![program.to_owned()];
        combined.extend_from_slice(args);
        (String::from("node"), combined)
    })
}

/// Last resort before the 3000 default: read a port out of whatever the entry
/// happens to say. `PORT` in `env` beats the command text, and each pattern is
/// tried against every string before the next pattern gets a turn.
///
/// Each candidate is matched on its own, so a port split across two arguments
/// (`["--port", "3000"]`) is deliberately not recognised.
fn extract_port_from_command(
    env: &BTreeMap<String, String>,
    program: Option<&str>,
    runtime_executable: Option<&str>,
    runtime_args: &[String],
    args: &[String],
) -> Option<u32> {
    if let Some(port) = env.get("PORT").filter(|value| !value.is_empty()) {
        return parse_int_prefix(port);
    }
    static FLAG: OnceLock<Regex> = OnceLock::new();
    static LOCALHOST: OnceLock<Regex> = OnceLock::new();
    static BARE: OnceLock<Regex> = OnceLock::new();
    let patterns = [
        FLAG.get_or_init(|| {
            Regex::new(r"(?:--port|--PORT|-p)\s+(\d+)").expect("valid port flag regex")
        }),
        LOCALHOST.get_or_init(|| Regex::new(r"localhost:(\d+)").expect("valid localhost regex")),
        BARE.get_or_init(|| Regex::new(r":(\d{4,5})(?:\s|$)").expect("valid bare port regex")),
    ];
    let mut candidates: Vec<&str> = Vec::new();
    candidates.extend(program);
    candidates.extend(runtime_executable);
    candidates.extend(runtime_args.iter().map(String::as_str));
    candidates.extend(args.iter().map(String::as_str));
    for pattern in patterns {
        for candidate in &candidates {
            if let Some(captures) = pattern.captures(candidate) {
                return captures
                    .get(1)
                    .and_then(|digits| digits.as_str().parse().ok());
            }
        }
    }
    None
}

/// `parseInt(value, 10)`: leading whitespace and sign, then as many digits as
/// there are, and nothing if there are none.
fn parse_int_prefix(value: &str) -> Option<u32> {
    let trimmed = value.trim_start();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let end = digits
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 || negative {
        return None;
    }
    digits[..end].parse().ok()
}

/// The last argument, minus one file extension, with colons flattened. Anything
/// shorter than two characters is not a useful label.
fn derive_name_from_args(args: &[String]) -> String {
    let Some(last) = args.last().filter(|value| !value.is_empty()) else {
        return String::from("preview");
    };
    static EXTENSION: OnceLock<Regex> = OnceLock::new();
    let stripped = EXTENSION
        .get_or_init(|| Regex::new(r"\.[^.]+$").expect("valid extension regex"))
        .replace(last, "");
    let flattened = stripped.replace(':', "-");
    if flattened.chars().count() >= 2 {
        flattened
    } else {
        String::from("preview")
    }
}

pub fn substitute_variables(value: &str, working_directory: &Path) -> String {
    let working_directory = working_directory.to_string_lossy();
    value
        .replace("${workspaceFolder}", &working_directory)
        .replace("${workspaceRoot}", &working_directory)
}

pub fn launch_json_path(working_directory: &Path) -> PathBuf {
    working_directory.join(".mework").join("launch.json")
}

// ---------------------------------------------------------------------------
// URL rules
// ---------------------------------------------------------------------------

/// Hosts the localhost rules apply to.
pub fn is_localhost_host(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "127.0.0.1"
        || host == "0.0.0.0"
        || host == "::1"
        || host == "[::1]"
}

/// A loopback address written so that [`is_localhost_host`] would miss it.
fn is_non_canonical_loopback(host: &str) -> bool {
    static LOOPBACK_IPV4: OnceLock<Regex> = OnceLock::new();
    let trimmed = host.trim_end_matches('.');
    if host != trimmed && is_localhost_host(trimmed) {
        return true;
    }
    let loopback_ipv4 = LOOPBACK_IPV4
        .get_or_init(|| Regex::new(r"^127\.\d+\.\d+\.\d+$").expect("valid loopback regex"));
    if loopback_ipv4.is_match(host) && host != "127.0.0.1" {
        return true;
    }
    host.starts_with("[::ffff:")
}

/// The six `url` rules, in the order the source applies them.
pub fn validate_url(value: &str) -> Result<Url, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("\"url\" must be a non-empty string".to_owned());
    }
    let Ok(url) = Url::parse(trimmed) else {
        return Err("\"url\" is not a valid URL".to_owned());
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("\"url\" must be http or https".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some_and(|value| !value.is_empty()) {
        return Err("\"url\" must not embed credentials".to_owned());
    }
    let host = url.host_str().unwrap_or_default();
    if is_non_canonical_loopback(host) {
        return Err(format!(
            "\"url\" host {} is a non-canonical loopback spelling \u{2014} write the canonical form (\"localhost\", a \"*.localhost\" name, \"127.0.0.1\", or \"[::1]\") so the localhost rules apply",
            json_quote(host)
        ));
    }
    if is_localhost_host(host) && !is_bare_origin(&url) {
        return Err(format!(
            "\"url\" is {}, a localhost address with a path or query. For security, a localhost \"url\" must be just the server's origin \u{2014} use {} and ask Claude to navigate to the specific page once the preview is open. (A config file must not point the preview at arbitrary local endpoints; other local services could be affected.)",
            json_quote(url.as_str()),
            json_quote(&format!("{}/", url.origin().ascii_serialization()))
        ));
    }
    Ok(url)
}

/// Whether `value` parses as an http(s) URL on a localhost host.
pub fn is_localhost_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        (url.scheme() == "http" || url.scheme() == "https")
            && is_localhost_host(url.host_str().unwrap_or_default())
    })
}

/// Whether `value` is a localhost URL carrying nothing but its origin.
pub fn is_bare_localhost_origin(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        (url.scheme() == "http" || url.scheme() == "https")
            && is_localhost_host(url.host_str().unwrap_or_default())
            && is_bare_origin(&url)
    })
}

fn is_bare_origin(url: &Url) -> bool {
    url.path() == "/"
        && !url.query().is_some_and(|query| !query.is_empty())
        && !url.fragment().is_some_and(|fragment| !fragment.is_empty())
}

fn url_port_or_default(url: &Url) -> u32 {
    u32::from(
        url.port()
            .unwrap_or(if url.scheme() == "https" { 443 } else { 80 }),
    )
}

/// WHATWG `host`: the hostname plus a non-default port.
fn url_host(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Model-facing messages
// ---------------------------------------------------------------------------

/// Trims a file-supplied string down to something safe to paste into a message:
/// one line, no confusable quote marks, no invisible characters, and no more
/// than [`SANITIZE_LIMIT`] characters. Any change at all — including a single
/// substituted quote — is signalled with a trailing ellipsis.
pub fn sanitize_for_message(value: &str) -> String {
    static LINE_BREAK: OnceLock<Regex> = OnceLock::new();
    static QUOTE_LIKE: OnceLock<Regex> = OnceLock::new();
    static INVISIBLE: OnceLock<Regex> = OnceLock::new();

    let first_line = LINE_BREAK
        .get_or_init(|| {
            Regex::new(r"[\r\n\x{000b}\x{000c}\x{0085}\x{2028}\x{2029}]")
                .expect("valid line break regex")
        })
        .split(value)
        .next()
        .unwrap_or_default();
    let unquoted = QUOTE_LIKE
        .get_or_init(|| {
            Regex::new(concat!(
                "[`\"",
                r"\x{00a8}\x{00b4}\x{0374}\x{0384}\x{0385}\x{02b9}-\x{02bd}\x{02ca}\x{02cb}",
                r"\x{02dd}\x{02ee}\x{02f4}-\x{02f6}\x{05f3}\x{05f4}\x{07f4}\x{07f5}\x{1fbd}",
                r"\x{1fbf}\x{1fcd}-\x{1fcf}\x{1fdd}-\x{1fdf}\x{1fed}-\x{1fef}\x{1ffd}\x{1ffe}",
                r"\x{201a}\x{201e}\x{2032}-\x{2037}\x{2057}\x{275b}-\x{2760}\x{276e}\x{276f}",
                r"\x{2e42}\x{3003}\x{301d}-\x{301f}\x{ff02}\x{ff07}\x{ff40}\x{1f676}-\x{1f678}",
                r"\p{Pi}\p{Pf}]"
            ))
            .expect("valid quote-like regex")
        })
        .replace_all(first_line, "'");
    let cleaned = INVISIBLE
        .get_or_init(|| Regex::new(r"[\p{Cc}\p{Cf}]").expect("valid invisible character regex"))
        .replace_all(&unquoted, "\u{fffd}");
    let truncated = if cleaned.chars().count() > SANITIZE_LIMIT {
        cleaned.chars().take(SANITIZE_LIMIT).collect::<String>()
    } else {
        cleaned.clone().into_owned()
    };
    if truncated == value {
        value.to_owned()
    } else {
        format!("{truncated}\u{2026}")
    }
}

/// `"a", "b"` — the way every message lists server names.
pub fn quote_join(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", sanitize_for_message(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Case-insensitive, and tolerant of a name the model only ever saw sanitized.
pub fn names_match(config_name: &str, requested: &str) -> bool {
    let requested = requested.to_lowercase();
    config_name.to_lowercase() == requested
        || sanitize_for_message(config_name).to_lowercase() == requested
}

fn malformed_reason_suffix(reasons: &[(String, String)]) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    format!(
        " \u{2014} {}",
        reasons
            .iter()
            .map(|(name, reason)| format!(
                "{}: {}",
                sanitize_for_message(name),
                sanitize_for_message(reason)
            ))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// What the model is told when no launch.json exists at all.
pub fn missing_launch_json_message(working_directory: &Path) -> String {
    format!(
        "No .mework/launch.json found. Create {} with this format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES} Then call preview_start with the server name.",
        launch_json_path(working_directory).display()
    )
}

/// The short line written to the host log. `None` for a usable file.
pub fn discovery_log_summary(discovery: &LaunchDiscovery) -> Option<String> {
    Some(match discovery {
        LaunchDiscovery::NotFound => "no .mework/launch.json".to_owned(),
        LaunchDiscovery::Unreadable { code, message } => {
            format!("launch.json could not be read ({code}): {message}")
        }
        LaunchDiscovery::ParseError { detail } => {
            format!("launch.json failed to parse: {detail}")
        }
        LaunchDiscovery::NoValidConfigs {
            configuration_count,
            malformed,
        } => {
            if *configuration_count == 0 {
                "launch.json contains no configurations".to_owned()
            } else {
                format!(
                    "none of the {configuration_count} configuration(s) in launch.json could be used ({} with a malformed field; the rest lack \"runtimeExecutable\" or \"program\")",
                    malformed.len()
                )
            }
        }
        LaunchDiscovery::NameNotFound { available } => format!(
            "no configuration with the requested name; available: {}",
            available.join(", ")
        ),
        LaunchDiscovery::Ok { .. } => return None,
    })
}

/// The warning the host emits for a problem worth a log line. A missing file
/// and an unmatched name are ordinary and stay silent.
pub fn discovery_warning(discovery: &LaunchDiscovery) -> Option<String> {
    match discovery {
        LaunchDiscovery::Ok { malformed, .. } => {
            if malformed.is_empty() {
                return None;
            }
            Some(format!(
                "skipped malformed launch.json entr{}: {}",
                if malformed.len() == 1 { "y" } else { "ies" },
                discovery.malformed_names().join(", ")
            ))
        }
        LaunchDiscovery::NotFound | LaunchDiscovery::NameNotFound { .. } => None,
        _ => discovery_log_summary(discovery),
    }
}

/// The wire reason a denial carries. `None` for a usable file.
pub fn discovery_deny_reason(discovery: &LaunchDiscovery) -> Option<LaunchDenyReason> {
    Some(match discovery {
        LaunchDiscovery::NotFound => LaunchDenyReason::LaunchConfigMissing,
        LaunchDiscovery::Unreadable { .. } => LaunchDenyReason::LaunchConfigUnreadable,
        LaunchDiscovery::ParseError { .. } | LaunchDiscovery::NoValidConfigs { .. } => {
            LaunchDenyReason::LaunchConfigInvalid
        }
        LaunchDiscovery::NameNotFound { .. } => LaunchDenyReason::LaunchConfigNameNotFound,
        LaunchDiscovery::Ok { .. } => return None,
    })
}

/// The full explanation the model gets for an unusable file. `None` for a
/// usable one.
pub fn discovery_model_message(
    discovery: &LaunchDiscovery,
    working_directory: &Path,
) -> Option<String> {
    let path = launch_json_path(working_directory);
    let path = path.display();
    Some(match discovery {
        LaunchDiscovery::NotFound => missing_launch_json_message(working_directory),
        LaunchDiscovery::Unreadable { code, message } => format!(
            "Found {path} but reading it failed with {code}: {message}. The path exists \u{2014} do not recreate it."
        ),
        LaunchDiscovery::ParseError { detail } => format!(
            "Found {path} but it could not be parsed: {detail}. Fix the file to match this format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES}"
        ),
        LaunchDiscovery::NoValidConfigs {
            configuration_count,
            malformed,
        } => {
            if *configuration_count == 0 {
                format!(
                    "Found {path} but it contains no configurations. Expected format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES}"
                )
            } else {
                let malformed_count = malformed.len();
                let mut message = format!(
                    "Found {path} with {configuration_count} {}, but none could be used.",
                    if *configuration_count == 1 {
                        "configuration"
                    } else {
                        "configurations"
                    }
                );
                if malformed_count > 0 {
                    message.push_str(&format!(
                        " {malformed_count} of them {} a supported shape but a malformed field{}.",
                        if malformed_count == 1 { "has" } else { "have" },
                        malformed_reason_suffix(&discovery.malformed_reasons())
                    ));
                }
                if malformed_count < *configuration_count {
                    message.push_str(
                        " Supported configurations need \"runtimeExecutable\" (optionally with \"runtimeArgs\"), \"program\", or a \"url\" to attach to an already-running server \u{2014} VS Code \"command\" / \"node-terminal\" style entries are not supported.",
                    );
                }
                message.push_str(&format!(
                    " Expected format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES}"
                ));
                message
            }
        }
        LaunchDiscovery::NameNotFound { available } => format!(
            "No matching server in {path}. Available servers: {}.",
            available
                .iter()
                .map(|name| sanitize_for_message(name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        LaunchDiscovery::Ok { .. } => return None,
    })
}

/// Picks the entry a `preview_start` call means.
///
/// `discovery` must come from a nameless [`LaunchConfigDiscovery::discover`]:
/// the fallbacks below need to see every usable entry and every malformed one,
/// which a name-filtered discovery has already thrown away.
pub fn select_server(
    discovery: &LaunchDiscovery,
    working_directory: &Path,
    requested_name: Option<&str>,
) -> Result<ServerConfig, LaunchDenial> {
    let LaunchDiscovery::Ok { config, malformed } = discovery else {
        return Err(LaunchDenial {
            reason: discovery_deny_reason(discovery)
                .unwrap_or(LaunchDenyReason::LaunchConfigInvalid),
            message: discovery_model_message(discovery, working_directory).unwrap_or_default(),
        });
    };
    let servers = &config.servers;
    let names = servers
        .iter()
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();

    if servers.len() > 1 && requested_name.is_none() {
        return Err(LaunchDenial {
            reason: LaunchDenyReason::LaunchConfigNameNotFound,
            message: format!(
                "Multiple server configurations found: {}. Specify which server to start by passing the name parameter (e.g., preview_start with name: \"frontend\" or name: \"backend\"). To start all servers, call preview_start separately for each.",
                quote_join(&names)
            ),
        });
    }

    let Some(requested_name) = requested_name else {
        return servers
            .first()
            .cloned()
            .ok_or_else(|| unknown_name_denial(discovery, &names, ""));
    };

    if let Some(server) = servers
        .iter()
        .find(|server| names_match(&server.name, requested_name))
    {
        return Ok(server.clone());
    }
    if malformed
        .iter()
        .any(|entry| names_match(&entry.name, requested_name))
    {
        let reasons = discovery.malformed_reasons();
        let detail = match reasons
            .iter()
            .find(|(name, _)| names_match(name, requested_name))
        {
            None => " Check its field types (\"runtimeArgs\" must be an array of strings, \"port\" a number).".to_owned(),
            Some((_, reason)) => format!(" {}", sanitize_for_message(reason)),
        };
        return Err(LaunchDenial {
            reason: LaunchDenyReason::LaunchConfigInvalid,
            message: format!(
                "\"{}\" exists in .mework/launch.json but its entry could not be used.{detail} Usable servers: {}.",
                sanitize_for_message(requested_name),
                quote_join(&names)
            ),
        });
    }
    // One usable entry and a clean file: the name was a guess, not a demand.
    if servers.len() == 1 && malformed.is_empty() {
        return Ok(servers[0].clone());
    }
    Err(unknown_name_denial(discovery, &names, requested_name))
}

fn unknown_name_denial(
    discovery: &LaunchDiscovery,
    names: &[String],
    requested_name: &str,
) -> LaunchDenial {
    let requested = sanitize_for_message(requested_name);
    let malformed_names = discovery.malformed_names();
    let note = if malformed_names.is_empty() {
        String::new()
    } else {
        format!(
            " Note: {} also exist(s) in the file but could not be used.{}",
            quote_join(&malformed_names),
            malformed_reason_suffix(&discovery.malformed_reasons())
        )
    };
    LaunchDenial {
        reason: LaunchDenyReason::LaunchConfigNameNotFound,
        message: format!(
            "No server named \"{requested}\" found in .mework/launch.json. Available servers: {}. Pass one of these names, or add a new configuration for \"{requested}\".{note}",
            quote_join(names)
        ),
    }
}

// ---------------------------------------------------------------------------
// Reading and field decoding
// ---------------------------------------------------------------------------

enum ReadOutcome {
    Read(String),
    Missing,
    Unreadable { code: String, message: String },
}

/// Reads the file the way Node does: bytes decoded as UTF-8 with replacement
/// rather than an error, so a stray byte surfaces as a parse problem instead of
/// an I/O one.
fn read_launch_json(path: &Path) -> ReadOutcome {
    match fs::read(path) {
        Ok(bytes) => ReadOutcome::Read(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) if is_missing_error(&error) => ReadOutcome::Missing,
        Err(error) => {
            let code = io_error_code(&error);
            let message = error.to_string();
            let message = message
                .strip_prefix(&format!("{code}: "))
                .unwrap_or(&message)
                .to_owned();
            ReadOutcome::Unreadable { code, message }
        }
    }
}

fn is_missing_error(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::NotFound {
        return true;
    }
    // ENOTDIR — a path component is a file. Windows reports that as NotFound.
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(20)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn io_error_code(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::InvalidInput => "EINVAL",
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::Interrupted => "EINTR",
        _ => "UNKNOWN",
    }
    .to_owned()
}

/// The dialect VS Code's `jsonc` parser accepts with `allowTrailingComma`:
/// comments and trailing commas, but none of the looser JSON5 spellings.
fn launch_json_parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

fn property_name<'a>(name: &'a ObjectPropName<'a>) -> &'a str {
    match name {
        ObjectPropName::String(literal) => literal.value.as_ref(),
        ObjectPropName::Word(literal) => literal.value,
    }
}

/// A present, non-null property. JavaScript treats `null` as "not set" for every
/// field the source reads through a truthiness check.
fn property<'a, 'b>(entry: &'a Object<'b>, key: &str) -> Option<&'a JsonValue<'b>> {
    entry
        .get(key)
        .map(|property| &property.value)
        .filter(|value| !matches!(value, JsonValue::NullKeyword(_)))
}

fn string_field(entry: &Object, key: &str) -> Result<Option<String>, String> {
    match property(entry, key) {
        None => Ok(None),
        Some(JsonValue::StringLit(text)) => Ok(Some(text.value.to_string())),
        Some(_) => Err(format!("\"{key}\" must be a string")),
    }
}

fn string_array_field(entry: &Object, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = property(entry, key) else {
        return Ok(Vec::new());
    };
    let JsonValue::Array(array) = value else {
        return Err(format!("\"{key}\" must be an array of strings"));
    };
    array
        .elements
        .iter()
        .map(|element| match element {
            JsonValue::StringLit(text) => Ok(text.value.to_string()),
            _ => Err(format!("\"{key}\" must be an array of strings")),
        })
        .collect()
}

fn bool_field(entry: &Object, key: &str) -> Result<Option<bool>, String> {
    match property(entry, key) {
        None => Ok(None),
        Some(JsonValue::BooleanLit(literal)) => Ok(Some(literal.value)),
        Some(_) => Err(format!("\"{key}\" must be a boolean")),
    }
}

/// A quoted port is accepted because the source flows the untyped value straight
/// into `server.listen`, where Node coerces it; rejecting it here would refuse a
/// config that works in the app we are copying.
fn port_field(entry: &Object) -> Result<Option<u32>, String> {
    let Some(value) = property(entry, "port") else {
        return Ok(None);
    };
    let literal = match value {
        JsonValue::NumberLit(number) => number.value.as_ref(),
        JsonValue::StringLit(text) => text.value.as_ref(),
        _ => return Err("\"port\" must be a number".to_owned()),
    };
    let port = literal
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|port| port.fract() == 0.0 && *port >= 0.0 && *port <= 65535.0)
        .ok_or_else(|| "\"port\" must be an integer between 0 and 65535".to_owned())?;
    Ok(Some(port as u32))
}

fn env_field(entry: &Object) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = property(entry, "env") else {
        return Ok(BTreeMap::new());
    };
    let JsonValue::Object(object) = value else {
        return Err("\"env\" must be an object of strings".to_owned());
    };
    object
        .properties
        .iter()
        .map(|property| match &property.value {
            JsonValue::StringLit(text) => Ok((
                property_name(&property.name).to_owned(),
                text.value.to_string(),
            )),
            _ => Err("\"env\" must be an object of strings".to_owned()),
        })
        .collect()
}

/// `url` is validated on key presence, so `"url": null` fails the string rule
/// rather than reading as "no url".
fn url_field_value(value: &JsonValue) -> Result<Url, String> {
    match value {
        JsonValue::StringLit(text) => validate_url(&text.value),
        _ => Err("\"url\" must be a non-empty string".to_owned()),
    }
}

fn json_quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery(source: &str) -> LaunchDiscovery {
        LaunchConfigDiscovery::new("/project").parse_source(source, None)
    }

    fn servers(source: &str) -> Vec<ServerConfig> {
        match discovery(source) {
            LaunchDiscovery::Ok { config, .. } => config.servers,
            other => panic!("expected a usable file, got {other:?}"),
        }
    }

    fn only_server(source: &str) -> ServerConfig {
        let mut servers = servers(source);
        assert_eq!(servers.len(), 1, "expected exactly one usable entry");
        servers.remove(0)
    }

    fn entry_server(entry: &str) -> ServerConfig {
        only_server(&format!(r#"{{"configurations":[{entry}]}}"#))
    }

    fn malformed_reason(source: &str) -> String {
        let discovery = discovery(source);
        let malformed = discovery.malformed();
        assert_eq!(malformed.len(), 1, "expected one malformed entry");
        malformed[0].reason.clone()
    }

    fn config_path() -> String {
        launch_json_path(Path::new("/project"))
            .display()
            .to_string()
    }

    #[test]
    fn runtime_executable_form_builds_the_command_and_defaults_the_port() {
        let server =
            entry_server(r#"{"name":"web","runtimeExecutable":"npm","runtimeArgs":["run","dev"]}"#);

        assert_eq!(server.name, "web");
        assert_eq!(server.command.as_deref(), Some("npm"));
        assert_eq!(server.args, vec!["run".to_owned(), "dev".to_owned()]);
        assert_eq!(server.port, 3000);
        assert_eq!(server.url, None);
        assert_eq!(server.auto_port, None);
        assert!(server.env.is_empty());
    }

    #[test]
    fn runtime_executable_appends_program_then_args() {
        let server = entry_server(
            r#"{"name":"web","runtimeExecutable":"node","runtimeArgs":["--inspect"],"program":"server.js","args":["--quiet"]}"#,
        );

        assert_eq!(server.command.as_deref(), Some("node"));
        assert_eq!(server.args, vec!["--inspect", "server.js", "--quiet"]);
    }

    #[test]
    fn program_alone_runs_under_node() {
        let server = entry_server(r#"{"program":"server.js","args":["--quiet"]}"#);

        assert_eq!(server.command.as_deref(), Some("node"));
        assert_eq!(server.args, vec!["server.js", "--quiet"]);
        assert_eq!(server.port, 3000);
    }

    #[test]
    fn attach_form_needs_a_url_and_no_command_or_type() {
        let server = entry_server(r#"{"url":"https://example.com/docs"}"#);

        assert_eq!(server.command, None);
        assert!(server.args.is_empty());
        assert_eq!(server.url.as_deref(), Some("https://example.com/docs"));
        assert_eq!(server.port, 0);
        assert_eq!(server.name, "example.com");
    }

    #[test]
    fn vs_code_command_and_node_terminal_entries_are_dropped_not_malformed() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"vscode","type":"node-terminal","command":"npm run dev"}]}"#,
        );

        assert_eq!(
            discovery,
            LaunchDiscovery::NoValidConfigs {
                configuration_count: 1,
                malformed: Vec::new()
            }
        );
    }

    #[test]
    fn a_type_field_also_drops_an_otherwise_valid_attach_entry() {
        assert_eq!(
            discovery(
                r#"{"configurations":[{"name":"a","type":"chrome","url":"https://example.com/"}]}"#
            ),
            LaunchDiscovery::NoValidConfigs {
                configuration_count: 1,
                malformed: Vec::new()
            }
        );
    }

    #[test]
    fn a_command_field_does_not_drop_an_entry_that_already_has_a_command() {
        let server = entry_server(
            r#"{"name":"web","runtimeExecutable":"npm","command":"ignored","type":"node-terminal"}"#,
        );

        assert_eq!(server.command.as_deref(), Some("npm"));
    }

    #[test]
    fn comments_and_trailing_commas_parse() {
        let server = only_server(
            "{\n\
              // the dev server\n\
              \"version\": \"0.0.1\",\n\
              \"configurations\": [\n\
                {\n\
                  \"name\": \"web\", /* inline */\n\
                  \"runtimeExecutable\": \"npm\",\n\
                  \"runtimeArgs\": [\"run\", \"dev\",],\n\
                },\n\
              ],\n\
            }",
        );

        assert_eq!(server.name, "web");
        assert_eq!(server.args, vec!["run", "dev"]);
    }

    #[test]
    fn an_unparseable_file_reports_the_parser_detail_with_an_offset() {
        let LaunchDiscovery::ParseError { detail } = discovery("{\"configurations\": [") else {
            panic!("expected a parse error");
        };

        assert!(detail.contains("at offset"), "{detail}");
    }

    #[test]
    fn an_empty_or_non_object_root_is_a_parse_error() {
        for source in ["", "   ", "[]", "42", "\"text\"", "null"] {
            assert_eq!(
                discovery(source),
                LaunchDiscovery::ParseError {
                    detail: "file is not a JSON object".to_owned()
                },
                "{source:?}"
            );
        }
    }

    #[test]
    fn configurations_must_be_an_array() {
        for source in [r#"{"configurations":{}}"#, r#"{"configurations":null}"#] {
            assert_eq!(
                discovery(source),
                LaunchDiscovery::ParseError {
                    detail: "\"configurations\" must be an array".to_owned()
                },
                "{source}"
            );
        }
    }

    #[test]
    fn a_missing_configurations_key_reports_no_configurations() {
        assert_eq!(
            discovery(r#"{"version":"0.0.1"}"#),
            LaunchDiscovery::NoValidConfigs {
                configuration_count: 0,
                malformed: Vec::new()
            }
        );
    }

    #[test]
    fn auto_verify_defaults_to_true_and_only_literal_false_disables_it() {
        let entry = r#""configurations":[{"runtimeExecutable":"npm"}]"#;
        let auto_verify = |prefix: &str| match discovery(&format!("{{{prefix}{entry}}}")) {
            LaunchDiscovery::Ok { config, .. } => config.auto_verify,
            other => panic!("expected a usable file, got {other:?}"),
        };

        assert!(auto_verify(""));
        assert!(auto_verify("\"autoVerify\":true,"));
        assert!(auto_verify("\"autoVerify\":\"no\","));
        assert!(auto_verify("\"autoVerify\":null,"));
        assert!(!auto_verify("\"autoVerify\":false,"));
    }

    #[test]
    fn port_resolution_prefers_the_explicit_port() {
        let server = entry_server(
            r#"{"port":8080,"url":"http://localhost:8080","runtimeExecutable":"npm","runtimeArgs":["--port 1234"],"env":{"PORT":"4321"}}"#,
        );

        assert_eq!(server.port, 8080);
    }

    #[test]
    fn a_quoted_port_is_accepted_like_the_source_does() {
        let server = entry_server(r#"{"runtimeExecutable":"npm","port":"3000"}"#);

        assert_eq!(server.port, 3000);
    }

    #[test]
    fn port_resolution_falls_to_the_localhost_url_then_the_command_then_the_default() {
        assert_eq!(
            entry_server(
                r#"{"url":"http://localhost:8080","runtimeExecutable":"npm","runtimeArgs":["--port 1234"]}"#
            )
            .port,
            8080
        );
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"npm","runtimeArgs":["--port 1234"]}"#).port,
            1234
        );
        assert_eq!(entry_server(r#"{"runtimeExecutable":"npm"}"#).port, 3000);
    }

    #[test]
    fn an_explicit_zero_port_is_treated_as_absent() {
        assert_eq!(
            entry_server(r#"{"port":0,"runtimeExecutable":"npm"}"#).port,
            3000
        );
    }

    #[test]
    fn a_non_localhost_url_does_not_supply_the_port() {
        assert_eq!(
            entry_server(r#"{"url":"https://example.com:8443/docs"}"#).port,
            0
        );
    }

    #[test]
    fn env_port_beats_the_command_text() {
        assert_eq!(
            entry_server(
                r#"{"runtimeExecutable":"npm","runtimeArgs":["--port 1234"],"env":{"PORT":"4321"}}"#
            )
            .port,
            4321
        );
    }

    #[test]
    fn command_text_ports_are_read_flag_then_localhost_then_bare() {
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"serve","runtimeArgs":["-p 5010"]}"#).port,
            5010
        );
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"serve","runtimeArgs":["--PORT 5011"]}"#).port,
            5011
        );
        assert_eq!(
            entry_server(
                r#"{"runtimeExecutable":"wait-on","runtimeArgs":["http://localhost:5012"]}"#
            )
            .port,
            5012
        );
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"serve","runtimeArgs":["--bind",":5013"]}"#).port,
            5013
        );
        // Each pattern sweeps every candidate before the next one runs, so the
        // flag wins even though the localhost URL comes first.
        assert_eq!(
            entry_server(
                r#"{"runtimeExecutable":"serve","runtimeArgs":["http://localhost:5014","--port 5015"]}"#
            )
            .port,
            5015
        );
    }

    #[test]
    fn a_port_split_across_two_arguments_is_not_recognised() {
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"serve","runtimeArgs":["--port","5010"]}"#).port,
            3000
        );
    }

    #[test]
    fn an_unparseable_env_port_falls_through() {
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"npm","env":{"PORT":"not-a-port"}}"#).port,
            3000
        );
    }

    #[test]
    fn a_localhost_url_must_match_the_entry_port() {
        assert_eq!(
            malformed_reason(
                r#"{"configurations":[{"name":"web","port":3000,"url":"http://localhost:8080","runtimeExecutable":"npm"}]}"#
            ),
            "\"url\" is \"http://localhost:8080/\" but this entry's server runs on port 3000 \u{2014} a localhost \"url\" must point at the entry's own server"
        );
    }

    #[test]
    fn url_must_be_a_non_empty_string() {
        assert_eq!(
            validate_url("   ").unwrap_err(),
            "\"url\" must be a non-empty string"
        );
        for source in [
            r#"{"configurations":[{"name":"a","url":null}]}"#,
            r#"{"configurations":[{"name":"a","url":123}]}"#,
            r#"{"configurations":[{"name":"a","url":""}]}"#,
        ] {
            assert_eq!(
                malformed_reason(source),
                "\"url\" must be a non-empty string",
                "{source}"
            );
        }
    }

    #[test]
    fn url_must_parse() {
        assert_eq!(
            validate_url("not a url").unwrap_err(),
            "\"url\" is not a valid URL"
        );
    }

    #[test]
    fn url_must_be_http_or_https() {
        for value in ["file:///etc/passwd", "ws://localhost:3000/"] {
            assert_eq!(
                validate_url(value).unwrap_err(),
                "\"url\" must be http or https",
                "{value}"
            );
        }
    }

    #[test]
    fn url_must_not_embed_credentials() {
        for value in [
            "http://user:secret@example.com/",
            "http://user@example.com/",
        ] {
            assert_eq!(
                validate_url(value).unwrap_err(),
                "\"url\" must not embed credentials",
                "{value}"
            );
        }
    }

    #[test]
    fn url_rejects_non_canonical_loopback_spellings() {
        assert_eq!(
            validate_url("http://localhost./").unwrap_err(),
            "\"url\" host \"localhost.\" is a non-canonical loopback spelling \u{2014} write the canonical form (\"localhost\", a \"*.localhost\" name, \"127.0.0.1\", or \"[::1]\") so the localhost rules apply"
        );
        for value in ["http://127.0.0.2/", "http://[::ffff:127.0.0.1]/"] {
            assert!(
                validate_url(value)
                    .unwrap_err()
                    .contains("non-canonical loopback spelling"),
                "{value}"
            );
        }
        for canonical in [
            "http://localhost/",
            "http://app.localhost/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://0.0.0.0/",
        ] {
            assert!(validate_url(canonical).is_ok(), "{canonical}");
        }
    }

    #[test]
    fn a_localhost_url_must_be_a_bare_origin() {
        assert_eq!(
            validate_url("http://localhost:3000/admin?wipe=1").unwrap_err(),
            "\"url\" is \"http://localhost:3000/admin?wipe=1\", a localhost address with a path or query. For security, a localhost \"url\" must be just the server's origin \u{2014} use \"http://localhost:3000/\" and ask Claude to navigate to the specific page once the preview is open. (A config file must not point the preview at arbitrary local endpoints; other local services could be affected.)"
        );
        assert!(validate_url("http://localhost:3000/#top").is_err());
        assert!(validate_url("http://localhost:3000/").is_ok());
        assert!(validate_url("http://localhost:3000").is_ok());
        // Non-localhost URLs may carry a path.
        assert!(validate_url("https://example.com/docs?x=1#top").is_ok());
    }

    #[test]
    fn localhost_predicates_cover_the_documented_spellings() {
        for host in [
            "localhost",
            "app.localhost",
            "127.0.0.1",
            "0.0.0.0",
            "::1",
            "[::1]",
        ] {
            assert!(is_localhost_host(host), "{host}");
        }
        assert!(!is_localhost_host("localhosts"));
        assert!(!is_localhost_host("example.com"));
        assert!(is_localhost_url("http://localhost:3000/anything"));
        assert!(!is_localhost_url("https://example.com/"));
        assert!(is_bare_localhost_origin("http://localhost:3000"));
        assert!(!is_bare_localhost_origin("http://localhost:3000/admin"));
        assert!(!is_bare_localhost_origin("https://example.com/"));
    }

    #[test]
    fn workspace_variables_are_substituted_in_command_args_and_cwd() {
        let server = entry_server(
            r#"{"name":"web","runtimeExecutable":"${workspaceFolder}/bin/serve","runtimeArgs":["--root","${workspaceRoot}/public"],"cwd":"${workspaceFolder}/app"}"#,
        );

        assert_eq!(server.command.as_deref(), Some("/project/bin/serve"));
        assert_eq!(server.args, vec!["--root", "/project/public"]);
        assert_eq!(server.cwd, PathBuf::from("/project/app"));
    }

    #[test]
    fn a_relative_cwd_is_joined_to_the_working_directory() {
        assert_eq!(
            entry_server(r#"{"name":"web","runtimeExecutable":"npm","cwd":"packages/app"}"#).cwd,
            Path::new("/project").join("packages/app")
        );
    }

    #[test]
    fn an_absent_cwd_is_the_working_directory() {
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"npm"}"#).cwd,
            PathBuf::from("/project")
        );
    }

    #[test]
    fn a_missing_name_is_derived_from_the_last_argument_or_the_url_host() {
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"npm","runtimeArgs":["run","dev"]}"#).name,
            "dev"
        );
        assert_eq!(entry_server(r#"{"program":"server.js"}"#).name, "server");
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"npm","runtimeArgs":["a:b:c"]}"#).name,
            "a-b-c"
        );
        // A one-character label is not worth showing.
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"npm","runtimeArgs":["x.js"]}"#).name,
            "preview"
        );
        assert_eq!(
            entry_server(r#"{"runtimeExecutable":"npm"}"#).name,
            "preview"
        );
        assert_eq!(
            entry_server(r#"{"url":"http://localhost:3000"}"#).name,
            "localhost:3000"
        );
    }

    #[test]
    fn malformed_fields_are_reported_per_entry() {
        let cases = [
            (
                r#"{"name":"a","runtimeArgs":"run dev"}"#,
                "\"runtimeArgs\" must be an array of strings",
            ),
            (
                r#"{"name":"a","runtimeArgs":[1]}"#,
                "\"runtimeArgs\" must be an array of strings",
            ),
            (
                r#"{"name":"a","runtimeExecutable":7}"#,
                "\"runtimeExecutable\" must be a string",
            ),
            (
                r#"{"name":"a","program":7}"#,
                "\"program\" must be a string",
            ),
            (
                r#"{"name":"a","runtimeExecutable":"npm","args":{}}"#,
                "\"args\" must be an array of strings",
            ),
            (
                r#"{"name":"a","runtimeExecutable":"npm","port":true}"#,
                "\"port\" must be a number",
            ),
            (
                r#"{"name":"a","runtimeExecutable":"npm","port":"http"}"#,
                "\"port\" must be an integer between 0 and 65535",
            ),
            (
                r#"{"name":"a","runtimeExecutable":"npm","port":70000}"#,
                "\"port\" must be an integer between 0 and 65535",
            ),
            (
                r#"{"name":"a","runtimeExecutable":"npm","autoPort":"yes"}"#,
                "\"autoPort\" must be a boolean",
            ),
            (
                r#"{"name":"a","runtimeExecutable":"npm","env":{"PORT":3000}}"#,
                "\"env\" must be an object of strings",
            ),
            (
                r#"{"name":"a","runtimeExecutable":"npm","cwd":5}"#,
                "\"cwd\" must be a string",
            ),
            (
                r#"{"name":5,"runtimeExecutable":"npm"}"#,
                "\"name\" must be a string",
            ),
        ];

        for (entry, expected) in cases {
            assert_eq!(
                malformed_reason(&format!(r#"{{"configurations":[{entry}]}}"#)),
                expected,
                "{entry}"
            );
        }
    }

    #[test]
    fn a_malformed_entry_without_a_string_name_is_reported_as_unnamed() {
        let discovery =
            discovery(r#"{"configurations":[{"runtimeExecutable":"npm","port":true}]}"#);

        assert_eq!(
            discovery.malformed_names(),
            vec!["(unnamed entry)".to_owned()]
        );
    }

    #[test]
    fn duplicate_malformed_names_collapse_in_reasons_but_not_in_the_name_list() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"a","runtimeExecutable":"npm","port":true},{"name":"a","runtimeExecutable":"npm","autoPort":"y"}]}"#,
        );

        assert_eq!(
            discovery.malformed_names(),
            vec!["a".to_owned(), "a".to_owned()]
        );
        assert_eq!(
            discovery.malformed_reasons(),
            vec![("a".to_owned(), "\"autoPort\" must be a boolean".to_owned())]
        );
    }

    #[test]
    fn nonobject_entries_are_skipped_but_still_counted() {
        assert_eq!(
            discovery(r#"{"configurations":[1,"two",null,[]]}"#),
            LaunchDiscovery::NoValidConfigs {
                configuration_count: 4,
                malformed: Vec::new()
            }
        );
    }

    #[test]
    fn a_requested_name_matches_case_insensitively_or_reports_what_exists() {
        let source = r#"{"configurations":[{"name":"Frontend","runtimeExecutable":"npm"},{"name":"backend","runtimeExecutable":"npm"}]}"#;
        let reader = LaunchConfigDiscovery::new("/project");

        let matched = reader.parse_source(source, Some("FRONTEND"));
        assert_eq!(matched.config().unwrap().servers.len(), 1);
        assert_eq!(matched.config().unwrap().servers[0].name, "Frontend");

        assert_eq!(
            reader.parse_source(source, Some("nope")),
            LaunchDiscovery::NameNotFound {
                available: vec!["Frontend".to_owned(), "backend".to_owned()]
            }
        );
    }

    #[test]
    fn discovery_reads_the_file_and_reports_a_missing_one() {
        let root = tempfile::tempdir().unwrap();
        let reader = LaunchConfigDiscovery::new(root.path());
        assert_eq!(reader.discover(None), LaunchDiscovery::NotFound);
        assert_eq!(reader.launch_json_path(), launch_json_path(root.path()));
        assert_eq!(reader.working_directory(), root.path());

        fs::create_dir_all(root.path().join(".mework")).unwrap();
        fs::write(
            reader.launch_json_path(),
            r#"{"configurations":[{"name":"web","runtimeExecutable":"npm","runtimeArgs":["run","dev"],"port":5173}]}"#,
        )
        .unwrap();

        let found = reader.discover(None);
        assert_eq!(found.config().unwrap().servers[0].port, 5173);
        assert_eq!(found.config().unwrap().servers[0].cwd, root.path());
    }

    #[test]
    fn launch_json_format_constants_are_verbatim() {
        assert_eq!(
            LAUNCH_JSON_FORMAT,
            "{\n  \"version\": \"0.0.1\",\n  \"configurations\": [\n    {\n      \"name\": \"<unique-name>\",\n      \"runtimeExecutable\": \"<command>\",\n      \"runtimeArgs\": [\"<args>\"],\n      \"port\": <port>\n    }\n  ]\n}"
        );
        assert!(LAUNCH_JSON_FORMAT_NOTES.starts_with(
            "Set \"runtimeExecutable\" to the command (e.g. \"npm\"), \"runtimeArgs\" to the arguments (e.g. [\"run\", \"dev\"]), and \"port\" to the server port."
        ));
        assert!(LAUNCH_JSON_FORMAT_NOTES
            .ends_with("Only include servers you actually need to preview."));
        assert_eq!(LAUNCH_JSON_FORMAT_NOTES.matches('\u{2014}').count(), 2);
        // Upstream is 703 UTF-16 code units; two em dashes make 707 UTF-8 bytes.
        assert_eq!(LAUNCH_JSON_FORMAT_NOTES.len(), 707);
        assert_eq!(LAUNCH_JSON_FORMAT.len(), 188);
    }

    #[test]
    fn missing_launch_json_message_is_verbatim() {
        let expected = format!(
            "No .mework/launch.json found. Create {} with this format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES} Then call preview_start with the server name.",
            config_path()
        );

        assert_eq!(missing_launch_json_message(Path::new("/project")), expected);
        assert_eq!(
            discovery_model_message(&LaunchDiscovery::NotFound, Path::new("/project")).unwrap(),
            expected
        );
        assert_eq!(
            discovery_log_summary(&LaunchDiscovery::NotFound).unwrap(),
            "no .mework/launch.json"
        );
    }

    #[test]
    fn unreadable_message_is_verbatim() {
        let discovery = LaunchDiscovery::Unreadable {
            code: "EACCES".to_owned(),
            message: "permission denied".to_owned(),
        };

        assert_eq!(
            discovery_model_message(&discovery, Path::new("/project")).unwrap(),
            format!(
                "Found {} but reading it failed with EACCES: permission denied. The path exists \u{2014} do not recreate it.",
                config_path()
            )
        );
        assert_eq!(
            discovery_log_summary(&discovery).unwrap(),
            "launch.json could not be read (EACCES): permission denied"
        );
        assert_eq!(
            discovery_deny_reason(&discovery),
            Some(LaunchDenyReason::LaunchConfigUnreadable)
        );
    }

    #[test]
    fn parse_error_message_is_verbatim() {
        let discovery = LaunchDiscovery::ParseError {
            detail: "Expected comma at offset 42".to_owned(),
        };

        assert_eq!(
            discovery_model_message(&discovery, Path::new("/project")).unwrap(),
            format!(
                "Found {} but it could not be parsed: Expected comma at offset 42. Fix the file to match this format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES}",
                config_path()
            )
        );
        assert_eq!(
            discovery_log_summary(&discovery).unwrap(),
            "launch.json failed to parse: Expected comma at offset 42"
        );
        assert_eq!(
            discovery_deny_reason(&discovery),
            Some(LaunchDenyReason::LaunchConfigInvalid)
        );
    }

    #[test]
    fn empty_configurations_message_is_verbatim() {
        let discovery = LaunchDiscovery::NoValidConfigs {
            configuration_count: 0,
            malformed: Vec::new(),
        };

        assert_eq!(
            discovery_model_message(&discovery, Path::new("/project")).unwrap(),
            format!(
                "Found {} but it contains no configurations. Expected format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES}",
                config_path()
            )
        );
        assert_eq!(
            discovery_log_summary(&discovery).unwrap(),
            "launch.json contains no configurations"
        );
    }

    #[test]
    fn unusable_configurations_message_names_both_causes() {
        let discovery = LaunchDiscovery::NoValidConfigs {
            configuration_count: 2,
            malformed: vec![MalformedEntry {
                name: "api".to_owned(),
                reason: "\"port\" must be a number".to_owned(),
            }],
        };

        assert_eq!(
            discovery_model_message(&discovery, Path::new("/project")).unwrap(),
            format!(
                "Found {} with 2 configurations, but none could be used. 1 of them has a supported shape but a malformed field \u{2014} api: 'port' must be a number\u{2026}. Supported configurations need \"runtimeExecutable\" (optionally with \"runtimeArgs\"), \"program\", or a \"url\" to attach to an already-running server \u{2014} VS Code \"command\" / \"node-terminal\" style entries are not supported. Expected format:\n{LAUNCH_JSON_FORMAT}\n{LAUNCH_JSON_FORMAT_NOTES}",
                config_path()
            )
        );
        assert_eq!(
            discovery_log_summary(&discovery).unwrap(),
            "none of the 2 configuration(s) in launch.json could be used (1 with a malformed field; the rest lack \"runtimeExecutable\" or \"program\")"
        );
    }

    #[test]
    fn a_single_unusable_configuration_uses_the_singular_forms() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"api","runtimeExecutable":"npm","port":true}]}"#,
        );

        let message = discovery_model_message(&discovery, Path::new("/project")).unwrap();
        assert!(
            message.starts_with(&format!(
                "Found {} with 1 configuration, but none could be used. 1 of them has a supported shape but a malformed field \u{2014} api: 'port' must be a number\u{2026}. Expected format:",
                config_path()
            )),
            "{message}"
        );
    }

    #[test]
    fn name_not_found_message_is_verbatim() {
        let discovery = LaunchDiscovery::NameNotFound {
            available: vec!["web".to_owned(), "api".to_owned()],
        };

        assert_eq!(
            discovery_model_message(&discovery, Path::new("/project")).unwrap(),
            format!(
                "No matching server in {}. Available servers: web, api.",
                config_path()
            )
        );
        assert_eq!(
            discovery_log_summary(&discovery).unwrap(),
            "no configuration with the requested name; available: web, api"
        );
        // A missing file and an unmatched name are ordinary; neither is warned about.
        assert_eq!(discovery_warning(&discovery), None);
        assert_eq!(discovery_warning(&LaunchDiscovery::NotFound), None);
    }

    #[test]
    fn a_usable_file_with_malformed_entries_warns_but_does_not_deny() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"web","runtimeExecutable":"npm"},{"name":"api","runtimeExecutable":"npm","port":true}]}"#,
        );

        assert_eq!(
            discovery_warning(&discovery).unwrap(),
            "skipped malformed launch.json entry: api"
        );
        assert_eq!(
            discovery_model_message(&discovery, Path::new("/project")),
            None
        );
        assert_eq!(discovery_deny_reason(&discovery), None);
        assert_eq!(discovery_log_summary(&discovery), None);
    }

    #[test]
    fn two_malformed_entries_warn_in_the_plural() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"web","runtimeExecutable":"npm"},{"name":"api","runtimeExecutable":"npm","port":true},{"name":"docs","runtimeExecutable":"npm","port":"y"}]}"#,
        );

        assert_eq!(
            discovery_warning(&discovery).unwrap(),
            "skipped malformed launch.json entries: api, docs"
        );
    }

    #[test]
    fn several_servers_without_a_name_are_denied_verbatim() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"frontend","runtimeExecutable":"npm"},{"name":"backend","runtimeExecutable":"npm"}]}"#,
        );

        let denial = select_server(&discovery, Path::new("/project"), None).unwrap_err();

        assert_eq!(denial.reason.as_str(), "launch_config_name_not_found");
        assert_eq!(
            denial.message,
            "Multiple server configurations found: \"frontend\", \"backend\". Specify which server to start by passing the name parameter (e.g., preview_start with name: \"frontend\" or name: \"backend\"). To start all servers, call preview_start separately for each."
        );
    }

    #[test]
    fn a_named_malformed_entry_is_denied_with_its_own_reason() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"web","runtimeExecutable":"npm"},{"name":"api","runtimeExecutable":"npm","port":true}]}"#,
        );

        let denial = select_server(&discovery, Path::new("/project"), Some("api")).unwrap_err();

        assert_eq!(denial.reason.as_str(), "launch_config_invalid");
        assert_eq!(
            denial.message,
            "\"api\" exists in .mework/launch.json but its entry could not be used. 'port' must be a number\u{2026} Usable servers: \"web\"."
        );
    }

    #[test]
    fn an_unknown_name_is_denied_and_lists_the_malformed_entries_too() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"web","runtimeExecutable":"npm"},{"name":"api","runtimeExecutable":"npm","port":true}]}"#,
        );

        let denial = select_server(&discovery, Path::new("/project"), Some("docs")).unwrap_err();

        assert_eq!(denial.reason.as_str(), "launch_config_name_not_found");
        assert_eq!(
            denial.message,
            "No server named \"docs\" found in .mework/launch.json. Available servers: \"web\". Pass one of these names, or add a new configuration for \"docs\". Note: \"api\" also exist(s) in the file but could not be used. \u{2014} api: 'port' must be a number\u{2026}"
        );
    }

    #[test]
    fn an_unknown_name_in_a_clean_multi_server_file_omits_the_malformed_note() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"web","runtimeExecutable":"npm"},{"name":"api","runtimeExecutable":"npm"}]}"#,
        );

        let denial = select_server(&discovery, Path::new("/project"), Some("docs")).unwrap_err();

        assert_eq!(
            denial.message,
            "No server named \"docs\" found in .mework/launch.json. Available servers: \"web\", \"api\". Pass one of these names, or add a new configuration for \"docs\"."
        );
    }

    #[test]
    fn one_clean_config_absorbs_an_unmatched_name() {
        let discovery =
            discovery(r#"{"configurations":[{"name":"web","runtimeExecutable":"npm"}]}"#);

        assert_eq!(
            select_server(&discovery, Path::new("/project"), Some("anything"))
                .unwrap()
                .name,
            "web"
        );
        assert_eq!(
            select_server(&discovery, Path::new("/project"), None)
                .unwrap()
                .name,
            "web"
        );
    }

    #[test]
    fn a_single_config_stops_absorbing_names_once_the_file_has_a_malformed_entry() {
        let discovery = discovery(
            r#"{"configurations":[{"name":"web","runtimeExecutable":"npm"},{"name":"api","runtimeExecutable":"npm","port":true}]}"#,
        );

        assert!(select_server(&discovery, Path::new("/project"), Some("anything")).is_err());
    }

    #[test]
    fn an_unusable_file_denies_with_the_mapped_reason_and_message() {
        let denial =
            select_server(&LaunchDiscovery::NotFound, Path::new("/project"), None).unwrap_err();

        assert_eq!(denial.reason.as_str(), "launch_config_missing");
        assert_eq!(
            denial.message,
            missing_launch_json_message(Path::new("/project"))
        );
    }

    #[test]
    fn sanitize_leaves_a_clean_string_alone_and_marks_every_change() {
        assert_eq!(sanitize_for_message("frontend"), "frontend");
        assert_eq!(sanitize_for_message(""), "");
        assert_eq!(sanitize_for_message("say \"hi\""), "say 'hi'\u{2026}");
        assert_eq!(sanitize_for_message("first\nsecond"), "first\u{2026}");
        assert_eq!(sanitize_for_message("bell\u{0007}"), "bell\u{fffd}\u{2026}");
        assert_eq!(
            sanitize_for_message("\u{201c}quoted\u{201d}"),
            "'quoted'\u{2026}"
        );
        let long = "a".repeat(200);
        assert_eq!(
            sanitize_for_message(&long),
            format!("{}\u{2026}", "a".repeat(SANITIZE_LIMIT))
        );
    }

    #[test]
    fn names_match_ignores_case_and_accepts_the_sanitized_spelling() {
        assert!(names_match("Frontend", "frontend"));
        assert!(names_match("say \"hi\"", "say 'hi'\u{2026}"));
        assert!(!names_match("frontend", "backend"));
    }

    #[test]
    fn quote_join_quotes_and_sanitizes() {
        assert_eq!(
            quote_join(&["web".to_owned(), "a\nb".to_owned()]),
            "\"web\", \"a\u{2026}\""
        );
        assert_eq!(quote_join(&[]), "");
    }

    #[test]
    fn substitute_variables_replaces_both_spellings_everywhere() {
        assert_eq!(
            substitute_variables(
                "${workspaceFolder}/a:${workspaceRoot}/b:${workspaceFolder}",
                Path::new("/project")
            ),
            "/project/a:/project/b:/project"
        );
        assert_eq!(
            substitute_variables("nothing to do", Path::new("/project")),
            "nothing to do"
        );
    }

    #[test]
    fn set_auto_verify_source_flips_an_existing_flag_and_keeps_the_comments() {
        let source =
            "{\n  // the dev server\n  \"autoVerify\": true,\n  \"configurations\": []\n}\n";

        let updated = set_auto_verify_source(source, false).unwrap();

        assert_eq!(
            updated,
            "{\n  // the dev server\n  \"autoVerify\": false,\n  \"configurations\": []\n}\n"
        );
        assert_eq!(set_auto_verify_source(&updated, true).unwrap(), source);
    }

    #[test]
    fn set_auto_verify_source_adds_a_missing_flag_without_disturbing_the_rest() {
        let source = "{\n  \"version\": \"0.0.1\",\n  \"configurations\": []\n}\n";

        let updated = set_auto_verify_source(source, false).unwrap();

        assert_eq!(
            updated,
            "{\n  \"autoVerify\": false,\n  \"version\": \"0.0.1\",\n  \"configurations\": []\n}\n"
        );
        assert!(matches!(
            LaunchConfigDiscovery::new("/project").parse_source(&updated, None),
            LaunchDiscovery::NoValidConfigs { .. }
        ));
    }

    #[test]
    fn set_auto_verify_source_writes_an_empty_object_without_a_trailing_comma() {
        let updated = set_auto_verify_source("{}", true).unwrap();

        assert_eq!(updated, "{\n  \"autoVerify\": true\n}");
        assert!(!updated.contains(",}"));
    }

    #[test]
    fn set_auto_verify_source_refuses_anything_that_is_not_an_object() {
        assert_eq!(set_auto_verify_source("", true), None);
        assert_eq!(set_auto_verify_source("[]", true), None);
        assert_eq!(set_auto_verify_source("{\"a\":", true), None);
    }
}
