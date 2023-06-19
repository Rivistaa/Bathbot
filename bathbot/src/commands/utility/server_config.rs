use std::sync::Arc;

use bathbot_macros::{command, SlashCommand};
use bathbot_psql::model::configs::{GuildConfig, ListSize, MinimizedPp, ScoreSize};
use bathbot_util::constants::GENERAL_ISSUE;
use eyre::Result;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::id::{marker::RoleMarker, Id};

use super::AuthorityCommandKind;
use crate::{
    commands::{EnableDisable, ShowHideOption},
    embeds::{EmbedData, ServerConfigEmbed},
    util::{interaction::InteractionCommand, InteractionCommandExt},
    Context,
};

#[derive(CommandModel, CreateCommand, SlashCommand)]
#[command(
    name = "serverconfig",
    dm_permission = false,
    desc = "Adjust configurations or authority roles for this server"
)]
#[flags(AUTHORITY, SKIP_DEFER)]
pub enum ServerConfig {
    #[command(name = "authorities")]
    Authorities(ServerConfigAuthorities),
    #[command(name = "edit")]
    Edit(ServerConfigEdit),
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "authorities",
    desc = "Adjust authority roles for a server",
    help = "To use certain commands, users require a special status.\n\
    This command adjusts the authority status of roles.\n\
    Any member with an authority role can use these higher commands.\n\n\
    Authority commands: `matchlive`, `prune`, `roleassign`, \
    `serverconfig`, `track`, `trackstream`."
)]
pub enum ServerConfigAuthorities {
    #[command(name = "add")]
    Add(ServerConfigAuthoritiesAdd),
    #[command(name = "remove")]
    Remove(ServerConfigAuthoritiesRemove),
    #[command(name = "list")]
    List(ServerConfigAuthoritiesList),
}

impl From<ServerConfigAuthorities> for AuthorityCommandKind {
    #[inline]
    fn from(args: ServerConfigAuthorities) -> Self {
        match args {
            ServerConfigAuthorities::Add(args) => Self::Add(args.role),
            ServerConfigAuthorities::Remove(args) => Self::Remove(args.role),
            ServerConfigAuthorities::List(_) => Self::List,
        }
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "add",
    desc = "Add authority status to a role",
    help = "Add authority status to a role.\n\
    Servers can have at most 10 authority roles."
)]
pub struct ServerConfigAuthoritiesAdd {
    #[command(desc = "Specify the role that should gain authority status")]
    role: Id<RoleMarker>,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "remove",
    desc = "Remove authority status from a role",
    help = "Remove authority status from a role.\n\
    You can only use this if the removed role would __not__ make you lose authority status yourself."
)]
pub struct ServerConfigAuthoritiesRemove {
    #[command(desc = "Specify the role that should gain authority status")]
    role: Id<RoleMarker>,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "list", desc = "Display all current authority roles")]
pub struct ServerConfigAuthoritiesList;

#[derive(CommandModel, CreateCommand)]
#[command(name = "edit", desc = "Adjust configurations for a server")]
pub struct ServerConfigEdit {
    #[command(desc = "Choose whether song commands can be used or not")]
    song_commands: Option<EnableDisable>,
    #[command(
        desc = "What size should the recent, compare, simulate, ... commands be?",
        help = "Some embeds are pretty chunky and show too much data.\n\
        With this option you can make those embeds minimized by default.\n\
        Affected commands are: `compare score`, `recent score`, `recent simulate`, \
        and any command showing top scores when the `index` option is specified.\n\
        Applies only if the member has not specified a config for themselves."
    )]
    score_embeds: Option<ScoreSize>,
    #[command(
        desc = "Adjust the amount of scores shown per page in top, rb, pinned, ...",
        help = "Adjust the amount of scores shown per page in top, rb, pinned, and mapper.\n\
        `Condensed` shows 10 scores, `Detailed` shows 5, and `Single` shows 1.\n\
        Applies only if the member has not specified a config for themselves."
    )]
    list_embeds: Option<ListSize>,
    #[command(
        desc = "Should the amount of retries be shown for the recent command?",
        help = "Should the amount of retries be shown for the `recent` command?\n\
        Applies only if the member has not specified a config for themselves."
    )]
    retries: Option<ShowHideOption>,
    #[command(
        min_value = 1,
        max_value = 100,
        desc = "Specify the default track limit for osu! top scores",
        help = "Specify the default track limit for tracking user's osu! top scores.\n\
        The value must be between 1 and 100, defaults to 50."
    )]
    track_limit: Option<i64>,
    #[command(
        desc = "Specify whether the recent command should show max or if-fc pp when minimized"
    )]
    minimized_pp: Option<MinimizedPp>,
    #[command(
        desc = "Should the recent command include a render button?",
        help = "Should the `recent` command include a render button?\n\
        The button would be a shortcut for the `/render` command.\n\
        If hidden, the button will never show. If shown, members \
        will have the option to choose via `/config`."
    )]
    render_button: Option<ShowHideOption>,
}

impl ServerConfigEdit {
    fn any(&self) -> bool {
        self.song_commands.is_some()
            || self.score_embeds.is_some()
            || self.list_embeds.is_some()
            || self.retries.is_some()
            || self.track_limit.is_some()
            || self.minimized_pp.is_some()
            || self.render_button.is_some()
    }
}

async fn slash_serverconfig(ctx: Arc<Context>, mut command: InteractionCommand) -> Result<()> {
    let args = ServerConfig::from_interaction(command.input_data())?;

    let guild_id = command.guild_id.unwrap();

    let guild = match ctx.cache.guild(guild_id).await {
        Ok(Some(guild)) => guild,
        Ok(None) => {
            warn!("Missing guild {guild_id} in cache");
            command.error(&ctx, GENERAL_ISSUE).await?;

            return Ok(());
        }
        Err(err) => {
            let _ = command.error(&ctx, GENERAL_ISSUE).await;

            return Err(err);
        }
    };

    let args = match args {
        ServerConfig::Authorities(args) => {
            return super::authorities(ctx, (&mut command).into(), args.into()).await
        }
        ServerConfig::Edit(edit) => edit,
    };

    if args.any() {
        let f = |config: &mut GuildConfig| {
            let ServerConfigEdit {
                score_embeds,
                list_embeds,
                minimized_pp,
                retries,
                song_commands,
                track_limit,
                render_button,
            } = args;

            if let Some(score_embeds) = score_embeds {
                config.score_size = Some(score_embeds);
            }

            if let Some(list_embeds) = list_embeds {
                config.list_size = Some(list_embeds);
            }

            if let Some(pp) = minimized_pp {
                config.minimized_pp = Some(pp);
            }

            if let Some(retries) = retries {
                config.show_retries = Some(retries == ShowHideOption::Show);
            }

            if let Some(limit) = track_limit {
                config.track_limit = Some(limit as u8);
            }

            if let Some(with_lyrics) = song_commands {
                config.allow_songs = Some(with_lyrics == EnableDisable::Enable);
            }

            if let Some(render_button) = render_button {
                config.render_button = Some(render_button == ShowHideOption::Show);
            }
        };

        if let Err(err) = ctx.guild_config().update(guild_id, f).await {
            let _ = command.error_callback(&ctx, GENERAL_ISSUE).await;

            return Err(err.wrap_err("failed to update guild config"));
        }
    }

    let config = ctx
        .guild_config()
        .peek(guild_id, GuildConfig::to_owned)
        .await;

    let mut authorities = Vec::with_capacity(config.authorities.len());

    for &role in config.authorities.iter() {
        if let Ok(Some(role)) = ctx.cache.role(guild_id, role).await {
            authorities.push(role.name.as_ref().to_owned());
        }
    }

    let embed = ServerConfigEmbed::new(guild, config, &authorities);
    let builder = embed.build().into();
    command.callback(&ctx, builder, false).await?;

    Ok(())
}
