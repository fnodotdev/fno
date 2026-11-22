//! The connect-a-host step: the left footer's host button opens it.
//! The fleet's hosts pick through, and the join line a new machine runs.
//! The connect work itself runs in the mux; this step is the app's own
//! door to it.

use gpui::{div, prelude::*, px, rgb, rgba, Context, Div, FontWeight, SharedString, Stateful};

use super::palette::Palette;
use super::ui::{self, Role};
use super::window::Root;

/// The step's state while it shows: the line under the join command.
pub(super) struct HostsState {
    pub note: Option<String>,
}

/// The join line a new machine runs: point its store at the fleet's
/// shared primary, then serve. None when this machine names no primary,
/// so the recipe names the missing key instead.
fn join_line(config: Option<&serde_json::Value>) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct JoinCfg {
        store: Option<StoreCfg>,
    }
    #[derive(serde::Deserialize)]
    struct StoreCfg {
        remote_url: Option<String>,
    }
    let parsed: JoinCfg = serde_json::from_value(config?.clone()).ok()?;
    let url = parsed.store?.remote_url.filter(|u| !u.trim().is_empty())?;
    Some(format!(
        "fno config set store.remote_url {url} && fno mux serve"
    ))
}

/// The step: the fleet's hosts and the join recipe, over a dim backdrop.
pub(super) fn render(root: &Root, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let Some(state) = root.hosts.as_ref() else {
        return div();
    };
    let panel = div()
        .id("hosts-panel")
        .flex()
        .flex_col()
        .w(px(560.0))
        .px_4()
        .py_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.panel))
        .font_family(ui::MONO)
        .text_size(px(ui::BODY))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(body(root, state, cx))
        .child(
            ui::role(div(), Role::Hint, &p)
                .pt(px(10.0))
                .child("Click a host to bring its seats forward · Esc to close"),
        );

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0000_0066))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|root, _, _, cx| root.close_hosts(cx)),
        )
        .child(panel)
}

/// The step's middle: the title, the fleet's hosts, the join recipe.
fn body(root: &Root, state: &HostsState, cx: &mut Context<Root>) -> Div {
    let p = root.palette;
    let mut panel = div().flex().flex_col().min_h_0();
    panel = panel.child(
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Connect a host"),
            )
            .child(ui::kbd("esc", &p)),
    );
    panel = panel.child(ui::role(div(), Role::Label, &p).pt(px(8.0)).child("Fleet"));
    let machines = root
        .board
        .fleet()
        .map(|f| f.machines.as_slice())
        .unwrap_or_default();
    if machines.is_empty() {
        panel = panel.child(
            ui::role(div(), Role::Hint, &p)
                .pt(px(4.0))
                .child("The fleet has not reported yet."),
        );
    }
    for m in machines {
        let name = m.display_name.clone().unwrap_or_else(|| m.host.clone());
        let seats: usize = m.leads.iter().map(|l| l.seats.len()).sum();
        let fresh = if m.fresh {
            "live".to_string()
        } else {
            format!("seen {} ago", ui::age_secs(m.age_s.max(0) as u64))
        };
        let mark = if m.local { " · this machine" } else { "" };
        panel = panel.child(host_row(
            &name,
            &format!("{seats} seats · {fresh}{mark}"),
            &p,
            cx,
        ));
    }
    panel = panel.child(
        ui::role(div(), Role::Label, &p)
            .pt(px(12.0))
            .child("Add a host"),
    );
    let config = crate::global_config_raw();
    match join_line(config.as_ref()) {
        Some(line) => {
            panel = panel.child(
                ui::role(div(), Role::Meta, &p)
                    .pt(px(4.0))
                    .child("On the new machine, run:"),
            );
            panel = panel.child(join_row(&line, &p, cx));
            if let Some(note) = state.note.as_deref() {
                panel = panel.child(
                    ui::role(div(), Role::Hint, &p)
                        .pt(px(4.0))
                        .child(note.to_string()),
                );
            }
        }
        None => {
            panel =
                panel.child(ui::role(div(), Role::Meta, &p).pt(px(4.0)).child(
                    "This machine names no fleet primary. Set store.remote_url to add hosts.",
                ));
        }
    }
    panel
}

/// One fleet host: the name on the left, its seats and freshness on the
/// right. Clicking it picks the host.
fn host_row(name: &str, meta: &str, p: &Palette, cx: &mut Context<Root>) -> Stateful<Div> {
    let picked = name.to_string();
    let id = SharedString::from(format!("hosts-row-{name}"));
    ui::clickable(
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(6.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .child(ui::clamp(name.to_string(), Role::Name, p).flex_1())
            .child(ui::clamp(meta.to_string(), Role::Meta, p)),
        p,
    )
    .on_click(cx.listener(move |root, _, _, cx| root.pick_host(picked.clone(), cx)))
}

/// The join line with its copy button. The line wraps at the panel
/// width; Copy holds the whole line either way.
fn join_row(line: &str, p: &Palette, cx: &mut Context<Root>) -> Div {
    let copied = line.to_string();
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.0))
        .pt(px(4.0))
        .child(ui::role(div().flex_1().min_w_0(), Role::Body, p).child(line.to_string()))
        .child(
            ui::clickable(
                div()
                    .id("hosts-copy")
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .child(ui::text("Copy", Role::Label, p)),
                p,
            )
            .on_click(cx.listener(move |root, _, _, cx| root.copy_join(copied.clone(), cx))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_join_line_names_the_primary_and_none_names_nothing() {
        let with = serde_json::json!({"store": {"remote_url": "https://primary.example"}});
        assert_eq!(
            join_line(Some(&with)),
            Some(
                "fno config set store.remote_url https://primary.example && fno mux serve"
                    .to_string()
            )
        );
        let bare = serde_json::json!({"store": {}});
        assert_eq!(join_line(Some(&bare)), None);
        assert_eq!(join_line(None), None);
    }
}
