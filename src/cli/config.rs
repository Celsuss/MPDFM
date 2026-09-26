//! `mpdfm config show` — what every setting resolved to, and which of the four
//! sources answered.
//!
//! This is the command to reach for when MPDFM is looking in the wrong place.
//! Printing the source next to the value turns "why is it reading
//! `/var/lib/mpd/playlists`?" into a question the output already answers.

use std::process::ExitCode;

use anyhow::Result;
use mpdfm_core::config::{Config, ConfigWarning};

use super::{Cli, EXIT_OK};

/// Print the resolved configuration.
pub fn show(cli: &Cli, config: &Config, warnings: &[ConfigWarning]) -> Result<ExitCode> {
    if cli.globals.json {
        print_json(config, warnings)?;
    } else {
        print_table(config, warnings);
    }
    Ok(EXIT_OK)
}

fn print_table(config: &Config, warnings: &[ConfigWarning]) {
    let settings = config.settings();
    let name_width = settings.iter().map(|s| s.name.len()).max().unwrap_or(0);
    let value_width = settings
        .iter()
        .map(|s| s.value.chars().count())
        .max()
        .unwrap_or(0);

    println!(
        "{:name_width$}  {:value_width$}  SOURCE",
        "SETTING", "VALUE"
    );
    for setting in &settings {
        // Pad on the character count rather than `{:width$}` on the string, so
        // a non-ASCII path (1 023 of them in this library) does not skew the
        // column by the difference between bytes and characters.
        let padding = " ".repeat(value_width.saturating_sub(setting.value.chars().count()));
        println!(
            "{:name_width$}  {}{padding}  {}",
            setting.name, setting.value, setting.source
        );
    }

    if !warnings.is_empty() {
        println!();
        for warning in warnings {
            println!("warning: {warning}");
        }
    }
}

fn print_json(config: &Config, warnings: &[ConfigWarning]) -> Result<()> {
    let settings: serde_json::Map<String, serde_json::Value> = config
        .settings()
        .into_iter()
        .map(|setting| {
            (
                setting.name.to_owned(),
                serde_json::json!({
                    "value": setting.value,
                    "source": setting.source.to_string(),
                }),
            )
        })
        .collect();
    let document = serde_json::json!({
        "settings": settings,
        "warnings": warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&document)?);
    Ok(())
}
