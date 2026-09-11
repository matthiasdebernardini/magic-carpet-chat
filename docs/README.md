# Magic Carpet Chat docs

An atlas of how the desktop app and its worker fit together. One data file feeds two views: an interactive isometric map and a text twin.

Open the map: serve this folder with any static server and open `atlas.html`. A `file://` open works in most browsers but the fonts may not load.

```sh
python3 -m http.server 8765 --directory docs
open http://localhost:8765/atlas.html
```

| File | Role | Edit it? |
|---|---|---|
| `atlas/data.mjs` | Single source of truth: structures, flows, chapters, decisions, questions | Yes |
| `atlas/template.html` + `atlas/build.mjs` | Renderer and generator | Presentation only |
| `atlas.html` | Built map | No, generated |
| `SYSTEM.md` | Built text twin | No, generated |
| `CONTEXT.md` | Glossary, one line per noun | By hand |

Deep links: `#ch=7` opens a chapter, `#sel=BW` pins a structure, `#in=RT` opens its steps, `#theme=light` forces the light palette. They combine with `&`.

Rebuild after every change to the data file:

```sh
node docs/atlas/build.mjs
```

Questions carry stable IDs (`Q-<code><n>`). Resolve or route a question in `data.mjs`; never delete one.
