use crate::{embeds::EmbedData, util::constants::DESCRIPTION_SIZE};

use itertools::Itertools;
use rosu::model::GameMode;
use std::fmt::Write;

pub struct TrackListEmbed {
    title: &'static str,
    description: String,
}

impl TrackListEmbed {
    pub fn new(users: Vec<(String, GameMode, usize)>) -> Vec<Self> {
        let mut embeds = Vec::with_capacity(1);
        let title = "Tracked osu! users in this channel (limit)";
        let mut description = String::with_capacity(256);
        users
            .into_iter()
            .group_by(|(_, mode, _)| *mode)
            .into_iter()
            .for_each(|(mode, group)| {
                let mode = match mode {
                    GameMode::STD => "osu!standard",
                    GameMode::MNA => "osu!mania",
                    GameMode::TKO => "osu!taiko",
                    GameMode::CTB => "osu!ctb",
                };
                description.reserve(256);
                let mut names = group.map(|(name, _, limit)| (name, limit));
                let (first_name, first_limit) = names.next().unwrap();
                let len = description.chars().count() + mode.len() + first_name.chars().count() + 7;
                if len > DESCRIPTION_SIZE {
                    embeds.push(Self {
                        title,
                        description: description.to_owned(),
                    });
                    description.clear();
                }
                let _ = writeln!(description, "__**{}**__", mode);
                let _ = write!(description, "`{}` ({})", first_name, first_limit);
                let mut with_comma = true;
                for (name, limit) in names {
                    let len = description.chars().count() + name.chars().count() + 9;
                    if len > DESCRIPTION_SIZE {
                        embeds.push(Self {
                            title,
                            description: description.to_owned(),
                        });
                        description.clear();
                        let _ = writeln!(description, "__**{}**__", mode);
                        with_comma = false;
                    }
                    let _ = write!(
                        description,
                        "{}`{}` ({})",
                        if with_comma { ", " } else { "" },
                        name,
                        limit,
                    );
                    with_comma = true;
                }
                description.push('\n');
            });
        if description.lines().count() > 1 {
            embeds.push(Self { title, description });
        }
        embeds
    }
}

impl EmbedData for TrackListEmbed {
    fn title(&self) -> Option<&str> {
        Some(self.title)
    }
    fn description(&self) -> Option<&str> {
        Some(&self.description)
    }
}
