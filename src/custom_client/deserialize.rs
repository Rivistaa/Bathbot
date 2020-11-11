use chrono::{offset::TimeZone, DateTime, Utc};
use rosu::model::{ApprovalStatus, GameMode, GameMods, Grade};
use serde::{de, Deserialize, Deserializer};
use std::{convert::TryFrom, str::FromStr};

pub fn adjust_mode<'de, D: Deserializer<'de>>(d: D) -> Result<GameMode, D::Error> {
    let m: &str = Deserialize::deserialize(d)?;
    let m = match m {
        "osu" => GameMode::STD,
        "taiko" => GameMode::TKO,
        "fruits" => GameMode::CTB,
        "mania" => GameMode::MNA,
        _ => panic!("Could not parse mode `{}`", m),
    };
    Ok(m)
}

pub fn str_to_maybe_date<'de, D>(d: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Deserialize::deserialize(d)?;
    s.map(|s| Utc.datetime_from_str(&s, "%F %T"))
        .transpose()
        .map_err(de::Error::custom)
}

pub fn str_to_date<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
    Ok(str_to_maybe_date(d)?.unwrap())
}

pub fn str_to_maybe_f32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    let s: Option<String> = Deserialize::deserialize(d)?;
    Ok(s.and_then(|s| f32::from_str(&s).ok()))
}

pub fn str_to_f32<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(str_to_maybe_f32(d)?.unwrap_or_else(|| 0.0))
}

pub fn num_to_mode<'de, D: Deserializer<'de>>(d: D) -> Result<GameMode, D::Error> {
    let num: u8 = Deserialize::deserialize(d)?;
    Ok(GameMode::from(num))
}

pub fn str_to_approved<'de, D: Deserializer<'de>>(d: D) -> Result<ApprovalStatus, D::Error> {
    let num: i8 = Deserialize::deserialize(d)?;
    ApprovalStatus::try_from(num).map_err(de::Error::custom)
}

pub fn adjust_mods<'de, D: Deserializer<'de>>(d: D) -> Result<GameMods, D::Error> {
    let s: String = Deserialize::deserialize(d)?;
    if "None" == s.as_str() {
        return Ok(GameMods::NoMod);
    }
    let mods = s
        .split(',')
        .map(GameMods::from_str)
        .collect::<Result<Vec<GameMods>, _>>()
        .map_err(de::Error::custom)?
        .into_iter()
        .fold(GameMods::NoMod, |mods, next| mods | next);
    Ok(mods)
}

pub fn str_to_grade<'de, D: Deserializer<'de>>(d: D) -> Result<Grade, D::Error> {
    let s: String = Deserialize::deserialize(d)?;
    Grade::from_str(s.as_str()).map_err(de::Error::custom)
}

pub fn expect_negative_u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let i: i32 = Deserialize::deserialize(d)?;
    Ok(i.max(0) as u32)
}
