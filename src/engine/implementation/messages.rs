use super::super::*;
use crate::ipc::Refused;
use tracing::info;

impl Engine {
    /// The web API's `POST api/message`: `text` reaches the agent that acts
    /// on `item` the way the item's own activity does. The delivery path is
    /// the one a relayed comment takes (`deliver_to`), so a gone workspace
    /// and agent are brought back first, and a session at its sign-in prompt
    /// is told when it is signed in rather than being handed a prompt it
    /// cannot read.
    ///
    /// An item ssf holds with no agent has nothing to tell, and that is a
    /// refusal rather than a delivery nobody reads.
    pub(in crate::engine) async fn message(&mut self, item: &str, text: &str) -> Result<Value> {
        let (repo, number) = self.locate(item)?;
        let target = self.owner_of(&repo, number);
        let id = session_id(&repo.name, target);
        if !self.peek(&repo, target).is_some_and(|st| st.seeded) {
            anyhow::bail!(Refused::conflict(format!(
                "{id} has no agent: ssf holds no session for it, so there is nothing to tell \
(write it on the item, or assign a session with `ssf assign {id} --harness …`)"
            )));
        }
        let delivery = match self.deliver_to(&repo, number, text, None).await {
            Ok(delivery) => delivery,
            // Understood, and the item is not in a state that takes it: the
            // session is blocked, and the daemon's own words say on what.
            Err(error) if is_blocked(&error) => {
                anyhow::bail!(Refused::conflict(format!("{error:#}")));
            }
            Err(error) => return Err(error),
        };
        let e = self.entry(&repo, target);
        e.terminal_handle = Some(delivery.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        let title = e.title.clone();
        info!(
            session = id,
            chars = text.chars().count(),
            "message delivered to the item's agent"
        );
        Ok(serde_json::json!({
            "session": id,
            "title": title,
            "delivered": true,
        }))
    }
}
