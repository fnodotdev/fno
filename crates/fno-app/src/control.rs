//! The control-channel consumer: drain the session's command file on a
//! poll cadence and apply the rows through the same paths clicks ride,
//! so an agent can focus, switch views and quit the app from the CLI.
use crate::window::Root;
use gpui::Context;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(400);

pub fn spawn_poll(session: String, cx: &mut Context<Root>) {
    cx.spawn(async move |this, cx| loop {
        cx.background_executor().timer(POLL).await;
        let cmds = fno::app_control::drain(&session);
        if cmds.is_empty() {
            continue;
        }
        if this
            .update(cx, |root, cx| root.apply_control(&cmds, cx))
            .is_err()
        {
            break;
        }
    })
    .detach();
}
