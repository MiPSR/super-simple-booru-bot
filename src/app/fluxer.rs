use std::{
	ops::Deref,
	sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use fluxer_neptunium::{
	cache::{Cached, CachedMessage},
	cached_payload::{CachedGuildCreate, CachedMessageCreate, CachedReady},
	events::{
		EventHandler,
		context::{Context, FileUploadParams},
	},
	http::endpoints::channel::{
		AttachmentBase, AttachmentRequest, CreateMessageBody, CreatePrivateChannel, EditMessageBody,
	},
	model::id::{Id, marker::UserMarker},
	prelude::*,
};

use crate::{cli, platform::PlatformKind};

use super::{
	Core,
	prefix::{self, PrefixContext, PrefixReply},
	secrets::FluxerSecret,
};

pub(crate) struct FluxerHandler {
	pub(crate) core: Arc<Core>,
}

impl Deref for FluxerHandler {
	type Target = Core;

	fn deref(&self) -> &Self::Target {
		&self.core
	}
}

#[async_trait]
impl EventHandler for FluxerHandler {
	async fn on_ready(&self, _ctx: Context, _data: Arc<CachedReady>) -> Result<(), EventError> {
		cli::bot_loaded();
		Ok(())
	}

	async fn on_guild_create(
		&self,
		_ctx: Context,
		data: Arc<CachedGuildCreate>,
	) -> Result<(), EventError> {
		let guild_id = data.id.into_inner() as i64;
		let name = data.guild.name.clone();
		let result: anyhow::Result<()> = (|| {
			let db = self.db.lock().expect("db mutex poisoned");
			let is_new = db.get_server(guild_id)?.is_none();
			db.add_server(guild_id, &name)?;
			if is_new {
				cli::server_ready();
			}
			Ok(())
		})();
		if let Err(err) = result {
			cli::error(&format!("{err:#}"), "ERROR");
		}
		Ok(())
	}

	async fn on_message_create(
		&self,
		ctx: Context,
		data: Arc<CachedMessageCreate>,
	) -> Result<(), EventError> {
		let message = &data.message;
		if message.author.bot {
			return Ok(());
		}
		if let Some(self_id) = ctx.get_user_id_from_token()
			&& message.author.id == self_id
		{
			return Ok(());
		}
		if !self.prefix_enabled() {
			return Ok(());
		}
		let prefix = self.command_prefix();
		let Some(command) = prefix::parse(&message.content, &prefix) else {
			return Ok(());
		};
		let mut limit = self.setting_number("max_concurrent_jobs").unwrap_or(4) as usize;
		if matches!(command.name.as_str(), "reload") {
			limit = limit.saturating_add(1);
		}
		let Some(_job) = self.jobs.try_start(limit) else {
			let responder = FluxerPrefix::new(
				&self.core,
				&ctx,
				data.guild_id.map(|guild| guild.into_inner() as i64),
				message.channel_id.into_inner() as i64,
				message.author.id.into_inner() as i64,
			);
			let _ = responder
				.reply(PrefixReply::text(
					"The bot is busy or reloading. Please try again shortly.",
				))
				.await;
			return Ok(());
		};
		let responder = FluxerPrefix::new(
			&self.core,
			&ctx,
			data.guild_id.map(|guild| guild.into_inner() as i64),
			message.channel_id.into_inner() as i64,
			message.author.id.into_inner() as i64,
		);
		if let Err(err) = prefix::execute(&responder, command).await {
			cli::error(&format!("{err:#}"), "ERROR");
			let _ = responder
				.reply(PrefixReply::text(format!(
					"Operation failed: {}",
					super::truncate_for_dm(&err.to_string(), 1500)
				)))
				.await;
		}
		Ok(())
	}
}

struct FluxerPrefix<'a> {
	core: &'a Core,
	ctx: &'a Context,
	guild: Option<i64>,
	channel: i64,
	user: i64,
	placeholder: Mutex<Option<Cached<CachedMessage>>>,
}

impl<'a> FluxerPrefix<'a> {
	fn new(core: &'a Core, ctx: &'a Context, guild: Option<i64>, channel: i64, user: i64) -> Self {
		Self {
			core,
			ctx,
			guild,
			channel,
			user,
			placeholder: Mutex::new(None),
		}
	}

	async fn channel(
		&self,
		channel_id: i64,
	) -> Result<Cached<fluxer_neptunium::cache::CachedChannel>> {
		self.ctx
			.fetch_channel(Id::new(channel_id as u64))
			.await
			.map_err(Into::into)
	}

	async fn upload(
		&self,
		channel_id: i64,
		attachment: (Vec<u8>, String),
	) -> Result<AttachmentRequest> {
		let (data, filename) = attachment;
		let content_type = content_type_for(&filename);
		self.ctx
			.upload_file(
				Id::new(channel_id as u64),
				FileUploadParams::builder()
					.filename(filename)
					.file_data(data)
					.content_type(content_type)
					.attachment(AttachmentBase::builder().build())
					.build(),
			)
			.await
			.map_err(Into::into)
	}

	fn with_dm_hint(&self, reply: PrefixReply) -> (String, Option<(Vec<u8>, String)>) {
		let mut content = reply.content;
		if let Some(reference) = reply.dm_reference {
			let hint = format!("DM: `{}dm {reference}`", self.core.command_prefix());
			if content.is_empty() {
				content = hint;
			} else {
				content.push('\n');
				content.push_str(&hint);
			}
		}
		(content, reply.attachment)
	}

	async fn send_to(&self, channel_id: i64, reply: PrefixReply) -> Result<()> {
		self.core.discord_pacer()?.wait().await;
		let (content, attachment) = self.with_dm_hint(reply);
		let attachment = match attachment {
			Some(attachment) => Some(self.upload(channel_id, attachment).await?),
			None => None,
		};
		let mut body = CreateMessageBody::from(content);
		if body.content.as_deref() == Some("") {
			body.content = None;
		}
		body.attachments = attachment.into_iter().collect();
		let channel = self.channel(channel_id).await?;
		channel.send_message(self.ctx, body).await?;
		Ok(())
	}

	async fn edit_placeholder(&self, reply: PrefixReply) -> Result<bool> {
		self.core.discord_pacer()?.wait().await;
		let placeholder = self
			.placeholder
			.lock()
			.expect("placeholder mutex poisoned")
			.clone();
		let Some(placeholder) = placeholder else {
			return Ok(false);
		};
		let (content, attachment) = self.with_dm_hint(reply);
		let attachment = match attachment {
			Some(attachment) => Some(self.upload(self.channel, attachment).await?),
			None => None,
		};
		let mut body = EditMessageBody::from(content);
		if body.content.as_deref() == Some("") {
			body.content = None;
		}
		body.attachments = attachment.map(|attachment| vec![attachment]);
		placeholder.edit(self.ctx, body).await?;
		Ok(true)
	}
}

#[async_trait]
impl PrefixContext for FluxerPrefix<'_> {
	fn core(&self) -> &Core {
		self.core
	}

	fn guild(&self) -> Option<i64> {
		self.guild
	}

	fn channel(&self) -> i64 {
		self.channel
	}

	fn user(&self) -> u64 {
		self.user as u64
	}

	async fn defer(&self) -> Result<()> {
		self.core.discord_pacer()?.wait().await;
		let channel = self.channel(self.channel).await?;
		let sent = channel.send_message(self.ctx, "Searching...").await?;
		*self.placeholder.lock().expect("placeholder mutex poisoned") = Some(sent);
		Ok(())
	}

	async fn reply(&self, reply: PrefixReply) -> Result<()> {
		match self.edit_placeholder(reply.clone()).await {
			Ok(true) => Ok(()),
			Ok(false) => self.send_to(self.channel, reply).await,
			Err(err) => {
				cli::error(&format!("could not edit prefix response: {err:#}"), "ERROR");
				self.send_to(self.channel, reply).await
			}
		}
	}

	async fn follow_up(&self, reply: PrefixReply) -> Result<()> {
		self.send_to(self.channel, reply).await
	}

	async fn send_dm(&self, user: i64, reply: PrefixReply) -> Result<()> {
		self.core.discord_pacer()?.wait().await;
		let channel = self
			.ctx
			.create_private_channel(CreatePrivateChannel::Dm(Id::<UserMarker>::new(user as u64)))
			.await?;
		self.send_to(channel.id.into_inner() as i64, reply).await
	}
}

fn content_type_for(filename: &str) -> String {
	let extension = filename
		.rsplit_once('.')
		.map(|(_, extension)| extension.to_ascii_lowercase())
		.unwrap_or_default();
	let mime = match extension.as_str() {
		"png" => "image/png",
		"jpg" | "jpeg" => "image/jpeg",
		"gif" => "image/gif",
		"webp" => "image/webp",
		"bmp" => "image/bmp",
		_ => "application/octet-stream",
	};
	mime.to_string()
}

pub(crate) async fn run_bot(db_path: &str, secret: &FluxerSecret, is_reload: bool) -> Result<bool> {
	if is_reload {
		cli::bot_reloading();
	} else {
		cli::bot_loading();
	}
	let (core, _language) = super::prepare_core(
		db_path,
		PlatformKind::Fluxer,
		&secret.token,
		secret.admin_user_id,
	)?;
	let handler = FluxerHandler {
		core: Arc::clone(&core),
	};
	let session = SessionConfig::builder().token(secret.token.clone()).build();
	let allowed_mentions =
		fluxer_neptunium::http::endpoints::channel::AllowedMentions::builder().build();
	let config = ClientConfig::builder()
		.instance_root(secret.instance.clone())
		.default_allowed_mentions(allowed_mentions)
		.build();
	let mut client = Client::create_with_config(session, config)
		.await
		.context("failed to create fluxer client")?;
	client.register_event_handler(handler);

	let reload_requested = Arc::clone(&core.reload_requested);
	let reload_notify = Arc::clone(&core.reload_notify);
	let jobs = Arc::clone(&core.jobs);
	let cancel = async move {
		tokio::select! {
			_ = reload_notify.notified() => false,
			_ = super::shutdown_signal() => true,
		}
	};
	let (_, shutdown) = client
		.start_cancellable(cancel)
		.await
		.context("fluxer gateway failed")?;

	if let Err(err) = drain_jobs(&jobs).await {
		cli::error(&format!("{err:#}"), "ERROR");
	}
	cli::unloaded_everything();
	if shutdown {
		return Ok(false);
	}
	let reload = reload_requested.load(std::sync::atomic::Ordering::Acquire);
	if reload {
		cli::bot_reloaded();
	}
	Ok(reload)
}

async fn drain_jobs(jobs: &Arc<super::JobTracker>) -> Result<()> {
	if tokio::time::timeout(std::time::Duration::from_secs(30), jobs.wait_idle())
		.await
		.is_err()
	{
		cli::error("active jobs exceeded shutdown deadline", "ERROR");
		jobs.cancel_all();
		tokio::time::timeout(std::time::Duration::from_secs(5), jobs.wait_idle())
			.await
			.context("could not cancel active jobs safely")?;
	}
	Ok(())
}
