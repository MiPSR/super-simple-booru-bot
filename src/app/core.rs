use std::{
	collections::HashMap,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	time::Duration,
};

use anyhow::Result;
use tokio::sync::Notify;

use crate::{
	booru::BooruClient, config::RuntimeValues, db::Database, i18n::I18n, pacing::ApiPacer,
};

use super::{JobTracker, PostCache};

/// Platform-neutral bot state shared by the Discord and Fluxer frontends.
pub(crate) struct Core {
	pub db: Arc<Mutex<Database>>,
	pub booru: BooruClient,
	pub pacer: ApiPacer,
	pub i18n: Arc<I18n>,
	pub i18ns: HashMap<String, I18n>,
	pub admin_user_id: u64,
	pub jobs: Arc<JobTracker>,
	pub reload_requested: Arc<AtomicBool>,
	pub reload_notify: Arc<Notify>,
	pub cached_posts: Arc<PostCache>,
	pub startup_error: Option<String>,
	pub bot_token: String,
}

impl Core {
	pub(crate) fn can_administer(&self, user: u64, guild: Option<i64>) -> Result<bool> {
		Ok(user == self.admin_user_id
			|| self
				.db
				.lock()
				.expect("db mutex poisoned")
				.is_moderator(user as i64, guild)?)
	}

	pub(crate) fn setting_number(&self, key: &str) -> Result<u64> {
		crate::settings::number(&self.db.lock().expect("db mutex poisoned"), key)
	}

	pub(crate) fn setting_string(&self, key: &str) -> Result<String> {
		crate::settings::get(&self.db.lock().expect("db mutex poisoned"), key)
	}

	pub(crate) fn prefix_enabled(&self) -> bool {
		self.setting_string("prefix_enabled").is_ok_and(|value| {
			matches!(
				value.trim().to_ascii_lowercase().as_str(),
				"true" | "1" | "yes" | "on"
			)
		})
	}

	pub(crate) fn command_prefix(&self) -> String {
		self.setting_string("command_prefix")
			.unwrap_or_else(|_| "!".to_string())
	}

	pub(crate) fn discord_pacer(&self) -> Result<ApiPacer> {
		Ok(self.pacer.with_interval(Duration::from_millis(
			self.setting_number("discord_api_pace_ms")?,
		)))
	}

	pub(crate) fn fetch_client(&self) -> Result<BooruClient> {
		let db = self.db.lock().expect("db mutex poisoned");
		let mut runtime = RuntimeValues {
			secrets: HashMap::from([
				("discord_token".into(), self.bot_token.clone()),
				("fluxer_token".into(), self.bot_token.clone()),
				("bot_token".into(), self.bot_token.clone()),
			]),
			settings: db.get_all_settings()?.into_iter().collect(),
			custom: HashMap::new(),
		};
		for (key, _) in crate::settings::DEFAULTS {
			runtime
				.settings
				.insert(key.to_string(), crate::settings::get(&db, key)?);
		}
		for param in db.get_all_booru_custom_parameters()? {
			runtime
				.custom
				.entry(param.booru_name)
				.or_default()
				.insert(param.key, param.value);
		}
		Ok(self.booru.with_runtime(runtime))
	}

	pub(crate) fn blacklist(&self, guild: Option<i64>, channel: i64) -> Result<Vec<String>> {
		let db = self.db.lock().expect("db mutex poisoned");
		let mut tags = crate::settings::get(&db, "booru_tag_blacklist")?
			.split(',')
			.map(|t| t.trim().to_ascii_lowercase())
			.filter(|t| !t.is_empty())
			.collect::<Vec<_>>();
		if let Some(guild) = guild
			&& let Some(config) = db.get_channel_config(guild, channel)?
		{
			tags.extend(config.banned_tags);
		}
		Ok(tags)
	}

	pub(crate) fn channel_i18n(&self, guild_id: Option<i64>, channel_id: i64) -> I18n {
		let Some(guild_id) = guild_id else {
			return (*self.i18n).clone();
		};

		let db = self.db.lock().expect("db mutex poisoned");
		if let Ok(Some(cfg)) = db.get_channel_config(guild_id, channel_id)
			&& let Some(ref lang) = cfg.language
			&& let Some(i18n) = self.i18ns.get(&lang.trim().to_ascii_lowercase())
		{
			return i18n.clone();
		}
		crate::settings::get(&db, "app_lang")
			.ok()
			.and_then(|lang| I18n::load(&lang).ok())
			.unwrap_or_else(|| (*self.i18n).clone())
	}

	pub(crate) fn request_reload(&self) -> Result<()> {
		anyhow::ensure!(self.jobs.begin_reload(), "reload already in progress");
		self.reload_requested.store(true, Ordering::Release);
		self.reload_notify.notify_one();
		Ok(())
	}
}

// Delivery locks are shared across all Core values so a channel cannot receive
// two simultaneous final deliveries even when both platforms use the same ID.
static DELIVERY_LOCKS: std::sync::OnceLock<
	Mutex<HashMap<i64, std::sync::Weak<tokio::sync::Mutex<()>>>>,
> = std::sync::OnceLock::new();

pub(crate) fn global_delivery_lock(channel: i64) -> Arc<tokio::sync::Mutex<()>> {
	let locks = DELIVERY_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
	let mut locks = locks.lock().expect("delivery mutex poisoned");
	locks.retain(|_, value| value.strong_count() > 0);
	if let Some(lock) = locks.get(&channel).and_then(std::sync::Weak::upgrade) {
		return lock;
	}
	let lock = Arc::new(tokio::sync::Mutex::new(()));
	locks.insert(channel, Arc::downgrade(&lock));
	lock
}
