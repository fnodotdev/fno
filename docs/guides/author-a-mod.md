# Author a mod pack

A pack is a directory with a `plugin.yaml` manifest and the files it names. Installing it teaches fno new panels: mods and layouts. A mod is one panel a host draws. A layout arranges whole panes. You write declarations, and fno draws them; the one exception is a command mod, which runs your program, and only after the user consents.

This page takes you from an empty directory to a mod you can see in the desktop app's Mods view. You need the fno CLI installed (`fno config plugins` is its door) and nothing else: a mod pack is plain files.

## The four mod kinds

Every mod declares exactly one render path. A manifest that declares two refuses to install, and `fno config plugins verify` names the problem.

### `run`: your program

| You declare | The user gets |
| --- | --- |
| `run: [./tool.sh]`, an argv the pack ships | Your program in a terminal pane, started only after consent |
| `preview` and `recipe` files | The preview image and the recipe's "What it shows" section in the detail pane |

A run mod is code. It starts off, and adding it shows the command and the commit before anything runs. Use it when the panel needs to compute.

### `feed`: a table from an fno read

| You declare | The user gets |
| --- | --- |
| `feed: {argv: [...], columns: [...]}`, an argv that starts with one of the allowlisted reads, such as `fno backlog board` | A live table the host redraws, with the columns you named |
| `rows`, a JSON pointer | Where the row array sits in that read's JSON output |

A feed mod runs no code of yours: its argv must be a known read, and a flag that writes (like `--output`) refuses. Use it when the panel is fno's own data, reshaped.

### `native`: a view fno already paints

| You declare | The user gets |
| --- | --- |
| `native: backlog-board`, the id of a built-in view | A row that opens that view in the app |

Use it to surface something fno already draws, such as the backlog board, as part of your pack.

### `view`: content you declare, the desktop app draws

| You declare | The user gets |
| --- | --- |
| `view: m/board-view.json`, a pack-relative path to a JSON file of blocks | The blocks drawn in order in the desktop app's detail pane |

A view mod is data all the way down: headings, text, lists, tables, key-value rows. Nothing runs, and no pane splits. Use it when the panel is content you can write down, such as release notes or a cheat sheet. The desktop app is the v1 renderer; the mux TUI lists view mods by kind and does not draw them yet.

## A minimal pack with one view mod

Create this layout in an empty directory:

```text
hello-view/
├── plugin.yaml
└── m/
    ├── preview.png
    ├── RECIPE.md
    └── board-view.json
```

The manifest, `plugin.yaml`:

```yaml
id: hello-view
version: 0.1.0
footnote_compat: {minimum: "0.1.0"}
mods:
  - id: release-notes
    title: Release notes
    summary: What this pack ships, as data
    preview: m/preview.png
    recipe: m/RECIPE.md
    view: m/board-view.json
```

Every path in a manifest is relative to the pack directory, and every file a mod names must exist inside it. The preview is a PNG. To make a one-pixel placeholder while you try things out:

```sh
echo 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==' | base64 --decode > m/preview.png
```

The recipe is a markdown file the detail pane reads from. A one-line file is enough:

```sh
echo '## What it shows' > m/RECIPE.md
echo '' >> m/RECIPE.md
echo 'The blocks declared in board-view.json, drawn in order.' >> m/RECIPE.md
```

The view file, `m/board-view.json`:

```json
{
  "title": "Release notes",
  "blocks": [
    { "kind": "heading", "text": "What shipped" },
    { "kind": "text", "text": "This view is data. Nothing here runs." },
    { "kind": "list", "items": ["Blocks draw in order", "An unknown block kind refuses to parse"] },
    { "kind": "key_value", "entries": [
      { "key": "kind", "value": "view" },
      { "key": "runs code", "value": "no" }
    ] },
    { "kind": "table", "columns": [
      { "key": "/day", "title": "Day" },
      { "key": "/note", "title": "Note" }
    ], "rows": [
      { "day": "Mon", "note": "first release" }
    ] }
  ]
}
```

A block is one of `heading`, `text`, `list`, `key_value` or `table`, tagged by its `kind`. A table's `columns` reuse the same column type a feed uses, and each column's `key` is a JSON pointer to a field of the row. Anything else refuses to parse, with an error that names the kind it does not know.

## Check the pack

```sh
fno config plugins verify hello-view
```

Every condition prints with its result, and `ok: True` means the pack installs cleanly. The check covers the manifest's shape, every file the mods name, the preview images, and the rule that a mod declares exactly one render path.

For a schema check outside fno, the published schema states the whole surface: `crates/fno-proto/schema/pack-ui.schema.json` in the [fno repository](https://github.com/fnodotdev/fno), or in the `fno-proto` crate's source on crates.io. The schema's roots are `Mod`, `Layout` and `View`, so the manifest's mod list and a standalone view file both validate against it. With [Node.js](https://nodejs.org) installed:

```sh
npx ajv-cli@5 validate -s pack-ui.schema.json -d hello-view/m/board-view.json
```

Editors that speak JSON Schema use the same file for completion while you write the view.

## Install and see it

From the pack's parent directory:

```sh
fno config plugins install ./hello-view
```

Nothing runs at install time for a view-only pack; the consent card is a command-mod ritual. List what installed:

```sh
fno config plugins ls --json
```

The row carries `"kind": "view"` and the resolved path to your view file. In the desktop app, open the Mods view from the title bar: the row sits under Installed, and selecting it draws your blocks in the detail pane. No Open button shows, because the view is already on the screen.

To share the pack, put the directory in a git repository and give people the URL:

```sh
fno config plugins install https://github.com/you/hello-view
```

A URL install shows the consent card before it copies anything, and records the commit it installed from. An edited tree refuses to launch until it is installed again.

## The version promise

The schema carries `pack_ui_schema_version`. While it is `1`, changes to these files' shape are additive only: a new optional field, a new block kind. A pack you write today keeps parsing. A change that removes or renames anything raises the version first, so you can read the number and know whether your files need work. The wire between fno processes has its own, separate version (`proto_version` in `mux-wire.schema.json`); the two numbers promise different files and never move together.
