use std::fmt::Write;

use rosu_v2::prelude::{GameMods, Grade, User};
use twilight_model::channel::embed::{Embed, EmbedField};

use crate::{
    commands::osu::{MinMaxAvg, Number, ProfileData, ProfileKind, Top100Stats},
    core::Context,
    embeds::EmbedData,
    util::{
        builder::{AuthorBuilder, EmbedBuilder, FooterBuilder},
        datetime::{how_long_ago_text, sec_to_minsec, NAIVE_DATETIME_FORMAT},
        numbers::{with_comma_float, with_comma_int},
        osu::grade_emote,
        Emote,
    },
};

pub struct ProfileEmbed {
    author: AuthorBuilder,
    description: String,
    fields: Vec<EmbedField>,
    footer: Option<FooterBuilder>,
    thumbnail: String,
}

impl ProfileEmbed {
    pub async fn new(ctx: &Context, kind: ProfileKind, data: &mut ProfileData) -> Self {
        match kind {
            ProfileKind::Compact => Self::compact(ctx, data).await,
            ProfileKind::UserStats => Self::user_stats(ctx, data).await,
            ProfileKind::Top100Stats => Self::top100_stats(ctx, data).await,
            ProfileKind::Top100Mods => Self::top100_mods(ctx, data).await,
            ProfileKind::Top100Mappers => Self::top100_mappers(ctx, data).await,
            ProfileKind::MapperStats => Self::mapper_stats(ctx, data).await,
        }
    }

    async fn compact(ctx: &Context, data: &mut ProfileData) -> Self {
        let bonus_pp = match data.bonus_pp(ctx).await {
            Some(pp) => format!("{pp:.2}pp"),
            None => "-".to_string(),
        };

        let ProfileData { user, .. } = data;

        let stats = user.statistics.as_ref().unwrap();
        let level = stats.level.float();
        let playtime = stats.playtime / 60 / 60;

        let mut description = format!(
            "Accuracy: `{acc:.2}%` • Level: `{level:.2}`\n\
            Playcount: `{playcount}` (`{playtime} hrs`) • {mode}\n\
            Bonus PP: `{bonus_pp}` • Medals: `{medals}`",
            acc = stats.accuracy,
            playcount = with_comma_int(stats.playcount),
            mode = Emote::from(user.mode).text(),
            medals = user.medals.as_ref().map_or(0, Vec::len),
        );

        if let Some(ref peak) = user.highest_rank {
            let _ = write!(
                description,
                "\nPeak rank: `#{rank}` (<t:{timestamp}:d>)",
                rank = with_comma_int(peak.rank),
                timestamp = peak.updated_at.unix_timestamp()
            );
        }

        Self {
            author: author!(user),
            description,
            fields: Vec::new(),
            footer: Some(Self::footer(user)),
            thumbnail: user.avatar_url.to_owned(),
        }
    }

    async fn user_stats(ctx: &Context, data: &mut ProfileData) -> Self {
        let bonus_pp = match data.bonus_pp(ctx).await {
            Some(pp) => format!("{pp:.2}pp"),
            None => "-".to_string(),
        };

        let score_rank = match data.score_rank(ctx).await {
            Some(rank) => format!("#{}", with_comma_int(rank)),
            None => "-".to_string(),
        };

        let ProfileData {
            user, author_id, ..
        } = data;

        let mut description = format!(
            "__**{mode} User statistics",
            mode = Emote::from(user.mode).text(),
        );

        if let Some(discord_id) = author_id {
            let _ = write!(description, " for <@{discord_id}>");
        }

        description.push_str(":**__");

        let stats = user.statistics.as_ref().unwrap();

        let hits_per_play = stats.total_hits as f32 / stats.playcount as f32;

        let peak_rank = match user.highest_rank {
            Some(ref peak) => format!(
                "#{rank} ({year}/{month:0>2})",
                rank = with_comma_int(peak.rank),
                year = peak.updated_at.year(),
                month = peak.updated_at.month() as u8,
            ),
            None => "-".to_string(),
        };

        let grades_value = format!(
            "{}{} {}{} {}{} {}{} {}{}",
            grade_emote(Grade::XH),
            stats.grade_counts.ssh,
            grade_emote(Grade::X),
            stats.grade_counts.ss,
            grade_emote(Grade::SH),
            stats.grade_counts.sh,
            grade_emote(Grade::S),
            stats.grade_counts.s,
            grade_emote(Grade::A),
            stats.grade_counts.a,
        );

        let playcount_value = format!(
            "{} / {} hrs",
            with_comma_int(stats.playcount),
            stats.playtime / 60 / 60
        );

        let fields = fields![
            "Ranked score", with_comma_int(stats.ranked_score).to_string(), true;
            "Max combo", with_comma_int(stats.max_combo).to_string(), true;
            "Accuracy", format!("{:.2}%", stats.accuracy), true;
            "Total score", with_comma_int(stats.total_score).to_string(), true;
            "Score rank", score_rank, true;
            "Level", format!("{:.2}", stats.level.float()), true;
            "Peak rank", peak_rank, true;
            "Bonus PP", bonus_pp, true;
            "Followers", with_comma_int(user.follower_count.unwrap_or(0)).to_string(), true;
            "Hits per play", with_comma_float(hits_per_play).to_string(), true;
            "Total hits", with_comma_int(stats.total_hits).to_string(), true;
            "Medals", format!("{}", user.medals.as_ref().unwrap().len()), true;
            "Grades", grades_value, false;
            "Play count / time", playcount_value, true;
            "Replays watched", with_comma_int(stats.replays_watched).to_string(), true;
        ];

        Self {
            author: author!(user),
            description,
            fields,
            footer: Some(Self::footer(user)),
            thumbnail: user.avatar_url.to_owned(),
        }
    }

    async fn top100_stats(ctx: &Context, data: &mut ProfileData) -> Self {
        let mode = data.user.mode;
        let author_id = data.author_id;

        let mut description = String::with_capacity(1024);

        let _ = write!(
            description,
            "__**{mode} Top100 statistics",
            mode = Emote::from(mode).text(),
        );

        if let Some(discord_id) = author_id {
            let _ = write!(description, " for <@{discord_id}>");
        }

        description.push_str(":**__\n");

        if let Some(stats) = data.top100stats(ctx).await {
            description.push_str("```\n");

            let Top100Stats {
                acc,
                combo,
                misses,
                pp,
                stars,
                ar,
                cs,
                hp,
                od,
                bpm,
                len,
            } = stats;

            fn min_avg_max<T: Number>(
                v: &MinMaxAvg<T>,
                f: fn(T) -> String,
            ) -> (String, String, String) {
                (f(v.min()), f(v.avg()), f(v.max()))
            }

            let combo_min = combo.min().to_string();
            let combo_avg = format!("{:.2}", combo.avg_float());
            let combo_max = combo.max().to_string();

            let misses_min = misses.min().to_string();
            let misses_avg = format!("{:.2}", misses.avg_float());
            let misses_max = misses.max().to_string();

            let (acc_min, acc_avg, acc_max) = min_avg_max(acc, |v| format!("{v:.2}"));
            let (pp_min, pp_avg, pp_max) = min_avg_max(pp, |v| format!("{v:.2}"));
            let (stars_min, stars_avg, stars_max) = min_avg_max(stars, |v| format!("{v:.2}"));
            let (ar_min, ar_avg, ar_max) = min_avg_max(ar, |v| format!("{v:.2}"));
            let (cs_min, cs_avg, cs_max) = min_avg_max(cs, |v| format!("{v:.2}"));
            let (hp_min, hp_avg, hp_max) = min_avg_max(hp, |v| format!("{v:.2}"));
            let (od_min, od_avg, od_max) = min_avg_max(od, |v| format!("{v:.2}"));
            let (bpm_min, bpm_avg, bpm_max) = min_avg_max(bpm, |v| format!("{v:.2}"));
            let (len_min, len_avg, len_max) = min_avg_max(len, |v| sec_to_minsec(v).to_string());

            let min_w = "Minimum"
                .len()
                .max(acc_min.len())
                .max(combo_min.len())
                .max(misses_min.len())
                .max(pp_min.len())
                .max(stars_min.len())
                .max(ar_min.len())
                .max(cs_min.len())
                .max(hp_min.len())
                .max(od_min.len())
                .max(bpm_min.len())
                .max(len_min.len());

            let avg_w = "Average"
                .len()
                .max(acc_avg.len())
                .max(combo_avg.len())
                .max(misses_avg.len())
                .max(pp_avg.len())
                .max(stars_avg.len())
                .max(ar_avg.len())
                .max(cs_avg.len())
                .max(hp_avg.len())
                .max(od_avg.len())
                .max(bpm_avg.len())
                .max(len_avg.len());

            let max_w = "Maximum"
                .len()
                .max(acc_max.len())
                .max(combo_max.len())
                .max(misses_max.len())
                .max(pp_max.len())
                .max(stars_max.len())
                .max(ar_max.len())
                .max(cs_max.len())
                .max(hp_max.len())
                .max(od_max.len())
                .max(bpm_max.len())
                .max(len_max.len());

            let _ = writeln!(
                description,
                "         | {min:^min_w$} | {avg:^avg_w$} | {max:^max_w$}",
                min = "Minimum",
                avg = "Average",
                max = "Maximum"
            );

            let _ = writeln!(
                description,
                "{dash:-^9}+-{dash:-^min_w$}-+-{dash:-^avg_w$}-+-{dash:-^max_w$}",
                dash = "-"
            );

            let _ = writeln!(
                description,
                "Accuracy | {acc_min:^min_w$} | {acc_avg:^avg_w$} | {acc_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "Combo    | {combo_min:^min_w$} | {combo_avg:^avg_w$} | {combo_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "Misses   | {misses_min:^min_w$} | {misses_avg:^avg_w$} | {misses_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "PP       | {pp_min:^min_w$} | {pp_avg:^avg_w$} | {pp_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "Stars    | {stars_min:^min_w$} | {stars_avg:^avg_w$} | {stars_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "AR       | {ar_min:^min_w$} | {ar_avg:^avg_w$} | {ar_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "CS       | {cs_min:^min_w$} | {cs_avg:^avg_w$} | {cs_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "HP       | {hp_min:^min_w$} | {hp_avg:^avg_w$} | {hp_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "OD       | {od_min:^min_w$} | {od_avg:^avg_w$} | {od_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "BPM      | {bpm_min:^min_w$} | {bpm_avg:^avg_w$} | {bpm_max:^max_w$}",
            );

            let _ = writeln!(
                description,
                "Length   | {len_min:^min_w$} | {len_avg:^avg_w$} | {len_max:^max_w$}",
            );

            description.push_str("```");
        } else {
            description.push_str("No top scores :(");
        };

        let ProfileData { user, .. } = data;

        Self {
            author: author!(user),
            description,
            fields: Vec::new(),
            footer: None,
            thumbnail: user.avatar_url.to_owned(),
        }
    }

    async fn top100_mods(ctx: &Context, data: &mut ProfileData) -> Self {
        let mut description = format!(
            "__**{mode} Top100 mods",
            mode = Emote::from(data.user.mode).text(),
        );

        if let Some(discord_id) = data.author_id {
            let _ = write!(description, " for <@{discord_id}>");
        }

        description.push_str(":**__\n");

        let fields = if let Some(stats) = data.top100mods(ctx).await {
            fn mod_value<V, F, const N: usize>(
                map: Vec<(GameMods, V)>,
                to_string: F,
                suffix: &str,
            ) -> Option<String>
            where
                F: Fn(&V) -> String,
            {
                let mut mods_len = [0; N];
                let mut vals_len = [0; N];

                let collected: Vec<_> = map
                    .into_iter()
                    .enumerate()
                    .map(|(i, (key, value))| {
                        let value = to_string(&value);

                        let i = i % N;
                        mods_len[i] = mods_len[i].max(key.len());
                        vals_len[i] = vals_len[i].max(value.len());

                        (key, value)
                    })
                    .collect();

                let mut iter = collected.iter().enumerate();

                if let Some((_, (mods, val))) = iter.next() {
                    let mut value = String::with_capacity(128);

                    let _ = write!(
                        value,
                        "`{mods}:{val:>0$}{suffix}`",
                        vals_len[0] + (mods_len[0].max(1) - mods.len().max(1)) * 2,
                    );

                    for (mut i, (mods, val)) in iter {
                        i %= N;

                        if i == 0 {
                            value.push('\n');
                        } else {
                            value.push_str(" • ");
                        }

                        let _ = write!(
                            value,
                            "`{mods}:{val:>0$}{suffix}`",
                            vals_len[i] + (mods_len[i].max(1) - mods.len().max(1)) * 2,
                        );
                    }

                    Some(value)
                } else {
                    None
                }
            }

            let mut fields = Vec::with_capacity(3);

            if let Some(val) = mod_value::<_, _, 4>(stats.percent_mods, u8::to_string, "%") {
                fields![fields { "Favourite mods", val, false }];
            }

            if let Some(val) = mod_value::<_, _, 3>(stats.percent_mod_comps, u8::to_string, "%") {
                fields![fields { "Favourite mod combinations", val, false }];
            }

            if let Some(val) = mod_value::<_, _, 3>(stats.pp_mod_comps, |pp| format!("{pp:.1}"), "")
            {
                fields![fields { "Profitable mod combinations (pp)", val, false }];
            }

            fields
        } else {
            description.push_str("No top scores :(");

            Vec::new()
        };

        let ProfileData { user, .. } = data;

        Self {
            author: author!(user),
            description,
            fields,
            footer: None,
            thumbnail: user.avatar_url.to_owned(),
        }
    }

    async fn top100_mappers(ctx: &Context, data: &mut ProfileData) -> Self {
        let mut description = format!(
            "__**{mode} Top100 mappers",
            mode = Emote::from(data.user.mode).text(),
        );

        if let Some(discord_id) = data.author_id {
            let _ = write!(description, " for <@{discord_id}>");
        }

        description.push_str(":**__\n");

        if let Some(mappers) = data.top100mappers(ctx).await {
            description.push_str("```\n");

            let mut names_len = 0;
            let mut pp_len = 2;
            let mut count_len = 1;

            let values: Vec<_> = mappers
                .iter()
                .map(|entry| {
                    let pp = format!("{:.2}", entry.pp);
                    let count = entry.count.to_string();

                    names_len = names_len.max(entry.name.len());
                    pp_len = pp_len.max(pp.len());
                    count_len = count_len.max(count.len());

                    (pp, count)
                })
                .collect();

            let _ = writeln!(
                description,
                "{blank:<names_len$} | {pp:^pp_len$} | {count:^count_len$}",
                blank = " ",
                pp = "PP",
                count = "#",
            );

            let _ = writeln!(
                description,
                "{dash:-<names_len$}-+-{dash:->pp_len$}-+-{dash:->count_len$}-",
                dash = "-",
            );

            for (entry, (pp, count)) in mappers.iter().zip(values) {
                let _ = writeln!(
                    description,
                    "{name:<names_len$} | {pp:>pp_len$} | {count:>count_len$}",
                    name = entry.name,
                );
            }

            description.push_str("```");
        } else {
            description.push_str("No top scores :(");
        }

        let ProfileData { user, .. } = data;

        Self {
            author: author!(user),
            description,
            fields: Vec::new(),
            footer: None,
            thumbnail: user.avatar_url.to_owned(),
        }
    }

    async fn mapper_stats(ctx: &Context, data: &mut ProfileData) -> Self {
        let own_maps_in_top100 = data.own_maps_in_top100(ctx).await;

        let ProfileData {
            user, author_id, ..
        } = data;

        let mut description = format!(
            "__**{mode} Mapper statistics",
            mode = Emote::from(user.mode).text(),
        );

        if let Some(discord_id) = author_id {
            let _ = write!(description, " for <@{discord_id}>");
        }

        description.push_str(":**__\n");

        let ranked_count = user.ranked_mapset_count.unwrap_or(0).to_string();
        let loved_count = user.loved_mapset_count.unwrap_or(0).to_string();
        let pending_count = user.pending_mapset_count.unwrap_or(0).to_string();
        let graveyard_count = user.graveyard_mapset_count.unwrap_or(0).to_string();
        let guest_count = user.guest_mapset_count.unwrap_or(0).to_string();

        let left_len = ranked_count
            .len()
            .max(pending_count.len())
            .max(guest_count.len());

        let right_len = loved_count.len().max(graveyard_count.len());

        let mapsets_value = format!(
            "`Ranked:  {:>left_len$}`  `Loved:     {:>right_len$}`\n\
            `Pending: {:>left_len$}`  `Graveyard: {:>right_len$}`\n\
            `Guest:   {:>left_len$}`",
            ranked_count, loved_count, pending_count, graveyard_count, guest_count,
        );

        let kudosu_value = format!(
            "`Available: {}` • `Total: {}`",
            user.kudosu.available, user.kudosu.total,
        );

        let mut fields = fields![
            "Mapsets", mapsets_value, false;
            "Kudosu", kudosu_value, false;
        ];

        if let Some(subscribers) = user.mapping_follower_count {
            fields![fields { "Subscribers", subscribers.to_string(), true }];
        }

        if let Some(count) = own_maps_in_top100 {
            fields![fields { "Own maps in top100", count.to_string(), true }];
        }

        Self {
            author: author!(user),
            description,
            fields,
            footer: None,
            thumbnail: user.avatar_url.to_owned(),
        }
    }

    fn footer(user: &User) -> FooterBuilder {
        let text = format!(
            "Joined osu! {} ({})",
            user.join_date.format(NAIVE_DATETIME_FORMAT).unwrap(),
            how_long_ago_text(&user.join_date),
        );

        FooterBuilder::new(text)
    }
}

impl EmbedData for ProfileEmbed {
    #[inline]
    fn build(self) -> Embed {
        let mut eb = EmbedBuilder::new()
            .author(self.author)
            .thumbnail(self.thumbnail);

        if !self.description.is_empty() {
            eb = eb.description(self.description);
        }

        if !self.fields.is_empty() {
            eb = eb.fields(self.fields);
        }

        if let Some(footer) = self.footer {
            eb = eb.footer(footer);
        }

        eb.build()
    }
}
