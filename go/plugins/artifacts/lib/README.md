# Vendored OpenUI viewer

`openui-bundle.min.js` and `openui-styles.css` are vendored, unmodified
build outputs of `@openuidev/browser-bundle` 0.1.1 from
[thesysdev/openui](https://github.com/thesysdev/openui), added in commit
f53cf694. The bundle contains OpenUI's renderer and component library plus
React; it is embedded into the `bough` binary (`//go:embed` in
`../artifacts.go`) and served at `/artifacts/_lib/`.

`openui-prompt.md` is the library's generated language reference with its
framing removed.

License: MIT, Copyright (c) Thesys Inc. — full text in `LICENSE.openui`.
React (MIT, Meta Platforms, Inc.) and the other bundled packages are
listed in the repository's `THIRD_PARTY_NOTICES.md`.

To update: replace the files with the matching build of a newer
`@openuidev/browser-bundle`, update the version here, and keep
`LICENSE.openui` in step with upstream.
