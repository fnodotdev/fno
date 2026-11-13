# Mux key reference

Generated from `default_bindings` and `preset_rows` in `crates/fno/src/keys.rs`. Do not edit by hand: an attended session rewrites it with `fno doctor test rust --manifest-path crates/fno/Cargo.toml --lib regenerate_key_reference -- --ignored`. An agent session runs no local tests: commit, push, and CI, or a test:priority grant.

Each row is one action id. Put the id in `~/.fno/keybindings.json` with a key string to rebind it. The file is one flat object, and `"preset"` is its one reserved key. A value of two to four keys split by spaces (`"g s"`) binds a multi-stroke chord. Its first key must not be bound alone. The mux reloads the file when it changes. The keys below come after the prefix.

| Action id | Built-in | tmux | zellij | Meaning |
|---|---|---|---|---|
| `split-right` | `%` | `%` | `r` | split right |
| `split-down` | `"` | `"` | `d` | split down |
| `split-up` | unbound | unbound | unbound | split up |
| `split-left` | unbound | unbound | unbound | split left |
| `focus-left` | `h` | `h` | `h` | focus left |
| `focus-down` | `j` | `j` | `j` | focus down |
| `focus-up` | `k` | `k` | `k` | focus up |
| `focus-right` | `l` | `_` | `l` | focus right |
| `resize-left` | unbound | unbound | unbound | resize left (ctrl+left, or bind a key) |
| `resize-down` | `J` | `J` | `J` | resize down |
| `resize-up` | `K` | `K` | `K` | resize up |
| `resize-right` | `L` | `L` | `L` | resize right |
| `close-pane` | `x` | `x` | `x` | close pane |
| `last-pane` | `;` | `;` | `;` | last pane |
| `swap-pane-prev` | `{` | `{` | `{` | swap with previous pane |
| `swap-pane-next` | `}` | `}` | `}` | swap with next pane |
| `zoom-pane` | unbound | `z` | unbound | zoom pane |
| `next-pane` | unbound | `o` | unbound | next pane |
| `diff-pane` | `D` | `D` | `D` | toggle git diff pane |
| `new-tab` | `c` | `c` | `c` | new tab |
| `new-workspace` | `N` | `N` | `N` | new workspace |
| `next-tab` | `n` | `n` | `n` | next tab |
| `prev-tab` | `p` | `p` | `p` | prev tab |
| `last-tab` | unbound | `l` | unbound | last tab |
| `close-tab` | `&` | `&` | `&` | close tab |
| `rename-tab` | `,` | `,` | `,` | rename tab |
| `cycle-section` | `z` | `u` | `z` | cycle active section |
| `move-tab-left` | `<` | `<` | `<` | move tab left |
| `move-tab-right` | `>` | `>` | `>` | move tab right |
| `move-tab-to` | `#` | `#` | `#` | move tab to an index |
| `prev-block` | `[` | `[` | `[` | jump prev block |
| `next-block` | `]` | `]` | `]` | jump next block |
| `select-block` | `v` | `v` | `v` | select block |
| `copy-selection` | `y` | `y` | `y` | copy selection |
| `redraw-pane` | `R` | `R` | `R` | ask the pane to repaint |
| `rerun-block` | `r` | `r` | `u` | rerun block |
| `search` | `/` | `/` | `/` | search scrollback |
| `find` | `f` | `f` | `f` | find: goto workspace/tab/pane/agent |
| `next-waiting` | `Z` | `Z` | `Z` | jump to the next agent waiting on you |
| `selector` | `w` | `w` | `w` | sideline row selector |
| `answers` | `a` | `a` | `a` | answer queue |
| `open-messages` | `M` | `M` | `M` | open Messages (mail) |
| `connections` | `A` | `A` | `A` | connections (accounts, combos) |
| `feed` | `e` | `e` | `e` | activity feed |
| `review-pr` | `I` | `I` | `I` | PR review beside the pane |
| `feedfocus` | `E` | `E` | `E` | focus the activity feed (arrows, enter) |
| `decisions` | `m` | `m` | `m` | decisions table (audit, retract, overrule) |
| `sideline-menu` | `P` | `P` | `P` | sideline menu (palette) |
| `add-remote-host` | `+` | `+` | `+` | add a remote host |
| `org` | `C` | `C` | `C` | the HQ modal (load, census, lanes) |
| `org-view` | `H` | `H` | `H` | HQ (fleet, machines, leads, seats) |
| `org-view-old-key` | `G` | `G` | `G` | HQ, the old key (goes away next release) |
| `notifications-bell` | `q` | `q` | `q` | open notifications for questions and announcements |
| `open-backlog-board` | `B` | `B` | `B` | open the backlog board |
| `open-backlog-board-old-key` | `O` | `O` | `O` | open the backlog board, the old key (goes away next release) |
| `cycle-sideline-view` | `V` | `V` | `V` | cycle sideline view (agents, backlog, HQ) |
| `toggle-sideline` | `b` | `b` | `b` | toggle sideline |
| `toggle-composer` | `i` | `i` | `i` | draft card in the focused workspace (reveals a hidden sideline) |
| `full-sideline` | `F` | `F` | `F` | full-screen sideline |
| `cycle-density` | `=` | `=` | `=` | cycle sideline density |
| `sort-agents` | `o` | `Q` | `o` | sort table columns |
| `cycle-agent-group` | `W` | `W` | `W` | group agents by workspace, lead, cwd, status, harness, or none |
| `toggle-card-head` | `Y` | `Y` | `Y` | show or hide the card header row |
| `toggle-group-by-lead` | `t` | `t` | `t` | nest workers under their lead in cards |
| `toggle-status` | `s` | `s` | `s` | toggle status |
| `settings` | `S` | `S` | `S` | settings (theme, toggles, prefix) |
| `sweep-threads` | `T` | `T` | `T` | sweep threads (close, reap) |
| `lift-hold` | `U` | `U` | `U` | lift the fleet hold (reserve, overload) |
| `share-focused-pane` | `X` | `X` | `X` | share the focused pane (experimental) |
| `show-pane-ids` | `\` | `\` | `\` | hold to show pane ids |
| `show-keys` | `?` | `?` | `?` | keys of this view (* for all) |
| `grab-work` | `g` | `g` | `g` | grab work (dispatch next ready) |
| `report` | `!` | `!` | `!` | report a problem |
| `detach` | `d` | `d` | `Q` | detach |
