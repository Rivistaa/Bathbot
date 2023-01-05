use std::{borrow::Cow, collections::HashMap};

use bathbot_model::RankingEntries;
use bathbot_psql::{
    model::osu::{UserModeStatsColumn, UserStatsColumn},
    Database,
};
use bathbot_util::IntHasher;
use eyre::{Result, WrapErr};
use rosu_v2::prelude::{GameMode, User, Username};

#[derive(Copy, Clone)]
pub struct OsuUserManager<'d> {
    psql: &'d Database,
}

impl<'d> OsuUserManager<'d> {
    pub fn new(psql: &'d Database) -> Self {
        Self { psql }
    }

    pub async fn user_id(self, username: &str, alt_username: Option<&str>) -> Result<Option<u32>> {
        self.psql
            .select_osu_id_by_osu_name(username, alt_username)
            .await
            .wrap_err("failed to get osu id")
    }

    pub async fn name(self, user_id: u32) -> Result<Option<Username>> {
        self.psql
            .select_osu_name_by_osu_id(user_id)
            .await
            .wrap_err("failed to get username")
    }

    pub async fn names(self, user_ids: &[i32]) -> Result<HashMap<u32, Username, IntHasher>> {
        self.psql
            .select_osu_usernames(user_ids)
            .await
            .wrap_err("failed to get usernames")
    }

    pub async fn ids(&self, names: &[String]) -> Result<HashMap<Username, u32>> {
        let escaped_names = if names.iter().any(|name| name.contains('_')) {
            let names: Vec<_> = names.iter().map(|name| name.replace('_', "\\_")).collect();

            Cow::Owned(names)
        } else {
            Cow::Borrowed(names)
        };

        self.psql
            .select_osu_user_ids(escaped_names.as_ref())
            .await
            .wrap_err("failed to get user ids")
    }

    pub async fn stats(
        self,
        discord_ids: &[i64],
        column: UserStatsColumn,
    ) -> Result<RankingEntries> {
        self.psql
            .select_osu_user_stats(discord_ids, column)
            .await
            .map(RankingEntries::from)
            .wrap_err("failed to get user stats")
    }

    pub async fn stats_mode(
        self,
        discord_ids: &[i64],
        mode: GameMode,
        column: UserModeStatsColumn,
    ) -> Result<RankingEntries> {
        self.psql
            .select_osu_user_mode_stats(discord_ids, mode, column)
            .await
            .map(RankingEntries::from)
            .wrap_err("failed to get user mode stats")
    }

    pub async fn store_name(self, user_id: u32, username: &str) -> Result<()> {
        self.psql
            .upsert_osu_username(user_id, username)
            .await
            .wrap_err("failed to upsert osu username")
    }

    pub async fn store_user(self, user: &User, mode: GameMode) -> Result<()> {
        self.psql
            .upsert_osu_user(user, mode)
            .await
            .wrap_err("failed to upsert osu user")
    }

    pub async fn remove_stats(self, user_id: u32) -> Result<()> {
        self.psql
            .delete_osu_user_stats(user_id)
            .await
            .wrap_err("failed to delete osu user stats")
    }
}
