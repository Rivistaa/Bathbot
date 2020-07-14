use crate::{
    core::{Command, CommandGroups, Context},
    BotResult, Error,
};

use std::sync::{atomic::Ordering, Arc};
use twilight::gateway::Event;
use uwl::Stream;

pub async fn handle_event(
    shard_id: u64,
    event: &Event,
    ctx: Arc<Context>,
    cmd_groups: Arc<CommandGroups>,
) -> BotResult<()> {
    match &event {
        // ####################
        // ## Gateway status ##
        // ####################
        Event::ShardReconnecting(_) => info!("Shard {} is attempting to reconnect", shard_id),
        Event::ShardResuming(_) => info!("Shard {} is resuming", shard_id),
        Event::Ready(_) => info!("Shard {} ready to go!", shard_id),
        Event::Resumed => info!("Shard {} successfully resumed", shard_id),
        Event::GatewayReconnect => info!("Gateway requested shard {} to reconnect", shard_id),
        Event::GatewayInvalidateSession(recon) => {
            if *recon {
                warn!(
                    "Gateway has invalidated session for shard {}, but its reconnectable",
                    shard_id
                );
            } else {
                return Err(Error::InvalidSession(shard_id));
            }
        }
        Event::GatewayHello(u) => {
            debug!("Registered with gateway {} on shard {}", u, shard_id);
        }

        // ###########
        // ## Other ##
        // ###########
        Event::MessageCreate(msg) => {
            ctx.stats.new_message(&ctx, msg);
            if msg.author.bot || msg.webhook_id.is_some() {
                return Ok(());
            }
            let prefixes = match msg.guild_id {
                Some(guild_id) => {
                    let guild = ctx.cache.get_guild(guild_id);
                    match guild {
                        Some(g) => {
                            if !g.complete.load(Ordering::SeqCst) {
                                debug!(
                                    "Message received in guild {} but guild not fully cached yet",
                                    g.id
                                );
                                return Ok(()); // not cached yet, just ignore for now
                            }
                        }
                        None => return Ok(()), // we didnt even get a guild create yet
                    }
                    let config = ctx.database.get_guild_config(guild_id.0).await?;
                    config.prefixes.clone()
                }
                None => vec!["<".to_owned(), "!!".to_owned()],
            };

            let mut stream = Stream::new(&msg.content);
            stream.take_while_char(|c| c.is_whitespace());
            if !(find_prefix(&prefixes, &mut stream) || msg.guild_id.is_none()) {
                return Ok(());
            }
            stream.take_while_char(|c| c.is_whitespace());
            match parse_invoke(&mut stream, &cmd_groups) {
                Invoke::Command(cmd) => debug!("Got command: {:?}", cmd),
                Invoke::Help(None) => debug!("Got help command"),
                Invoke::Help(Some(cmd)) => debug!("Got help command for {:?}", cmd),
                Invoke::FailedHelp(name) => debug!("Got failed help for `{}`", name),
                Invoke::UnrecognisedCommand(name) => {}
            }
        }
        _ => (),
    }
    Ok(())
}

pub fn find_prefix<'a>(prefixes: &[String], stream: &mut Stream<'a>) -> bool {
    let prefix = prefixes.iter().find_map(|p| {
        let peeked = stream.peek_for_char(p.chars().count());
        if p == peeked {
            Some(peeked)
        } else {
            None
        }
    });
    if let Some(prefix) = &prefix {
        stream.increment(prefix.chars().count());
    }
    prefix.is_some()
}

fn parse_invoke(stream: &mut Stream<'_>, groups: &CommandGroups) -> Invoke {
    let name = stream.peek_until_char(|c| c.is_whitespace()).to_lowercase();
    stream.increment(name.chars().count());
    stream.take_while_char(|c| c.is_whitespace());
    match name.as_str() {
        "h" | "help" => {
            let name = stream.peek_until_char(|c| c.is_whitespace()).to_lowercase();
            stream.increment(name.chars().count());
            stream.take_while_char(|c| c.is_whitespace());
            if name.is_empty() {
                Invoke::Help(None)
            } else if let Some(cmd) = groups.get(name.as_str()) {
                Invoke::Help(Some(cmd))
            } else {
                Invoke::FailedHelp(name)
            }
        }
        _ => {
            if let Some(cmd) = groups.get(name.as_str()) {
                let name = stream.peek_until_char(|c| c.is_whitespace()).to_lowercase();
                for sub_cmd in cmd.sub_commands {
                    if sub_cmd.names.contains(&name.as_str()) {
                        stream.increment(name.chars().count());
                        stream.take_while_char(|c| c.is_whitespace());
                        // TODO: Check permissions & co
                        // check_discrepancy(ctx, msg, config, &cmd.options)?;
                        return Invoke::Command(sub_cmd);
                    }
                }
                // TODO: Check permissions & co
                // check_discrepancy(ctx, msg, config, &cmd.options)?;
                Invoke::Command(cmd)
            } else {
                Invoke::UnrecognisedCommand(name)
            }
        }
    }
}

#[derive(Debug)]
pub enum Invoke {
    Command(&'static Command),
    Help(Option<&'static Command>),
    FailedHelp(String),
    UnrecognisedCommand(String),
}
