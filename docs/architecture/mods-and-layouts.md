# Mods and layouts

A mod is a panel a user adds to the fno mux. A layout is a whole-window arrangement whose slots name mods. Both are components of a function pack, so they install, verify and list through the one plugin host, `fno config plugins` ([function-pack-substrate](function-pack-substrate.md)). There is no second registry.

## The manifest

A pack declares mods and layouts in its `plugin.yaml` beside any other component. A pack of only mods is a valid pack.

```yaml
id: my-panels
version: 0.1.0
footnote_compat: {minimum: "0.3.0"}
mods:
  - id: verdicts
    title: Review verdicts
    summary: The newest review verdict on each branch.
    preview: mods/verdicts/preview.png
    recipe: mods/verdicts/RECIPE.md
    place: split            # split (default), popup or sideline
    feed:
      argv: [fno-agents, review-summary, --ledger]
      rows: ""              # JSON pointer to the row array; empty when the output is the array
      columns:
        - {key: /branch, title: branch}
        - {key: /verdict, title: verdict}
layouts:
  - id: review-desk
    title: Review desk
    summary: The calling pane on the left, open PRs on the right.
    preview: layouts/review-desk.png
    recipe: layouts/review-desk.md
    tree: {split: {axis: Horizontal, children: [{weight: 2, tree: {slot: main}}, {weight: 1, tree: {slot: side}}]}}
    slots:
      - {name: main, binding: anchor}
      - {name: side, binding: {mod: verdicts}}
```

A mod declares exactly one render path:

| Path | What runs | Starts |
|---|---|---|
| `feed` | One fno read from a fixed list. `fno config plugins open` draws its JSON rows as a table. No pack program runs. | on |
| `run` | The pack's own argv, from the pack directory. | off, until switched on |
| `native` | Nothing. It names a view the mux paints itself, such as `backlog-board`. | on, and stays on |

Verify checks each mod and layout. The id is one safe path component, and the mod has one render path. A feed starts with a listed read, such as `fno backlog board` or `fno-agents review-summary`, and carries no `--output` or `--fix` flag. The list is `FEED_READS` in `crates/fno-agents/src/spawn/plugins/ui.rs`. A feed starts on, so it must not write state on every redraw. A command outside the list belongs in a `run` mod, which starts off. Mod and layout ids share one id space, because a switch is keyed by id. The `preview` is a PNG under 2 MiB and 4096 pixels a side. The `recipe` exists. Both files sit inside the pack directory. A layout binds exactly one `anchor` slot, every tree leaf names a slot, and every `mod` binding names a mod of the same pack.

A pack that declares no mods or layouts keeps the digest it had before these fields existed.

## Commands

| Command | Does |
|---|---|
| `fno config plugins verify <dir> --json` | Verify; the JSON carries `mods` and `layouts` arrays. |
| `fno config plugins activate <dir>` | Install a local pack. |
| `fno config plugins install <git-url>` | Install a mods-and-layouts pack from a URL, behind consent. |
| `fno config plugins enable\|disable <pack>[/<id>]` | Switch one component, or every component of a pack. |
| `fno config plugins ls --json` | Each pack with its `mods`, `layouts` (preview and recipe as absolute paths, `enabled`, `runs_code`, `kind`) and `source`. |
| `fno config plugins open <pack>/<mod>` | Show one mod in this terminal. |
| `fno config plugins gallery [--json]` | Published packs from GitHub topics. Installs nothing. |

The mux settings modal has an Extensions tab. It reads the same `ls --json` feed as the GPUI app, so both show one list. Each mod row carries its switch, its preview and recipe (they open in the file viewer), and an `open in a pane` row. `install from URL` runs the install in a new pane, so the consent card and its prompt reach a real terminal. `browse published packs` opens the gallery in a pane too, so a slow network read never holds the mux.

## The trust boundary

A URL install runs third-party content, so no step before consent lets that content run.

1. Stage. A shallow clone with `core.hooksPath=/dev/null` and no submodules, into `<role root>/.packs/.staging-*`. No repo hook, submodule or script runs.
2. Measure. The manifest parses and verify passes on the staged tree. A sha256 covers every file: path and bytes, sorted, `.git` left out. A symlink refuses. Roles, skills, agents, adapters, workflows and evaluators reach agents. A URL pack that declares one refuses, so those install from a local path only.
3. Show. The consent card prints the URL, the commit and the content digest. It names every argv a mod runs or reads and every declared effect. It ends with "not reviewed by fno".
4. Consent. An attended terminal answers yes. Otherwise install files a `local.plugin-install` effect request bound to the content digest. It prints `fno inbox approvals decide <digest> --as <principal> --approve` and exits 2. A principal that config names for that class approves it, and the next install of the same commit proceeds. An agent can stage. Only the operator consents.
5. Commit. A URL pack never replaces a pack id installed from a local path or another URL. The tree moves to `<role root>/.packs/<pack>/`. The registry records the URL, the commit, the digest, the consenting principal and the time. Before `open` runs a command mod, it hashes the pack again and refuses on any change. A new commit is a new digest, so a new consent. Nothing updates on its own.

## Publish a pack

1. Put `plugin.yaml`, a `preview.png` per component and a `RECIPE.md` per component in a public git repo. The recipe says what the panel shows and every command it runs.
2. Tag the repo with the topic for what it carries: `fno-mod`, `fno-layout` or `fno-pack`.
3. Check it: `fno config plugins verify <repo dir> --json` exits 0.

The text an author can paste into an agent: "Install the fno pack at `<git URL>`: run `fno config plugins install <git URL>`, show me the consent card, and wait for my approval."

## Not yet

A layout slot that holds a mod is declared and verified, but `fno mux layout graft` cannot realize a `mod` binding yet. Native mods list and preview, and their views open from the mux as before.
