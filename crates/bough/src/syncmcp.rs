//! Invariant: `bough sync-mcp` adopts Claude Code's MCP grants BY REFERENCE and owns exactly one
//! file, `$BOUGH_HOME/bough.mcp.patch.yml`, which it regenerates wholesale — no token ever lands
//! on disk, only `${keychain:…}` references `mcp-rmcp` resolves at connect time, and no human
//! file is ever rewritten (the human's `bough.patch.yml` sits ABOVE this layer and outranks it,
//! §0.5). The keychain item is the one Claude Code itself maintains; bough reads which grants
//! exist and never touches their values here.

use std::path::Path;
use std::process::ExitCode;

/// The keychain item Claude Code stores its MCP OAuth grants in, and the map they live under.
/// Protocol constants of Claude Code's own storage, not tunables.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const GRANTS_KEY: &str = "mcpOAuth";

/// One grant, as the keychain names it: the full map key (`<serverName>|<machine-hash>`), the
/// server's own name, and where it lives.
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub key: String,
    pub server_name: String,
    pub server_url: String,
    /// Claude Code's `expiresAt`, milliseconds since the epoch; absent or zero when the grant
    /// never recorded one — which the live keychain shows only on grants whose connects fail.
    pub expires_at_ms: u64,
}

impl Grant {
    /// Whether the grant LOOKS dead at `now_ms`. A dead grant is adopted `disabled: true`: the
    /// child row's connect happens at activation, so one stale token must never fail a whole
    /// boot. Re-auth in `claude`, run `bough sync-mcp` again, and the row comes back enabled.
    pub fn stale(&self, now_ms: u64) -> bool {
        self.expires_at_ms == 0 || self.expires_at_ms < now_ms
    }
}

/// Parse the credentials JSON into grants, sorted by server name so the rendered file is
/// deterministic. A grant without a `serverUrl` is skipped BY NAME on stderr, never silently.
pub fn grants_of(credentials_json: &str) -> Result<Vec<Grant>, String> {
    let doc: serde_json::Value =
        serde_json::from_str(credentials_json).map_err(|e| format!("not JSON: {e}"))?;
    let Some(map) = doc.get(GRANTS_KEY).and_then(|v| v.as_object()) else {
        return Ok(Vec::new());
    };
    let mut grants = Vec::new();
    for (key, v) in map {
        let name = v
            .get("serverName")
            .and_then(|s| s.as_str())
            .unwrap_or_else(|| key.split('|').next().unwrap_or(key));
        match v.get("serverUrl").and_then(|s| s.as_str()) {
            Some(url) => grants.push(Grant {
                key: key.clone(),
                server_name: name.to_string(),
                server_url: url.to_string(),
                expires_at_ms: v.get("expiresAt").and_then(|e| e.as_u64()).unwrap_or(0),
            }),
            None => eprintln!("bough: grant `{name}` carries no serverUrl; skipped"),
        }
    }
    grants.sort_by(|a, b| a.server_name.cmp(&b.server_name));
    Ok(grants)
}

/// The row name a grant gets: the last `:`-segment of Claude Code's server name
/// (`plugin:slack:slack` → `slack`) — unless that would collide with another grant's, in which
/// case the FULL name stands so two grants can never silently merge into one row.
pub fn row_names(grants: &[Grant]) -> Vec<String> {
    let short = |g: &Grant| {
        g.server_name
            .rsplit(':')
            .next()
            .unwrap_or(&g.server_name)
            .to_string()
    };
    let names: Vec<String> = grants.iter().map(short).collect();
    grants
        .iter()
        .zip(&names)
        .map(|(g, n)| {
            if names.iter().filter(|m| *m == n).count() > 1 {
                g.server_name.clone()
            } else {
                n.clone()
            }
        })
        .collect()
}

/// Render the whole layer file. Deterministic in the grants' sorted order; the timeouts mirror
/// the base row's (`bundles/bough-base.yml`, `mcp.rmcp`) because a patch layer REPLACES an
/// entry's config map and this one must therefore carry every field.
pub fn render(grants: &[Grant], now_ms: u64) -> String {
    let mut out = String::from(
        "# Written by `bough sync-mcp` and REGENERATED WHOLESALE on every run — do not edit.\n\
         # Claude Code's MCP grants, adopted by reference: each Authorization value is a\n\
         # `${keychain:…}` pointer mcp-rmcp resolves at connect time; no token lives here.\n\
         # To override a server by hand, put an `mcp.rmcp` entry in bough.patch.yml — the\n\
         # user layer sits above this one and its config replaces this whole map (§0.5).\n\
         entries:\n  mcp.rmcp:\n    config:\n      servers:\n",
    );
    for (grant, name) in grants.iter().zip(row_names(grants)) {
        if grant.stale(now_ms) {
            out.push_str(&format!(
                "        # `{name}`'s grant looks expired: re-auth it in `claude`, then run \
                 `bough sync-mcp` again.\n"
            ));
        }
        out.push_str(&format!(
            "        - name: {name}\n          transport:\n            kind: http\n            \
             url: \"{}\"\n            headers:\n              Authorization: \"Bearer \
             ${{keychain:{KEYCHAIN_SERVICE}#{GRANTS_KEY}.{}.accessToken}}\"\n",
            grant.server_url, grant.key
        ));
        if grant.stale(now_ms) {
            out.push_str("          disabled: true\n");
        }
    }
    out.push_str("      connect_timeout_ms: 15000\n      call_timeout_ms: 120000\n");
    out
}

/// The verb. Reads the grants, regenerates the layer file (rename over the old one, so the watch
/// never sees a torn document), and says what it adopted. `--dry-run` prints instead of writing.
pub fn run(dry_run: bool) -> ExitCode {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            eprintln!(
                "bough: no `{KEYCHAIN_SERVICE}` item in the login keychain ({}). \
                 Run `claude` and connect an MCP server there first.",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("bough: could not run `security`: {e}");
            return ExitCode::FAILURE;
        }
    };
    let grants = match grants_of(&String::from_utf8_lossy(&out.stdout)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bough: the keychain item is not what Claude Code writes: {e}");
            return ExitCode::FAILURE;
        }
    };
    if grants.is_empty() {
        eprintln!(
            "bough: the keychain holds no MCP grants; connect a server in `claude` first. \
             Nothing written."
        );
        return ExitCode::FAILURE;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let text = render(&grants, now_ms);
    let path = bough_util::mcp_patch_path();
    if dry_run {
        print!("{text}");
        eprintln!("bough: dry run; {} not written", path.display());
        return ExitCode::SUCCESS;
    }
    if let Err(e) = write_atomically(&path, &text) {
        eprintln!("bough: could not write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    for (grant, name) in grants.iter().zip(row_names(&grants)) {
        if grant.stale(now_ms) {
            println!(
                "  {name} \u{2192} {} (DISABLED: the grant looks expired; re-auth it in \
                 `claude`, then run `bough sync-mcp` again)",
                grant.server_url
            );
        } else {
            println!("  {name} \u{2192} {}", grant.server_url);
        }
    }
    println!(
        "bough: {} server(s) adopted into {}; a running bough recomposes live \
         (`/connectors` shows them go READY)",
        grants.len(),
        path.display()
    );
    ExitCode::SUCCESS
}

/// Rename over the old file: the patch watch must never read a torn document.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    bough_util::ensure_dir(dir)?;
    let tmp = path.with_extension("yml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "mcpOAuth": {
            "plugin:slack:slack|38801a7d845718b3": {
                "serverName": "plugin:slack:slack",
                "serverUrl": "https://mcp.slack.com/mcp",
                "accessToken": "SECRET",
                "expiresAt": 9999999999999
            },
            "linear-server|638130d5ab3558f4": {
                "serverName": "linear-server",
                "serverUrl": "https://mcp.linear.app/mcp",
                "accessToken": "SECRET",
                "expiresAt": 1000
            },
            "urlless|deadbeef": { "serverName": "urlless", "accessToken": "SECRET" }
        }
    }"#;

    #[test]
    fn grants_are_parsed_sorted_and_a_urlless_one_is_skipped() {
        let g = grants_of(FIXTURE).expect("parses");
        assert_eq!(
            g.iter().map(|g| g.server_name.as_str()).collect::<Vec<_>>(),
            vec!["linear-server", "plugin:slack:slack"],
            "sorted by name, the url-less grant skipped"
        );
        assert_eq!(g[1].key, "plugin:slack:slack|38801a7d845718b3");
    }

    #[test]
    fn a_plugin_name_shortens_and_a_collision_keeps_the_full_name() {
        let g = |name: &str| Grant {
            key: format!("{name}|hash"),
            server_name: name.to_string(),
            server_url: "https://x/mcp".to_string(),
            expires_at_ms: 9_999_999_999_999,
        };
        assert_eq!(
            row_names(&[g("plugin:slack:slack"), g("linear-server")]),
            vec!["slack", "linear-server"]
        );
        assert_eq!(
            row_names(&[g("plugin:a:slack"), g("plugin:b:slack")]),
            vec!["plugin:a:slack", "plugin:b:slack"],
            "two grants may never silently merge into one row"
        );
    }

    #[test]
    fn a_stale_grant_is_adopted_disabled_so_one_dead_token_never_fails_a_boot() {
        let grants = grants_of(FIXTURE).unwrap();
        // linear's fixture grant expired at 1000ms; slack's is far in the future.
        let text = render(&grants, 2_000_000);
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
        let servers = doc["entries"]["mcp.rmcp"]["config"]["servers"]
            .as_sequence()
            .expect("a list")
            .clone();
        let disabled = |name: &str| {
            servers
                .iter()
                .find(|s| s["name"] == name)
                .map(|s| s["disabled"] == true)
        };
        assert_eq!(disabled("linear-server"), Some(true), "{text}");
        assert_eq!(disabled("slack"), Some(false), "{text}");
        assert!(
            text.contains("looks expired"),
            "the reason is in the file: {text}"
        );
    }

    #[test]
    fn the_rendered_file_is_a_reference_not_a_secret_and_composes_as_yaml() {
        let grants = grants_of(FIXTURE).unwrap();
        let text = render(&grants, 0);
        assert!(
            !text.contains("SECRET"),
            "no token may ever land on disk: {text}"
        );
        assert!(text.contains(
            "${keychain:Claude Code-credentials#mcpOAuth.linear-server|638130d5ab3558f4.accessToken}"
        ));
        assert!(text.contains("- name: slack\n"), "{text}");
        // The file must parse as the patch document shape.
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
        let servers = &doc["entries"]["mcp.rmcp"]["config"]["servers"];
        assert_eq!(servers.as_sequence().map(|s| s.len()), Some(2), "{text}");
        assert_eq!(
            doc["entries"]["mcp.rmcp"]["config"]["connect_timeout_ms"],
            serde_yaml::Value::from(15000),
            "a patch replaces the whole config map, so every field must be carried"
        );
    }
}
