use std::{env, fs};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub(crate) struct DiscordSecret {
	pub token: String,
	pub admin_user_id: u64,
	pub application_id: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct FluxerSecret {
	pub token: String,
	pub admin_user_id: u64,
	pub instance: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SecretConfig {
	pub discord: Option<DiscordSecret>,
	pub fluxer: Option<FluxerSecret>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecretConfig {
	discord_token: Option<String>,
	admin_user_id: Option<u64>,
	discord_application_id: Option<u64>,
	discord: Option<RawDiscord>,
	fluxer: Option<RawFluxer>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDiscord {
	token: Option<String>,
	admin_user_id: Option<u64>,
	application_id: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFluxer {
	token: Option<String>,
	admin_user_id: Option<u64>,
	instance: Option<String>,
}

pub(crate) fn parse() -> Result<SecretConfig> {
	let path =
		env::var("SUPER_SIMPLE_BOORU_BOT_SECRETS_FILE").unwrap_or_else(|_| "secrets.toml".into());
	parse_path(std::path::Path::new(&path))
}

fn parse_path(path: &std::path::Path) -> Result<SecretConfig> {
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let mode = fs::metadata(path)
			.with_context(|| format!("cannot access runtime secrets file {}", path.display()))?
			.permissions()
			.mode();
		if mode & 0o077 != 0 {
			bail!(
				"runtime secrets file {} must not be accessible by group or others (use chmod 600)",
				path.display()
			);
		}
	}
	let raw = fs::read_to_string(path)
		.with_context(|| format!("failed to read runtime secrets file {}", path.display()))?;
	parse_str(&raw)
}

fn parse_str(raw: &str) -> Result<SecretConfig> {
	let raw: RawSecretConfig = toml::from_str(raw).map_err(|_| {
		anyhow::anyhow!("invalid secrets TOML: check the required keys and value types")
	})?;

	let legacy_discord = raw.discord_token.is_some()
		|| raw.discord_application_id.is_some()
		|| (raw.admin_user_id.is_some() && raw.fluxer.is_none());
	if legacy_discord && raw.discord.is_some() {
		bail!("configure Discord either at the top level or under [discord], not both");
	}

	let discord = if let Some(nested) = raw.discord {
		Some(parse_discord(
			nested.token,
			nested.admin_user_id.or(raw.admin_user_id),
			nested.application_id.or(raw.discord_application_id),
		)?)
	} else if legacy_discord {
		Some(parse_discord(
			raw.discord_token,
			raw.admin_user_id,
			raw.discord_application_id,
		)?)
	} else {
		None
	};

	let fluxer = raw
		.fluxer
		.map(|nested| parse_fluxer(nested, raw.admin_user_id))
		.transpose()?;

	anyhow::ensure!(
		discord.is_some() || fluxer.is_some(),
		"secrets.toml must configure at least one of [discord] or [fluxer]"
	);

	Ok(SecretConfig { discord, fluxer })
}

fn parse_discord(
	token: Option<String>,
	admin_user_id: Option<u64>,
	application_id: Option<u64>,
) -> Result<DiscordSecret> {
	let token = required_token(token, "discord token")?;
	if !token.contains('.') {
		// Discord bot tokens contain at least one dot. Fluxer tokens also do; this
		// catches accidentally swapping the two sections.
		bail!("secrets.toml discord token is malformed");
	}
	let admin_user_id = admin_user_id
		.filter(|id| *id > 0)
		.context("secrets.toml discord admin_user_id is required and must be greater than zero")?;
	let application_id = application_id
		.filter(|id| *id > 0)
		.context("secrets.toml discord application_id is required and must be greater than zero")?;
	Ok(DiscordSecret {
		token,
		admin_user_id,
		application_id,
	})
}

fn parse_fluxer(nested: RawFluxer, fallback_admin: Option<u64>) -> Result<FluxerSecret> {
	let token = required_token(nested.token, "fluxer token")?;
	let admin_user_id = nested
		.admin_user_id
		.or(fallback_admin)
		.filter(|id| *id > 0)
		.context("secrets.toml fluxer admin_user_id is required and must be greater than zero")?;
	let instance = nested
		.instance
		.unwrap_or_else(|| "https://fluxer.app".to_string())
		.trim()
		.trim_end_matches('/')
		.to_string();
	let url = reqwest::Url::parse(&instance).context("secrets.toml fluxer instance is invalid")?;
	let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
	anyhow::ensure!(
		matches!(url.scheme(), "https" | "http") && url.host_str().is_some(),
		"fluxer instance must be an HTTP(S) URL"
	);
	anyhow::ensure!(
		url.scheme() == "https" || local,
		"fluxer instance must use HTTPS unless it is localhost"
	);
	anyhow::ensure!(
		url.username().is_empty() && url.password().is_none(),
		"fluxer instance must not contain credentials"
	);
	Ok(FluxerSecret {
		token,
		admin_user_id,
		instance,
	})
}

fn required_token(token: Option<String>, label: &str) -> Result<String> {
	let token = token.unwrap_or_default().trim().to_string();
	anyhow::ensure!(!token.is_empty(), "secrets.toml {label} is required");
	Ok(token)
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;
	use std::os::unix::fs::PermissionsExt;
	#[test]
	fn secret_errors_never_print_credential_lines_and_permissions_are_checked() {
		let path = std::env::temp_dir().join(format!(
			"super-simple-booru-bot-secrets-test-{}",
			crate::app::post_cache::unique_id()
		));
		let mut options = fs::OpenOptions::new();
		use std::{io::Write, os::unix::fs::OpenOptionsExt};
		let mut file = options
			.write(true)
			.create_new(true)
			.mode(0o600)
			.open(&path)
			.unwrap();
		write!(
			file,
			"discord_token = \"audit-canary-secret\"\nadmin_user_id = invalid"
		)
		.unwrap();
		let error = parse_path(&path).err().unwrap().to_string();
		assert!(!error.contains("audit-canary-secret"));
		fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
		assert!(
			parse_path(&path)
				.err()
				.unwrap()
				.to_string()
				.contains("chmod 600")
		);
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn parses_legacy_discord_and_fluxer_configurations() {
		let discord =
			parse_str("discord_token = \"123.abc\"\nadmin_user_id = 1\ndiscord_application_id = 2")
				.unwrap();
		assert!(discord.fluxer.is_none());
		assert_eq!(discord.discord.unwrap().application_id, 2);

		let fluxer = parse_str(
			"[fluxer]\ntoken = \"123.abc\"\nadmin_user_id = 3\ninstance = \"https://example.test/\"",
		)
		.unwrap();
		let fluxer = fluxer.fluxer.unwrap();
		assert_eq!(fluxer.admin_user_id, 3);
		assert_eq!(fluxer.instance, "https://example.test");

		let fluxer = parse_str("admin_user_id = 4\n[fluxer]\ntoken = \"123.abc\"").unwrap();
		assert!(fluxer.discord.is_none());
		assert_eq!(fluxer.fluxer.unwrap().admin_user_id, 4);
	}

	#[test]
	fn rejects_empty_configuration_and_insecure_remote_instances() {
		assert!(parse_str("").is_err());
		assert!(
			parse_str("[fluxer]\ntoken = \"123.abc\"\ninstance = \"http://example.test\"").is_err()
		);
	}
}
