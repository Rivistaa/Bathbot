use crate::{database::GuildConfig, Context};

use twilight::model::id::GuildId;

impl Context {
    pub fn config_authorities(&self, guild_id: GuildId) -> Vec<u64> {
        let config = self.data.guilds.entry(guild_id).or_default();
        config.authorities.clone()
    }

    pub fn config_authorities_collect<F, T>(&self, guild_id: GuildId, f: F) -> Vec<T>
    where
        F: FnMut(u64) -> T,
    {
        let config = self.data.guilds.entry(guild_id).or_default();
        config.authorities.iter().copied().map(f).collect()
    }

    pub fn config_prefixes(&self, guild_id: GuildId) -> Vec<String> {
        let config = self.data.guilds.entry(guild_id).or_default();
        config.prefixes.clone()
    }

    pub fn config_first_prefix(&self, guild_id: Option<GuildId>) -> String {
        match guild_id {
            Some(guild_id) => {
                let config = self.data.guilds.entry(guild_id).or_default();
                config.prefixes[0].clone()
            }
            None => "<".to_owned(),
        }
    }

    pub fn config_lyrics(&self, guild_id: GuildId) -> bool {
        let config = self.data.guilds.entry(guild_id).or_default();
        config.with_lyrics
    }

    pub fn update_config<F>(&self, guild_id: GuildId, f: F)
    where
        F: FnOnce(&mut GuildConfig),
    {
        let mut config = self.data.guilds.entry(guild_id).or_default();
        f(config.value_mut());
        config.modified = true;
    }
}