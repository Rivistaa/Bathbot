mod channel;
mod emoji;
mod guild;
mod member;
mod role;
mod user;

pub use channel::CachedChannel;
pub use emoji::CachedEmoji;
pub use guild::{CachedGuild, ColdStorageGuild};
pub use member::{CachedMember, ColdStorageMember};
pub use role::CachedRole;
pub use user::CachedUser;

use crate::{
    core::{BotStats, Context, ShardState},
    BotResult, Error,
};
use twilight::model::channel::{Channel, GuildChannel, PrivateChannel};
use twilight::model::gateway::payload::{MemberUpdate, RequestGuildMembers};
use twilight::model::gateway::presence::{ActivityType, Status};

use darkredis::ConnectionPool;
use dashmap::DashMap;
use futures::future;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use twilight::gateway::Event;
use twilight::model::id::{ChannelId, EmojiId, GuildId, UserId};
use twilight::model::user::User;

pub struct Cache {
    // cluster info
    cluster_id: u64,

    //cache
    pub guilds: DashMap<GuildId, Arc<CachedGuild>>,
    pub guild_channels: DashMap<ChannelId, Arc<CachedChannel>>,
    pub private_channels: DashMap<ChannelId, Arc<CachedChannel>>,
    pub dm_channels_by_user: DashMap<UserId, Arc<CachedChannel>>,
    pub users: DashMap<UserId, Arc<CachedUser>>,
    pub emoji: DashMap<EmojiId, Arc<CachedEmoji>>,
    // is this even possible to get accurate across multiple clusters?
    pub filling: AtomicBool,

    pub unavailable_guilds: RwLock<Vec<GuildId>>,
    pub expected: RwLock<Vec<GuildId>>,

    pub stats: Arc<BotStats>,
    pub missing_per_shard: DashMap<u64, AtomicU64>,
}

impl Cache {
    pub fn new(stats: Arc<BotStats>) -> Self {
        Cache {
            cluster_id: 0,
            guilds: DashMap::new(),
            guild_channels: DashMap::new(),
            private_channels: DashMap::new(),
            dm_channels_by_user: DashMap::new(),
            users: DashMap::new(),
            emoji: DashMap::new(),
            filling: AtomicBool::new(true),
            unavailable_guilds: RwLock::new(vec![]),
            expected: RwLock::new(vec![]),
            stats,
            missing_per_shard: DashMap::new(),
        }
    }

    pub fn reset(&self) {
        self.guilds.clear();
        self.guild_channels.clear();
        self.users.clear();
        self.emoji.clear();
        self.filling.store(true, Ordering::SeqCst);
        self.private_channels.clear();
    }

    pub async fn update(&self, shard_id: u64, event: &Event, ctx: Arc<Context>) -> BotResult<()> {
        match event {
            Event::Ready(ready) => {
                self.missing_per_shard
                    .insert(shard_id, AtomicU64::new(ready.guilds.len() as u64));
            }
            Event::GuildCreate(e) => {
                trace!("Received guild create event for \"{}\" ({})", e.name, e.id);
                let cached_guild = self.guilds.get(&e.id);
                if let Some(cached_guild) = cached_guild {
                    if !cached_guild.complete.load(Ordering::SeqCst) {
                        self.stats.guild_counts.partial.dec();
                    } else {
                        self.stats.guild_counts.loaded.dec();
                    }
                    self.nuke_guild_cache(cached_guild.value())
                }
                let guild = CachedGuild::from(e.0.clone());
                for channel in &guild.channels {
                    self.guild_channels
                        .insert(channel.get_id(), channel.value().clone());
                }
                self.stats.channel_count.add(guild.channels.len() as i64);
                for emoji in &guild.emoji {
                    self.emoji.insert(emoji.id, emoji.clone());
                }
                // We dont need this mutable but acquire a write lock regardless to prevent potential deadlocks
                let mut list = self.unavailable_guilds.write().unwrap();
                match list.iter().position(|id| id.0 == guild.id.0) {
                    Some(index) => {
                        list.remove(index);
                        info!("Guild \"{}\" ({}) available again", guild.name, guild.id);
                    }
                    None => {}
                }
                // Trigger member chunk events
                let data = RequestGuildMembers::new_all(guild.id, None);
                ctx.cluster
                    .command(shard_id, &data)
                    .await
                    .map_err(Error::TwilightCluster)?;
                // Add to cache
                self.guilds.insert(e.id, Arc::new(guild));
                self.stats.guild_counts.partial.inc();
            }
            Event::GuildUpdate(update) => {
                trace!(
                    "Receive guild update for \"{}\" ({})",
                    update.name,
                    update.id
                );
                debug!("{:?}", update);
                match self.get_guild(update.id) {
                    Some(old_guild) => {
                        old_guild.update(&update.0);
                    }
                    None => {
                        warn!(
                            "Got guild update for \"{}\" ({}) but guild was not found in cache",
                            update.name, update.id
                        );
                    }
                }
            }
            Event::GuildDelete(guild) => match self.get_guild(guild.id) {
                Some(cached_guild) => {
                    if !cached_guild.complete.load(Ordering::SeqCst) {
                        self.stats.guild_counts.partial.dec();
                    } else {
                        self.stats.guild_counts.loaded.dec();
                    }
                    if guild.unavailable {
                        self.guild_unavailable(&cached_guild);
                    }
                    self.nuke_guild_cache(&cached_guild)
                }
                None => {}
            },
            Event::MemberChunk(chunk) => {
                trace!(
                    "Received member chunk {}/{} (nonce: {:?}) for guild {}",
                    chunk.chunk_index + 1,
                    chunk.chunk_count,
                    chunk.nonce,
                    chunk.guild_id
                );
                match self.get_guild(chunk.guild_id) {
                    Some(guild) => {
                        for (user_id, member) in chunk.members.clone() {
                            self.get_or_insert_user(&member.user);
                            let member = CachedMember::from_member(&member, self);
                            member.user.mutual_servers.fetch_add(1, Ordering::SeqCst);
                            guild.members.insert(user_id, Arc::new(member));
                        }
                        self.stats.user_counts.total.add(chunk.members.len() as i64);
                        if (chunk.chunk_count - 1) == chunk.chunk_index && chunk.nonce.is_none() {
                            debug!(
                                "Finished processing chunks for \"{}\" ({}), {:?} guilds to go...",
                                guild.name,
                                guild.id.0,
                                self.stats.guild_counts.partial.get()
                            );
                            guild.complete.store(true, Ordering::SeqCst);
                            let shard_missing = self
                                .missing_per_shard
                                .get(&shard_id)
                                .unwrap()
                                .fetch_sub(1, Ordering::Relaxed);
                            if shard_missing == 1 {
                                // this shard is ready
                                info!("All guilds cached for shard {}", shard_id);
                                if chunk.nonce.is_none() && self.shard_cached(shard_id) {
                                    ctx.set_shard_activity(
                                        shard_id,
                                        Status::Online,
                                        ActivityType::Watching,
                                        String::from("the gears turn"),
                                    )
                                    .await?
                                }
                            }
                            self.stats.guild_counts.partial.dec();
                            self.stats.guild_counts.loaded.inc();
                            // if we where at 1 we are now at 0
                            if self.stats.guild_counts.partial.get() == 0
                                && self.filling.load(Ordering::Relaxed)
                                && ctx
                                    .shard_states
                                    .iter()
                                    .all(|state| state.value() == &ShardState::Ready)
                            {
                                info!(
                                    "Initial cache filling completed for cluster {}",
                                    self.cluster_id
                                );
                                self.filling.store(false, Ordering::SeqCst);
                            }
                        }
                    }
                    None => {
                        error!(
                            "Received member chunks for guild {} before its creation",
                            chunk.guild_id
                        );
                    }
                }
            }

            Event::ChannelCreate(event) => {
                // TODO: Add more details
                trace!("Received channel create event");
                match &event.0 {
                    Channel::Group(_group) => {}
                    Channel::Guild(guild_channel) => {
                        let guild_id = match guild_channel {
                            GuildChannel::Category(category) => category.guild_id,
                            GuildChannel::Text(text) => text.guild_id,
                            GuildChannel::Voice(voice) => voice.guild_id,
                        };
                        match guild_id {
                            Some(guild_id) => {
                                let channel = CachedChannel::from_guild_channel(guild_channel, guild_id);
                                match self.get_guild(guild_id) {
                                    Some(guild) => {
                                        let arced = Arc::new(channel);
                                        guild.channels.insert(arced.get_id(), arced.clone());
                                        self.guild_channels.insert(arced.get_id(), arced);
                                        self.stats.channel_count.inc();
                                    }
                                    None => error!(
                                        "Channel create received for #{} **``{}``** in guild **``{}``** but this guild does not exist in cache!",
                                        channel.get_name(),
                                        channel.get_id(),
                                        guild_id
                                    ),
                                }
                            }
                            None => warn!(
                                "We got a channel create event for a guild type channel without guild id!"
                            ),
                        }
                    }
                    Channel::Private(private_channel) => {
                        self.insert_private_channel(private_channel);
                    }
                };
            }
            Event::ChannelUpdate(channel) => match &channel.0 {
                Channel::Group(_group) => {}
                Channel::Guild(guild_channel) => {
                    let guild_id = match guild_channel {
                        GuildChannel::Category(cateogry) => cateogry.guild_id,
                        GuildChannel::Text(text) => text.guild_id,
                        GuildChannel::Voice(voice) => voice.guild_id,
                    };
                    match guild_id {
                        Some(guild_id) => match self.get_guild(guild_id) {
                            Some(guild) => {
                                let channel =
                                    CachedChannel::from_guild_channel(guild_channel, guild.id);
                                let arced = Arc::new(channel);
                                guild.channels.insert(arced.get_id(), arced.clone());
                                self.guild_channels.insert(arced.get_id(), arced);
                            }
                            None => warn!(
                                "Got channel update for guild {} but guild not cached",
                                guild_id
                            ),
                        },
                        None => {
                            warn!("Got channel update for guild type channel without guild id!")
                        }
                    }
                }
                Channel::Private(private) => {
                    self.insert_private_channel(private);
                }
            },
            Event::ChannelDelete(channel) => {
                // TODO: Add more info
                trace!("Received channel delete event for a channel");
                match &channel.0 {
                    Channel::Group(_group) => {}
                    Channel::Guild(guild_channel) => {
                        let (guild_id, channel_id) = match guild_channel {
                            GuildChannel::Text(text) => (text.guild_id, text.id),
                            GuildChannel::Voice(voice) => (voice.guild_id, voice.id),
                            GuildChannel::Category(category) => (category.guild_id, category.id),
                        };
                        match guild_id {
                            Some(guild_id) => match self.get_guild(guild_id) {
                                Some(guild) => {
                                    guild.channels.remove(&channel_id);
                                    self.stats.channel_count.dec();
                                }
                                None => {
                                    warn!("Got channel delete event for channel {} for guild {} but guild not in cache", channel_id, guild_id);
                                }
                            },
                            None => {
                                warn!("Got channel delete event for channel {} of some guild but without guild id", channel_id);
                            }
                        }
                    }
                    // Do these even ever get deleted?
                    Channel::Private(channel) => {
                        self.private_channels.remove(&channel.id);
                        if channel.recipients.len() == 1 {
                            self.dm_channels_by_user.remove(&channel.recipients[0].id);
                        }
                    }
                }
            }

            Event::MemberAdd(event) => {
                debug!("{} joined {}", event.user.id, event.guild_id);
                match self.get_guild(event.guild_id) {
                    Some(guild) => {
                        let member = CachedMember::from_member(&event.0, &self);
                        member.user.mutual_servers.fetch_add(1, Ordering::SeqCst);
                        guild.members.insert(event.user.id, Arc::new(member));
                        guild.member_count.fetch_add(1, Ordering::Relaxed);

                        self.stats.user_counts.total.inc();
                    }
                    None => warn!(
                        "Received member add event for guild {} before guild create",
                        event.guild_id
                    ),
                }
            }
            Event::MemberUpdate(event) => {
                let e = event.clone();
                let c = ctx.clone();
                if !Cache::member_update(shard_id, &c, &e, true).await {
                    tokio::spawn(async move {
                        tokio::time::delay_for(Duration::from_millis(100)).await;
                        Cache::member_update(shard_id, &c, &e, false).await;
                    });
                }
            }
            Event::MemberRemove(event) => {
                debug!("{} left {}", event.user.id, event.guild_id);
                match self.get_guild(event.guild_id) {
                    Some(guild) => match guild.members.remove_take(&event.user.id) {
                        Some(member) => {
                            let servers = member.user.mutual_servers.fetch_sub(1, Ordering::SeqCst);
                            if servers == 1 {
                                self.users.remove(&member.user.id);
                                self.stats.user_counts.unique.dec();
                            }
                            self.stats.user_counts.total.dec();
                        }
                        None => {
                            if guild.complete.load(Ordering::SeqCst) {
                                warn!("Received member remove event for member that is not in that guild");
                            }
                        }
                    },
                    None => warn!(
                        "Received member remove event for guild {} but guild not in cache",
                        event.guild_id
                    ),
                }
            }

            Event::RoleCreate(event) => match self.get_guild(event.guild_id) {
                Some(guild) => {
                    guild
                        .roles
                        .insert(event.role.id, Arc::new(CachedRole::from_role(&event.role)));
                }
                None => warn!(
                    "Received role create event for guild {} but guild not in cache",
                    event.guild_id
                ),
            },
            Event::RoleUpdate(event) => match self.get_guild(event.guild_id) {
                Some(guild) => {
                    guild
                        .roles
                        .insert(event.role.id, Arc::new(CachedRole::from_role(&event.role)));
                }
                None => warn!(
                    "Received role update event for guild {} but guild not in cache",
                    event.guild_id
                ),
            },
            Event::RoleDelete(event) => match self.get_guild(event.guild_id) {
                Some(guild) => {
                    guild.roles.remove(&event.role_id);
                }
                None => warn!(
                    "Received role delete event for guild {} but guild not in cache",
                    event.guild_id
                ),
            },
            _ => {}
        }
        Ok(())
    }

    async fn member_update(
        shard_id: u64,
        ctx: &Arc<Context>,
        event: &Box<MemberUpdate>,
        retry: bool,
    ) -> bool {
        debug!("Member {} updated in {}", event.user.id, event.guild_id);
        match ctx.cache.get_guild(event.guild_id) {
            Some(guild) => {
                let member = guild.members.get(&event.user.id);
                if member.is_none() && retry {
                    return false;
                }
                match ctx.cache.get_user(event.user.id) {
                    Some(user) => {
                        if !user.is_same_as(&event.user) {
                            // Just update the global cache if it's different
                            // we will receive an event for all mutual servers if the inner user changed
                            ctx.cache.users.insert(
                                event.user.id,
                                Arc::new(CachedUser::from_user(&event.user)),
                            );
                        }
                    }
                    None => {
                        if guild.complete.load(Ordering::SeqCst) {
                            warn!(
                                "Received member update with uncached inner user: {}",
                                event.user.id
                            );
                            ctx.cache.get_or_insert_user(&event.user);
                        }
                    }
                }
                match member {
                    Some(member) => {
                        guild
                            .members
                            .insert(member.user.id, Arc::new(member.update(&*event, &ctx.cache)));
                    }
                    None => {
                        if guild.complete.load(Ordering::SeqCst) {
                            warn!(
                                "Received member update for unknown member {} in guild {}",
                                event.user.id, guild.id
                            );
                            let data = RequestGuildMembers::new_single_user_with_nonce(
                                guild.id,
                                event.user.id,
                                None,
                                Some(String::from("missing_user")),
                            );
                            let _ = ctx.cluster.command(shard_id, &data).await;
                        }
                    }
                }
            }
            None => {
                warn!(
                    "Received member update for uncached guild {}",
                    event.guild_id
                );
            }
        };
        true
    }

    // ###################
    // ## Cache updates ##
    // ###################

    fn nuke_guild_cache(&self, guild: &CachedGuild) {
        for channel in &guild.channels {
            self.guild_channels.remove(channel.key());
        }
        self.stats.channel_count.sub(guild.channels.len() as i64);
        for member in &guild.members {
            let remaining = member.user.mutual_servers.fetch_sub(1, Ordering::SeqCst);
            if remaining == 1 {
                self.users.remove(&member.user.id);
                self.stats.user_counts.unique.dec();
            }
        }
        self.stats.user_counts.total.sub(guild.members.len() as i64);
        for emoji in &guild.emoji {
            self.emoji.remove(&emoji.id);
        }
    }

    pub fn get_guild(&self, guild_id: GuildId) -> Option<Arc<CachedGuild>> {
        match self.guilds.get(&guild_id) {
            Some(guard) => Some(guard.value().clone()),
            None => None,
        }
    }

    fn guild_unavailable(&self, guild: &CachedGuild) {
        warn!(
            "Guild \"{}\" ({}) became unavailable due to outage",
            guild.name, guild.id
        );
        self.stats.guild_counts.outage.inc();
        let mut list = self.unavailable_guilds.write().unwrap();
        list.push(guild.id);
    }

    pub fn get_user(&self, user_id: UserId) -> Option<Arc<CachedUser>> {
        match self.users.get(&user_id) {
            Some(guard) => Some(guard.value().clone()),
            None => None,
        }
    }

    pub fn get_or_insert_user(&self, user: &User) -> Arc<CachedUser> {
        match self.get_user(user.id) {
            Some(user) => user,
            None => {
                let arc = Arc::new(CachedUser::from_user(user));
                self.users.insert(arc.id, arc.clone());
                self.stats.user_counts.unique.inc();
                arc
            }
        }
    }

    pub fn insert_private_channel(&self, private_channel: &PrivateChannel) -> Arc<CachedChannel> {
        let channel = CachedChannel::from_private(private_channel, self);
        let arced = Arc::new(channel);
        match arced.as_ref() {
            CachedChannel::DM { receiver, .. } => {
                self.dm_channels_by_user.insert(receiver.id, arced.clone());
            }
            _ => {}
        };
        self.private_channels.insert(arced.get_id(), arced.clone());
        arced
    }

    pub fn shard_cached(&self, shard_id: u64) -> bool {
        match self.missing_per_shard.get(&shard_id) {
            Some(atomic) => atomic.value().load(Ordering::Relaxed) == 0,
            None => true, // we cold resumed so have everything
        }
    }

    // ##################
    // ## Freeze cache ##
    // ##################

    pub async fn prepare_cold_resume(&self, redis: &ConnectionPool) -> (usize, usize) {
        // Clear global caches so arcs can be cleaned up
        self.guild_channels.clear();
        // We do not want to drag along DM channels, we get guild creates for them when they send a message anyways
        self.private_channels.clear();
        let mut tasks = vec![];
        let mut user_tasks = vec![];
        // Collect their work first before they start sabotaging each other again >.>
        let mut work_orders: Vec<Vec<GuildId>> = vec![];
        let mut count = 0;
        let mut list = vec![];
        for guard in self.guilds.iter() {
            count +=
                guard.members.len() + guard.channels.len() + guard.emoji.len() + guard.roles.len();
            list.push(guard.key().clone());
            if count > 100_000 {
                work_orders.push(list);
                list = vec![];
                count = 0;
            }
        }
        if list.len() > 0 {
            work_orders.push(list)
        }
        debug!("Freezing {} guilds", self.stats.guild_counts.loaded.get());
        for i in 0..work_orders.len() {
            tasks.push(self._prepare_cold_resume_guild(redis, work_orders[i].clone(), i));
        }
        let guild_chunks = tasks.len();
        future::join_all(tasks).await;
        count = 0;
        let user_chunks = (self.users.len() / 100_000 + 1) as usize;
        let mut user_work_orders: Vec<Vec<UserId>> = vec![vec![]; user_chunks];
        for guard in self.users.iter() {
            user_work_orders[count % user_chunks].push(guard.key().clone());
            count += 1;
        }
        debug!("Freezing {} users", self.users.len());
        for i in 0..user_chunks {
            user_tasks.push(self._prepare_cold_resume_user(redis, user_work_orders[i].clone(), i));
        }
        debug!("joining futures...");
        future::join_all(user_tasks).await;
        debug!("futures joined");
        self.users.clear();
        (guild_chunks, user_chunks)
    }

    async fn _prepare_cold_resume_guild(
        &self,
        redis: &ConnectionPool,
        todo: Vec<GuildId>,
        index: usize,
    ) -> Result<(), Error> {
        debug!(
            "Guild dumper {} started freezing {} guilds",
            index,
            todo.len()
        );
        println!("getting connection...");
        let mut connection = redis.get().await;
        println!("got connection");
        let mut to_dump = Vec::with_capacity(todo.len());
        for key in todo {
            let g = self.guilds.remove_take(&key).unwrap();
            to_dump.push(ColdStorageGuild::from(g));
        }
        let serialized = serde_json::to_string(&to_dump).unwrap();
        println!("set_and_expire...");
        connection
            .set_and_expire_seconds(
                format!("cb_cluster_{}_guild_chunk_{}", self.cluster_id, index),
                serialized,
                180,
            )
            .await?;
        println!(
            "stored in: cb_cluster_{}_guild_chunk_{}",
            self.cluster_id, index
        );
        Ok(())
    }

    async fn _prepare_cold_resume_user(
        &self,
        redis: &ConnectionPool,
        todo: Vec<UserId>,
        index: usize,
    ) -> Result<(), Error> {
        debug!("Worker {} freezing {} users", index, todo.len());
        let mut connection = redis.get().await;
        let mut chunk = Vec::with_capacity(todo.len());
        for key in todo {
            let user = self.users.remove_take(&key).unwrap();
            chunk.push(CachedUser {
                id: user.id.clone(),
                username: user.username.clone(),
                discriminator: user.discriminator.clone(),
                avatar: user.avatar.clone(),
                bot_user: user.bot_user,
                system_user: user.system_user,
                public_flags: user.public_flags.clone(),
                mutual_servers: AtomicU64::new(0),
            });
        }
        let serialized = serde_json::to_string(&chunk).unwrap();
        connection
            .set_and_expire_seconds(
                format!("cb_cluster_{}_user_chunk_{}", self.cluster_id, index),
                serialized,
                180,
            )
            .await?;
        Ok(())
    }

    // ###################
    // ## Defrost cache ##
    // ###################

    async fn defrost_users(&self, redis: &ConnectionPool, index: usize) -> BotResult<()> {
        let key = format!("cb_cluster_{}_user_chunk_{}", self.cluster_id, index);
        let mut connection = redis.get().await;
        let mut users: Vec<CachedUser> = serde_json::from_str(
            &String::from_utf8(connection.get(&key).await?.unwrap()).unwrap(),
        )?;
        connection.del(key).await?;
        debug!("Worker {} found {} users to defrost", index, users.len());
        for user in users.drain(..) {
            self.users.insert(user.id, Arc::new(user));
            self.stats.user_counts.unique.inc();
        }
        Ok(())
    }

    async fn defrost_guilds(&self, redis: &ConnectionPool, index: usize) -> BotResult<()> {
        let key = format!("cb_cluster_{}_guild_chunk_{}", self.cluster_id, index);
        let mut connection = redis.get().await;
        let mut guilds: Vec<ColdStorageGuild> = serde_json::from_str(
            &String::from_utf8(connection.get(&key).await?.unwrap()).unwrap(),
        )?;
        connection.del(key).await?;
        debug!("Worker {} found {} guilds to defrost", index, guilds.len());
        for cold_guild in guilds.drain(..) {
            let guild = CachedGuild::defrost(&self, cold_guild);
            for channel in &guild.channels {
                self.guild_channels
                    .insert(channel.get_id(), channel.value().clone());
            }
            self.stats.channel_count.add(guild.channels.len() as i64);
            for emoji in &guild.emoji {
                self.emoji.insert(emoji.id, emoji.clone());
            }
            self.stats.user_counts.total.add(guild.members.len() as i64);
            self.guilds.insert(guild.id, Arc::new(guild));
            self.stats.guild_counts.loaded.inc();
        }
        Ok(())
    }

    pub async fn restore_cold_resume(
        &self,
        redis: &ConnectionPool,
        guild_chunks: usize,
        user_chunks: usize,
    ) -> BotResult<()> {
        let mut user_defrosters = Vec::with_capacity(user_chunks);
        for i in 0..user_chunks {
            user_defrosters.push(self.defrost_users(redis, i));
        }
        for result in future::join_all(user_defrosters).await {
            if let Err(why) = result {
                return Err(Error::CacheDefrost("users", Box::new(why)));
            }
        }
        let mut guild_defrosters = Vec::with_capacity(guild_chunks);
        for i in 0..guild_chunks {
            guild_defrosters.push(self.defrost_guilds(redis, i));
        }
        for result in future::join_all(guild_defrosters).await {
            if let Err(why) = result {
                return Err(Error::CacheDefrost("guilds", Box::new(why)));
            }
        }
        self.filling.store(false, Ordering::SeqCst);
        info!(
            "Cache defrosting complete, now holding {} users ({} unique) from {} guilds, and {} channels",
            self.stats.user_counts.total.get(),
            self.stats.user_counts.unique.get(),
            self.stats.guild_counts.loaded.get(),
            self.stats.channel_count.get(),
        );
        Ok(())
    }
}

fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}

fn is_true(t: &bool) -> bool {
    !t
}

fn get_true() -> bool {
    true
}
