//! An example Sideband bot on the native QUIC protocol (`docs/bots.md`).
//!
//! It answers `!echo <text>` with the text and `!roll NdM` (for example
//! `!roll 2d6`) with the dice and their sum, in the channel the command came
//! from. [`reply`] is the pure part; [`run`] wires it to a signed-in
//! [`Client`]. `src/main.rs` signs in with a bot token from the environment.

use ss_client_core::{Client, ClientEvent, v1};
use ss_core::Id;

/// Most dice in one roll, and most sides on one die.
const MAX_DICE: u32 = 20;
const MAX_SIDES: u32 = 1000;

/// The answer to one message, if it is a command this bot knows.
/// `roll(sides)` returns a number from 1 to `sides`.
pub fn reply(content: &str, mut roll: impl FnMut(u32) -> u32) -> Option<String> {
    let content = content.trim();
    if let Some(text) = content.strip_prefix("!echo ") {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_owned());
    }
    let spec = content.strip_prefix("!roll")?.trim();
    let spec = if spec.is_empty() { "1d6" } else { spec };
    let (count, sides) = spec.split_once('d')?;
    let count: u32 = if count.is_empty() {
        1
    } else {
        count.parse().ok()?
    };
    let sides: u32 = sides.parse().ok()?;
    if !(1..=MAX_DICE).contains(&count) || !(2..=MAX_SIDES).contains(&sides) {
        return Some(format!(
            "I roll 1 to {MAX_DICE} dice of 2 to {MAX_SIDES} sides."
        ));
    }
    let dice: Vec<u32> = (0..count).map(|_| roll(sides)).collect();
    let total: u32 = dice.iter().sum();
    let shown: Vec<String> = dice.iter().map(u32::to_string).collect();
    Some(format!("{spec}: {} = {total}", shown.join(" + ")))
}

/// A fair roll from 1 to `sides`, from the operating system's randomness.
pub fn fair(sides: u32) -> u32 {
    let mut bytes = [0_u8; 8];
    // A failed random source rolls a 1 rather than stopping the bot.
    let _ = getrandom::fill(&mut bytes);
    let value = u64::from_le_bytes(bytes) % u64::from(sides.max(1));
    u32::try_from(value).unwrap_or(0) + 1
}

/// Answers commands until the client stops. Ignores the bot's own messages.
pub async fn run(client: &Client) -> anyhow::Result<()> {
    let mut events = client.subscribe();
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            // Fell behind the client's event buffer: skip ahead.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        };
        match event {
            ClientEvent::MessageReceived { channel, message } => {
                if let Some(text) = answer(client, channel, message) {
                    let _sent = client
                        .send_message(v1::SendMessage {
                            channel_id: channel.as_bytes().to_vec().into(),
                            content: text,
                            ..Default::default()
                        })
                        .await?;
                }
            }
            ClientEvent::Removed { .. } => return Ok(()),
            _ => {}
        }
    }
}

/// The reply to `message` in `channel`, unless the bot wrote it.
fn answer(client: &Client, channel: Id, message: Id) -> Option<String> {
    let state = client.state();
    let state = state.read();
    let me = state.own_user.as_ref()?.id.clone();
    let found = state
        .messages
        .get(&channel)?
        .ordered()
        .into_iter()
        .find(|cached| cached.id.as_ref() == message.as_bytes())?;
    if found.author.as_ref().is_some_and(|author| author.id == me) {
        return None;
    }
    reply(&found.content, fair)
}

#[cfg(test)]
mod tests {
    use super::reply;

    #[test]
    fn echoes_and_rolls() {
        assert_eq!(reply("!echo  hello ", |_| 1).as_deref(), Some("hello"));
        assert_eq!(reply("!roll 2d6", |_| 3).as_deref(), Some("2d6: 3 + 3 = 6"));
        assert_eq!(reply("!roll", |_| 4).as_deref(), Some("1d6: 4 = 4"));
        assert!(reply("!roll 99d6", |_| 1).unwrap().starts_with("I roll"));
        assert_eq!(reply("hello", |_| 1), None);
        assert_eq!(reply("!roll dice", |_| 1), None);
    }
}
