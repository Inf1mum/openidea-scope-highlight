# Scope Highlight for OpenIDEA

Three Find Action commands that focus the code around your caret:

- **Scope Highlight: Highlight Current Scope** — paints the innermost bracket
  or tag pair enclosing the caret with the editor's `highlight` token.
- **Scope Highlight: Dim Outside Current Scope** — the same pair keeps its
  brackets marked while everything outside it is painted `muted`.
- **Scope Highlight: Clear Scope Highlighting** — restores normal presentation.
  The plugin API does not let a plugin bind a plain key such as `Escape`
  (plain keys are for typing), so Clear is a command you can bind to `Escape`
  yourself in the keymap editor; a plugin-bindable Escape is requested in the
  API-gap issue linked below.

Built for [OpenIDEA](https://github.com/l0ng-ai/openidea) on the plugin API's
[`decorations` interface](https://github.com/l0ng-ai/openidea/blob/main/wit/decorations.wit)
(`highlight` and `muted` styles). Implemented for
[l0ng-ai/openidea#3770](https://github.com/l0ng-ai/openidea/issues/3770).

## Behaviour

- Presentation only: text, selection and undo history are never touched.
- A scope clears when its document changes (a `document-changed`
  subscription) and on the Clear command.
- Every open file keeps its own scope: split editors of different files stay
  independent (the API keys decoration sets by path, so two editors of the
  same file share one scope), and the two paint commands are only offered
  while an editor is focused (`when = "editorFocus"`).
- Matching is text-level and bounded (`MAX_SCAN` UTF-16 units in each
  direction): `()`, `[]`, `{}`, and paired tags (`<div> … </div>`, JSX and
  HTML alike) are found even when the closer is far beyond any viewport
  lookahead. A candidate only counts when its closer exists, so a
  `Vec<u32>` is never mistaken for a tag pair, and one stray unmatched
  bracket does not hide the scope around it. A caret with no matched
  enclosing pair leaves the presentation unchanged.
- The scan does not know strings or comments, so a bracket inside a string
  can pair with one outside it; the pair is still only used when it closes.

## Development

```sh
cargo test                # unit tests against the SDK's mock host
openidea-plugin build     # → dist/plugin.wasm, then `check`
openidea-plugin link      # load the folder in the IDE, then Settings › Plugins › Reload
```

## License

MIT
