use anyhow::{Result, ensure};

use crate::{booru::ImageResult, cli, config::BooruConfig};

use super::{
	Core,
	post_cache::CachedPost,
	prefix::{PrefixContext, PrefixReply},
	responses::{
		custom_command_name, custom_tag_count, discord_name_component, format_image_response,
		format_inline_image_response, log_final_selection, normalize_user_tag,
	},
};

pub(crate) type Candidate = (BooruConfig, Vec<String>, Vec<String>);

pub(crate) fn candidates(
	core: &Core,
	guild: i64,
	channel: i64,
	name: &str,
	tag_values: &[String],
) -> Result<Option<Vec<Candidate>>> {
	let mut blacklist = core.blacklist(Some(guild), channel)?;
	let i18n = core.channel_i18n(Some(guild), channel);
	let db = core.db.lock().expect("db mutex poisoned");
	for row in db.get_enabled_boorus()? {
		if custom_command_name(&row.name) == name {
			let booru = BooruConfig::from_row(&row)?;
			let tags = tag_values
				.iter()
				.take(custom_tag_count(&booru))
				.map(|tag| normalize_user_tag(tag, &booru.character_space_replacement))
				.filter(|tag| !tag.is_empty())
				.collect::<Vec<_>>();
			ensure!(!tags.is_empty(), i18n.custom_command_no_tags());
			ensure!(
				tags.iter().all(|tag| tag.chars().count() <= 256)
					&& tags.iter().map(String::len).sum::<usize>() <= 4096,
				"tag query is too long"
			);
			return Ok(Some(vec![(booru, tags, blacklist)]));
		}
	}
	let Some(pattern) = db
		.get_unique_pattern_names()?
		.into_iter()
		.find(|pattern| pattern == name || discord_name_component(pattern) == name)
	else {
		return Ok(None);
	};
	// Never bypass the server whitelist: moderators are guild-scoped too.
	ensure!(
		db.get_server_tags(guild)?.contains(&pattern),
		i18n.tag_not_registered(&pattern)
	);
	let mut candidates = Vec::new();
	for pattern_row in db.get_tag_patterns(Some(&pattern))? {
		let Some(row) = db
			.get_booru_by_id(pattern_row.booru_id)?
			.filter(|booru| booru.enabled)
		else {
			continue;
		};
		let mut tags = Vec::new();
		let base = blacklist.len();
		for entry in db.get_pattern_entries(pattern_row.id)? {
			if entry.is_excluded {
				blacklist.push(entry.tag);
			} else {
				tags.push(entry.tag);
			}
		}
		if !tags.is_empty() {
			candidates.push((BooruConfig::from_row(&row)?, tags, blacklist.clone()));
		}
		blacklist.truncate(base);
	}
	ensure!(
		!candidates.is_empty(),
		"no enabled provider for this pattern"
	);
	Ok(Some(candidates))
}

pub(crate) struct DeliveryRequest<'a> {
	pub core: &'a Core,
	pub guild: Option<i64>,
	pub channel: i64,
	pub booru: &'a BooruConfig,
	pub tags: &'a [String],
	pub blacklist: &'a [String],
	pub test: bool,
	pub ctx: &'a dyn PrefixContext,
}

pub(crate) async fn fetch_and_deliver(request: DeliveryRequest<'_>) -> Result<()> {
	let DeliveryRequest {
		core,
		guild,
		channel,
		booru,
		tags,
		blacklist,
		test,
		ctx,
	} = request;
	let history = |source: &str| {
		core.db
			.lock()
			.expect("db mutex poisoned")
			.art_history_exists(source, channel)
			.unwrap_or(true)
	};
	let client = core.fetch_client()?;
	let image = client
		.random_image(
			booru,
			tags,
			blacklist,
			if test { None } else { Some(&history) },
		)
		.await?;
	// Serialize only the final deduplication/delivery/commit, never hold a DB lock over await.
	let lock = super::core::global_delivery_lock(channel);
	let _delivery = lock.lock().await;
	if !test && let Some(source) = image.history_source() {
		ensure!(
			!core
				.db
				.lock()
				.expect("db mutex poisoned")
				.art_history_exists(source, channel)?,
			"post was already delivered in this channel"
		);
	}
	let reference = deliver_image(core, channel, ctx, booru, &image).await?;
	if !test && let Some(source) = image.history_source() {
		let db = core.db.lock().expect("db mutex poisoned");
		ensure!(
			db.register_art(source, channel, guild, Some(&booru.name))?,
			"history changed during delivery"
		);
		db.link_delivered_post(&reference, source, channel)?;
	}
	Ok(())
}

async fn deliver_image(
	core: &Core,
	channel: i64,
	ctx: &dyn PrefixContext,
	booru: &BooruConfig,
	image: &ImageResult,
) -> Result<String> {
	let (content, attachment) = if booru.embed_image {
		let inline_image = core
			.fetch_client()?
			.inline_image(booru, &image.image_url)
			.await?;
		log_final_selection(image, &inline_image);
		let content =
			format_inline_image_response(booru, image, inline_image.compressed).unwrap_or_default();
		(content, Some((inline_image.data, inline_image.filename)))
	} else {
		cli::final_kept(
			image.post_url.as_deref(),
			image.upstream_source_url.as_deref(),
			&image.image_url,
		);
		(format_image_response(booru, image, false), None)
	};

	let cache_id = core.cached_posts.store(
		CachedPost {
			image: image.clone(),
			booru_name: booru.name.clone(),
			embed_image: booru.embed_image,
		},
		channel,
	)?;
	let reply = PrefixReply {
		content,
		attachment,
		dm_reference: Some(cache_id.clone()),
	};
	if let Err(err) = ctx.reply(reply).await {
		core.cached_posts.remove(&cache_id)?;
		return Err(err);
	}
	Ok(cache_id)
}
