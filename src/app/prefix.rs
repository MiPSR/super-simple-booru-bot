use anyhow::{Context as _, Result, ensure};
use async_trait::async_trait;
use rand::RngExt;

use crate::{cli, config::BooruConfig};

use super::{
	Core,
	commands_core::{self, Candidate},
	responses::{custom_command_name, format_recent_art_links},
};

pub(crate) struct PrefixCommand {
	pub name: String,
	pub raw_args: String,
	pub args: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct PrefixReply {
	pub content: String,
	pub attachment: Option<(Vec<u8>, String)>,
	pub dm_reference: Option<String>,
}

impl PrefixReply {
	pub(crate) fn text(content: impl Into<String>) -> Self {
		Self {
			content: content.into(),
			attachment: None,
			dm_reference: None,
		}
	}
}

#[async_trait]
pub(crate) trait PrefixContext: Send + Sync {
	fn core(&self) -> &Core;
	fn guild(&self) -> Option<i64>;
	fn channel(&self) -> i64;
	fn user(&self) -> u64;
	async fn defer(&self) -> Result<()>;
	async fn reply(&self, reply: PrefixReply) -> Result<()>;
	async fn follow_up(&self, reply: PrefixReply) -> Result<()>;
	async fn send_dm(&self, user: i64, reply: PrefixReply) -> Result<()>;
}

pub(crate) fn parse(content: &str, prefix: &str) -> Option<PrefixCommand> {
	let rest = content.strip_prefix(prefix)?.trim_start();
	if rest.is_empty() {
		return None;
	}
	let mut split = rest.splitn(2, char::is_whitespace);
	let name = split.next()?.trim().to_ascii_lowercase();
	if name.is_empty() {
		return None;
	}
	let raw_args = split.next().unwrap_or("").trim().to_string();
	let args = raw_args
		.split_whitespace()
		.map(str::to_string)
		.collect::<Vec<_>>();
	Some(PrefixCommand {
		name,
		raw_args,
		args,
	})
}

pub(crate) async fn execute(ctx: &dyn PrefixContext, command: PrefixCommand) -> Result<()> {
	let core = ctx.core();
	cli::app_input(ctx.guild(), ctx.channel(), ctx.user() as i64, &command.name);

	match command.name.as_str() {
		"help" => {
			ctx.reply(PrefixReply::text(help_text(core, ctx.guild())))
				.await?;
			Ok(())
		}
		"status" => status(ctx).await,
		"reload" => reload(ctx).await,
		"booru-test" => booru_test(ctx, &command.raw_args).await,
		"tag-test" => tag_test(ctx, &command.args).await,
		"art-history" | "history" => art_history(ctx, &command.args).await,
		"dm" => dm(ctx, &command.args).await,
		"administrate" | "admin" => administrate(ctx, &command.raw_args).await,
		_ => custom_or_pattern(ctx, &command).await,
	}
}

fn help_text(core: &Core, guild: Option<i64>) -> String {
	let prefix = core.command_prefix();
	let mut lines = vec![
		format!("Prefix commands (`{prefix}`):"),
		format!("`{prefix}help`"),
		format!("`{prefix}status`"),
		format!("`{prefix}reload`"),
		format!("`{prefix}booru-test <name> <tags>`"),
		format!("`{prefix}tag-test <tag>`"),
		format!("`{prefix}art-history [count]`"),
		format!("`{prefix}dm <reference>`"),
		format!("`{prefix}administrate <action>` (use `{prefix}administrate help`)"),
	];
	if let Ok(db) = core.db.lock() {
		if let Ok(boorus) = db.get_enabled_boorus() {
			let names = boorus
				.iter()
				.map(|booru| format!("`{prefix}{}`", custom_command_name(&booru.name)))
				.collect::<Vec<_>>();
			if !names.is_empty() {
				lines.push(format!("Provider commands: {}", names.join(", ")));
			}
		}
		if let Some(guild) = guild
			&& let Ok(tags) = db.get_server_tags(guild)
			&& !tags.is_empty()
		{
			let names = tags
				.iter()
				.map(|tag| format!("`{prefix}{tag}`"))
				.collect::<Vec<_>>();
			lines.push(format!("Pattern commands: {}", names.join(", ")));
		}
	}
	lines.join("\n")
}

async fn status(ctx: &dyn PrefixContext) -> Result<()> {
	let core = ctx.core();
	ensure!(
		core.can_administer(ctx.user(), ctx.guild())?,
		"status is restricted to the administrator and guild moderators"
	);
	let mut text = if core.startup_error.is_some() {
		"Status: degraded".to_string()
	} else {
		"Status: OK".to_string()
	};
	if let Some(err) = &core.startup_error {
		text.push_str(&format!(
			"\nStartup error: {}",
			err.chars().take(1500).collect::<String>()
		));
	}
	if let Ok(db) = core.db.lock() {
		text.push_str(&format!(
			"\nProviders: {}",
			db.get_all_boorus().map(|v| v.len()).unwrap_or(0)
		));
		text.push_str(&format!(
			"\nValidated servers: {}",
			db.get_validated_servers().map(|v| v.len()).unwrap_or(0)
		));
		text.push_str(&format!(
			"\nConfigured channels: {}",
			db.count_channels().unwrap_or(0)
		));
	}
	ctx.reply(PrefixReply::text(text)).await
}

async fn reload(ctx: &dyn PrefixContext) -> Result<()> {
	let core = ctx.core();
	ensure!(
		core.can_administer(ctx.user(), ctx.guild())?,
		"reload is restricted to the administrator and guild moderators"
	);
	core.request_reload()?;
	ctx.reply(PrefixReply::text(
		"Reload requested. Waiting for active jobs, then restarting this platform.",
	))
	.await
}

async fn booru_test(ctx: &dyn PrefixContext, raw: &str) -> Result<()> {
	let core = ctx.core();
	ensure!(
		ctx.user() == core.admin_user_id,
		"provider testing is administrator-only"
	);
	let mut split = raw.splitn(2, char::is_whitespace);
	let name = split.next().context("usage: booru-test <name> <tags>")?;
	let tags = split.next().unwrap_or("").trim();
	ensure!(!tags.is_empty(), "usage: booru-test <name> <tags>");
	let row = {
		core.db
			.lock()
			.expect("db mutex poisoned")
			.get_booru_by_name(name)?
			.context("provider not found")?
	};
	let booru = BooruConfig::from_row(&row)?;
	let tags = super::commands::test_tags(tags, &booru)?;
	let blacklist = core.blacklist(ctx.guild(), ctx.channel())?;
	ctx.defer().await?;
	commands_core::fetch_and_deliver(commands_core::DeliveryRequest {
		core,
		guild: ctx.guild(),
		channel: ctx.channel(),
		booru: &booru,
		tags: &tags,
		blacklist: &blacklist,
		test: true,
		ctx,
	})
	.await
}

async fn tag_test(ctx: &dyn PrefixContext, args: &[String]) -> Result<()> {
	let core = ctx.core();
	let Some(guild) = require_guild_channel(ctx).await? else {
		return Ok(());
	};
	ensure!(
		core.can_administer(ctx.user(), Some(guild))?,
		"tag testing is restricted to guild moderators and the administrator"
	);
	let tag = args.first().context("usage: tag-test <tag>")?;
	let Some(candidates) = commands_core::candidates(core, guild, ctx.channel(), tag, &[])? else {
		ctx.reply(PrefixReply::text(format!(
			"Unknown tag pattern `{tag}`. Use `{}help`.",
			core.command_prefix()
		)))
		.await?;
		return Ok(());
	};
	if candidates.is_empty() {
		return Ok(());
	}
	ctx.defer().await?;
	run_candidates(ctx, Some(guild), candidates, true).await
}

async fn custom_or_pattern(ctx: &dyn PrefixContext, command: &PrefixCommand) -> Result<()> {
	let core = ctx.core();
	let Some(guild) = require_guild_channel(ctx).await? else {
		return Ok(());
	};
	let tags = tags_from_raw(&command.raw_args);
	let Some(candidates) =
		commands_core::candidates(core, guild, ctx.channel(), &command.name, &tags)?
	else {
		ctx.reply(PrefixReply::text(format!(
			"Unknown command `{}{}`. Use `{}help`.",
			core.command_prefix(),
			command.name,
			core.command_prefix()
		)))
		.await?;
		return Ok(());
	};
	if candidates.is_empty() {
		return Ok(());
	}
	ctx.defer().await?;
	run_candidates(ctx, Some(guild), candidates, false).await
}

async fn run_candidates(
	ctx: &dyn PrefixContext,
	guild: Option<i64>,
	candidates: Vec<Candidate>,
	test: bool,
) -> Result<()> {
	let core = ctx.core();
	let attempts = core.setting_number("booru_fetch_retry_limit")? as usize + 1;
	let mut last_error = anyhow::anyhow!("no eligible image found");
	for attempt in 1..=attempts {
		let index = rand::rng().random_range(0..candidates.len());
		let (booru, tags, blacklist) = &candidates[index];
		match commands_core::fetch_and_deliver(commands_core::DeliveryRequest {
			core,
			guild,
			channel: ctx.channel(),
			booru,
			tags,
			blacklist,
			test,
			ctx,
		})
		.await
		{
			Ok(()) => return Ok(()),
			Err(err) => {
				last_error = err;
				cli::final_retry(&last_error.to_string(), attempt);
				if attempt < attempts {
					tokio::time::sleep(std::time::Duration::from_millis(
						250 * attempt.min(8) as u64,
					))
					.await;
				}
			}
		}
	}
	let i18n = core.channel_i18n(guild, ctx.channel());
	ctx.reply(PrefixReply::text(
		i18n.could_not_find_image(&last_error.to_string()),
	))
	.await?;
	Err(last_error)
}

async fn art_history(ctx: &dyn PrefixContext, args: &[String]) -> Result<()> {
	let core = ctx.core();
	let Some(guild) = require_guild_channel(ctx).await? else {
		return Ok(());
	};
	let count = args
		.first()
		.and_then(|value| value.parse::<usize>().ok())
		.unwrap_or(10)
		.clamp(1, 100_000);
	let entries = {
		core.db
			.lock()
			.expect("db mutex poisoned")
			.recent_art(ctx.channel(), count)?
	};
	let links = entries
		.into_iter()
		.map(|entry| entry.source_link)
		.collect::<Vec<_>>();
	let i18n = core.channel_i18n(Some(guild), ctx.channel());
	let summary = if links.is_empty() {
		i18n.art_history_no_links().to_string()
	} else if links.len() < count {
		i18n.art_history_showing_all(count, links.len())
	} else {
		i18n.art_history_showing_count(links.len())
	};
	let body = format!("{summary}\n{}", format_recent_art_links(&links));
	if body.chars().count() <= 1900 {
		ctx.reply(PrefixReply::text(body)).await
	} else {
		ctx.reply(PrefixReply {
			content: summary,
			attachment: Some((
				body.into_bytes(),
				i18n.art_history_attachment_filename().into(),
			)),
			dm_reference: None,
		})
		.await
	}
}

async fn dm(ctx: &dyn PrefixContext, args: &[String]) -> Result<()> {
	let core = ctx.core();
	let reference = args.first().context("usage: dm <reference>")?;
	let Some(post) = core.cached_posts.get(reference, ctx.channel())? else {
		ctx.reply(PrefixReply::text(
			"This post is no longer retained in history.",
		))
		.await?;
		return Ok(());
	};
	let mut content = super::post_cache::format_dm_content(&post);
	let mut attachment = None;
	if post.embed_image {
		let booru = {
			let db = core.db.lock().expect("db mutex poisoned");
			BooruConfig::from_row(
				&db.get_booru_by_name(&post.booru_name)?
					.context("provider no longer configured")?,
			)?
		};
		let image = core
			.fetch_client()?
			.inline_image(&booru, &post.image.image_url)
			.await?;
		attachment = Some((image.data, image.filename));
	}
	if content.is_empty() && attachment.is_none() {
		content = post.image.image_url;
	}
	match ctx
		.send_dm(
			ctx.user() as i64,
			PrefixReply {
				content,
				attachment,
				dm_reference: None,
			},
		)
		.await
	{
		Ok(()) => {
			ctx.reply(PrefixReply::text("Sent to your DMs.")).await?;
			Ok(())
		}
		Err(err) => {
			cli::error(&err.to_string(), "DM_ERROR");
			ctx.reply(PrefixReply::text(
				"Could not send the DM. Check that your DMs are open.",
			))
			.await
		}
	}
}

async fn administrate(ctx: &dyn PrefixContext, action: &str) -> Result<()> {
	let core = ctx.core();
	ensure!(
		core.can_administer(ctx.user(), ctx.guild())?,
		"administration requires administrator or guild-moderator access"
	);
	let text = super::admin::text::execute(core, ctx.guild(), ctx.channel(), ctx.user(), action);
	send_long(ctx, text).await
}

async fn send_long(ctx: &dyn PrefixContext, text: String) -> Result<()> {
	let mut chunks = Vec::new();
	let mut current = String::new();
	for line in text.lines() {
		if current.chars().count() + line.chars().count() + 1 > 1900 {
			if !current.is_empty() {
				chunks.push(std::mem::take(&mut current));
			}
			if line.chars().count() > 1900 {
				chunks.push(line.chars().take(1900).collect());
				continue;
			}
		}
		current.push_str(line);
		current.push('\n');
	}
	if !current.is_empty() {
		chunks.push(current);
	}
	if chunks.is_empty() {
		chunks.push("(empty)".to_string());
	}
	for (index, chunk) in chunks.into_iter().enumerate() {
		if index == 0 {
			ctx.reply(PrefixReply::text(chunk)).await?;
		} else {
			ctx.follow_up(PrefixReply::text(chunk)).await?;
		}
	}
	Ok(())
}

async fn require_guild_channel(ctx: &dyn PrefixContext) -> Result<Option<i64>> {
	let core = ctx.core();
	let Some(guild) = ctx.guild() else {
		ctx.reply(PrefixReply::text("Use this command in a server."))
			.await?;
		return Ok(None);
	};
	let (validated, allowed) = {
		let db = core.db.lock().expect("db mutex poisoned");
		(
			db.get_server(guild)?.is_some_and(|server| server.validated),
			db.has_channel(guild, ctx.channel())?,
		)
	};
	let i18n = core.channel_i18n(Some(guild), ctx.channel());
	if !validated {
		ctx.reply(PrefixReply::text(i18n.server_not_validated()))
			.await?;
		return Ok(None);
	}
	if !allowed && !core.can_administer(ctx.user(), Some(guild))? {
		ctx.reply(PrefixReply::text(i18n.channel_not_allowed()))
			.await?;
		return Ok(None);
	}
	Ok(Some(guild))
}

fn tags_from_raw(raw: &str) -> Vec<String> {
	if raw.contains(',') {
		raw.split(',')
			.map(|tag| tag.trim().to_string())
			.filter(|tag| !tag.is_empty())
			.collect()
	} else {
		raw.split_whitespace()
			.map(str::to_string)
			.collect::<Vec<_>>()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_prefix_commands() {
		let command = parse("!booru-test danbooru tag_one,tag_two", "!").unwrap();
		assert_eq!(command.name, "booru-test");
		assert_eq!(command.raw_args, "danbooru tag_one,tag_two");
		assert_eq!(command.args, ["danbooru", "tag_one,tag_two"]);

		let command = parse("?art-history 5", "?").unwrap();
		assert_eq!(command.name, "art-history");
		assert_eq!(command.args, ["5"]);
	}

	#[test]
	fn ignores_empty_or_unprefixed_messages() {
		assert!(parse("!   ", "!").is_none());
		assert!(parse("status", "!").is_none());
		assert!(parse("", "!").is_none());
	}

	#[test]
	fn comma_tags_keep_spaces_and_whitespace_tags_split() {
		assert_eq!(tags_from_raw("a b"), ["a", "b"]);
		assert_eq!(
			tags_from_raw(" long tag , second tag "),
			["long tag", "second tag"]
		);
	}
}
