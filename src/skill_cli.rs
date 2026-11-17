//! Installs the bundled agent skill where a supported client discovers user-level skills.
use clap::{Args, Subcommand, ValueEnum};
use cqlmatic::contracts::Diagnostic;
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Skill folder name; clients require it to equal the `name` in `SKILL.md`.
const NAME: &str = "cqlmatic-companion";
const SKILL_SOURCE: &str = "integrations/agent";
const SKILL: &str = include_str!("../integrations/agent/SKILL.md");
const REFERENCES_SOURCE: &str = "docs";
/// Every guide `SKILL.md` links to, installed beside it under `references/`.
const REFERENCES: [(&str, &str); 6] = [
    ("CONTRACTS.md", include_str!("../docs/CONTRACTS.md")),
    ("ENVIRONMENT.md", include_str!("../docs/ENVIRONMENT.md")),
    ("MCP-COMPANION.md", include_str!("../docs/MCP-COMPANION.md")),
    (
        "QUERY-REFERENCE.md",
        include_str!("../docs/QUERY-REFERENCE.md"),
    ),
    (
        "RESULT-ASSESSMENT.md",
        include_str!("../docs/RESULT-ASSESSMENT.md"),
    ),
    (
        "TROUBLESHOOTING.md",
        include_str!("../docs/TROUBLESHOOTING.md"),
    ),
];

#[derive(Subcommand)]
pub enum SkillCommand {
    /// Write the bundled skill and its reference guides into a client's skill directory.
    Install(InstallArgs),
}

#[derive(Args)]
pub struct InstallArgs {
    #[arg(
        long,
        value_enum,
        required_unless_present = "dir",
        conflicts_with = "dir",
        help = "Client whose user-level skill directory receives the skill"
    )]
    client: Option<Client>,
    #[arg(
        long,
        value_name = "DIR",
        help = "Skill directory of another client; the skill is written to DIR/cqlmatic-companion"
    )]
    dir: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum Client {
    ClaudeCode,
    Codex,
    VsCode,
    CopilotCli,
    CopilotApp,
    CopilotVscode,
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Written {
    created: usize,
    updated: usize,
    unchanged: usize,
}

pub fn run(command: SkillCommand) -> Result<i32, Diagnostic> {
    let SkillCommand::Install(args) = command;
    let root = match (args.client, args.dir) {
        (Some(client), _) => client_root(client, |name| env::var_os(name).map(PathBuf::from))?,
        (None, Some(dir)) => dir,
        (None, None) => return Err(err("SKILL_TARGET", "Choose --client or --dir.")),
    };
    let root = std::path::absolute(root)
        .map_err(|_| err("SKILL_TARGET", "Cannot resolve the skill directory."))?;
    let written = install(&root)?;
    println!(
        "Installed agent skill {NAME} at {}\n{} created, {} updated, {} unchanged.\nStart a new agent session or reload skills to use it. Installing the skill grants no search authorization.",
        root.join(NAME).display(),
        written.created,
        written.updated,
        written.unchanged
    );
    Ok(0)
}

/// The user-level skill directory a client reads.
fn client_root(
    client: Client,
    var: impl Fn(&str) -> Option<PathBuf>,
) -> Result<PathBuf, Diagnostic> {
    let home = || {
        var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok_or_else(|| {
            err(
                "SKILL_TARGET",
                "Home directory is unavailable; pass the client's skill directory with --dir.",
            )
        })
    };
    let root = match client {
        Client::ClaudeCode => match var("CLAUDE_CONFIG_DIR") {
            Some(root) => root,
            None => home()?.join(".claude"),
        },
        // Copilot stops reading the shared directory when its home is relocated.
        Client::CopilotCli | Client::CopilotApp | Client::CopilotVscode => {
            match var("COPILOT_HOME") {
                Some(root) => root,
                None => home()?.join(".agents"),
            }
        }
        Client::Codex | Client::VsCode => home()?.join(".agents"),
    };
    Ok(root.join("skills"))
}

fn install(root: &Path) -> Result<Written, Diagnostic> {
    let skill = root.join(NAME);
    let references = skill.join("references");
    for directory in [&skill, &references] {
        match fs::symlink_metadata(directory) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(err(
                    "SKILL_TARGET",
                    format!(
                        "{} exists and is not a directory. Move it aside, then install again.",
                        directory.display()
                    ),
                ));
            }
            Err(_) => fs::create_dir_all(directory).map_err(|_| {
                err(
                    "SKILL_WRITE",
                    format!("Cannot create {}.", directory.display()),
                )
            })?,
        }
    }
    let mut written = Written::default();
    write(
        &skill.join("SKILL.md"),
        &installed(SKILL_SOURCE, SKILL),
        &mut written,
    )?;
    for (name, text) in REFERENCES {
        write(
            &references.join(name),
            &installed(REFERENCES_SOURCE, text),
            &mut written,
        )?;
    }
    Ok(written)
}

fn write(path: &Path, text: &str, written: &mut Written) -> Result<(), Diagnostic> {
    let exists = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => true,
        Ok(_) => {
            return Err(err(
                "SKILL_TARGET",
                format!(
                    "{} exists and is not a regular file. Move it aside, then install again.",
                    path.display()
                ),
            ));
        }
        Err(_) => false,
    };
    if exists && fs::read(path).is_ok_and(|current| current == text.as_bytes()) {
        written.unchanged += 1;
        return Ok(());
    }
    let failed = || err("SKILL_WRITE", format!("Cannot write {}.", path.display()));
    // A complete file replaces the old one in a single step.
    let temporary = path.with_extension(format!("md.tmp-{}", std::process::id()));
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(failed());
    }
    if exists {
        written.updated += 1;
    } else {
        written.created += 1;
    }
    Ok(())
}

/// The text of a bundled file as installed: links to bundled guides stay local, and every
/// other repository-relative link points at this release in the public repository.
fn installed(source_directory: &str, text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("](") {
        let (before, after) = rest.split_at(start + 2);
        output.push_str(before);
        let Some(end) = after.find(')') else {
            rest = after;
            break;
        };
        let target = &after[..end];
        output.push_str(&link(source_directory, target).unwrap_or_else(|| target.to_string()));
        rest = &after[end..];
    }
    output.push_str(rest);
    output
}

/// The installed form of one repository-relative link target, or `None` to leave it as written.
fn link(source_directory: &str, target: &str) -> Option<String> {
    let (path, fragment) = match target.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (target, None),
    };
    if path.is_empty()
        || path.contains(':')
        || !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
    {
        return None;
    }
    let mut parts: Vec<&str> = source_directory.split('/').collect();
    for part in path.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    let resolved = parts.join("/");
    let local = resolved
        .strip_prefix("docs/")
        .filter(|name| REFERENCES.iter().any(|(bundled, _)| bundled == name));
    let mut link = match local {
        Some(name) if source_directory == REFERENCES_SOURCE => name.to_string(),
        Some(name) => format!("references/{name}"),
        None => format!(
            "{}/blob/v{}/{resolved}",
            env!("CARGO_PKG_REPOSITORY"),
            env!("CARGO_PKG_VERSION")
        ),
    };
    if let Some(fragment) = fragment {
        link.push('#');
        link.push_str(fragment);
    }
    Some(link)
}

fn err(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = env::temp_dir().join(format!(
                "cqlmatic-skill-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn targets(text: &str) -> Vec<&str> {
        text.split("](")
            .skip(1)
            .filter_map(|after| after.split_once(')').map(|(target, _)| target))
            .collect()
    }

    #[test]
    fn installed_skill_is_self_contained() {
        let skill = installed(SKILL_SOURCE, SKILL);
        assert!(skill.starts_with(&format!("---\nname: {NAME}\n")));
        let mut linked = Vec::new();
        for target in targets(&skill) {
            let name = target
                .strip_prefix("references/")
                .unwrap_or_else(|| panic!("SKILL.md links outside the skill folder: {target}"));
            linked.push(name.split('#').next().unwrap());
        }
        linked.sort_unstable();
        linked.dedup();
        assert_eq!(linked, REFERENCES.map(|(name, _)| name));
        let release = format!(
            "{}/blob/v{}/",
            env!("CARGO_PKG_REPOSITORY"),
            env!("CARGO_PKG_VERSION")
        );
        for (name, text) in REFERENCES {
            for target in targets(&installed(REFERENCES_SOURCE, text)) {
                let path = target.split('#').next().unwrap();
                assert!(
                    path.is_empty()
                        || path.starts_with("https://")
                        || REFERENCES.iter().any(|(bundled, _)| *bundled == path),
                    "{name} keeps an unresolved link: {target}"
                );
                assert!(
                    !path.starts_with(env!("CARGO_PKG_REPOSITORY")) || path.starts_with(&release),
                    "{name} links outside this release: {target}"
                );
            }
        }
    }

    #[test]
    fn links_resolve_from_their_source_directory() {
        for (source, target, expected) in [
            (
                SKILL_SOURCE,
                "../../docs/MCP-COMPANION.md#session",
                Some("references/MCP-COMPANION.md#session".to_string()),
            ),
            (
                REFERENCES_SOURCE,
                "CONTRACTS.md",
                Some("CONTRACTS.md".to_string()),
            ),
            (
                REFERENCES_SOURCE,
                "INVENTORY.md#actions",
                Some(format!(
                    "{}/blob/v{}/docs/INVENTORY.md#actions",
                    env!("CARGO_PKG_REPOSITORY"),
                    env!("CARGO_PKG_VERSION")
                )),
            ),
            (
                REFERENCES_SOURCE,
                "../examples/mcp/README.md",
                Some(format!(
                    "{}/blob/v{}/examples/mcp/README.md",
                    env!("CARGO_PKG_REPOSITORY"),
                    env!("CARGO_PKG_VERSION")
                )),
            ),
            (REFERENCES_SOURCE, "https://example.com/a.html", None),
            (REFERENCES_SOURCE, "#anchor", None),
            (REFERENCES_SOURCE, "../../outside.md", None),
            (REFERENCES_SOURCE, "\"a[]\", b", None),
        ] {
            assert_eq!(link(source, target), expected, "{source} {target}");
        }
    }

    #[test]
    fn clients_map_to_their_skill_directories() {
        let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let plain = |name: &str| (name == home).then(|| PathBuf::from("home"));
        assert_eq!(
            client_root(Client::ClaudeCode, plain).unwrap(),
            Path::new("home").join(".claude").join("skills")
        );
        for client in [
            Client::Codex,
            Client::VsCode,
            Client::CopilotCli,
            Client::CopilotApp,
            Client::CopilotVscode,
        ] {
            assert_eq!(
                client_root(client, plain).unwrap(),
                Path::new("home").join(".agents").join("skills"),
                "{client:?}"
            );
        }
        let relocated = |name: &str| match name {
            "CLAUDE_CONFIG_DIR" => Some(PathBuf::from("claude")),
            "COPILOT_HOME" => Some(PathBuf::from("copilot")),
            name if name == home => Some(PathBuf::from("home")),
            _ => None,
        };
        assert_eq!(
            client_root(Client::ClaudeCode, relocated).unwrap(),
            Path::new("claude").join("skills")
        );
        assert_eq!(
            client_root(Client::CopilotCli, relocated).unwrap(),
            Path::new("copilot").join("skills")
        );
        assert_eq!(
            client_root(Client::Codex, relocated).unwrap(),
            Path::new("home").join(".agents").join("skills")
        );
        assert_eq!(
            client_root(Client::Codex, |_| None).unwrap_err().code,
            "SKILL_TARGET"
        );
    }

    #[test]
    fn install_creates_then_keeps_then_updates_files() {
        let temp = Temp::new();
        let root = temp.0.join("skills");
        let total = REFERENCES.len() + 1;
        assert_eq!(
            install(&root).unwrap(),
            Written {
                created: total,
                ..Written::default()
            }
        );
        let skill = root.join(NAME);
        assert_eq!(
            fs::read_to_string(skill.join("SKILL.md")).unwrap(),
            installed(SKILL_SOURCE, SKILL)
        );
        for (name, text) in REFERENCES {
            assert_eq!(
                fs::read_to_string(skill.join("references").join(name)).unwrap(),
                installed(REFERENCES_SOURCE, text)
            );
        }
        assert_eq!(
            install(&root).unwrap(),
            Written {
                unchanged: total,
                ..Written::default()
            }
        );
        fs::write(skill.join("SKILL.md"), b"older release").unwrap();
        fs::write(skill.join("notes.md"), b"kept").unwrap();
        assert_eq!(
            install(&root).unwrap(),
            Written {
                updated: 1,
                unchanged: total - 1,
                ..Written::default()
            }
        );
        assert_eq!(fs::read(skill.join("notes.md")).unwrap(), b"kept");
        let leftovers: Vec<_> = fs::read_dir(&skill)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn install_refuses_targets_that_are_not_plain_files_or_directories() {
        let temp = Temp::new();
        let root = temp.0.join("skills");
        fs::create_dir(&root).unwrap();
        fs::write(root.join(NAME), b"a file").unwrap();
        assert_eq!(install(&root).unwrap_err().code, "SKILL_TARGET");
        fs::remove_file(root.join(NAME)).unwrap();
        fs::create_dir_all(root.join(NAME).join("SKILL.md")).unwrap();
        assert_eq!(install(&root).unwrap_err().code, "SKILL_TARGET");
        #[cfg(unix)]
        {
            let elsewhere = temp.0.join("elsewhere");
            fs::create_dir(&elsewhere).unwrap();
            fs::remove_dir_all(root.join(NAME)).unwrap();
            std::os::unix::fs::symlink(&elsewhere, root.join(NAME)).unwrap();
            assert_eq!(install(&root).unwrap_err().code, "SKILL_TARGET");
            assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        }
    }
}
