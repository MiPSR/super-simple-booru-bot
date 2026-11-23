use anyhow::{Context, Result, bail};

use crate::db::{BooruRow, ChannelConfig};

use super::super::Core;

pub(crate) fn execute(
	core: &Core,
	guild: Option<i64>,
	channel: i64,
	user: u64,
	action: &str,
) -> String {
	let parts: Vec<&str> = action.trim().splitn(2, char::is_whitespace).collect();
	let category = parts[0].trim().to_ascii_lowercase();
	let rest = parts.get(1).copied().unwrap_or("").trim();

	if user != core.admin_user_id {
		let Some(current_guild) = guild else {
			return "moderators need server context; run this in a server channel".to_string();
		};
		match category.as_str() {
			"" | "help" | "tags" | "channel" | "channels" | "patterns" | "pattern"
			| "server-tags" => {}
			"server" | "servers" => {
				let (target_guild, after) = match parse_id(rest, Some(current_guild)) {
					Ok(value) => value,
					Err(err) => return err_text(err),
				};
				if target_guild != current_guild {
					return "Moderators can only manage their own guild.".to_string();
				}
				let sub = after
					.split_whitespace()
					.next()
					.unwrap_or("")
					.to_ascii_lowercase();
				if !matches!(sub.as_str(), "channel" | "patterns" | "tags") {
					return "Only the administrator can validate or list servers.".to_string();
				}
			}
			_ => return "Only the administrator can manage shared configuration.".to_string(),
		}
	}

	match category.as_str() {
		"" | "help" => core.i18n.administrate_help().to_string(),
		"booru" => booru(core, rest),
		"tags" => tags(core, rest),
		"moderator" | "moderators" | "mod" => moderator(core, rest),
		"server" | "servers" => server(core, rest, guild, channel),
		"channel" | "channels" => channel_action(core, rest, guild, channel),
		"patterns" | "pattern" | "tags-whitelist" | "server-tags" => server_tags(core, rest, guild),
		"setting" | "settings" => setting(core, rest),
		_ => format!("unknown administrate command: {category}\nuse 'administrate help'"),
	}
}

fn usage(text: &str) -> String {
	format!("usage: {text}")
}

fn split_required<'a>(input: &'a str, help: &str) -> Result<(&'a str, &'a str)> {
	let parts: Vec<&str> = input.trim().splitn(2, char::is_whitespace).collect();
	if parts.len() < 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
		bail!("{help}");
	}
	Ok((parts[0].trim(), parts[1].trim()))
}

fn single_arg<'a>(input: &'a str, help: &str) -> Result<&'a str> {
	let value = input.trim();
	if value.is_empty() || value.contains(char::is_whitespace) {
		bail!("{help}");
	}
	Ok(value)
}

fn parse_csv(input: &str) -> Vec<String> {
	input
		.split(',')
		.map(|s| s.trim().to_string())
		.filter(|s| !s.is_empty())
		.collect()
}

fn parse_id(input: &str, default: Option<i64>) -> Result<(i64, &str)> {
	if input.trim().is_empty() {
		return default
			.map(|id| (id, ""))
			.context("no id provided; specify one or use this command in a server channel");
	}
	let parts: Vec<&str> = input.trim().splitn(2, char::is_whitespace).collect();
	if let Ok(id) = parts[0].parse::<i64>() {
		Ok((id, parts.get(1).copied().unwrap_or("").trim()))
	} else {
		default
			.map(|id| (id, input.trim()))
			.context(format!("expected a numeric id, got '{}'", parts[0]))
	}
}

fn booru(core: &Core, rest: &str) -> String {
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let sub = parts[0].trim().to_ascii_lowercase();
	let args = parts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"" | "help" => {
			usage("booru list|show|add|edit|delete|enable|disable|parameter|tag-pattern")
		}
		"list" => with_db(core, |db| {
			let boorus = db.get_all_boorus()?;
			if boorus.is_empty() {
				return Ok("no boorus".to_string());
			}
			Ok(boorus
				.iter()
				.map(|b| {
					format!(
						"{} [{}]",
						b.name,
						if b.enabled { "enabled" } else { "disabled" }
					)
				})
				.collect::<Vec<_>>()
				.join("\n"))
		}),
		"show" => match single_arg(args, "usage: booru show <name>") {
			Ok(name) => with_db(core, |db| match db.get_booru_by_name(name)? {
				Some(row) => Ok(format_booru(&row)),
				None => Ok(format!("booru {name} not found")),
			}),
			Err(err) => err_text(err),
		},
		"add" => match split_required(args, "usage: booru add <name> <json>") {
			Ok((name, json)) => match crate::app::admin::booru::from_json(name, json) {
				Ok(row) => with_db(core, |db| {
					db.add_booru(&row)?;
					Ok(format!("booru {name} added"))
				}),
				Err(err) => err_text(err),
			},
			Err(err) => err_text(err),
		},
		"edit" => match split_required(args, "usage: booru edit <name> <field> <value>") {
			Ok((name, rest)) => {
				match split_required(rest, "usage: booru edit <name> <field> <value>") {
					Ok((field, value)) => with_db(core, |db| {
						let Some(mut row) = db.get_booru_by_name(name)? else {
							return Ok(format!("booru {name} not found"));
						};
						crate::app::admin::booru::edit_field(&mut row, field, value)?;
						db.update_booru(&row)?;
						Ok(format!("booru {name} updated"))
					}),
					Err(err) => err_text(err),
				}
			}
			Err(err) => err_text(err),
		},
		"delete" => match single_arg(args, "usage: booru delete <name>") {
			Ok(name) => with_db(core, |db| {
				db.delete_booru(name)?;
				Ok(format!("booru {name} deleted"))
			}),
			Err(err) => err_text(err),
		},
		"enable" | "disable" => match single_arg(args, "usage: booru enable|disable <name>") {
			Ok(name) => with_db(core, |db| {
				db.set_booru_enabled(name, sub == "enable")?;
				Ok(format!(
					"booru {name} {}",
					if sub == "enable" {
						"enabled"
					} else {
						"disabled"
					}
				))
			}),
			Err(err) => err_text(err),
		},
		"parameter" | "parameters" | "param" | "params" => parameters(core, args),
		"tag-pattern" | "tag-patterns" => tag_patterns(core, args),
		_ => usage("booru list|show|add|edit|delete|enable|disable|parameter|tag-pattern"),
	}
}

fn format_booru(row: &BooruRow) -> String {
	format!(
		"name: {}\nenabled: {}\nembed_image: {}\nmax_tags: {}\nsupports_character: {}\npage_size: {}\npage_base: {}\ntag_separator: {}\nencode_tag_separator: {}\ntag_spaces_as_plus: {}\ncharacter_space_replacement: {}\npace_ms: {}\ndescription: {}\ncount_url: {}\ncount_path: {}\nposts_url: {}\nposts_path: {}\nfile_url_path: {}\nsource_url_path: {}\ndetail_url: {}\ndetail_id_path: {}\ndetail_file_url_path: {}\ndetail_source_url_path: {}\npost_url: {}\nheaders: <{} configured; values hidden>\nenv_params: {}\nfetch_options: {}",
		row.name,
		row.enabled,
		row.embed_image,
		row.max_tags,
		row.supports_character,
		row.page_size,
		row.page_base,
		row.tag_separator,
		row.encode_tag_separator,
		row.tag_spaces_as_plus,
		row.character_space_replacement,
		row.pace_ms,
		if row.description.is_empty() {
			"null"
		} else {
			&row.description
		},
		row.count_url.as_deref().unwrap_or("null"),
		row.count_path_json,
		row.posts_url,
		row.posts_path_json,
		row.file_url_path_json,
		row.source_url_path_json,
		row.detail_url.as_deref().unwrap_or("null"),
		row.detail_id_path_json,
		row.detail_file_url_path_json,
		row.detail_source_url_path_json,
		row.post_url.as_deref().unwrap_or("null"),
		serde_json::from_str::<serde_json::Value>(&row.headers_json)
			.ok()
			.and_then(|v| v.as_object().map(|o| o.len()))
			.unwrap_or(0),
		row.env_params_json,
		row.fetch_options_json,
	)
}

fn parameters(core: &Core, rest: &str) -> String {
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let sub = parts[0].trim().to_ascii_lowercase();
	let args = parts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"set" => match split_required(args, "usage: booru parameter set <booru> <key> <value>") {
			Ok((booru, rest)) => {
				match split_required(rest, "usage: booru parameter set <booru> <key> <value>") {
					Ok((key, value)) => with_db(core, |db| {
						db.set_booru_custom_parameter(booru, key, value)?;
						Ok(format!("booru parameter {key} set for {booru}"))
					}),
					Err(err) => err_text(err),
				}
			}
			Err(err) => err_text(err),
		},
		"delete" | "remove" => {
			match split_required(args, "usage: booru parameter delete <booru> <key>") {
				Ok((booru, key)) => with_db(core, |db| {
					db.delete_booru_custom_parameter(booru, key)?;
					Ok(format!("booru parameter {key} deleted for {booru}"))
				}),
				Err(err) => err_text(err),
			}
		}
		"list" => match single_arg(args, "usage: booru parameter list <booru>") {
			Ok(booru) => with_db(core, |db| {
				let params = db.get_booru_custom_parameters(booru)?;
				if params.is_empty() {
					Ok(format!("no booru parameters for {booru}"))
				} else {
					Ok(params
						.iter()
						.map(|p| format!("{} = {}", p.key, p.value))
						.collect::<Vec<_>>()
						.join("\n"))
				}
			}),
			Err(err) => err_text(err),
		},
		_ => usage("booru parameter set|delete|list"),
	}
}

fn tag_patterns(core: &Core, rest: &str) -> String {
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let sub = parts[0].trim().to_ascii_lowercase();
	let args = parts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"add" => match split_required(
			args,
			"usage: booru tag-pattern add <booru> <name> <included_csv> [excluded_csv]",
		) {
			Ok((booru, rest)) => {
				let items: Vec<&str> = rest.splitn(3, char::is_whitespace).collect();
				if items.len() < 2 {
					return usage(
						"booru tag-pattern add <booru> <name> <included_csv> [excluded_csv]",
					);
				}
				let included = parse_csv(items[1]);
				let excluded = items.get(2).map(|v| parse_csv(v)).unwrap_or_default();
				with_db(core, |db| {
					db.add_tag_pattern(items[0], booru, &included, &excluded)?;
					Ok(format!("tag pattern '{}' added for {booru}", items[0]))
				})
			}
			Err(err) => err_text(err),
		},
		"remove" | "delete" => {
			match split_required(args, "usage: booru tag-pattern remove <booru> <name>") {
				Ok((booru, name)) => with_db(core, |db| {
					db.delete_tag_pattern(name, Some(booru))?;
					Ok(format!("tag pattern '{name}' removed from {booru}"))
				}),
				Err(err) => err_text(err),
			}
		}
		"list" => match single_arg(args, "usage: booru tag-pattern list <booru>") {
			Ok(booru) => with_db(core, |db| {
				let Some(row) = db.get_booru_by_name(booru)? else {
					return Ok(format!("booru {booru} not found"));
				};
				let patterns = db.get_tag_patterns(None)?;
				let mut lines = Vec::new();
				for pattern in patterns.iter().filter(|p| p.booru_id == row.id) {
					let entries = db.get_pattern_entries(pattern.id).unwrap_or_default();
					lines.push(format!("{} ({} entries)", pattern.name, entries.len()));
					for entry in entries {
						lines.push(format!(
							"  {}{}",
							if entry.is_excluded { "-" } else { "+" },
							entry.tag
						));
					}
				}
				if lines.is_empty() {
					Ok(format!("no tag patterns for {booru}"))
				} else {
					Ok(lines.join("\n"))
				}
			}),
			Err(err) => err_text(err),
		},
		_ => usage("booru tag-pattern add|remove|list"),
	}
}

fn tags(core: &Core, rest: &str) -> String {
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let first = parts[0].trim().to_ascii_lowercase();
	if first == "list" || first.is_empty() {
		return with_db(core, |db| {
			let patterns = db.get_tag_patterns(None)?;
			if patterns.is_empty() {
				return Ok("no tag patterns".to_string());
			}
			let mut lines = Vec::new();
			for pattern in &patterns {
				let booru = db
					.get_booru_by_id(pattern.booru_id)?
					.map(|b| b.name)
					.unwrap_or_else(|| format!("<missing:{}>", pattern.booru_id));
				let entries = db.get_pattern_entries(pattern.id)?;
				lines.push(format!(
					"{} -> {} ({} entries)",
					pattern.name,
					booru,
					entries.len()
				));
				for entry in entries {
					lines.push(format!(
						"  {}{}",
						if entry.is_excluded { "-" } else { "+" },
						entry.tag
					));
				}
			}
			Ok(lines.join("\n"))
		});
	}

	let name = parts[0];
	let args = parts.get(1).copied().unwrap_or("").trim();
	let subparts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
	let sub = subparts[0].trim().to_ascii_lowercase();
	let subargs = subparts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"add" => {
			let items: Vec<&str> = subargs.splitn(3, char::is_whitespace).collect();
			if items.len() < 2 {
				return usage("tags <name> add <booru> <included_csv> [excluded_csv]");
			}
			let included = parse_csv(items[1]);
			let excluded = items.get(2).map(|v| parse_csv(v)).unwrap_or_default();
			with_db(core, |db| {
				db.add_tag_pattern(name, items[0], &included, &excluded)?;
				Ok(format!("tag pattern '{name}' added for {}", items[0]))
			})
		}
		"delete" | "remove" => with_db(core, |db| {
			db.delete_tag_pattern(name, (!subargs.is_empty()).then_some(subargs))?;
			Ok(format!("tag pattern '{name}' deleted"))
		}),
		_ => usage(
			"tags list | tags <name> add <booru> <included_csv> [excluded_csv] | tags <name> delete [booru]",
		),
	}
}

fn moderator(core: &Core, rest: &str) -> String {
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let first = parts[0].trim().to_ascii_lowercase();
	if first == "list" || first.is_empty() {
		return with_db(core, |db| {
			let moderators = db.list_moderators()?;
			if moderators.is_empty() {
				Ok("no moderators".to_string())
			} else {
				Ok(moderators
					.iter()
					.map(|(user, guild)| {
						format!(
							"{user} guild={}",
							guild.map(|g| g.to_string()).unwrap_or_else(|| "-".into())
						)
					})
					.collect::<Vec<_>>()
					.join("\n"))
			}
		});
	}

	let user_id: i64 = match parts[0].parse() {
		Ok(id) => id,
		Err(_) => return format!("invalid user_id '{}'", parts[0]),
	};
	let args = parts.get(1).copied().unwrap_or("").trim();
	let subparts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
	let sub = subparts[0].trim().to_ascii_lowercase();
	let subargs = subparts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"add" => {
			let Ok(guild_id) = subargs.parse::<i64>() else {
				return usage("moderator <user_id> add <guild_id>");
			};
			with_db(core, |db| {
				db.add_moderator(user_id, Some(guild_id), core.admin_user_id as i64)?;
				Ok(format!("moderator {user_id} added to guild {guild_id}"))
			})
		}
		"remove" | "delete" => {
			let Ok(guild_id) = subargs.parse::<i64>() else {
				return usage("moderator <user_id> remove <guild_id>");
			};
			with_db(core, |db| {
				db.remove_moderator(user_id, Some(guild_id))?;
				Ok(format!("moderator {user_id} removed from guild {guild_id}"))
			})
		}
		_ => usage("moderator list | moderator <user_id> add|remove <guild_id>"),
	}
}

fn server(core: &Core, rest: &str, default_guild: Option<i64>, default_channel: i64) -> String {
	let (guild_id, rest) = match parse_id(rest, default_guild) {
		Ok(value) => value,
		Err(err) => return err_text(err),
	};
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let sub = parts[0].trim().to_ascii_lowercase();
	let args = parts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"validate" | "unvalidate" => with_db(core, |db| {
			db.set_validated(guild_id, sub == "validate")?;
			Ok(format!(
				"server {guild_id} {}",
				if sub == "validate" {
					"validated"
				} else {
					"unvalidated"
				}
			))
		}),
		"list" => with_db(core, |db| {
			let servers = db.get_all_servers()?;
			if servers.is_empty() {
				Ok("no servers".to_string())
			} else {
				Ok(servers
					.iter()
					.map(|s| {
						format!(
							"{} {} [{}]",
							s.guild_id,
							s.name,
							if s.validated { "validated" } else { "pending" }
						)
					})
					.collect::<Vec<_>>()
					.join("\n"))
			}
		}),
		"validated" => with_db(core, |db| {
			let servers = db.get_validated_servers()?;
			if servers.is_empty() {
				Ok("no validated servers".to_string())
			} else {
				Ok(servers
					.iter()
					.map(|s| format!("{} {}", s.guild_id, s.name))
					.collect::<Vec<_>>()
					.join("\n"))
			}
		}),
		"channel" => server_channel(core, guild_id, args, default_channel),
		"patterns" | "tags" => server_tags_for(core, guild_id, args),
		"" | "help" => {
			usage("server [guild_id] validate|unvalidate|list|validated|channel|patterns")
		}
		_ => usage("server [guild_id] validate|unvalidate|list|validated|channel|patterns"),
	}
}

fn server_channel(core: &Core, guild_id: i64, args: &str, default_channel: i64) -> String {
	let (channel_id, rest) = match parse_id(args, Some(default_channel)) {
		Ok(value) => value,
		Err(err) => return err_text(err),
	};
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let sub = parts[0].trim().to_ascii_lowercase();
	let subargs = parts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"add" => with_db(core, |db| {
			if db.has_channel(guild_id, channel_id)? {
				bail!("channel {channel_id} is already configured for server {guild_id}");
			}
			db.set_channel_config(&ChannelConfig {
				guild_id,
				channel_id,
				language: None,
				banned_tags: Vec::new(),
			})?;
			Ok(format!("channel {channel_id} added to server {guild_id}"))
		}),
		"remove" | "delete" => with_db(core, |db| {
			db.remove_channel(guild_id, channel_id)?;
			Ok(format!(
				"channel {channel_id} removed from server {guild_id}"
			))
		}),
		"set" => {
			let (language, banned) = parse_channel_settings(subargs);
			with_db(core, |db| {
				db.set_channel_config(&ChannelConfig {
					guild_id,
					channel_id,
					language,
					banned_tags: banned,
				})?;
				Ok(format!("channel {channel_id} configured"))
			})
		}
		"list" => with_db(core, |db| {
			let channels = db.get_guild_channels(guild_id)?;
			if channels.is_empty() {
				Ok("no channels configured".to_string())
			} else {
				Ok(channels
					.iter()
					.map(|c| {
						format!(
							"channel={} lang={} banned={:?}",
							c.channel_id,
							c.language.as_deref().unwrap_or("default"),
							c.banned_tags
						)
					})
					.collect::<Vec<_>>()
					.join("\n"))
			}
		}),
		_ => {
			usage("server <guild_id> channel <channel_id> add|remove|set|list [lang] [banned_csv]")
		}
	}
}

fn parse_channel_settings(input: &str) -> (Option<String>, Vec<String>) {
	let parts: Vec<&str> = input.splitn(2, char::is_whitespace).collect();
	let language = parts
		.first()
		.map(|v| v.trim())
		.filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("default"))
		.map(str::to_string);
	let banned = parts.get(1).map(|v| parse_csv(v)).unwrap_or_default();
	(language, banned)
}

fn channel_action(core: &Core, rest: &str, guild: Option<i64>, default_channel: i64) -> String {
	let Some(guild_id) = guild else {
		return err_text(anyhow::anyhow!(
			"no server context; use 'server <guild_id> channel ...'"
		));
	};
	let (channel_id, rest) = match parse_id(rest, Some(default_channel)) {
		Ok(value) => value,
		Err(err) => return err_text(err),
	};
	server_channel(
		core,
		guild_id,
		&format!("{channel_id} {rest}"),
		default_channel,
	)
}

fn server_tags(core: &Core, rest: &str, guild: Option<i64>) -> String {
	let Some(guild_id) = guild else {
		return err_text(anyhow::anyhow!(
			"no server context; use 'server <guild_id> patterns ...'"
		));
	};
	server_tags_for(core, guild_id, rest)
}

fn server_tags_for(core: &Core, guild_id: i64, rest: &str) -> String {
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let sub = parts[0].trim().to_ascii_lowercase();
	let args = parts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"add" => match single_arg(args, "usage: patterns add <name>") {
			Ok(name) => with_db(core, |db| {
				db.add_server_tag(guild_id, name)?;
				Ok(format!("pattern {name} enabled in server {guild_id}"))
			}),
			Err(err) => err_text(err),
		},
		"remove" | "delete" => match single_arg(args, "usage: patterns remove <name>") {
			Ok(name) => with_db(core, |db| {
				db.remove_server_tag(guild_id, name)?;
				Ok(format!("pattern {name} removed from server {guild_id}"))
			}),
			Err(err) => err_text(err),
		},
		"list" | "" => with_db(core, |db| {
			let tags = db.get_server_tags(guild_id)?;
			if tags.is_empty() {
				Ok("no patterns enabled".to_string())
			} else {
				Ok(tags.join("\n"))
			}
		}),
		_ => usage("patterns add|remove|list [name]"),
	}
}

fn setting(core: &Core, rest: &str) -> String {
	let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
	let first = parts[0].trim().to_ascii_lowercase();
	if first == "list" || first.is_empty() {
		return with_db(core, |db| {
			let settings = db.get_all_settings()?;
			if settings.is_empty() {
				Ok("no overridden settings".to_string())
			} else {
				Ok(settings
					.iter()
					.map(|(key, value)| format!("{key} = {value}"))
					.collect::<Vec<_>>()
					.join("\n"))
			}
		});
	}

	let key = parts[0];
	let args = parts.get(1).copied().unwrap_or("").trim();
	let subparts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
	let sub = subparts[0].trim().to_ascii_lowercase();
	let value = subparts.get(1).copied().unwrap_or("").trim();
	match sub.as_str() {
		"get" => with_db(core, |db| match db.get_setting(key)? {
			Some(value) => Ok(format!("{key} = {value}")),
			None => Ok(format!(
				"{key} is not overridden (default: {})",
				crate::settings::default_value(db, key)
			)),
		}),
		"set" => {
			if value.is_empty() {
				return usage("setting <key> set <value>");
			}
			if let Err(err) = crate::settings::validate(key, value) {
				return err_text(err);
			}
			with_db(core, |db| {
				db.set_setting(key, value)?;
				Ok(format!("{key} set"))
			})
		}
		"delete" | "unset" => with_db(core, |db| {
			db.delete_setting(key)?;
			Ok(format!("{key} deleted"))
		}),
		_ => usage("setting list | setting <key> get|set|delete [value]"),
	}
}

fn with_db(core: &Core, f: impl FnOnce(&crate::db::Database) -> Result<String>) -> String {
	let db = core.db.lock().expect("db mutex poisoned");
	match f(&db) {
		Ok(text) => text,
		Err(err) => err_text(err),
	}
}

fn err_text(err: anyhow::Error) -> String {
	format!("error: {err:#}")
}

#[cfg(test)]
mod tests {
	use std::{
		collections::HashMap,
		sync::{Arc, Mutex},
		time::Duration,
	};

	use tokio::sync::Notify;

	use crate::{
		app::{Core, JobTracker, PostCache},
		booru::BooruClient,
		config::RuntimeValues,
		db::Database,
		i18n::I18n,
		pacing::ApiPacer,
		platform::PlatformKind,
	};

	use super::*;

	fn core() -> Arc<Core> {
		let db = Arc::new(Mutex::new(
			Database::open(":memory:", 3, PlatformKind::Discord).unwrap(),
		));
		let runtime = Arc::new(RuntimeValues {
			secrets: HashMap::new(),
			settings: HashMap::new(),
			custom: HashMap::new(),
		});
		let mut i18ns = HashMap::new();
		for lang in I18n::available_languages() {
			i18ns.insert(lang.to_string(), I18n::load(lang).unwrap());
		}
		Arc::new(Core {
			db: Arc::clone(&db),
			booru: BooruClient::new(ApiPacer::new(Duration::ZERO), runtime).unwrap(),
			pacer: ApiPacer::new(Duration::ZERO),
			i18n: Arc::new(I18n::load("en").unwrap()),
			i18ns,
			admin_user_id: 1,
			jobs: Arc::new(JobTracker::new()),
			reload_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
			reload_notify: Arc::new(Notify::new()),
			cached_posts: Arc::new(PostCache::new(db)),
			startup_error: None,
			bot_token: "test".into(),
		})
	}

	#[test]
	fn administrate_text_commands_respect_scope_and_update_the_database() {
		let core = core();
		{
			let db = core.db.lock().unwrap();
			db.add_server(1, "test").unwrap();
		}

		assert_eq!(
			execute(&core, Some(1), 2, 1, "server validate"),
			"server 1 validated"
		);
		assert!(
			core.db
				.lock()
				.unwrap()
				.get_server(1)
				.unwrap()
				.unwrap()
				.validated
		);
		assert_eq!(
			execute(&core, Some(1), 2, 2, "booru list"),
			"Only the administrator can manage shared configuration."
		);
		assert_eq!(
			execute(&core, Some(1), 2, 2, "server 999 channel 2 add"),
			"Moderators can only manage their own guild."
		);
		assert_eq!(
			execute(&core, Some(1), 2, 1, "channel 2 add"),
			"channel 2 added to server 1"
		);
		assert_eq!(
			execute(&core, Some(1), 2, 1, "setting prefix_enabled set true"),
			"prefix_enabled set"
		);
		assert_eq!(
			execute(&core, Some(1), 2, 1, "setting prefix_enabled get"),
			"prefix_enabled = true"
		);
		assert_eq!(
			execute(&core, Some(1), 2, 1, "booru show missing"),
			"booru missing not found"
		);
	}
}
