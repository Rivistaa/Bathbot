use crate::{
    arguments::{Args, NameArgs},
    custom_client::SnipeRecent,
    embeds::{EmbedData, SnipedEmbed},
    unwind_error,
    util::{
        constants::{HUISMETBENEN_ISSUE, OSU_API_ISSUE},
        MessageExt, SNIPE_COUNTRIES,
    },
    BotResult, Context,
};

use chrono::{Date, DateTime, Duration, Utc};
use image::{png::PngEncoder, ColorType};
use itertools::Itertools;
use plotters::{
    coord::{
        ranged1d::SegmentedCoord,
        types::{RangedCoordusize, RangedSlice},
        Shift,
    },
    prelude::*,
};
use rosu::model::GameMode;
use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    sync::Arc,
};
use twilight_model::channel::Message;

#[command]
#[short_desc("TODO")]
#[long_desc("TODO")]
#[usage("[username]")]
#[example("badewanne3")]
#[aliases("snipes")]
#[bucket("snipe")]
async fn sniped(ctx: Arc<Context>, msg: &Message, args: Args) -> BotResult<()> {
    let args = NameArgs::new(&ctx, args);
    let name = match args.name.or_else(|| ctx.get_link(msg.author.id.0)) {
        Some(name) => name,
        None => return super::require_link(&ctx, msg).await,
    };
    let user = match ctx.osu().user(name.as_str()).mode(GameMode::STD).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            let content = format!("Could not find user `{}`", name);
            return msg.error(&ctx, content).await;
        }
        Err(why) => {
            let _ = msg.error(&ctx, OSU_API_ISSUE).await;
            return Err(why.into());
        }
    };
    let client = &ctx.clients.custom;
    let now = Utc::now();
    let (sniper_fut, snipee_fut) = if SNIPE_COUNTRIES.contains_key(user.country.as_str()) {
        (
            client.get_national_snipes(&user, true, now - Duration::weeks(8), now),
            client.get_national_snipes(&user, false, now - Duration::weeks(8), now),
        )
    } else {
        let content = format!(
            "`{}`'s country {} is not supported :(",
            user.username, user.country
        );
        return msg.error(&ctx, content).await;
    };
    let (sniper, snipee) = match tokio::try_join!(sniper_fut, snipee_fut) {
        Ok((sniper, snipee)) => {
            let sniper: Vec<_> = sniper
                .into_iter()
                .filter(|score| score.sniped.is_some())
                .collect();
            (sniper, snipee)
        }
        Err(why) => {
            let _ = msg.error(&ctx, HUISMETBENEN_ISSUE).await;
            return Err(why.into());
        }
    };
    let graph = match graphs(user.username.as_str(), &sniper, &snipee) {
        Ok(graph_option) => graph_option,
        Err(why) => {
            unwind_error!(warn, why, "Error while creating sniped graph: {}");
            None
        }
    };

    let data = SnipedEmbed::new(user, sniper, snipee);

    // Sending the embed
    let embed = data.build().build()?;
    let m = ctx.http.create_message(msg.channel_id).embed(embed)?;
    let response = if let Some(graph) = graph {
        m.attachment("sniped_graph.png", graph).await?
    } else {
        m.await?
    };
    response.reaction_delete(&ctx, msg.author.id);
    Ok(())
}

const W: u32 = 1350;
const H: u32 = 350;

fn graphs(
    name: &str,
    sniper: &[SnipeRecent],
    snipee: &[SnipeRecent],
) -> BotResult<Option<Vec<u8>>> {
    if sniper.is_empty() && snipee.is_empty() {
        return Ok(None);
    }

    static LEN: usize = W as usize * H as usize;
    let mut buf = vec![0; LEN * 3]; // PIXEL_SIZE = 3

    {
        let root = BitMapBackend::with_buffer(&mut buf, (W, H)).into_drawing_area();
        root.fill(&WHITE)?;

        match (sniper.is_empty(), snipee.is_empty()) {
            (false, true) => draw_sniper(&root, name, sniper)?,
            (true, false) => draw_snipee(&root, name, snipee)?,
            (false, false) => {
                let (left, right) = root.split_horizontally(W / 2);
                draw_sniper(&left, name, sniper)?;
                draw_snipee(&right, name, snipee)?
            }
            (true, true) => unreachable!(),
        }
    }

    // Encode buf to png
    let mut png_bytes: Vec<u8> = Vec::with_capacity(LEN);
    let png_encoder = PngEncoder::new(&mut png_bytes);
    png_encoder.encode(&buf, W, H, ColorType::Rgb8)?;
    Ok(Some(png_bytes))
}

type ContextType<'a> = Cartesian2d<SegmentedCoord<RangedSlice<'a, Date<Utc>>>, RangedCoordusize>;
type DrawingError<DB> = Result<(), DrawingAreaErrorKind<<DB as DrawingBackend>::ErrorType>>;

fn draw_sniper<DB: DrawingBackend>(
    root: &DrawingArea<DB, Shift>,
    name: impl AsRef<str>,
    sniper: &[SnipeRecent],
) -> DrawingError<DB> {
    let (dates, sniper) = prepare_sniper(sniper);
    let max = sniper
        .iter()
        .map(|(_, v)| v.last().copied())
        .max()
        .flatten()
        .unwrap_or(0);

    let mut chart = ChartBuilder::on(root)
        .x_label_area_size(30)
        .y_label_area_size(35)
        .margin_right(5)
        .caption(format!("Sniped by {}", name.as_ref()), ("sans-serif", 25))
        .build_cartesian_2d((&dates[..]).into_segmented(), 0..max + 1)?;

    draw_mesh(&mut chart)?;

    // Bars
    for (i, (name, values)) in sniper.into_iter().enumerate() {
        let color = HSLColor(i as f64 * 0.1, 0.5, 0.5);
        chart
            .draw_series(
                Histogram::vertical(&chart)
                    .data(
                        values
                            .into_iter()
                            .enumerate()
                            .map(|(i, count)| (&dates[i], count)),
                    )
                    .style(color.filled()),
            )?
            .label(name)
            .legend(move |(x, y)| Circle::new((x, y), 2, color.filled()));
    }

    draw_legend(&mut chart)?;

    Ok(())
}

fn draw_snipee<DB: DrawingBackend>(
    root: &DrawingArea<DB, Shift>,
    name: impl AsRef<str>,
    snipee: &[SnipeRecent],
) -> DrawingError<DB> {
    let (dates, snipee) = prepare_snipee(snipee);
    let max = snipee
        .iter()
        .map(|(_, v)| v.last().copied())
        .max()
        .flatten()
        .unwrap_or(0);

    let mut chart = ChartBuilder::on(root)
        .x_label_area_size(30)
        .y_label_area_size(35)
        .margin_right(5)
        .caption(format!("Sniped {}", name.as_ref()), ("sans-serif", 25))
        .build_cartesian_2d((&dates[..]).into_segmented(), 0..max + 1)?;

    draw_mesh(&mut chart)?;

    // Bars
    for (i, (name, values)) in snipee.into_iter().enumerate() {
        let color = HSLColor(i as f64 * 0.1, 0.5, 0.5);
        chart
            .draw_series(
                Histogram::vertical(&chart)
                    .data(
                        values
                            .into_iter()
                            .enumerate()
                            .map(|(i, count)| (&dates[i], count)),
                    )
                    .style(color.filled()),
            )?
            .label(name)
            .legend(move |(x, y)| Circle::new((x, y), 2, color.filled()));
    }

    draw_legend(&mut chart)?;

    Ok(())
}

fn draw_mesh<DB: DrawingBackend>(chart: &mut ChartContext<DB, ContextType>) -> DrawingError<DB> {
    chart
        .configure_mesh()
        .disable_x_mesh()
        .x_label_formatter(&|date: &SegmentValue<&Date<Utc>>| match date {
            SegmentValue::CenterOf(date) | SegmentValue::Exact(date) => {
                date.format("%Y-%m-%d").to_string()
            }
            _ => unreachable!(),
        })
        .label_style(("sans-serif", 13))
        .draw()
}

fn draw_legend<'a, DB: DrawingBackend + 'a>(
    chart: &mut ChartContext<'a, DB, ContextType>,
) -> DrawingError<DB> {
    chart
        .configure_series_labels()
        .border_style(BLACK.stroke_width(2))
        .background_style(&RGBColor(192, 192, 192))
        .position(SeriesLabelPosition::UpperLeft)
        .legend_area_size(13)
        .label_font(("sans-serif", 14, FontStyle::Bold))
        .draw()?;

    Ok(())
}

fn prepare_snipee(scores: &[SnipeRecent]) -> (Vec<Date<Utc>>, Vec<(&str, Vec<usize>)>) {
    let total = scores.iter().fold(HashMap::new(), |mut map, score| {
        *map.entry(score.sniper.as_str()).or_insert(0) += 1;
        map
    });
    let mut final_order: Vec<_> = total.into_iter().collect();
    final_order.sort_unstable_by_key(|(_, c)| Reverse(*c));
    final_order.truncate(10);

    let names: HashSet<_> = final_order.iter().map(|(name, _)| *name).collect();

    let categorized: Vec<_> = scores
        .iter()
        .scan(Utc::now() - chrono::Duration::weeks(7), |state, score| {
            if !names.contains(score.sniper.as_str()) {
                return Some(None);
            }
            if score.date > *state {
                while score.date > *state {
                    *state = *state + chrono::Duration::weeks(1);
                }
            }
            Some(Some((score.sniper.as_str(), *state)))
        })
        .filter_map(|o| o)
        .collect();

    finish_preparing(names, categorized)
}

fn prepare_sniper(scores: &[SnipeRecent]) -> (Vec<Date<Utc>>, Vec<(&str, Vec<usize>)>) {
    let total = scores.iter().filter(|score| score.sniped.is_some()).fold(
        HashMap::new(),
        |mut map, score| {
            *map.entry(score.sniped.as_deref().unwrap()).or_insert(0) += 1;
            map
        },
    );
    let mut final_order: Vec<_> = total.into_iter().collect();
    final_order.sort_unstable_by_key(|(_, c)| Reverse(*c));
    final_order.truncate(10);

    let names: HashSet<_> = final_order.iter().map(|(name, _)| *name).collect();

    let categorized: Vec<_> = scores
        .iter()
        .filter(|score| score.sniped.is_some())
        .scan(Utc::now() - chrono::Duration::weeks(7), |state, score| {
            if !names.contains(score.sniped.as_deref().unwrap()) {
                return Some(None);
            }
            if score.date > *state {
                while score.date > *state {
                    *state = *state + chrono::Duration::weeks(1);
                }
            }
            Some(Some((score.sniped.as_deref().unwrap(), *state)))
        })
        .filter_map(|o| o)
        .collect();

    finish_preparing(names, categorized)
}

fn finish_preparing<'a>(
    names: HashSet<&'a str>,
    categorized: Vec<(&'a str, DateTime<Utc>)>,
) -> (Vec<Date<Utc>>, Vec<(&'a str, Vec<usize>)>) {
    let (dates, counts): (Vec<_>, Vec<_>) = categorized
        .into_iter()
        .group_by(|(_, date)| *date)
        .into_iter()
        .map(|(date, group)| {
            let counts =
                group
                    .into_iter()
                    .map(|(name, _)| name)
                    .fold(HashMap::new(), |mut map, name| {
                        *map.entry(name).or_insert(0) += 1;
                        map
                    });
            (date.date(), counts)
        })
        .unzip();

    let mut total: HashMap<_, _> = names.into_iter().map(|name| (name, Vec::new())).collect();

    for counts in counts.into_iter() {
        for (name, values) in total.iter_mut() {
            values.push(counts.get(name).copied().unwrap_or(0));
        }
    }

    for values in total.values_mut() {
        for i in 1..values.len() {
            values[i] += values[i - 1];
        }
    }

    let mut total: Vec<_> = total.into_iter().collect();
    total.sort_unstable_by_key(|(_, values)| Reverse(values.last().copied()));

    for (i, j) in (1..total.len()).zip(0..total.len() - 1).rev() {
        for k in 0..dates.len() {
            total[j].1[k] += total[i].1[k];
        }
    }

    (dates, total)
}