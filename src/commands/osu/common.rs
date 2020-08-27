use crate::{
    arguments::{Args, MultNameArgs},
    embeds::{CommonEmbed, EmbedData},
    pagination::{CommonPagination, Pagination},
    util::{constants::OSU_API_ISSUE, get_combined_thumbnail, MessageExt},
    BotResult, Context,
};

use futures::future::{try_join_all, TryFutureExt};
use itertools::Itertools;
use rayon::prelude::*;
use rosu::{
    backend::requests::BeatmapRequest,
    models::{GameMode, Score, User},
};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fmt::Write,
    sync::Arc,
};
use twilight::model::channel::Message;

#[allow(clippy::cognitive_complexity)]
async fn common_main(
    mode: GameMode,
    ctx: Arc<Context>,
    msg: &Message,
    args: Args<'_>,
) -> BotResult<()> {
    let mut args = MultNameArgs::new(&ctx, args, 10);
    let names = match args.names.len() {
        0 => {
            let content = "You need to specify at least one osu username. \
                If you're not linked, you must specify at least two names.";
            return msg.error(&ctx, content).await;
        }
        1 => match ctx.get_link(msg.author.id.0) {
            Some(name) => {
                args.names.push(name);
                args.names
            }
            None => {
                let prefix = ctx.config_first_prefix(msg.guild_id);
                let content = format!(
                    "Since you're not linked via `{}link`, \
                    you must specify at least two names.",
                    prefix
                );
                return msg.error(&ctx, content).await;
            }
        },
        _ => args.names,
    };

    if names.iter().unique().count() == 1 {
        let content = "Give at least two different names";
        return msg.error(&ctx, content).await;
    }

    // Retrieve all users
    let user_futs = names
        .iter()
        .enumerate()
        .map(|(i, name)| ctx.osu_user(&name, mode).map_ok(move |user| (i, user)));
    let users: HashMap<u32, User> = match try_join_all(user_futs).await {
        Ok(users) => match users.par_iter().find_any(|(_, user)| user.is_none()) {
            Some((idx, _)) => {
                let content = format!("User `{}` was not found", names[*idx]);
                return msg.error(&ctx, content).await;
            }
            None => users
                .into_iter()
                .filter_map(|(_, user)| user.map(|user| (user.user_id, user)))
                .collect(),
        },
        Err(why) => {
            let _ = msg.error(&ctx, OSU_API_ISSUE).await;
            return Err(why.into());
        }
    };

    if users.values().map(|u| u.user_id).unique().count() == 1 {
        let content = "Give at least two different users";
        return msg.error(&ctx, content).await;
    }

    // Retrieve each user's top scores
    let score_futs = users
        .iter()
        .map(|(_, user)| user.get_top_scores(ctx.osu(), 100, mode));
    let mut all_scores = match try_join_all(score_futs).await {
        Ok(all_scores) => all_scores,
        Err(why) => {
            let _ = msg.error(&ctx, OSU_API_ISSUE).await;
            return Err(why.into());
        }
    };

    // Consider only scores on common maps
    let mut map_ids: HashSet<u32> = all_scores
        .par_iter()
        .map(|scores| scores.par_iter().flat_map(|s| s.beatmap_id))
        .flatten()
        .collect();
    map_ids.retain(|&id| {
        all_scores
            .iter()
            .all(|scores| scores.iter().any(|s| s.beatmap_id.unwrap() == id))
    });
    all_scores
        .par_iter_mut()
        .for_each(|scores| scores.retain(|s| map_ids.contains(&s.beatmap_id.unwrap())));

    // Flatten scores, sort by beatmap id, then group by beatmap id
    let mut all_scores: Vec<Score> = all_scores.into_iter().flatten().collect();
    all_scores.sort_unstable_by(|s1, s2| s1.beatmap_id.cmp(&s2.beatmap_id));
    let mut all_scores: HashMap<u32, Vec<Score>> = all_scores
        .into_iter()
        .group_by(|score| score.beatmap_id.unwrap())
        .into_iter()
        .map(|(map_id, scores)| (map_id, scores.collect()))
        .collect();

    // Sort each group by pp value, then take the best 3
    all_scores.par_iter_mut().for_each(|(_, scores)| {
        scores.sort_unstable_by(|s1, s2| s2.pp.partial_cmp(&s1.pp).unwrap_or(Ordering::Equal));
        scores.truncate(3);
    });

    // Consider only the top 10 maps with the highest avg pp among the users
    let mut pp_avg: Vec<(u32, f32)> = all_scores
        .par_iter()
        .map(|(&map_id, scores)| {
            let sum = scores.iter().fold(0.0, |sum, next| sum + next.pp.unwrap());
            (map_id, sum / scores.len() as f32)
        })
        .collect();
    pp_avg.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

    // Try retrieving all maps of common scores from the database
    let mut maps = {
        let map_id_vec = map_ids.iter().copied().collect_vec();
        match ctx.psql().get_beatmaps(&map_id_vec).await {
            Ok(maps) => maps,
            Err(why) => {
                warn!("Error while getting maps from DB: {}", why);
                HashMap::default()
            }
        }
    };
    let amount_common = map_ids.len();
    map_ids.retain(|id| !maps.contains_key(id));

    // Retrieve all missing maps from the API
    let missing_maps: Option<Vec<_>> = if map_ids.is_empty() {
        None
    } else {
        let map_futs = map_ids.into_iter().map(|id| {
            BeatmapRequest::new()
                .map_id(id)
                .queue_single(ctx.osu())
                .map_ok(move |map| (id, map))
        });
        match try_join_all(map_futs).await {
            Ok(maps_result) => match maps_result.par_iter().find_any(|(_, map)| map.is_none()) {
                Some((id, _)) => {
                    let content = format!("API returned no result for map id {}", id);
                    return msg.error(&ctx, content).await;
                }
                None => {
                    let maps = maps_result
                        .into_iter()
                        .map(|(id, map)| {
                            let map = map.unwrap();
                            maps.insert(id, map.clone());
                            map
                        })
                        .collect();
                    Some(maps)
                }
            },
            Err(why) => {
                let _ = msg.error(&ctx, OSU_API_ISSUE).await;
                return Err(why.into());
            }
        }
    };

    // Accumulate all necessary data
    let len = names.iter().map(|name| name.len() + 4).sum::<usize>() + 4;
    let mut content = String::with_capacity(len);
    let len = names.len();
    let mut iter = names.into_iter();
    let last = iter.next_back();
    let _ = write!(content, "`{}`", iter.next().unwrap());
    for name in iter {
        let _ = write!(content, ", `{}`", name);
    }
    if let Some(name) = last {
        if len > 2 {
            content.push(',');
        }
        let _ = write!(content, " and `{}`", name);
    }
    if amount_common == 0 {
        content.push_str(" have no common scores");
    } else {
        let _ = write!(
            content,
            " have {} common beatmap{} in their top 100",
            amount_common,
            if amount_common > 1 { "s" } else { "" }
        );
    }

    // Keys have no strict order, hence inconsistent result
    let thumbnail_fut = async {
        let user_ids: Vec<u32> = users.keys().copied().collect();
        get_combined_thumbnail(&ctx, &user_ids).await
    };
    let data_fut = async {
        let id_pps = &pp_avg[..10.min(pp_avg.len())];
        CommonEmbed::new(&users, &all_scores, &maps, id_pps, 0)
    };
    let (thumbnail_result, data) = tokio::join!(thumbnail_fut, data_fut);
    let thumbnail = match thumbnail_result {
        Ok(thumbnail) => Some(thumbnail),
        Err(why) => {
            warn!("Error while combining avatars: {}", why);
            None
        }
    };

    // Creating the embed
    let embed = data.build().build()?;
    let mut m = ctx.http.create_message(msg.channel_id);
    m = match thumbnail {
        Some(bytes) => m.attachment("avatar_fuse.png", bytes),
        None => m,
    };
    let response = m.content(content)?.embed(embed)?.await?;

    // Add missing maps to database
    if let Some(maps) = missing_maps {
        match ctx.psql().insert_beatmaps(&maps).await {
            Ok(n) if n < 2 => {}
            Ok(n) => info!("Added {} maps to DB", n),
            Err(why) => warn!("Error while adding maps to DB: {}", why),
        }
    }

    // Skip pagination if too few entries
    if pp_avg.len() <= 10 {
        response.reaction_delete(&ctx, msg.author.id);
        return Ok(());
    }

    // Pagination
    let pagination = CommonPagination::new(response, users, all_scores, maps, pp_avg);
    let owner = msg.author.id;
    tokio::spawn(async move {
        if let Err(why) = pagination.start(&ctx, owner, 60).await {
            warn!("Pagination error (common): {}", why)
        }
    });
    Ok(())
}

#[command]
#[short_desc("Compare maps of players' top100s")]
#[long_desc(
    "Compare the users' top 100 and check which \
     maps appear in each top list (up to 10 users)"
)]
#[usage("[name1] [name2] ...")]
#[example("badewanne3 \"nathan on osu\" idke")]
pub async fn common(ctx: Arc<Context>, msg: &Message, args: Args) -> BotResult<()> {
    common_main(GameMode::STD, ctx, msg, args).await
}

#[command]
#[short_desc("Compare maps of players' top100s")]
#[long_desc(
    "Compare the mania users' top 100 and check which \
     maps appear in each top list (up to 10 users)"
)]
#[usage("[name1] [name2] ...")]
#[example("badewanne3 \"nathan on osu\" idke")]
#[aliases("commonm")]
pub async fn commonmania(ctx: Arc<Context>, msg: &Message, args: Args) -> BotResult<()> {
    common_main(GameMode::MNA, ctx, msg, args).await
}

#[command]
#[short_desc("Compare maps of players' top100s")]
#[long_desc(
    "Compare the taiko users' top 100 and check which \
     maps appear in each top list (up to 10 users)"
)]
#[usage("[name1] [name2] ...")]
#[example("badewanne3 \"nathan on osu\" idke")]
#[aliases("commont")]
pub async fn commontaiko(ctx: Arc<Context>, msg: &Message, args: Args) -> BotResult<()> {
    common_main(GameMode::TKO, ctx, msg, args).await
}

#[command]
#[short_desc("Compare maps of players' top100s")]
#[long_desc(
    "Compare the ctb users' top 100 and check which \
     maps appear in each top list (up to 10 users)"
)]
#[usage("[name1] [name2] ...")]
#[example("badewanne3 \"nathan on osu\" idke")]
#[aliases("commonc")]
pub async fn commonctb(ctx: Arc<Context>, msg: &Message, args: Args) -> BotResult<()> {
    common_main(GameMode::CTB, ctx, msg, args).await
}
