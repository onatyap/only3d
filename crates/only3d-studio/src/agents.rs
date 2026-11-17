//! Connecting agents. only3d is an MCP server, so any agent app that speaks MCP can drive it,
//! whatever model runs inside: this registers it with the common ones, gives the entry to paste
//! into any other, and installs a skill for agents that work through a shell instead (the skill
//! calls `only3d call <tool>`, which works on the open window).
//!
//! Used by `only3d mcp install <app>` and the window's Connect an Agent panel. Config files are
//! merged, never replaced: other entries stay, the old file is kept as `.bak`, and a file that
//! can't be read as plain JSON is left alone (the error says what to paste instead).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// An app people run agents in.
pub struct AgentApp {
    /// `only3d mcp install <id>`.
    pub id: &'static str,
    pub name: &'static str,
    /// Who makes it and which models it runs.
    pub models: &'static str,
}

pub const APPS: &[AgentApp] = &[
    AgentApp { id: "claude-code", name: "Claude Code", models: "Anthropic · Claude, or DeepSeek, Kimi and others through their Anthropic-compatible APIs" },
    AgentApp { id: "claude-desktop", name: "Claude Desktop", models: "Anthropic · Claude" },
    AgentApp { id: "codex", name: "Codex", models: "OpenAI · GPT models, in the Codex app and the codex CLI" },
    AgentApp { id: "gemini", name: "Gemini CLI", models: "Google · Gemini" },
    AgentApp { id: "antigravity", name: "Antigravity", models: "Google · Gemini and other models" },
    AgentApp { id: "opencode", name: "opencode", models: "Open source · any provider: Anthropic, OpenAI, Google, DeepSeek, Kimi, local models" },
    AgentApp { id: "kimi", name: "Kimi Code", models: "Moonshot AI · Kimi" },
    AgentApp { id: "cursor", name: "Cursor", models: "Anysphere · models from several providers" },
    AgentApp { id: "skill", name: "Agent skill", models: "Agents that read Agent Skills and run shell commands; no MCP needed" },
    AgentApp { id: "other", name: "Other MCP app", models: "Any other MCP client: paste the server entry" },
];

pub fn app(id: &str) -> Option<&'static AgentApp> {
    APPS.iter().find(|a| a.id == id)
}

/// Where each app keeps its settings (from the environment, or under a test folder).
#[derive(Clone, Debug)]
pub struct Places {
    pub home: PathBuf,
    /// ~/Library/Application Support on macOS.
    pub app_support: PathBuf,
    /// $XDG_CONFIG_HOME or ~/.config.
    pub xdg_config: PathBuf,
    /// $CODEX_HOME or ~/.codex.
    pub codex: PathBuf,
    /// $KIMI_CODE_HOME or ~/.kimi-code.
    pub kimi: PathBuf,
    /// Look for command-line apps (`claude`) on PATH too.
    pub search_path: bool,
}

impl Places {
    pub fn from_env() -> Places {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = std::env::home_dir().unwrap_or_default();
        let app_support = if cfg!(target_os = "macos") {
            home.join("Library").join("Application Support")
        } else if cfg!(windows) {
            var("APPDATA").unwrap_or_else(|| home.clone())
        } else {
            var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"))
        };
        Places {
            xdg_config: var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            codex: var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            kimi: var("KIMI_CODE_HOME").unwrap_or_else(|| home.join(".kimi-code")),
            app_support,
            home,
            search_path: true,
        }
    }

    /// Everything under one folder (tests).
    pub fn under(home: &Path) -> Places {
        Places { home: home.to_path_buf(), app_support: home.join("Library/Application Support"), xdg_config: home.join(".config"), codex: home.join(".codex"), kimi: home.join(".kimi-code"), search_path: false }
    }

    /// The settings file only3d writes for `id`.
    pub fn file(&self, id: &str) -> Option<PathBuf> {
        Some(match id {
            "claude-code" => self.home.join(".claude.json"),
            "claude-desktop" => self.app_support.join("Claude").join("claude_desktop_config.json"),
            "codex" => self.codex.join("config.toml"),
            "gemini" => self.home.join(".gemini").join("settings.json"),
            // older versions read ~/.gemini/antigravity/mcp_config.json, now a link to this one
            "antigravity" => self.home.join(".gemini").join("config").join("mcp_config.json"),
            "opencode" => {
                let dir = self.xdg_config.join("opencode");
                let jsonc = dir.join("opencode.jsonc");
                if jsonc.exists() && !dir.join("opencode.json").exists() {
                    jsonc
                } else {
                    dir.join("opencode.json")
                }
            }
            "kimi" => self.kimi.join("mcp.json"),
            "cursor" => self.home.join(".cursor").join("mcp.json"),
            "skill" => self.home.join(".agents").join("skills").join("only3d").join("SKILL.md"),
            _ => return None,
        })
    }

    /// The `claude` command, on PATH or where its installers put it.
    fn claude(&self) -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = if self.search_path { std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default() } else { Vec::new() };
        dirs.extend([self.home.join(".local/bin"), self.home.join(".claude/local"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
        dirs.into_iter().map(|d| d.join("claude")).find(|p| p.is_file())
    }

    fn on_path(&self, cmd: &str) -> bool {
        self.search_path && std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
    }
}

/// The MCP server entry most apps take.
pub fn entry(exe: &Path) -> Value {
    json!({"command": exe.to_string_lossy(), "args": ["mcp"]})
}

/// What to paste to set `id` up by hand.
pub fn snippet(id: &str, exe: &Path) -> String {
    let e = exe.to_string_lossy();
    match id {
        "claude-code" => format!("claude mcp add --scope user only3d -- \"{e}\" mcp"),
        "codex" => toml_block(exe),
        "opencode" => pretty(&json!({"mcp": {"only3d": opencode_entry(exe)}})),
        "skill" => skill_text(exe),
        _ => pretty(&json!({"mcpServers": {"only3d": entry(exe)}})),
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn opencode_entry(exe: &Path) -> Value {
    json!({"type": "local", "command": [exe.to_string_lossy(), "mcp"], "enabled": true})
}

fn toml_block(exe: &Path) -> String {
    let e = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    format!("[mcp_servers.only3d]\ncommand = \"{e}\"\nargs = [\"mcp\"]\n")
}

/// Is the app on this computer, and does it already have only3d?
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub found: bool,
    pub added: bool,
}

pub fn status(p: &Places, id: &str) -> Status {
    let file = p.file(id);
    let read_json = |f: &Path| std::fs::read_to_string(f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let has = |f: &Option<PathBuf>, key: &str| f.as_deref().and_then(read_json).is_some_and(|v| v.get(key).and_then(|m| m.get("only3d")).is_some());
    match id {
        "claude-code" => Status { found: p.claude().is_some(), added: has(&file, "mcpServers") },
        "claude-desktop" => Status { found: Path::new("/Applications/Claude.app").exists() || p.app_support.join("Claude").is_dir(), added: has(&file, "mcpServers") },
        "codex" => {
            let added = file.as_deref().and_then(|f| std::fs::read_to_string(f).ok()).is_some_and(|t| t.lines().any(is_our_header));
            Status { found: p.codex.is_dir() || p.on_path("codex"), added }
        }
        "gemini" => Status { found: p.home.join(".gemini").is_dir() || p.on_path("gemini"), added: has(&file, "mcpServers") },
        "antigravity" => {
            let found = p.home.join(".gemini/config").is_dir() || p.home.join(".gemini/antigravity").is_dir() || Path::new("/Applications/Antigravity.app").exists();
            Status { found, added: has(&file, "mcpServers") }
        }
        "opencode" => {
            // opencode.jsonc may hold comments: look for the key in the text
            let added = has(&file, "mcp") || file.as_deref().and_then(|f| std::fs::read_to_string(f).ok()).is_some_and(|t| t.contains("\"only3d\""));
            Status { found: p.xdg_config.join("opencode").is_dir() || p.on_path("opencode"), added }
        }
        "kimi" => Status { found: p.kimi.is_dir() || p.on_path("kimi"), added: has(&file, "mcpServers") },
        "cursor" => Status { found: p.home.join(".cursor").is_dir() || Path::new("/Applications/Cursor.app").exists(), added: has(&file, "mcpServers") },
        "skill" => Status { found: true, added: file.is_some_and(|f| f.exists()) },
        _ => Status { found: true, added: false },
    }
}

/// What installing `id` changes, in a sentence.
pub fn plan(p: &Places, id: &str) -> String {
    let at = |f: Option<PathBuf>| f.map(|f| tilde(&p.home, &f)).unwrap_or_default();
    match id {
        "claude-code" => "Runs `claude mcp add --scope user`, so only3d is there in every project.".into(),
        "skill" => format!("Writes {}: how to drive only3d with `only3d call <tool>` from a shell. It works on the window you have open.", at(p.file(id))),
        "other" => "Paste this into the app's MCP settings. The app starts only3d itself; with the window open it works on what you see.".into(),
        _ => format!("Adds only3d to {} and keeps the old file as .bak. Restart the app to load it.", at(p.file(id))),
    }
}

fn tilde(home: &Path, f: &Path) -> String {
    match f.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => f.display().to_string(),
    }
}

/// Register only3d with `id`. Returns what was done.
pub fn install(p: &Places, id: &str, exe: &Path) -> Result<String, String> {
    let name = app(id).map(|a| a.name).ok_or_else(|| format!("unknown app `{id}`; one of: {}", APPS.iter().map(|a| a.id).collect::<Vec<_>>().join(", ")))?;
    let file = p.file(id);
    let manual = || format!("add this by hand:\n{}", snippet(id, exe));
    match id {
        "other" => Ok(snippet(id, exe)),
        "claude-code" => {
            let claude = p.claude().ok_or_else(|| format!("Claude Code's `claude` command isn't installed (or isn't on PATH); {}", manual()))?;
            // replace an older entry (another build's path) instead of failing on it
            let _ = std::process::Command::new(&claude).args(["mcp", "remove", "only3d", "--scope", "user"]).output();
            let o = std::process::Command::new(&claude).args(["mcp", "add", "--scope", "user", "only3d", "--"]).arg(exe).arg("mcp").output().map_err(|e| format!("could not run {}: {e}", claude.display()))?;
            if o.status.success() {
                Ok("Claude Code: only3d is registered for every project (`claude mcp list` shows it). Start a new session to load it.".into())
            } else {
                Err(format!("`claude mcp add` failed: {}", String::from_utf8_lossy(&o.stderr).trim()))
            }
        }
        "codex" => {
            let f = file.ok_or("no path")?;
            let old = read_opt(&f)?;
            let new = set_toml_table(old.as_deref().unwrap_or(""), exe);
            write_keeping(&f, old.as_deref(), &new)?;
            Ok(format!("{name}: added only3d to {}. Restart the Codex app or start a new codex session.", tilde(&p.home, &f)))
        }
        "skill" => {
            let f = file.ok_or("no path")?;
            write_keeping(&f, None, &skill_text(exe))?;
            Ok(format!("Wrote {}. Agents that read Agent Skills from ~/.agents/skills find it in their next session.", tilde(&p.home, &f)))
        }
        _ => {
            let f = file.ok_or("no path")?;
            let (key, value) = if id == "opencode" { ("mcp", opencode_entry(exe)) } else { ("mcpServers", entry(exe)) };
            let old = read_opt(&f)?;
            let mut cfg: Value = match &old {
                Some(t) if t.trim().is_empty() => json!({}),
                Some(t) => serde_json::from_str(t).map_err(|e| format!("{} isn't plain JSON ({e}), so it was left alone; {}", tilde(&p.home, &f), manual()))?,
                None => json!({}),
            };
            if !cfg.is_object() {
                return Err(format!("{} doesn't hold a JSON object; {}", tilde(&p.home, &f), manual()));
            }
            if id == "opencode" && cfg.get("$schema").is_none() {
                cfg["$schema"] = json!("https://opencode.ai/config.json");
            }
            if !cfg.get(key).is_some_and(Value::is_object) {
                cfg[key] = json!({});
            }
            cfg[key]["only3d"] = value;
            write_keeping(&f, old.as_deref(), &(pretty(&cfg) + "\n"))?;
            Ok(format!("{name}: added only3d to {}. Restart {name} to load it.", tilde(&p.home, &f)))
        }
    }
}

fn read_opt(f: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(f) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", f.display())),
    }
}

/// Write `new`, keeping `old` (when there was a file) as `<file>.bak`.
fn write_keeping(f: &Path, old: Option<&str>, new: &str) -> Result<(), String> {
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    if let Some(old) = old {
        let mut bak = f.as_os_str().to_owned();
        bak.push(".bak");
        std::fs::write(PathBuf::from(bak), old).map_err(|e| format!("cannot back up {}: {e}", f.display()))?;
    }
    std::fs::write(f, new).map_err(|e| format!("cannot write {}: {e}", f.display()))
}

/// `[mcp_servers.only3d]` or one of its sub-tables.
fn is_our_header(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("[mcp_servers.only3d]") || l.starts_with("[mcp_servers.only3d.")
}

fn is_header(line: &str) -> bool {
    let l = line.split('#').next().unwrap_or("").trim();
    l.starts_with('[') && l.ends_with(']') && !l.contains('=')
}

/// The TOML text with only3d's table (and sub-tables) replaced by this binary's entry; the rest
/// is kept as it was, comments included.
fn set_toml_table(text: &str, exe: &Path) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if is_header(line) {
            skipping = is_our_header(line);
        }
        if !skipping {
            out.push(line);
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push_str("\n\n");
    }
    s.push_str(&toml_block(exe));
    s
}

/// The skill for agents without MCP: the shell commands, then the same guidance MCP agents get.
pub fn skill_text(exe: &Path) -> String {
    const GUIDE: &str = include_str!("../../../.claude/skills/only3d/SKILL.md");
    let body = GUIDE.split("## The loop that works").nth(1).unwrap_or("");
    let body = body.split("\n## Workflows").next().unwrap_or(body);
    let e = exe.display();
    format!(
        "---\nname: only3d\ndescription: Use when designing 3D-printable parts, slicing them, or running a Bambu Lab printer (H2D) with only3d from the shell: modelling, printability checks, slicing for time and grams, printing with the user's approval, filament spools and print history.\n---\n\n\
# Working with only3d from the shell\n\n\
only3d is a parametric modelling, slicing and printing studio built for a Bambu Lab H2D. Every\n\
action is a tool, and each tool is a shell command:\n\n\
```sh\n\
{e} tools --brief                  # every tool, one line each\n\
{e} tools                          # the same, with each tool's JSON arguments\n\
{e} call describe                  # what is on the plate, what the user selected\n\
{e} call add_object '{{\"name\": \"cup\", \"shape\": {{\"type\": \"cylinder\", \"d\": 60, \"h\": 80}}}}'\n\
{e} call render '{{}}' --image-dir /tmp   # writes PNGs: open them and look\n\
```\n\n\
- With the only3d window open, calls work on what the user sees and show up there, labelled\n  \
\"agent\". Without one, add `-p <file>.only3d.json` (loaded, and saved back when it changes).\n\
- `--json` prints the structured result. A failed call prints `{{\"ok\": false, \"error\": ...}}` with\n  \
a `code` and a `hint`, and exits with 2: follow the hint.\n\
- Printing, stopping and other printer actions wait for the user to approve them in the window.\n\n\
## The loop that works{body}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("o3d_agents_{tag}_{}_{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn json_apps_are_merged_and_unreadable_files_left_alone() {
        let home = tmp("json");
        let p = Places::under(&home);
        let exe = Path::new("/opt/only3d/bin/only3d");
        // an existing config keeps its other servers and settings, and gets a .bak
        let f = p.file("cursor").unwrap();
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, r#"{"mcpServers": {"other": {"command": "x"}}, "theme": 1}"#).unwrap();
        assert!(!status(&p, "cursor").added);
        install(&p, "cursor", exe).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["only3d"]["args"], json!(["mcp"]));
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["theme"], 1);
        assert!(f.with_extension("json.bak").exists());
        assert!(status(&p, "cursor").added);
        // a new file is created, with opencode's own shape
        install(&p, "opencode", exe).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p.file("opencode").unwrap()).unwrap()).unwrap();
        assert_eq!(v["mcp"]["only3d"]["command"], json!(["/opt/only3d/bin/only3d", "mcp"]));
        assert_eq!(v["mcp"]["only3d"]["type"], "local");
        // an empty file (as Antigravity leaves it) counts as no settings
        let a = p.file("antigravity").unwrap();
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, "").unwrap();
        install(&p, "antigravity", exe).unwrap();
        assert!(status(&p, "antigravity").added);
        // a file with comments is not rewritten
        let g = p.file("gemini").unwrap();
        std::fs::create_dir_all(g.parent().unwrap()).unwrap();
        std::fs::write(&g, "{ // mine\n \"theme\": 1 }").unwrap();
        let e = install(&p, "gemini", exe).unwrap_err();
        assert!(e.contains("left alone") && e.contains("mcpServers"), "{e}");
        assert_eq!(std::fs::read_to_string(&g).unwrap(), "{ // mine\n \"theme\": 1 }");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn codex_table_is_replaced_and_the_rest_kept() {
        let exe = Path::new("/new/only3d");
        let old = "# my settings\nmodel = \"gpt\"\n\n[mcp_servers.only3d]\ncommand = \"/old/only3d\"\nargs = [\"mcp\"]\n\n[mcp_servers.only3d.env]\nA = \"1\"\n\n[projects.\"/x\"]\ntrust_level = \"trusted\"\n";
        let new = set_toml_table(old, exe);
        assert!(new.starts_with("# my settings\nmodel = \"gpt\"\n"));
        assert!(new.contains("[projects.\"/x\"]\ntrust_level = \"trusted\""));
        assert!(!new.contains("/old/only3d") && !new.contains("A = \"1\""));
        assert_eq!(new.matches("[mcp_servers.only3d]").count(), 1);
        assert!(new.ends_with("[mcp_servers.only3d]\ncommand = \"/new/only3d\"\nargs = [\"mcp\"]\n"));
        // installing writes it, and a second install doesn't duplicate it
        let home = tmp("codex");
        let p = Places::under(&home);
        install(&p, "codex", exe).unwrap();
        install(&p, "codex", exe).unwrap();
        let text = std::fs::read_to_string(p.file("codex").unwrap()).unwrap();
        assert_eq!(text.matches("[mcp_servers.only3d]").count(), 1);
        assert!(status(&p, "codex").added);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_skill_tells_shell_agents_how_to_call_tools() {
        let t = skill_text(Path::new("/x/only3d"));
        assert!(t.starts_with("---\nname: only3d\n"));
        assert!(t.contains("/x/only3d call describe"));
        assert!(t.contains("## The loop that works") && t.contains("## Printing: the user decides"));
        assert!(!t.contains("mcp__only3d"), "MCP-only advice stays out");
        assert!(APPS.iter().all(|a| app(a.id).is_some()));
    }
}
