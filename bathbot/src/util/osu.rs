use std::{
    array::IntoIter,
    borrow::Cow,
    cmp::Ordering,
    convert::identity,
    fmt::{Display, Formatter, Result as FmtResult},
    io::Cursor,
    mem::MaybeUninit,
};

use bathbot_model::{rosu_v2::user::User, OsuStatsParams, ScoreSlim};
use bathbot_util::{
    datetime::SecToMinSec,
    numbers::{round, WithComma},
    MessageOrigin, ModsFormatter, ScoreExt,
};
use eyre::{Result, WrapErr};
use futures::{stream::FuturesOrdered, StreamExt};
use image::{
    imageops::FilterType, DynamicImage, GenericImage, GenericImageView, ImageOutputFormat,
};
use rosu_pp::{
    any::DifficultyAttributes, catch::CatchPerformance, osu::OsuPerformance,
    taiko::TaikoPerformance,
};
use rosu_v2::{
    model::mods::{
        DifficultyAdjustCatch, DifficultyAdjustMania, DifficultyAdjustOsu, DifficultyAdjustTaiko,
        GameMod, GameMods,
    },
    prelude::{GameModIntermode, GameMode, Grade, LegacyScoreStatistics, RankStatus, Score},
};
use time::OffsetDateTime;

use crate::{
    core::{BotConfig, Context},
    embeds::HitResultFormatter,
    manager::{redis::RedisData, OsuMap},
};

pub fn grade_emote(grade: Grade) -> &'static str {
    BotConfig::get().grade(grade)
}

// TODO: make struct that implements Display
pub fn grade_completion_mods<S: ScoreExt>(
    score: &S,
    mode: GameMode,
    n_objects: u32,
) -> Cow<'static, str> {
    let mods = score.mods();
    let grade = score.grade();
    let score_hits = score.total_hits(mode as u8);

    grade_completion_mods_raw(mods, grade, score_hits, mode, n_objects)
}

/// Careful about the grade!
///
/// The osu!api no longer uses `Grade::F` but this method expects `Grade::F`
/// for fails.
pub fn grade_completion_mods_raw(
    mods: &GameMods,
    grade: Grade,
    score_hits: u32,
    mode: GameMode,
    n_objects: u32,
) -> Cow<'static, str> {
    let grade_str = BotConfig::get().grade(grade);
    let mods_fmt = ModsFormatter::new(mods);

    match (
        mods.is_empty(),
        grade == Grade::F && mode != GameMode::Catch,
    ) {
        (true, true) => format!("{grade_str}@{}%", completion(score_hits, n_objects)).into(),
        (false, true) => format!(
            "{grade_str}@{}% +{mods_fmt}",
            completion(score_hits, n_objects)
        )
        .into(),
        (true, false) => grade_str.into(),
        (false, false) => format!("{grade_str} +{mods_fmt}").into(),
    }
}

fn completion(score_hits: u32, n_objects: u32) -> u32 {
    if n_objects != 0 {
        100 * score_hits / n_objects
    } else {
        100
    }
}

pub struct TopCounts {
    pub top1s: Cow<'static, str>,
    pub top1s_rank: Option<String>,
    pub top8s: Cow<'static, str>,
    pub top8s_rank: Option<String>,
    pub top15s: Cow<'static, str>,
    pub top15s_rank: Option<String>,
    pub top25s: Cow<'static, str>,
    pub top25s_rank: Option<String>,
    pub top50s: Cow<'static, str>,
    pub top50s_rank: Option<String>,
    pub top100s: Cow<'static, str>,
    pub top100s_rank: Option<String>,
    pub last_update: Option<OffsetDateTime>,
}

impl TopCounts {
    pub fn count_len(&self) -> usize {
        self.top100s.len()
    }

    pub async fn request(user: &RedisData<User>, mode: GameMode) -> Result<Self> {
        Self::request_osustats(user, mode).await
    }

    async fn request_osustats(user: &RedisData<User>, mode: GameMode) -> Result<Self> {
        let mut counts = [
            MaybeUninit::uninit(),
            MaybeUninit::uninit(),
            MaybeUninit::uninit(),
            MaybeUninit::uninit(),
            MaybeUninit::uninit(),
        ];

        let mut params = OsuStatsParams::new(user.username());
        params.mode(mode);
        let mut params_clone = params.clone();
        let mut get_amount = true;

        let mut iter = [100, 50, 25, 15, 8].into_iter().zip(counts.iter_mut());

        // Try to request 2 ranks concurrently
        while let Some((next_rank, next_count)) = iter.next() {
            if !get_amount {
                next_count.write("0".into());

                continue;
            }

            params.max_rank(next_rank);
            let next_fut = Context::client().get_global_scores(&params);

            let count = match iter.next() {
                Some((next_next_rank, next_next_count)) => {
                    params_clone.max_rank(next_next_rank);

                    let next_next_fut = Context::client().get_global_scores(&params_clone);

                    let (next_raw, next_next_raw) = tokio::try_join!(next_fut, next_next_fut)
                        .wrap_err("Failed to get global scores count")?;

                    let next_count_ = next_raw.count()?;
                    let next_next_count_ = next_next_raw.count()?;

                    next_count.write(WithComma::new(next_count_).to_string().into());
                    next_next_count.write(WithComma::new(next_next_count_).to_string().into());

                    next_next_count_
                }
                None => {
                    let next_raw = next_fut
                        .await
                        .wrap_err("Failed to get global scores count")?;

                    let next_count_ = next_raw.count()?;
                    next_count.write(WithComma::new(next_count_).to_string().into());

                    next_count_
                }
            };

            if count == 0 {
                get_amount = false;
            }
        }

        let top1s = match user {
            RedisData::Original(user) => user.scores_first_count,
            RedisData::Archive(user) => user.scores_first_count,
        };

        let top1s = WithComma::new(top1s).to_string().into();

        let [top100s, top50s, top25s, top15s, top8s] = counts;

        // SAFETY: All counts were initialized in the loop
        let this = unsafe {
            Self {
                top1s,
                top1s_rank: None,
                top8s: top8s.assume_init(),
                top8s_rank: None,
                top15s: top15s.assume_init(),
                top15s_rank: None,
                top25s: top25s.assume_init(),
                top25s_rank: None,
                top50s: top50s.assume_init(),
                top50s_rank: None,
                top100s: top100s.assume_init(),
                top100s_rank: None,
                last_update: None,
            }
        };

        Ok(this)
    }
}

pub struct TopCount<'a> {
    pub top_n: u8,
    pub count: Cow<'a, str>,
    pub rank: Option<Cow<'a, str>>,
}

pub struct TopCountsIntoIter {
    top_n: IntoIter<u8, 6>,
    counts: IntoIter<Option<Cow<'static, str>>, 6>,
    ranks: IntoIter<Option<String>, 6>,
}

impl Iterator for TopCountsIntoIter {
    type Item = TopCount<'static>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let count = TopCount {
            top_n: self.top_n.next()?,
            count: self.counts.next().flatten()?,
            rank: self.ranks.next()?.map(Cow::Owned),
        };

        Some(count)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.top_n.size_hint()
    }
}

impl IntoIterator for TopCounts {
    type IntoIter = TopCountsIntoIter;
    type Item = TopCount<'static>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        let Self {
            top1s,
            top1s_rank,
            top8s,
            top8s_rank,
            top15s,
            top15s_rank,
            top25s,
            top25s_rank,
            top50s,
            top50s_rank,
            top100s,
            top100s_rank,
            last_update: _,
        } = self;

        let top_n = [1, 8, 15, 25, 50, 100];

        let counts = [
            Some(top1s),
            Some(top8s),
            Some(top15s),
            Some(top25s),
            Some(top50s),
            Some(top100s),
        ];

        let ranks = [
            top1s_rank,
            top8s_rank,
            top15s_rank,
            top25s_rank,
            top50s_rank,
            top100s_rank,
        ];

        TopCountsIntoIter {
            top_n: top_n.into_iter(),
            counts: counts.into_iter(),
            ranks: ranks.into_iter(),
        }
    }
}

pub struct TopCountsIter<'a> {
    top_n: IntoIter<u8, 6>,
    counts: IntoIter<Option<&'a str>, 6>,
    ranks: IntoIter<Option<&'a str>, 6>,
}

impl<'a> Iterator for TopCountsIter<'a> {
    type Item = TopCount<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let count = TopCount {
            top_n: self.top_n.next()?,
            count: self.counts.next().flatten().map(Cow::Borrowed)?,
            rank: self.ranks.next()?.map(Cow::Borrowed),
        };

        Some(count)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.top_n.size_hint()
    }
}

impl<'a> IntoIterator for &'a TopCounts {
    type IntoIter = TopCountsIter<'a>;
    type Item = TopCount<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        let TopCounts {
            top1s,
            top1s_rank,
            top8s,
            top8s_rank,
            top15s,
            top15s_rank,
            top25s,
            top25s_rank,
            top50s,
            top50s_rank,
            top100s,
            top100s_rank,
            last_update: _,
        } = self;

        let top_n = [1, 8, 15, 25, 50, 100];

        let counts = [
            Some(top1s.as_ref()),
            Some(top8s.as_ref()),
            Some(top15s.as_ref()),
            Some(top25s.as_ref()),
            Some(top50s.as_ref()),
            Some(top100s.as_ref()),
        ];

        let ranks = [
            top1s_rank.as_deref(),
            top8s_rank.as_deref(),
            top15s_rank.as_deref(),
            top25s_rank.as_deref(),
            top50s_rank.as_deref(),
            top100s_rank.as_deref(),
        ];

        TopCountsIter {
            top_n: top_n.into_iter(),
            counts: counts.into_iter(),
            ranks: ranks.into_iter(),
        }
    }
}

#[derive(Clone)]
pub struct IfFc {
    mode: GameMode,
    pub statistics: LegacyScoreStatistics,
    pub pp: f32,
}

impl IfFc {
    pub async fn new(score: &ScoreSlim, map: &OsuMap) -> Option<Self> {
        let mode = score.mode;
        let mut calc = Context::pp(map).mods(&score.mods).mode(score.mode);
        let attrs = calc.difficulty().await;

        if score.is_fc(mode, attrs.max_combo()) {
            return None;
        }

        let mods = score.mods.bits();
        let stats = &score.statistics;

        let (pp, statistics, mode) = match attrs {
            DifficultyAttributes::Osu(attrs) => {
                let total_objects = map.n_objects();
                let passed_objects =
                    stats.count_300 + stats.count_100 + stats.count_50 + stats.count_miss;

                let mut n300 = stats.count_300 + total_objects.saturating_sub(passed_objects);

                let count_hits = total_objects - stats.count_miss;
                let ratio = 1.0 - (n300 as f32 / count_hits as f32);
                let new100s = (ratio * stats.count_miss as f32).ceil() as u32;

                n300 += stats.count_miss.saturating_sub(new100s);
                let n100 = stats.count_100 + new100s;
                let n50 = stats.count_50;

                let attrs = OsuPerformance::from(attrs.to_owned())
                    .mods(mods)
                    .n300(n300)
                    .n100(n100)
                    .n50(n50)
                    .calculate();

                let statistics = LegacyScoreStatistics {
                    count_300: n300,
                    count_100: n100,
                    count_50: n50,
                    count_geki: stats.count_geki,
                    count_katu: stats.count_katu,
                    count_miss: 0,
                };

                (attrs.pp as f32, statistics, GameMode::Osu)
            }
            DifficultyAttributes::Taiko(attrs) => {
                let total_objects = map.n_circles();
                let passed_objects =
                    (stats.count_300 + stats.count_100 + stats.count_miss) as usize;

                let mut n300 =
                    stats.count_300 as usize + total_objects.saturating_sub(passed_objects);

                let count_hits = total_objects - stats.count_miss as usize;
                let ratio = 1.0 - (n300 as f32 / count_hits as f32);
                let new100s = (ratio * stats.count_miss as f32).ceil() as u32;

                n300 += stats.count_miss.saturating_sub(new100s) as usize;
                let n100 = (stats.count_100 + new100s) as usize;

                let acc = 100.0 * (2 * n300 + n100) as f32 / (2 * total_objects) as f32;

                let attrs = TaikoPerformance::from(attrs.to_owned())
                    .mods(mods)
                    .accuracy(acc as f64)
                    .calculate();

                let statistics = LegacyScoreStatistics {
                    count_300: n300 as u32,
                    count_100: n100 as u32,
                    count_geki: stats.count_geki,
                    count_katu: stats.count_katu,
                    count_50: stats.count_50,
                    count_miss: 0,
                };

                (attrs.pp as f32, statistics, GameMode::Taiko)
            }
            DifficultyAttributes::Catch(attrs) => {
                let total_objects = attrs.max_combo();
                let passed_objects = stats.count_300 + stats.count_100 + stats.count_miss;

                let missing = total_objects - passed_objects;
                let missing_fruits =
                    missing.saturating_sub(attrs.n_droplets.saturating_sub(stats.count_100));

                let missing_droplets = missing - missing_fruits;

                let n_fruits = stats.count_300 + missing_fruits;
                let n_droplets = stats.count_100 + missing_droplets;
                let n_tiny_droplet_misses = stats.count_katu;
                let n_tiny_droplets = attrs.n_tiny_droplets.saturating_sub(n_tiny_droplet_misses);

                let attrs = CatchPerformance::from(attrs.to_owned())
                    .mods(mods)
                    .fruits(n_fruits)
                    .droplets(n_droplets)
                    .tiny_droplets(n_tiny_droplets)
                    .tiny_droplet_misses(n_tiny_droplet_misses)
                    .calculate();

                let statistics = LegacyScoreStatistics {
                    count_300: n_fruits,
                    count_100: n_droplets,
                    count_50: n_tiny_droplets,
                    count_geki: stats.count_geki,
                    count_katu: stats.count_katu,
                    count_miss: 0,
                };

                (attrs.pp as f32, statistics, GameMode::Catch)
            }
            DifficultyAttributes::Mania(_) => return None,
        };

        Some(Self {
            mode,
            statistics,
            pp,
        })
    }

    pub fn accuracy(&self) -> f32 {
        self.statistics.accuracy(self.mode)
    }

    pub fn hitresults(&self) -> HitResultFormatter {
        HitResultFormatter::new(self.mode, self.statistics.clone())
    }
}

pub async fn get_combined_thumbnail<'s>(
    avatar_urls: impl IntoIterator<Item = &'s str>,
    amount: u32,
    width: Option<u32>,
) -> Result<Vec<u8>> {
    let width = width.map_or(128, |w| w.max(128));
    let mut combined = DynamicImage::new_rgba8(width, 128);
    let w = (width / amount).min(128);
    let total_offset = (width - amount * w) / 2;

    // Future stream
    let mut pfp_futs: FuturesOrdered<_> = avatar_urls
        .into_iter()
        .map(|url| Context::client().get_avatar(url))
        .collect();

    let mut next = pfp_futs.next().await;
    let mut i = 0;

    // Closure that stitches the stripe onto the combined image
    let mut img_combining = |img: DynamicImage, i: u32| {
        let img = img.resize_exact(128, 128, FilterType::Lanczos3);

        let dst_offset = total_offset + i * w;

        let src_offset = if amount == 1 {
            0
        } else {
            (w < 128) as u32 * i * (128 - w) / (amount - 1)
        };

        for i in 0..w {
            for j in 0..128 {
                let pixel = img.get_pixel(src_offset + i, j);
                combined.put_pixel(dst_offset + i, j, pixel);
            }
        }
    };

    // Process the stream elements
    while let Some(pfp_result) = next {
        let pfp = pfp_result?;
        let img = image::load_from_memory(&pfp)?;
        let (res, _) = tokio::join!(pfp_futs.next(), async { img_combining(img, i) });
        next = res;
        i += 1;
    }

    let capacity = width as usize * 128;
    let png_bytes: Vec<u8> = Vec::with_capacity(capacity);
    let mut cursor = Cursor::new(png_bytes);
    combined.write_to(&mut cursor, ImageOutputFormat::Png)?;

    Ok(cursor.into_inner())
}

pub struct MapInfo<'a> {
    map: &'a OsuMap,
    stars: f32,
    mods: Option<&'a GameMods>,
    clock_rate: Option<f32>,
}

impl<'a> MapInfo<'a> {
    pub fn new(map: &'a OsuMap, stars: f32) -> Self {
        Self {
            map,
            stars,
            mods: None,
            clock_rate: None,
        }
    }

    pub fn mods(&mut self, mods: &'a GameMods) -> &mut Self {
        self.mods = Some(mods);

        self
    }

    pub fn clock_rate(&mut self, clock_rate: Option<f32>) -> &mut Self {
        self.clock_rate = clock_rate;

        self
    }

    pub fn keys(mods: u32, cs: f32) -> f32 {
        if (mods & GameModIntermode::OneKey.bits().unwrap()) > 0 {
            1.0
        } else if (mods & GameModIntermode::TwoKeys.bits().unwrap()) > 0 {
            2.0
        } else if (mods & GameModIntermode::ThreeKeys.bits().unwrap()) > 0 {
            3.0
        } else if (mods & GameModIntermode::FourKeys.bits().unwrap()) > 0 {
            4.0
        } else if (mods & GameModIntermode::FiveKeys.bits().unwrap()) > 0 {
            5.0
        } else if (mods & GameModIntermode::SixKeys.bits().unwrap()) > 0 {
            6.0
        } else if (mods & GameModIntermode::SevenKeys.bits().unwrap()) > 0 {
            7.0
        } else if (mods & GameModIntermode::EightKeys.bits().unwrap()) > 0 {
            8.0
        } else if (mods & GameModIntermode::NineKeys.bits().unwrap()) > 0 {
            9.0
        } else {
            round(cs)
        }
    }
}

impl Display for MapInfo<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let mods = self.mods.map_or(0, GameMods::bits);

        let mut builder = self.map.attributes();

        let clock_rate = self
            .clock_rate
            .or_else(|| self.mods.and_then(GameMods::clock_rate));

        if let Some(clock_rate) = clock_rate {
            builder = builder.clock_rate(f64::from(clock_rate));
        }

        if let Some(mods) = self.mods {
            for gamemod in mods.iter() {
                match gamemod {
                    GameMod::DifficultyAdjustOsu(m) => {
                        let DifficultyAdjustOsu {
                            circle_size,
                            approach_rate,
                            drain_rate,
                            overall_difficulty,
                            ..
                        } = m;

                        if let Some(cs) = circle_size {
                            builder = builder.cs(*cs, false);
                        }

                        if let Some(ar) = approach_rate {
                            builder = builder.ar(*ar, false);
                        }

                        if let Some(hp) = drain_rate {
                            builder = builder.hp(*hp, false);
                        }

                        if let Some(od) = overall_difficulty {
                            builder = builder.od(*od, false);
                        }
                    }
                    GameMod::DifficultyAdjustTaiko(m) => {
                        let DifficultyAdjustTaiko {
                            drain_rate,
                            overall_difficulty,
                            ..
                        } = m;

                        if let Some(hp) = drain_rate {
                            builder = builder.hp(*hp, false);
                        }

                        if let Some(od) = overall_difficulty {
                            builder = builder.od(*od, false);
                        }
                    }
                    GameMod::DifficultyAdjustCatch(m) => {
                        let DifficultyAdjustCatch {
                            circle_size,
                            approach_rate,
                            drain_rate,
                            overall_difficulty,
                            ..
                        } = m;

                        if let Some(cs) = circle_size {
                            builder = builder.cs(*cs, false);
                        }

                        if let Some(ar) = approach_rate {
                            builder = builder.ar(*ar, false);
                        }

                        if let Some(hp) = drain_rate {
                            builder = builder.hp(*hp, false);
                        }

                        if let Some(od) = overall_difficulty {
                            builder = builder.od(*od, false);
                        }
                    }
                    GameMod::DifficultyAdjustMania(m) => {
                        let DifficultyAdjustMania {
                            drain_rate,
                            overall_difficulty,
                            ..
                        } = m;

                        if let Some(hp) = drain_rate {
                            builder = builder.hp(*hp, false);
                        }

                        if let Some(od) = overall_difficulty {
                            builder = builder.od(*od, false);
                        }
                    }
                    _ => {}
                }
            }
        }

        let attrs = builder.mods(mods).build();

        let clock_rate = attrs.clock_rate;
        let mut sec_drain = self.map.seconds_drain();
        let mut bpm = self.map.bpm();

        if (clock_rate - 1.0).abs() > f64::EPSILON {
            let clock_rate = clock_rate as f32;

            bpm *= clock_rate;
            sec_drain = (sec_drain as f32 / clock_rate) as u32;
        }

        let (cs_key, cs_value) = if self.map.mode() == GameMode::Mania {
            ("Keys", Self::keys(mods, attrs.cs as f32))
        } else {
            ("CS", round(attrs.cs as f32))
        };

        write!(
            f,
            "Length: `{len}` BPM: `{bpm}` Objects: `{objs}`\n\
            {cs_key}: `{cs_value}` AR: `{ar}` OD: `{od}` HP: `{hp}` Stars: `{stars}`",
            len = SecToMinSec::new(sec_drain),
            bpm = round(bpm),
            objs = self.map.n_objects(),
            ar = round(attrs.ar as f32),
            od = round(attrs.od as f32),
            hp = round(attrs.hp as f32),
            stars = round(self.stars),
        )
    }
}

/// Note that all contained indices start at 0.
pub enum PersonalBestIndex {
    /// Found the score in the top100
    FoundScore { idx: usize },
    /// There was a score on the same map with more pp in the top100
    FoundBetter { idx: usize },
    /// Found another score on the same map and the
    /// same mods that has more score but less pp
    ScoreV1d { would_be_idx: usize, old_idx: usize },
    /// Score is ranked and has enough pp to be in but wasn't found
    Presumably { idx: usize },
    /// Score is not ranked but has enough pp to be in the top100
    IfRanked { idx: usize },
    /// Score does not have enough pp to be in the top100
    NotTop100,
}

impl PersonalBestIndex {
    pub fn new(score: &ScoreSlim, map_id: u32, status: RankStatus, top100: &[Score]) -> Self {
        // Note that the index is determined through float
        // comparisons which could result in issues
        let idx = top100
            .binary_search_by(|probe| {
                probe
                    .pp
                    .and_then(|pp| score.pp.partial_cmp(&pp))
                    .unwrap_or(Ordering::Less)
            })
            .unwrap_or_else(identity);

        if idx == 100 {
            return Self::NotTop100;
        } else if !matches!(status, RankStatus::Ranked | RankStatus::Approved) {
            return Self::IfRanked { idx };
        } else if top100.get(idx).filter(|&top| score.is_eq(top)).is_some() {
            // If multiple scores have the exact same pp as the given
            // score then `idx` might not belong to the given score.
            // Chances are pretty slim though so this should be fine.
            return Self::FoundScore { idx };
        }

        let (better, worse) = top100.split_at(idx);

        // A case that's not covered is when there is a score
        // with more pp on the same map with the same mods that has
        // less score than the current score. Sounds really fringe though.
        if let Some(idx) = better.iter().position(|top| top.map_id == map_id) {
            Self::FoundBetter { idx }
        } else if let Some(i) = worse.iter().position(|top| {
            top.map_id == map_id && top.mods == score.mods && top.score > score.score
        }) {
            Self::ScoreV1d {
                would_be_idx: idx,
                old_idx: idx + i,
            }
        } else {
            Self::Presumably { idx }
        }
    }

    pub fn into_embed_description(self, origin: &MessageOrigin) -> Option<String> {
        match self {
            PersonalBestIndex::FoundScore { idx } => Some(format!("Personal Best #{}", idx + 1)),
            PersonalBestIndex::FoundBetter { .. } => None,
            PersonalBestIndex::ScoreV1d {
                would_be_idx,
                old_idx,
            } => Some(format!(
                "Personal Best #{idx} ([v1'd]({origin} \
                \"there is a play on the same map with the same mods that has more score\"\
                ) by #{old})",
                idx = would_be_idx + 1,
                old = old_idx + 1
            )),
            PersonalBestIndex::Presumably { idx } => Some(format!(
                "Personal Best #{} [(?)]({origin} \
                \"the top100 did not include this score likely because the api \
                wasn't done processing but presumably the score is in there\")",
                idx + 1
            )),
            PersonalBestIndex::IfRanked { idx } => {
                Some(format!("Personal Best #{} (if ranked)", idx + 1))
            }
            PersonalBestIndex::NotTop100 => None,
        }
    }
}
