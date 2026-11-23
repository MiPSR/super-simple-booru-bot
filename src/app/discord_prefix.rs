use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use serenity::all::{
	CreateActionRow, CreateAttachment, CreateButton, CreateMessage, EditMessage, MessageId,
	ReactionType, UserId,
};

use crate::cli;

use super::{
	Core, Handler,
	prefix::{PrefixContext, PrefixReply},
};

pub(crate) struct DiscordPrefix<'a> {
	handler: &'a Handler,
	ctx: &'a serenity::client::Context,
	message: &'a serenity::model::channel::Message,
	placeholder: Mutex<Option<MessageId>>,
}

impl<'a> DiscordPrefix<'a> {
	pub(crate) fn new(
		handler: &'a Handler,
		ctx: &'a serenity::client::Context,
		message: &'a serenity::model::channel::Message,
	) -> Self {
		Self {
			handler,
			ctx,
			message,
			placeholder: Mutex::new(None),
		}
	}

	fn components(reference: Option<&str>) -> Vec<CreateActionRow> {
		let Some(reference) = reference else {
			return Vec::new();
		};
		vec![CreateActionRow::Buttons(vec![
			CreateButton::new(format!("dm:{reference}"))
				.label("Send to DM")
				.emoji(ReactionType::Unicode("📬".to_string()))
				.style(serenity::all::ButtonStyle::Secondary),
		])]
	}

	fn create_message(&self, reply: PrefixReply) -> CreateMessage {
		let mut message =
			CreateMessage::new().allowed_mentions(serenity::all::CreateAllowedMentions::new());
		if !reply.content.is_empty() {
			message = message.content(reply.content);
		}
		if let Some((data, filename)) = reply.attachment {
			message = message.add_file(CreateAttachment::bytes(data, filename));
		}
		let components = Self::components(reply.dm_reference.as_deref());
		if !components.is_empty() {
			message = message.components(components);
		}
		message
	}
}

#[async_trait]
impl PrefixContext for DiscordPrefix<'_> {
	fn core(&self) -> &Core {
		self.handler.core()
	}

	fn guild(&self) -> Option<i64> {
		self.message.guild_id.map(|guild| guild.get() as i64)
	}

	fn channel(&self) -> i64 {
		self.message.channel_id.get() as i64
	}

	fn user(&self) -> u64 {
		self.message.author.id.get()
	}

	async fn defer(&self) -> Result<()> {
		self.handler.discord_pacer()?.wait().await;
		let sent = self
			.message
			.channel_id
			.say(&self.ctx.http, "Searching...")
			.await?;
		*self.placeholder.lock().expect("placeholder mutex poisoned") = Some(sent.id);
		Ok(())
	}

	async fn reply(&self, reply: PrefixReply) -> Result<()> {
		let placeholder = *self.placeholder.lock().expect("placeholder mutex poisoned");
		if let Some(message_id) = placeholder {
			let edit_reply = reply.clone();
			let mut edit =
				EditMessage::new().allowed_mentions(serenity::all::CreateAllowedMentions::new());
			if !edit_reply.content.is_empty() {
				edit = edit.content(edit_reply.content);
			}
			if let Some((data, filename)) = edit_reply.attachment {
				edit = edit.new_attachment(CreateAttachment::bytes(data, filename));
			}
			let components = Self::components(edit_reply.dm_reference.as_deref());
			if !components.is_empty() {
				edit = edit.components(components);
			}
			self.handler.discord_pacer()?.wait().await;
			if let Err(err) = self
				.message
				.channel_id
				.edit_message(&self.ctx.http, message_id, edit)
				.await
			{
				cli::error(&format!("could not edit prefix response: {err}"), "ERROR");
				self.message
					.channel_id
					.send_message(&self.ctx.http, self.create_message(reply))
					.await?;
			}
		} else {
			self.handler.discord_pacer()?.wait().await;
			self.message
				.channel_id
				.send_message(&self.ctx.http, self.create_message(reply))
				.await?;
		}
		Ok(())
	}

	async fn follow_up(&self, reply: PrefixReply) -> Result<()> {
		self.handler.discord_pacer()?.wait().await;
		self.message
			.channel_id
			.send_message(&self.ctx.http, self.create_message(reply))
			.await?;
		Ok(())
	}

	async fn send_dm(&self, user: i64, reply: PrefixReply) -> Result<()> {
		self.handler.discord_pacer()?.wait().await;
		UserId::new(user as u64)
			.direct_message(&self.ctx.http, self.create_message(reply))
			.await?;
		Ok(())
	}
}
