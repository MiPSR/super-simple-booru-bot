use crate::{db::Database, i18n::I18n};
use anyhow::{Result, ensure};

pub const DEFAULTS: &[(&str, &str)] = &[
	("app_lang", "en"),
	("api_rate_pace_ms", "0"),
	("discord_api_pace_ms", "0"),
	("booru_fetch_retry_limit", "3"),
	("booru_source_link_history_limit", "1000"),
	("booru_tag_blacklist", ""),
	("command_sync_interval_secs", "86400"),
	("http_timeout_secs", "30"),
	("max_concurrent_jobs", "4"),
	("prefix_enabled", "false"),
	("command_prefix", "!"),
];

pub fn default_value(db: &Database, key: &str) -> String {
	if key == "prefix_enabled" {
		return db.platform().default_prefix_enabled().to_string();
	}
	DEFAULTS
		.iter()
		.find(|(name, _)| *name == key)
		.map(|(_, value)| value.to_string())
		.unwrap_or_default()
}

pub fn validate(key: &str, value: &str) -> Result<()> {
	ensure!(
		!key.trim().is_empty() && key.chars().count() <= 100 && value.chars().count() <= 4000,
		"setting key/value is empty or too long"
	);
	let bounds = match key {
		"app_lang" => {
			I18n::load(value)?;
			return Ok(());
		}
		"api_rate_pace_ms" | "discord_api_pace_ms" => Some((0, 60_000)),
		"booru_fetch_retry_limit" => Some((0, 20)),
		"booru_source_link_history_limit" => Some((1, 100_000)),
		"command_sync_interval_secs" => Some((60, 2_678_400)),
		"http_timeout_secs" => Some((1, 120)),
		"max_concurrent_jobs" => Some((1, 32)),
		"command_prefix" => {
			ensure!(
				(1..=10).contains(&value.chars().count())
					&& !value.chars().any(char::is_whitespace),
				"command_prefix must be 1..10 non-whitespace characters"
			);
			return Ok(());
		}
		"prefix_enabled" => {
			match value.trim().to_ascii_lowercase().as_str() {
				"true" | "1" | "yes" | "on" | "false" | "0" | "no" | "off" => {}
				_ => anyhow::bail!("{key} must be a boolean"),
			}
			return Ok(());
		}
		_ => None,
	};
	if let Some((min, max)) = bounds {
		let number: u64 = value
			.parse()
			.map_err(|_| anyhow::anyhow!("{key} must be a whole number"))?;
		ensure!((min..=max).contains(&number), "{key} must be {min}..{max}");
	}
	Ok(())
}

pub fn get(db: &Database, key: &str) -> Result<String> {
	let value = db
		.get_setting(key)?
		.unwrap_or_else(|| default_value(db, key));
	validate(key, &value)?;
	Ok(value)
}

pub fn number(db: &Database, key: &str) -> Result<u64> {
	Ok(get(db, key)?.parse()?)
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn validates_defaults_and_bounds() {
		for (key, value) in DEFAULTS {
			validate(key, value).unwrap();
		}
		for (key, value) in [
			("app_lang", "bad"),
			("booru_fetch_retry_limit", "-1"),
			("http_timeout_secs", "0"),
			("max_concurrent_jobs", "1000"),
			("command_prefix", " "),
			("prefix_enabled", "maybe"),
		] {
			assert!(validate(key, value).is_err());
		}
	}

	#[test]
	fn prefix_defaults_depend_on_platform() {
		let discord =
			crate::db::Database::open(":memory:", 3, crate::platform::PlatformKind::Discord)
				.unwrap();
		let fluxer =
			crate::db::Database::open(":memory:", 3, crate::platform::PlatformKind::Fluxer)
				.unwrap();
		assert_eq!(default_value(&discord, "prefix_enabled"), "false");
		assert_eq!(default_value(&fluxer, "prefix_enabled"), "true");
		assert_eq!(default_value(&fluxer, "command_prefix"), "!");
		assert_eq!(get(&fluxer, "prefix_enabled").unwrap(), "true");
	}

	#[test]
	fn database_is_bound_to_one_platform() {
		let path = std::env::temp_dir().join(format!(
			"super-simple-booru-bot-platform-test-{}",
			crate::app::post_cache::unique_id()
		));
		let discord =
			crate::db::Database::open(&path, 3, crate::platform::PlatformKind::Discord).unwrap();
		drop(discord);
		assert!(
			crate::db::Database::open(&path, 3, crate::platform::PlatformKind::Fluxer).is_err()
		);
		std::fs::remove_file(path).unwrap();
	}
}
