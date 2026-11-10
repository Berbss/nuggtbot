use crate::model::Command;
use std::collections::BTreeMap;

/// Match a chat message against the commands for `platform`. Honors mode
/// (basic/regex), location (start/exact/anywhere), case sensitivity, aliases,
/// and platform filtering.
pub fn match_command<'a>(commands: &'a [Command], text: &str, platform: &str) -> Option<&'a Command> {
    commands.iter().find(|c| {
        if !c.platforms.is_empty() && !c.platforms.iter().any(|p| p.eq_ignore_ascii_case(platform)) {
            return false;
        }
        let triggers = std::iter::once(&c.trigger).chain(c.aliases.iter());
        triggers.filter(|t| !t.trim().is_empty()).any(|t| matches_one(text, t, c))
    })
}

fn matches_one(text: &str, alias: &str, c: &Command) -> bool {
    if c.mode.eq_ignore_ascii_case("regex") {
        let pat = if c.case_sensitive { alias.to_string() } else { format!("(?i){alias}") };
        return regex::Regex::new(&pat).map(|re| re.is_match(text)).unwrap_or(false);
    }
    let (hay, needle) = if c.case_sensitive {
        (text.trim().to_string(), alias.trim().to_string())
    } else {
        (text.trim().to_lowercase(), alias.trim().to_lowercase())
    };
    match c.location.to_lowercase().as_str() {
        "exact" => hay == needle,
        "anywhere" => hay.contains(&needle),
        _ => hay == needle || hay.starts_with(&format!("{needle} ")), // start (default)
    }
}

/// Expand placeholders. Supports Streamer.bot style `%userName%`, `%rawInput%`,
/// `%input0%`/`%input1%`… and our own `$user`, `$var.NAME`. `args` are the words
/// after the command trigger.
pub fn render(response: &str, user: &str, args: &[String], variables: &BTreeMap<String, String>) -> String {
    let raw = args.join(" ");
    let mut out = response
        .replace("%userName%", user)
        .replace("%user%", user)
        .replace("%rawInput%", &raw)
        .replace("%input%", &raw)
        .replace("$user", user);
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("%input{i}%"), a);
    }
    for (name, value) in variables {
        out = out
            .replace(&format!("$var.{name}"), value)
            .replace(&format!("%{name}%"), value);
    }
    // Any %inputN% with no matching arg becomes empty (SB behavior).
    if out.contains("%input") {
        if let Ok(re) = regex::Regex::new(r"%input\d+%") {
            out = re.replace_all(&out, "").to_string();
        }
    }
    out
}
