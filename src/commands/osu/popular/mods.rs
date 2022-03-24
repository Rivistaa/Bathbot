use std::sync::Arc;

use eyre::Report;
use rkyv::{Deserialize, Infallible};

use crate::{
    core::{commands::CommandData, Context},
    custom_client::OsuTrackerModsEntry,
    embeds::EmbedData,
    embeds::OsuTrackerModsEmbed,
    pagination::{OsuTrackerModsPagination, Pagination},
    util::{constants::OSUTRACKER_ISSUE, numbers, MessageExt},
    BotResult,
};

pub(super) async fn mods_(ctx: Arc<Context>, data: CommandData<'_>) -> BotResult<()> {
    let counts: Vec<OsuTrackerModsEntry> = match ctx.redis().osutracker_stats().await {
        Ok(stats) => stats
            .get()
            .user
            .mods_count
            .deserialize(&mut Infallible)
            .unwrap(),
        Err(err) => {
            let _ = data.error(&ctx, OSUTRACKER_ISSUE).await;

            return Err(err.into());
        }
    };

    let pages = numbers::div_euclid(20, counts.len());
    let initial = &counts[..counts.len().min(20)];

    let embed = OsuTrackerModsEmbed::new(initial, (1, pages))
        .into_builder()
        .build();

    let response_raw = data.create_message(&ctx, embed.into()).await?;

    if counts.len() <= 20 {
        return Ok(());
    }

    let response = response_raw.model().await?;

    let pagination = OsuTrackerModsPagination::new(response, counts);
    let owner = data.author()?.id;

    tokio::spawn(async move {
        if let Err(err) = pagination.start(&ctx, owner, 60).await {
            warn!("{:?}", Report::new(err));
        }
    });

    Ok(())
}