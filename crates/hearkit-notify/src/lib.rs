pub mod discord;
pub mod email;
pub mod mattermost;
pub mod slack;

pub use discord::DiscordNotifier;
pub use email::EmailNotifier;
pub use mattermost::MattermostNotifier;
pub use slack::SlackNotifier;

use anyhow::Result;
use async_trait::async_trait;
use hearkit_llm::Analysis;

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn post_summary(&self, meeting_title: &str, analysis: &Analysis) -> Result<()>;
    fn name(&self) -> &str;
}
