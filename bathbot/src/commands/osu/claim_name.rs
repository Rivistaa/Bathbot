use std::sync::Arc;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use bathbot_macros::SlashCommand;
use bathbot_util::{constants::OSU_API_ISSUE, MessageBuilder};
use eyre::{Report, Result};
use futures::{future, stream::FuturesUnordered, TryStreamExt};
use once_cell::sync::OnceCell;
use rkyv::{Deserialize, Infallible};
use rosu_v2::prelude::{GameMode, OsuError};
use twilight_interactions::command::{CommandModel, CreateCommand};

use crate::{
    core::Context,
    embeds::ClaimNameEmbed,
    embeds::EmbedData,
    manager::redis::{
        osu::{User, UserArgs},
        RedisData,
    },
    util::{interaction::InteractionCommand, InteractionCommandExt},
};

#[derive(CommandModel, CreateCommand, SlashCommand)]
#[command(
    name = "claimname",
    help = "If a player has not signed in for at least 6 months and has no plays,\
    their username may be claimed.\n\
    If that player does have any plays across all game modes, \
    a [non-linear function](https://www.desmos.com/calculator/b89siyv9j8) is used to calculate \
    how much extra time is added to those 6 months.\n\
    This is to prevent people from stealing the usernames of active or recently retired players."
)]
/// Check how much longer to wait until a name is up for grabs
pub struct ClaimName {
    /// Specify a username
    name: String,
}

async fn slash_claimname(ctx: Arc<Context>, mut command: InteractionCommand) -> Result<()> {
    let ClaimName { name } = ClaimName::from_interaction(command.input_data())?;

    let content = if name.chars().count() > 15 {
        Some("Names can have at most 15 characters so your name won't be accepted".to_owned())
    } else if let Some(c) = name
        .chars()
        .find(|c| !matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '[' | ']' | '_' | ' '))
    {
        Some(format!(
            "`{c}` is an invalid character for usernames so `{name}` won't be accepted"
        ))
    } else if name.len() < 3 {
        Some(format!(
            "Names must be at least 3 characters long so `{name}` won't be accepted"
        ))
    } else if name.contains('_') && name.contains(' ') {
        Some(format!(
            "Names may contains underscores or spaces but not both \
            so `{name}` won't be accepted"
        ))
    } else if name.starts_with(' ') || name.ends_with(' ') {
        Some(format!(
            "Names can't start or end with spaces so `{name}` won't be accepted"
        ))
    } else {
        None
    };

    if let Some(content) = content {
        let builder = MessageBuilder::new().embed(content);
        command.update(&ctx, &builder).await?;

        return Ok(());
    }

    let user_id = match UserArgs::username(&ctx, &name).await {
        UserArgs::Args(args) => args.user_id,
        UserArgs::User { user, .. } => user.user_id,
        UserArgs::Err(OsuError::NotFound) => {
            let content = if ClaimNameValidator::is_valid(&name) {
                format!("User `{name}` was not found, the name should be available to claim")
            } else {
                format!("`{name}` does not seem to be taken but it likely won't be accepted")
            };

            let builder = MessageBuilder::new().embed(content);
            command.update(&ctx, &builder).await?;

            return Ok(());
        }
        UserArgs::Err(err) => {
            let _ = command.error(&ctx, OSU_API_ISSUE).await;
            let err = Report::new(err).wrap_err("Failed to get user");

            return Err(err);
        }
    };

    let args = [
        GameMode::Osu,
        GameMode::Taiko,
        GameMode::Catch,
        GameMode::Mania,
    ]
    .map(|mode| UserArgs::user_id(user_id).mode(mode));

    let user_fut = args
        .into_iter()
        .map(|args| ctx.redis().osu_user(args))
        .collect::<FuturesUnordered<_>>()
        .try_fold(None, |user: Option<User>, next| match user {
            Some(mut user) => {
                next.peek_stats(|stats| match user.statistics {
                    Some(ref mut accum) => accum.playcount += stats.playcount,
                    None => user.statistics = Some(stats.to_owned()),
                });

                let next_highest_rank = match next {
                    RedisData::Original(next) => next.highest_rank,
                    RedisData::Archived(next) => {
                        next.highest_rank.deserialize(&mut Infallible).unwrap()
                    }
                };

                match (user.highest_rank.as_mut(), next_highest_rank) {
                    (Some(curr), Some(next)) if curr.rank > next.rank => *curr = next,
                    (None, next @ Some(_)) => user.highest_rank = next,
                    _ => {}
                }

                future::ready(Ok(Some(user)))
            }
            None => future::ready(Ok(Some(next.into_original()))),
        });

    let user = match user_fut.await {
        Ok(user) => user.unwrap(),
        Err(err) => {
            let _ = command.error(&ctx, OSU_API_ISSUE).await;
            let err = Report::new(err).wrap_err("Failed to get user");

            return Err(err);
        }
    };

    let embed = ClaimNameEmbed::new(&user, &name).build();
    let builder = MessageBuilder::new().embed(embed);
    command.update(&ctx, &builder).await?;

    Ok(())
}

pub struct ClaimNameValidator;

impl ClaimNameValidator {
    pub fn is_valid(prefix: &str) -> bool {
        !VALIDATOR
            .get_or_init(|| {
                let needles = [
                    "qfqqz",
                    "dppljf{",
                    "difbu",
                    "ojhhfs",
                    "mpmj",
                    "gvdl",
                    "ejmep",
                    "gbhhpu",
                    "dvou",
                    "tijhfupsb",
                    "qpso",
                    "cbodip",
                    "qfojt",
                    "wbhjob",
                    "qvttz",
                    "ejdl",
                    "dpdl",
                    "brvjmb",
                    "ijumfs",
                    "ibdl",
                    "tibwju",
                    "gsjfoepl",
                ]
                .into_iter()
                .map(String::from)
                .map(|mut needle| {
                    unsafe { needle.as_bytes_mut() }
                        .iter_mut()
                        .for_each(|byte| *byte -= 1);

                    needle
                });

                AhoCorasickBuilder::new()
                    .ascii_case_insensitive(true)
                    .dfa(true)
                    .build_with_size(needles)
                    .unwrap()
            })
            .is_match(prefix)
    }
}

static VALIDATOR: OnceCell<AhoCorasick<u16>> = OnceCell::new();
